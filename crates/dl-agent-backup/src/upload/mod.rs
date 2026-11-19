//! Where finished backups go. One [`Uploader`] per destination kind: a local (or mapped)
//! folder, S3-compatible storage (AWS S3, Cloudflare R2, MinIO; multipart above 16 MiB) and
//! Azure Blob (block blobs above 32 MiB), Google Drive (resumable uploads) and OneDrive
//! (Graph upload sessions; parallel parts above 20 MiB). A provider this build does not know
//! fails with `backup_destination_unsupported`; a new destination kind is one [`Kind`]
//! variant, one arm in [`Real::open`] and one module.

pub mod azure;
pub mod gdrive;
pub mod local;
pub mod oauth;
pub mod onedrive;
pub mod s3;
pub mod sharedkey;
pub mod sigv4;

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use dl_json::{Map, Value};
use dl_tds::CancelToken;

use crate::Error;

/// Progress callback: bytes sent so far, total bytes.
pub type Progress<'a> = &'a mut dyn FnMut(u64, u64);

/// One destination's operations.
pub trait Uploader: Send {
    /// Uploads `file` and returns the cloud path the hub stores (`s3://bucket/key`, a blob
    /// URL without any SAS token, or a folder path).
    fn upload(
        &self,
        file: &Path,
        cancel: &CancelToken,
        progress: Progress<'_>,
    ) -> Result<String, Error>;
    /// Downloads `cloud_path` into `dest`, returning the bytes written.
    fn download(&self, cloud_path: &str, dest: &Path, cancel: &CancelToken) -> Result<u64, Error>;
    /// Deletes `cloud_path`; a copy that is already gone counts as deleted.
    fn delete(&self, cloud_path: &str) -> Result<(), Error>;
}

/// Opens uploaders for destinations; [`Real`] in production, fakes in tests.
pub trait Uploaders: Send + Sync {
    /// The uploader for `dest`.
    fn open(&self, dest: &Destination) -> Result<Box<dyn Uploader>, Error>;
}

/// The real destination kinds.
#[derive(Debug, Default, Clone, Copy)]
pub struct Real;

impl Uploaders for Real {
    fn open(&self, dest: &Destination) -> Result<Box<dyn Uploader>, Error> {
        match Kind::of(&dest.provider) {
            Kind::Local => Ok(Box::new(local::Local::from_tokens(&dest.tokens)?)),
            Kind::S3 => Ok(Box::new(s3::S3::from_tokens(&dest.tokens)?)),
            Kind::Azure => Ok(Box::new(azure::Azure::from_tokens(&dest.tokens)?)),
            Kind::GoogleDrive => Ok(Box::new(gdrive::GDrive::from_tokens(&dest.tokens)?)),
            Kind::OneDrive => Ok(Box::new(onedrive::OneDrive::from_tokens(&dest.tokens)?)),
            Kind::Unknown => Err(Error::Unsupported(format!(
                "unknown destination provider {:?}",
                dest.provider
            ))),
        }
    }
}

/// Destination kinds by the hub's provider name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `LocalFolder` / `Local`.
    Local,
    /// `AWSS3` (also `S3`, `R2`).
    S3,
    /// `AzureBlob`.
    Azure,
    /// `GoogleDrive`.
    GoogleDrive,
    /// `OneDrive`.
    OneDrive,
    /// Anything else.
    Unknown,
}

impl Kind {
    /// The kind for a provider name (case-insensitive).
    pub fn of(provider: &str) -> Kind {
        match provider.trim().to_ascii_lowercase().as_str() {
            "local" | "localfolder" | "folder" => Kind::Local,
            "awss3" | "s3" | "r2" | "cloudflarer2" => Kind::S3,
            "azureblob" | "azure" => Kind::Azure,
            "googledrive" | "gdrive" => Kind::GoogleDrive,
            "onedrive" => Kind::OneDrive,
            _ => Kind::Unknown,
        }
    }
}

/// A destination with its decrypted tokens (secrets; never logged or reported).
#[derive(Clone, PartialEq)]
pub struct Destination {
    /// The hub's provider name.
    pub provider: String,
    /// The decrypted token object (`accessKeyId`, `connectionString`, …) with the
    /// destination's `endpoint` merged in.
    pub tokens: Value,
}

impl std::fmt::Debug for Destination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Destination")
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

impl Destination {
    /// Builds a destination from the decrypted token blob, as Go's `buildCloudCreds`: the
    /// row's provider and endpoint win over the blob's.
    pub fn from_plain(
        provider: &str,
        plain_tokens: &str,
        endpoint: Option<&str>,
    ) -> Result<Destination, Error> {
        let mut tokens = dl_json::from_str::<Value>(plain_tokens)
            .map_err(|_| Error::Credential("cloud tokens are not a JSON object".into()))?;
        let Value::Object(map) = &mut tokens else {
            return Err(Error::Credential(
                "cloud tokens are not a JSON object".into(),
            ));
        };
        if let Some(e) = endpoint.map(str::trim).filter(|e| !e.is_empty()) {
            map.insert("endpoint", e);
        }
        let provider = if provider.is_empty() {
            text(&tokens, "provider")
        } else {
            provider.to_owned()
        };
        Ok(Destination { provider, tokens })
    }

    /// The JSON kept in a queued job.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("provider", self.provider.as_str());
        m.insert("tokens", self.tokens.clone());
        Value::Object(m)
    }

    /// Reads [`Destination::to_json`].
    pub fn from_json(v: &Value) -> Option<Destination> {
        Some(Destination {
            provider: v.get("provider")?.as_str()?.to_owned(),
            tokens: v.get("tokens")?.clone(),
        })
    }
}

