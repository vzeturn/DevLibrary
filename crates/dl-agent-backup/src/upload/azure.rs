//! Azure Blob Storage block blobs, following Go's `up_azure.go`: the destination's
//! `connectionString` starts with `conn:` (a storage connection string, Shared Key or
//! `SharedAccessSignature`) or `sas:` (a SAS URL); the container is `bucketName`, else
//! `backups`; the blob is the file's base name. Files up to 32 MiB go up in one Put Blob,
//! larger ones as 32 MiB Put Block calls and a Put Block List (uncommitted blocks of a failed
//! upload are dropped by Azure after a week). The cloud path is the blob URL **without** any
//! SAS token: Go kept the token there, which put a credential into backup history.

use std::fs::File;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use dl_encoding::base64::STANDARD;
use dl_http::{Method, Request, Response, Url, encode_path_segment, encode_query_component};
use dl_json::Value;
use dl_tds::CancelToken;

use super::sharedkey::{SharedKey, http_date};
use super::{Progress, Uploader, base_name, http_error, read_full, text};
use crate::Error;

/// Block size; files up to this size go up in a single Put Blob.
pub const BLOCK: usize = 32 * 1024 * 1024;
const DEFAULT_CONTAINER: &str = "backups";
const VERSION: &str = "2021-08-06";
// Azurite's published development account (UseDevelopmentStorage=true); not a secret.
const DEV_ACCOUNT: &str = "devstoreaccount1";
const DEV_KEY: &str =
    "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFMgrO8kwCW/QDUz4qPA==";

/// How requests are authorized.
#[derive(Clone, PartialEq, Eq)]
pub enum Auth {
    /// Shared Key signing.
    Key(SharedKey),
    /// A SAS token appended to every request's query (without the leading `?`).
    Sas(String),
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Auth::Key(k) => write!(f, "Key({})", k.account()),
            Auth::Sas(_) => f.write_str("Sas(..)"),
        }
    }
}

/// The blob service a destination points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// `scheme://authority`.
    pub origin: String,
    /// Path prefix before `/<container>` (`/devstoreaccount1` on Azurite, usually empty).
    pub base: String,
    /// Authorization.
    pub auth: Auth,
    /// The container named by the SAS URL's path, if any.
    pub sas_container: Option<String>,
}

fn endpoint(raw: &str, what: &str) -> Result<Url, Error> {
    let url = Url::parse(raw)
        .map_err(|_| Error::Upload(format!("azure blob: {what} is not a valid URL")))?;
    let host = url.host().trim_start_matches('[').trim_end_matches(']');
    match url.scheme() {
        dl_http::Scheme::Https => Ok(url),
        dl_http::Scheme::Http if matches!(host, "localhost" | "127.0.0.1" | "::1") => Ok(url),
        dl_http::Scheme::Http => Err(Error::Upload(format!(
            "azure blob: {what} uses http:// with non-localhost host {host:?}; use https://"
        ))),
    }
}

/// Parses a storage connection string (the part after `conn:`). The string itself never
/// appears in an error.
pub fn parse_connection_string(s: &str) -> Result<Service, Error> {
    let mut fields: Vec<(String, String)> = Vec::new();
    for part in s.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let (k, v) = part.split_once('=').ok_or_else(|| {
            Error::Upload("azure blob: connection string has a part without '='".into())
        })?;
        fields.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
    }
    let get = |k: &str| {
        fields
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    };
    if get("usedevelopmentstorage").eq_ignore_ascii_case("true") {
        return Ok(Service {
            origin: "http://127.0.0.1:10000".into(),
            base: format!("/{DEV_ACCOUNT}"),
            auth: Auth::Key(SharedKey::new(DEV_ACCOUNT, DEV_KEY)?),
            sas_container: None,
        });
    }
    let account = get("accountname");
    let url = match get("blobendpoint") {
        "" if account.is_empty() => {
            return Err(Error::Upload(
                "azure blob: connection string has no AccountName or BlobEndpoint".into(),
            ));
        }
        "" => {
            let proto = match get("defaultendpointsprotocol") {
                "" => "https",
                p => p,
            };
            let suffix = match get("endpointsuffix") {
                "" => "core.windows.net",
                s => s,
            };
            endpoint(
                &format!("{proto}://{account}.blob.{suffix}"),
                "account endpoint",
            )?
        }
        e => endpoint(e, "BlobEndpoint")?,
    };
    let auth = match (get("sharedaccesssignature"), get("accountkey")) {
        (sas, _) if !sas.is_empty() => Auth::Sas(sas.trim_start_matches('?').to_owned()),
        (_, key) if !key.is_empty() && !account.is_empty() => {
            Auth::Key(SharedKey::new(account, key)?)
        }
        _ => {
            return Err(Error::Upload(
                "azure blob: connection string needs AccountName and AccountKey, or SharedAccessSignature".into(),
            ));
        }
    };
    Ok(Service {
        origin: format!("{}://{}", url.scheme().as_str(), url.authority()),
        base: url.path().trim_end_matches('/').to_owned(),
        auth,
        sas_container: None,
    })
}

