//! Azure Storage Shared Key authorization (Blob service, version 2015-02-21 and later), as
//! the Go SDK's `SharedKeyCredential` computes it.
//! <https://learn.microsoft.com/rest/api/storageservices/authorize-with-shared-key>

use dl_encoding::base64::STANDARD;
use dl_hash::{Hmac, Sha256};

use crate::Error;
use crate::schedule::cron::civil_from_days;

/// An account name and its decoded key.
#[derive(Clone, PartialEq, Eq)]
pub struct SharedKey {
    account: String,
    key: Vec<u8>,
}

impl std::fmt::Debug for SharedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedKey")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

/// The RFC 1123 date Azure expects in `x-ms-date`.
pub fn http_date(secs: i64) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let t = secs.rem_euclid(86_400);
    format!(
        "{}, {d:02} {} {y:04} {:02}:{:02}:{:02} GMT",
        DAYS[(days + 4).rem_euclid(7) as usize],
        MONTHS[(m - 1) as usize],
        t / 3600,
        t / 60 % 60,
        t % 60
    )
}

fn header<'a>(headers: &'a [(&str, &str)], name: &str) -> &'a str {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| *v)
        .unwrap_or("")
}

impl SharedKey {
    /// `key` is the base64 account key from the connection string.
    pub fn new(account: &str, key_b64: &str) -> Result<SharedKey, Error> {
        let key = STANDARD
            .decode(key_b64.trim())
            .map_err(|_| Error::Upload("azure blob: AccountKey is not valid base64".into()))?;
        Ok(SharedKey {
            account: account.to_owned(),
            key,
        })
    }

    /// The account name.
    pub fn account(&self) -> &str {
        &self.account
    }

    /// The string to sign for a request to `escaped_path` with raw query `query`.
    pub fn string_to_sign(
        &self,
        method: &str,
        headers: &[(&str, &str)],
        escaped_path: &str,
        query: &str,
    ) -> String {
        let length = match header(headers, "content-length") {
            "0" => "",
            l => l,
        };
        let mut ms: Vec<(String, &str)> = headers
            .iter()
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), *v))
            .filter(|(k, _)| k.starts_with("x-ms-"))
            .collect();
        ms.sort();
        let canonical_headers: Vec<String> = ms.iter().map(|(k, v)| format!("{k}:{v}")).collect();
        [
            method,
            header(headers, "content-encoding"),
            header(headers, "content-language"),
            length,
            header(headers, "content-md5"),
            header(headers, "content-type"),
            "",
            header(headers, "if-modified-since"),
            header(headers, "if-match"),
            header(headers, "if-none-match"),
            header(headers, "if-unmodified-since"),
            header(headers, "range"),
            &canonical_headers.join("\n"),
            &self.canonical_resource(escaped_path, query),
        ]
        .join("\n")
    }

    fn canonical_resource(&self, escaped_path: &str, query: &str) -> String {
        let path = if escaped_path.is_empty() {
            "/"
        } else {
            escaped_path
        };
        let mut out = format!("/{}{path}", self.account);
        let mut params: Vec<(String, Vec<String>)> = Vec::new();
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let (k, v) = (decode(k), decode(v));
            match params.iter_mut().find(|(n, _)| *n == k) {
                Some((_, vs)) => vs.push(v),
                None => params.push((k, vec![v])),
            }
        }
        params.sort();
        for (k, mut vs) in params {
            vs.sort();
            out.push_str(&format!("\n{}:{}", k.to_ascii_lowercase(), vs.join(",")));
        }
        out
    }

    /// The `Authorization` header value.
    pub fn authorization(
        &self,
        method: &str,
        headers: &[(&str, &str)],
        escaped_path: &str,
        query: &str,
    ) -> String {
        let to_sign = self.string_to_sign(method, headers, escaped_path, query);
        let mac = Hmac::<Sha256>::mac(&self.key, to_sign.as_bytes());
        format!("SharedKey {}:{}", self.account, STANDARD.encode(mac))
    }
}

fn decode(s: &str) -> String {
    let plus = s.replace('+', " ");
    match dl_http::percent_decode(&plus) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(_) => plus,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured from the Go SDK (azblob v1.6.0 SharedKeyCredential) through a recording
    // transport; tests/fixtures/make-azure-vectors.go.txt is the generator.
    const KEY: &str = "ZGwtYWdlbnQtYmFja3VwIHRlc3Qga2V5LCBub3QgYSBzZWNyZXQ=";
    const DATE: &str = "Wed, 18 Nov 2026 23:50:47 GMT";
    const PATH: &str = "/backups/db%201.bak";

    fn auth(method: &str, headers: &[(&str, &str)], query: &str) -> String {
        SharedKey::new("devacct", KEY)
            .unwrap()
            .authorization(method, headers, PATH, query)
    }

    #[test]
    fn put_blob_vector() {
        let h = [
            ("accept", "application/xml"),
            ("content-length", "5"),
            ("content-type", "application/octet-stream"),
            ("x-ms-date", DATE),
            ("x-ms-blob-type", "BlockBlob"),
            ("x-ms-version", "2025-01-05"),
        ];
        assert_eq!(
            auth("PUT", &h, ""),
            "SharedKey devacct:AESQQ9Yd65hFHZYhpA+goihtvfYOPWgvRHA8WGbl+JA="
        );
    }

    #[test]
    fn put_block_and_block_list_vectors() {
        let h = [
            ("content-length", "5"),
            ("content-type", "application/octet-stream"),
            ("x-ms-date", DATE),
            ("x-ms-version", "2025-01-05"),
        ];
        assert_eq!(
            auth("PUT", &h, "blockid=YmxvY2stMDAwMDAwMDE%3D&comp=block"),
            "SharedKey devacct:+pIVrjbMSpB2claXkO5gstBoVkZmocF6IUrTpCSDCrc="
        );
        let h = [
            ("content-length", "99"),
            ("content-type", "application/xml"),
            ("x-ms-date", DATE),
            ("x-ms-version", "2025-01-05"),
        ];
        assert_eq!(
            auth("PUT", &h, "comp=blocklist"),
            "SharedKey devacct:q7piD0ROLWakY51d/uvpqWoDEFHjd2//h3GpU3VPYOM="
        );
    }

    #[test]
    fn get_and_delete_vectors() {
        let h = [
            ("x-ms-date", DATE),
            ("x-ms-range", "bytes=0-4"),
            ("x-ms-version", "2025-01-05"),
        ];
        assert_eq!(
            auth("GET", &h, ""),
            "SharedKey devacct:MSydjr82PucGsGTJS7dwm/+9MnTl5YX1jh17dQSdDuc="
        );
        let h = [
            ("x-ms-date", DATE),
            ("x-ms-version", "2025-01-05"),
            ("content-length", "0"),
        ];
        assert_eq!(
            auth("DELETE", &h, ""),
            "SharedKey devacct:iB1nhnYe8xQ0FHNq3AE9l1Q/4X5uXAmEs8LBG9yiG0s="
        );
    }

    #[test]
    fn dates_and_secrets() {
        assert_eq!(http_date(1_795_045_847), DATE);
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        let k = SharedKey::new("devacct", KEY).unwrap();
        assert!(!format!("{k:?}").contains("ZGwt"));
        assert!(SharedKey::new("a", "not base64!").is_err());
    }
}
