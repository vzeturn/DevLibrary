//! AWS Signature Version 4 for S3 requests (header signing, signed payload).
//! <https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-header-based-auth.html>

use dl_encoding::hex;
use dl_hash::{Digest, Hmac, Sha256};

use crate::schedule::cron::civil_from_days;

/// SHA-256 of an empty payload.
pub const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// The static credentials and scope a request is signed for.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    /// Access key id (not secret, but kept out of logs anyway).
    pub access_key: String,
    /// Secret access key.
    pub secret_key: String,
    /// Region (`auto` for R2).
    pub region: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

/// `YYYYMMDDTHHMMSSZ` for `secs` since the Unix epoch.
pub fn amz_date(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        t / 3600,
        t / 60 % 60,
        t % 60
    )
}

/// Hex SHA-256 of `data` (the `x-amz-content-sha256` value).
pub fn payload_hash(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// URI-encodes per SigV4: unreserved characters stay, everything else is `%XX`;
/// `/` stays when `keep_slash` (object keys in the path).
pub fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if unreserved || (keep_slash && b == b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The canonical query string: names and values encoded, sorted by name then value.
pub fn canonical_query(params: &[(&str, &str)]) -> String {
    let mut enc: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| (uri_encode(k, false), uri_encode(v, false)))
        .collect();
    enc.sort();
    enc.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// One request to sign. `headers` must include `host`, `x-amz-date` and
/// `x-amz-content-sha256`; names are matched case-insensitively.
pub struct Canonical<'a> {
    /// `GET`, `PUT`, …
    pub method: &'a str,
    /// The encoded path, starting with `/`.
    pub uri: &'a str,
    /// The canonical query string ([`canonical_query`]).
    pub query: &'a str,
    /// Header name/value pairs to sign.
    pub headers: &'a [(&'a str, &'a str)],
    /// Hex SHA-256 of the payload.
    pub payload_hash: &'a str,
}

/// The `Authorization` header value for `req`, signed at `amz_date` for service `s3`.
pub fn authorization(req: &Canonical<'_>, creds: &Credentials, amz_date: &str) -> String {
    let mut hs: Vec<(String, String)> = req
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), collapse(v)))
        .collect();
    hs.sort();
    let canonical_headers: String = hs.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed: Vec<&str> = hs.iter().map(|(k, _)| k.as_str()).collect();
    let signed = signed.join(";");
    let canonical = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        req.method, req.uri, req.query, canonical_headers, signed, req.payload_hash
    );
    let day = &amz_date[..8.min(amz_date.len())];
    let scope = format!("{day}/{}/s3/aws4_request", creds.region);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex::encode(Sha256::digest(canonical.as_bytes()))
    );
    let k_date = Hmac::<Sha256>::mac(format!("AWS4{}", creds.secret_key).as_bytes(), day);
    let k_region = Hmac::<Sha256>::mac(k_date.as_ref(), &creds.region);
    let k_service = Hmac::<Sha256>::mac(k_region.as_ref(), "s3");
    let k_signing = Hmac::<Sha256>::mac(k_service.as_ref(), "aws4_request");
    let signature = hex::encode(Hmac::<Sha256>::mac(k_signing.as_ref(), to_sign.as_bytes()));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope},SignedHeaders={signed},Signature={signature}",
        creds.access_key
    )
}

/// Trims and collapses runs of spaces, as SigV4 canonical header values require.
fn collapse(v: &str) -> String {
    v.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // The examples in the S3 SigV4 documentation ("Examples: Signature Calculations in AWS
    // Signature Version 4", header-based authentication).
    fn creds() -> Credentials {
        Credentials {
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            region: "us-east-1".into(),
        }
    }
    const HOST: &str = "examplebucket.s3.amazonaws.com";
    const DATE: &str = "20130524T000000Z";

    fn signature(auth: &str) -> &str {
        auth.rsplit("Signature=").next().unwrap()
    }

    #[test]
    fn amz_date_formats_utc() {
        assert_eq!(amz_date(1_369_353_600), DATE);
        assert_eq!(amz_date(0), "19700101T000000Z");
    }

    #[test]
    fn get_object_vector() {
        let headers = [
            ("Host", HOST),
            ("Range", "bytes=0-9"),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", DATE),
        ];
        let req = Canonical {
            method: "GET",
            uri: "/test.txt",
            query: "",
            headers: &headers,
            payload_hash: EMPTY_SHA256,
        };
        let auth = authorization(&req, &creds(), DATE);
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,\
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,\
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn put_object_vector() {
        let body = b"Welcome to Amazon S3.";
        let hash = payload_hash(body);
        assert_eq!(
            hash,
            "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072"
        );
        let headers = [
            ("Date", "Fri, 24 May 2013 00:00:00 GMT"),
            ("Host", HOST),
            ("x-amz-content-sha256", hash.as_str()),
            ("x-amz-date", DATE),
            ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
        ];
        let uri = format!("/{}", uri_encode("test$file.text", true));
        assert_eq!(uri, "/test%24file.text");
        let req = Canonical {
            method: "PUT",
            uri: &uri,
            query: "",
            headers: &headers,
            payload_hash: &hash,
        };
        assert_eq!(
            signature(&authorization(&req, &creds(), DATE)),
            "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
        );
    }

    #[test]
    fn get_bucket_lifecycle_vector() {
        let headers = [
            ("Host", HOST),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", DATE),
        ];
        let query = canonical_query(&[("lifecycle", "")]);
        assert_eq!(query, "lifecycle=");
        let req = Canonical {
            method: "GET",
            uri: "/",
            query: &query,
            headers: &headers,
            payload_hash: EMPTY_SHA256,
        };
        assert_eq!(
            signature(&authorization(&req, &creds(), DATE)),
            "fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543"
        );
    }

    #[test]
    fn list_objects_vector() {
        let headers = [
            ("Host", HOST),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", DATE),
        ];
        let query = canonical_query(&[("prefix", "J"), ("max-keys", "2")]);
        assert_eq!(query, "max-keys=2&prefix=J");
        let req = Canonical {
            method: "GET",
            uri: "/",
            query: &query,
            headers: &headers,
            payload_hash: EMPTY_SHA256,
        };
        assert_eq!(
            signature(&authorization(&req, &creds(), DATE)),
            "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
        );
    }

    #[test]
    fn encoding_rules() {
        assert_eq!(uri_encode("a b/c~d", true), "a%20b/c~d");
        assert_eq!(uri_encode("a b/c", false), "a%20b%2Fc");
        assert_eq!(uri_encode("đ", false), "%C4%91");
        assert_eq!(collapse("  a   b "), "a b");
        assert!(!format!("{:?}", creds()).contains("EXAMPLEKEY"));
    }
}