/// Parses a SAS URL (the part after `sas:`). Like Go, its path is replaced by
/// `/<container>/<blob>`; a container in that path is used when `bucketName` is empty.
pub fn parse_sas_url(s: &str) -> Result<Service, Error> {
    let url = endpoint(s, "SAS URL")?;
    let query = url.query().unwrap_or("").to_owned();
    if query.is_empty() {
        return Err(Error::Upload(
            "azure blob: SAS URL has no token (query string)".into(),
        ));
    }
    let first = url
        .path()
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("");
    let sas_container = match first {
        "" => None,
        c => Some(
            String::from_utf8_lossy(&dl_http::percent_decode(c).unwrap_or_default()).into_owned(),
        ),
    };
    Ok(Service {
        origin: format!("{}://{}", url.scheme().as_str(), url.authority()),
        base: String::new(),
        auth: Auth::Sas(query),
        sas_container,
    })
}

/// Go's `parseAzureBlobCloudPath` over the path after `base`: `.../<container>/<blob>`.
pub fn parse_cloud_path(cloud_path: &str, base: &str) -> Result<(String, String), Error> {
    let shown = cloud_path.split('?').next().unwrap_or_default();
    let bad = || {
        Error::Upload(format!(
            "invalid Azure blob cloud path {shown:?}: expected .../<container>/<blob>"
        ))
    };
    let url = Url::parse(cloud_path).map_err(|_| bad())?;
    let path = url.path();
    let path = path
        .strip_prefix(base)
        .filter(|p| p.starts_with('/'))
        .unwrap_or(path);
    let (container, blob) = path
        .trim_start_matches('/')
        .split_once('/')
        .ok_or_else(bad)?;
    if container.is_empty() || blob.is_empty() {
        return Err(bad());
    }
    let decode = |s: &str| {
        dl_http::percent_decode(s)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .ok_or_else(bad)
    };
    Ok((decode(container)?, decode(blob)?))
}

/// One Azure Blob destination.
#[derive(Debug)]
pub struct Azure {
    service: Service,
    container: String,
    http: dl_http::Client,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Azure {
    /// Reads `connectionString` and `bucketName`.
    pub fn from_tokens(t: &Value) -> Result<Azure, Error> {
        let raw = text(t, "connectionString");
        let service = if raw.is_empty() {
            return Err(Error::Upload(
                "azure blob: ConnectionString is empty — must start with 'conn:' or 'sas:'".into(),
            ));
        } else if let Some(c) = raw.strip_prefix("conn:") {
            parse_connection_string(c)?
        } else if let Some(s) = raw.strip_prefix("sas:") {
            parse_sas_url(s)?
        } else {
            return Err(Error::Upload(
                "azure ConnectionString must start with 'conn:' or 'sas:'".into(),
            ));
        };
        let container = match text(t, "bucketName") {
            b if !b.is_empty() => b,
            _ => service
                .sas_container
                .clone()
                .unwrap_or_else(|| DEFAULT_CONTAINER.to_owned()),
        };
        Ok(Azure {
            service,
            container,
            http: super::http_client(),
        })
    }

    /// The container uploads go to.
    pub fn container(&self) -> &str {
        &self.container
    }

    /// The URL path of a blob.
    pub fn blob_path(&self, container: &str, blob: &str) -> String {
        format!(
            "{}/{}/{}",
            self.service.base,
            encode_path_segment(container),
            encode_path_segment(blob)
        )
    }

    /// The cloud path reported to the hub: the blob URL without any SAS token.
    pub fn cloud_path(&self, blob: &str) -> String {
        format!(
            "{}{}",
            self.service.origin,
            self.blob_path(&self.container, blob)
        )
    }

    fn send(
        &self,
        what: &str,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Vec<u8>,
        extra: &[(&str, &str)],
    ) -> Result<Response, Error> {
        let mut q: Vec<String> = query
            .iter()
            .map(|(k, v)| format!("{k}={}", encode_query_component(v)))
            .collect();
        if let Auth::Sas(sas) = &self.service.auth {
            q.push(sas.clone());
        }
        let raw = match q.join("&") {
            s if s.is_empty() => format!("{}{path}", self.service.origin),
            s => format!("{}{path}?{s}", self.service.origin),
        };
        // The URL may carry a SAS token: never put it into an error.
        let url =
            Url::parse(&raw).map_err(|_| Error::Upload(format!("Azure {what}: bad blob URL")))?;
        let date = http_date(now_secs());
        let length = body.len().to_string();
        let mut headers: Vec<(&str, &str)> = vec![("x-ms-date", &date), ("x-ms-version", VERSION)];
        headers.extend_from_slice(extra);
        let auth = match &self.service.auth {
            Auth::Key(key) => {
                let mut signed = headers.clone();
                signed.push(("content-length", &length));
                Some(key.authorization(
                    method.as_str(),
                    &signed,
                    url.path(),
                    url.query().unwrap_or(""),
                ))
            }
            Auth::Sas(_) => None,
        };
        let mut req = Request::new(method, url).with_body(body);
        for (k, v) in headers
            .iter()
            .copied()
            .chain(auth.as_deref().map(|a| ("authorization", a)))
        {
            req = req
                .with_header(k, v)
                .map_err(|e| Error::Upload(format!("Azure {what}: header {k}: {e}")))?;
        }
        self.http
            .request(&req)
            .map_err(|e| Error::Upload(format!("Azure {what}: {e}")))
    }

    fn ok(what: &str, resp: Response) -> Result<Response, Error> {
        if resp.is_success() {
            Ok(resp)
        } else {
            Err(http_error(&format!("Azure {what}"), &resp))
        }
    }

    fn blocks(
        &self,
        path: &str,
        f: &mut File,
        total: u64,
        cancel: &CancelToken,
        progress: Progress<'_>,
    ) -> Result<(), Error> {
        let mut list = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList>");
        let mut buf = vec![0u8; BLOCK];
        let (mut n, mut sent) = (0u32, 0u64);
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let len = read_full(f, &mut buf)?;
            if len == 0 {
                break;
            }
            n += 1;
            // Every id of one blob must have the same length.
            let id = STANDARD.encode(format!("dl-agent-block-{n:08}"));
            let what = format!("put block {n}");
            let q = [("comp", "block"), ("blockid", id.as_str())];
            let ct = [("content-type", "application/octet-stream")];
            Self::ok(
                &what,
                self.send(&what, Method::Put, path, &q, buf[..len].to_vec(), &ct)?,
            )?;
            list.push_str(&format!("<Latest>{id}</Latest>"));
            sent += len as u64;
            progress(sent, total);
        }
        list.push_str("</BlockList>");
        let ct = [("content-type", "application/xml")];
        let resp = self.send(
            "put block list",
            Method::Put,
            path,
            &[("comp", "blocklist")],
            list.into_bytes(),
            &ct,
        )?;
        Self::ok("put block list", resp).map(|_| ())
    }
}