/// A string field of a token object, empty when missing.
pub fn text(tokens: &Value, key: &str) -> String {
    tokens
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// A boolean field of a token object.
pub fn flag(tokens: &Value, key: &str) -> bool {
    tokens.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// The file name of a local path (Go's `filepath.Base`).
pub fn base_name(file: &Path) -> Result<String, Error> {
    file.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .ok_or_else(|| Error::Upload(format!("backup file {} has no usable name", file.display())))
}

/// The HTTP client the cloud uploaders use: generous per-request deadline, parts fit the
/// body limit.
pub fn http_client() -> dl_http::Client {
    dl_http::Client::with_options(dl_http::ClientOptions {
        timeout: Some(Duration::from_secs(15 * 60)),
        connect_timeout: Some(Duration::from_secs(15)),
        max_body: 48 * 1024 * 1024,
        user_agent: Some(concat!("dl-agent-backup/", env!("CARGO_PKG_VERSION")).to_owned()),
        ..dl_http::ClientOptions::default()
    })
}

/// Reads up to `buf.len()` bytes, fewer only at end of file.
pub fn read_full(f: &mut File, buf: &mut [u8]) -> Result<usize, Error> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Error::Upload(format!("read backup file: {e}"))),
        }
    }
    Ok(n)
}

/// Downloads with ranged GETs of `chunk` bytes until the object ends. `fetch(start, end)`
/// returns the bytes and the object's total size from `Content-Range`.
pub fn ranged_download(
    dest: &Path,
    chunk: u64,
    cancel: &CancelToken,
    mut fetch: impl FnMut(u64, u64) -> Result<(Vec<u8>, Option<u64>), Error>,
) -> Result<u64, Error> {
    let mut out =
        File::create(dest).map_err(|e| Error::Upload(format!("create {}: {e}", dest.display())))?;
    let mut at = 0u64;
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let (bytes, total) = fetch(at, at + chunk - 1)?;
        out.write_all(&bytes)
            .map_err(|e| Error::Upload(format!("write {}: {e}", dest.display())))?;
        at += bytes.len() as u64;
        let done = match total {
            Some(t) => at >= t,
            None => (bytes.len() as u64) < chunk,
        };
        if done || bytes.is_empty() {
            break;
        }
    }
    out.sync_all()
        .map_err(|e| Error::Upload(format!("flush {}: {e}", dest.display())))?;
    Ok(at)
}

/// The total size in a `Content-Range: bytes a-b/total` header.
pub fn content_range_total(header: Option<&str>) -> Option<u64> {
    header?.rsplit('/').next()?.trim().parse().ok()
}

/// An XML element's text (`<Code>…</Code>`), enough for the S3/Azure error and id bodies.
pub fn xml_text(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_owned())
}

/// An error line for a failed storage call: status plus the service's error code and
/// message, never the request (it carries signatures).
pub fn http_error(what: &str, resp: &dl_http::Response) -> Error {
    let body = String::from_utf8_lossy(&resp.body);
    let code = xml_text(&body, "Code").unwrap_or_default();
    let msg = xml_text(&body, "Message").unwrap_or_default();
    let msg: String = msg
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect();
    Error::Upload(
        format!("{what}: HTTP {} {code} {msg}", resp.status.as_u16())
            .trim_end()
            .to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_kinds() {
        assert_eq!(Kind::of("AWSS3"), Kind::S3);
        assert_eq!(Kind::of("AzureBlob"), Kind::Azure);
        assert_eq!(Kind::of("LocalFolder"), Kind::Local);
        assert_eq!(Kind::of("GoogleDrive"), Kind::GoogleDrive);
        assert_eq!(Kind::of("OneDrive"), Kind::OneDrive);
        assert_eq!(Kind::of("Dropbox"), Kind::Unknown);
    }

    #[test]
    fn unknown_providers_are_unsupported() {
        // Go TestUpload_UnknownProvider skipped the upload; here the job fails clearly.
        for p in ["Dropbox", "", "Box"] {
            let d = Destination {
                provider: p.into(),
                tokens: Value::Object(Map::new()),
            };
            let err = Real.open(&d).err().unwrap();
            assert_eq!(err.code(), "backup_destination_unsupported", "{p}");
        }
    }

    #[test]
    fn drive_destinations_need_their_oauth_tokens() {
        for p in ["GoogleDrive", "OneDrive"] {
            let d = Destination {
                provider: p.into(),
                tokens: Value::Object(Map::new()),
            };
            let err = Real.open(&d).err().unwrap();
            assert_eq!(err.code(), "backup_upload_failed", "{p}");
            assert!(err.to_string().contains("credentials incomplete"), "{p}");
        }
    }

    #[test]
    fn destination_merges_the_row_endpoint_and_hides_tokens() {
        let d = Destination::from_plain(
            "AWSS3",
            r#"{"accessKeyId":"AK","secretAccessKey":"SECRET","bucketName":"b","endpoint":"https://old"}"#,
            Some("https://acc.r2.cloudflarestorage.com"),
        )
        .unwrap();
        assert_eq!(
            text(&d.tokens, "endpoint"),
            "https://acc.r2.cloudflarestorage.com"
        );
        assert!(!format!("{d:?}").contains("SECRET"));
        assert_eq!(Destination::from_json(&d.to_json()), Some(d));
        assert_eq!(
            Destination::from_plain("AWSS3", "not json", None)
                .unwrap_err()
                .code(),
            "backup_credential_error"
        );
    }

    #[test]
    fn helpers() {
        assert_eq!(content_range_total(Some("bytes 0-9/123")), Some(123));
        assert_eq!(content_range_total(Some("bytes */*")), None);
        assert_eq!(
            xml_text("<E><Code>NoSuchKey</Code></E>", "Code").as_deref(),
            Some("NoSuchKey")
        );
        assert_eq!(xml_text("<E/>", "Code"), None);
    }
}