impl Uploader for Azure {
    fn upload(
        &self,
        file: &Path,
        cancel: &CancelToken,
        progress: Progress<'_>,
    ) -> Result<String, Error> {
        let blob = base_name(file)?;
        let path = self.blob_path(&self.container, &blob);
        let mut f =
            File::open(file).map_err(|e| Error::Upload(format!("azure blob: open file: {e}")))?;
        let total = f
            .metadata()
            .map(|m| m.len())
            .map_err(|e| Error::Upload(format!("azure blob: stat local file: {e}")))?;
        if total <= BLOCK as u64 {
            let mut body = vec![0u8; total as usize];
            let n = read_full(&mut f, &mut body)?;
            body.truncate(n);
            let h = [
                ("x-ms-blob-type", "BlockBlob"),
                ("content-type", "application/octet-stream"),
            ];
            Self::ok(
                "put blob",
                self.send("put blob", Method::Put, &path, &[], body, &h)?,
            )?;
            progress(total, total);
        } else {
            self.blocks(&path, &mut f, total, cancel, progress)?;
        }
        Ok(self.cloud_path(&blob))
    }

    fn download(&self, cloud_path: &str, dest: &Path, cancel: &CancelToken) -> Result<u64, Error> {
        let (container, blob) = parse_cloud_path(cloud_path, &self.service.base)?;
        let path = self.blob_path(&container, &blob);
        super::ranged_download(dest, BLOCK as u64, cancel, |a, b| {
            let range = format!("bytes={a}-{b}");
            let resp = self.send(
                "get blob",
                Method::Get,
                &path,
                &[],
                Vec::new(),
                &[("x-ms-range", &range)],
            )?;
            match resp.status.as_u16() {
                206 => {
                    let total = super::content_range_total(resp.headers.get("content-range"));
                    Ok((resp.body, total))
                }
                200 => {
                    let total = Some(a + resp.body.len() as u64);
                    Ok((resp.body, total))
                }
                416 if a == 0 => Ok((Vec::new(), Some(0))),
                _ => Err(http_error("Azure get blob", &resp)),
            }
        })
    }

    fn delete(&self, cloud_path: &str) -> Result<(), Error> {
        let (container, blob) = parse_cloud_path(cloud_path, &self.service.base)?;
        let path = self.blob_path(&container, &blob);
        let resp = self.send("delete blob", Method::Delete, &path, &[], Vec::new(), &[])?;
        if resp.is_success() || resp.status.as_u16() == 404 {
            Ok(())
        } else {
            Err(http_error("Azure delete blob", &resp))
        }
    }
}

#[cfg(test)]
#[path = "azure_tests.rs"]
mod tests;
