//! S3-compatible storage (AWS S3, Cloudflare R2, MinIO) with SigV4 and multipart uploads,
//! following Go's `up_s3.go`: 16 MiB parts, region `auto` when an endpoint is set (R2) and
//! `ap-southeast-1` otherwise, `https` endpoints only (plain `http` for localhost), key
//! `<remoteFolderPath>/<file name>`, cloud path `s3://<bucket>/<key>`.

use std::fs::File;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use dl_http::{Method, Request, Response, Url};
use dl_json::Value;
use dl_tds::CancelToken;

use super::sigv4::{self, Canonical, Credentials};
use super::{Progress, Uploader, base_name, flag, http_error, read_full, text, xml_text};
use crate::Error;

/// Part size of a multipart upload; smaller files go up in one PUT.
pub const PART: usize = 16 * 1024 * 1024;
const R2_AUTO_REGION: &str = "auto";
const DEFAULT_REGION: &str = "ap-southeast-1";

/// One S3-compatible destination.
pub struct S3 {
    creds: Credentials,
    bucket: String,
    folder: String,
    endpoint: Option<Url>,
    path_style: bool,
    http: dl_http::Client,
}

impl std::fmt::Debug for S3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3")
            .field("bucket", &self.bucket)
            .field("region", &self.creds.region)
            .finish_non_exhaustive()
    }
}

/// Go's `validateS3Endpoint`: `https`, or `http` only for a loopback host.
pub fn validate_endpoint(raw: &str) -> Result<Url, Error> {
    let url = Url::parse(raw)
        .map_err(|e| Error::Upload(format!("S3 endpoint: invalid endpoint URL {raw:?}: {e}")))?;
    let host = url.host().trim_start_matches('[').trim_end_matches(']');
    match url.scheme() {
        dl_http::Scheme::Https => Ok(url),
        dl_http::Scheme::Http if matches!(host, "localhost" | "127.0.0.1" | "::1") => Ok(url),
        dl_http::Scheme::Http => Err(Error::Upload(format!(
            "S3 endpoint: endpoint {raw:?} uses http:// with non-localhost host {host:?} — credentials would be sent in cleartext; use https://"
        ))),
    }
}

/// Go's `parseS3CloudPath`: `s3://bucket/key`.
pub fn parse_cloud_path(path: &str) -> Result<(String, String), Error> {
    let rest = path.strip_prefix("s3://").ok_or_else(|| {
        Error::Upload(format!(
            "invalid S3 cloud path {path:?}: missing \"s3://\" prefix"
        ))
    })?;
    match rest.split_once('/') {
        Some((b, k)) if !b.is_empty() && !k.is_empty() => Ok((b.to_owned(), k.to_owned())),
        _ => Err(Error::Upload(format!(
            "invalid S3 cloud path {path:?}: expected format s3://bucket/key"
        ))),
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl S3 {
    /// Reads `accessKeyId`, `secretAccessKey`, `bucketName`, `region`, `endpoint`,
    /// `usePathStyle` and `remoteFolderPath`.
    pub fn from_tokens(t: &Value) -> Result<S3, Error> {
        let (access_key, secret_key) = (text(t, "accessKeyId"), text(t, "secretAccessKey"));
        if access_key.is_empty() || secret_key.is_empty() {
            return Err(Error::Upload(
                "AWS S3 credentials incomplete (need accessKeyId, secretAccessKey)".into(),
            ));
        }
        let bucket = text(t, "bucketName");
        if bucket.is_empty() {
            return Err(Error::Upload("AWS S3 bucket name is required".into()));
        }
        let endpoint = Some(text(t, "endpoint"))
            .filter(|e| !e.is_empty())
            .map(|e| validate_endpoint(&e))
            .transpose()?;
        let region = match text(t, "region") {
            r if !r.is_empty() => r,
            _ if endpoint.is_some() => R2_AUTO_REGION.to_owned(),
            _ => DEFAULT_REGION.to_owned(),
        };
        Ok(S3 {
            creds: Credentials {
                access_key,
                secret_key,
                region,
            },
            bucket,
            folder: text(t, "remoteFolderPath").trim_matches('/').to_owned(),
            endpoint,
            path_style: flag(t, "usePathStyle"),
            http: super::http_client(),
        })
    }

    /// The object key for a local file.
    pub fn key_for(&self, file_name: &str) -> String {
        if self.folder.is_empty() {
            file_name.to_owned()
        } else {
            format!("{}/{file_name}", self.folder)
        }
    }

    /// `(scheme://authority, path)` of an object: virtual-hosted unless `usePathStyle`.
    pub fn locate(&self, bucket: &str, key: &str) -> Result<(String, String), Error> {
        if key.split('/').any(|s| s == "." || s == "..") {
            return Err(Error::Upload(format!("S3 key {key:?} has a dot segment")));
        }
        let key = sigv4::uri_encode(key, true);
        Ok(match &self.endpoint {
            None => (
                format!("https://{bucket}.s3.{}.amazonaws.com", self.creds.region),
                format!("/{key}"),
            ),
            Some(e) => {
                let scheme = e.scheme().as_str();
                let base = e.path().trim_end_matches('/');
                if self.path_style {
                    (
                        format!("{scheme}://{}", e.authority()),
                        format!("{base}/{bucket}/{key}"),
                    )
                } else {
                    (
                        format!("{scheme}://{bucket}.{}", e.authority()),
                        format!("{base}/{key}"),
                    )
                }
            }
        })
    }

    fn send(
        &self,
        what: &str,
        method: Method,
        (bucket, key): (&str, &str),
        query: &[(&str, &str)],
        body: Vec<u8>,
        extra: &[(&str, &str)],
    ) -> Result<Response, Error> {
        let (origin, path) = self.locate(bucket, key)?;
        let q = sigv4::canonical_query(query);
        let raw = if q.is_empty() {
            format!("{origin}{path}")
        } else {
            format!("{origin}{path}?{q}")
        };
        let url =
            Url::parse(&raw).map_err(|e| Error::Upload(format!("S3 {what}: bad URL: {e}")))?;
        let host = url.authority();
        let date = sigv4::amz_date(now_secs());
        let hash = sigv4::payload_hash(&body);
        let mut headers: Vec<(&str, &str)> = vec![
            ("host", host.as_str()),
            ("x-amz-date", date.as_str()),
            ("x-amz-content-sha256", hash.as_str()),
        ];
        headers.extend_from_slice(extra);
        let auth = sigv4::authorization(
            &Canonical {
                method: method.as_str(),
                uri: url.path(),
                query: &q,
                headers: &headers,
                payload_hash: &hash,
            },
            &self.creds,
            &date,
        );
        let mut req = Request::new(method, url).with_body(body);
        for (k, v) in headers
            .iter()
            .chain([("authorization", auth.as_str())].iter())
        {
            req = req
                .with_header(k, v)
                .map_err(|e| Error::Upload(format!("S3 {what}: header {k}: {e}")))?;
        }
        self.http
            .request(&req)
            .map_err(|e| Error::Upload(format!("S3 {what}: {e}")))
    }

    fn ok(what: &str, resp: Response) -> Result<Response, Error> {
        // CompleteMultipartUpload can answer 200 with an <Error> body.
        if resp.is_success() && !String::from_utf8_lossy(&resp.body).contains("<Error>") {
            Ok(resp)
        } else {
            Err(http_error(&format!("S3 {what}"), &resp))
        }
    }

    fn multipart(
        &self,
        key: &str,
        f: &mut File,
        total: u64,
        cancel: &CancelToken,
        progress: Progress<'_>,
    ) -> Result<(), Error> {
        let at = (self.bucket.as_str(), key);
        let resp = self.send(
            "create multipart upload",
            Method::Post,
            at,
            &[("uploads", "")],
            Vec::new(),
            &[],
        )?;
        let resp = Self::ok("create multipart upload", resp)?;
        let upload_id = xml_text(&String::from_utf8_lossy(&resp.body), "UploadId")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::Upload("S3 create multipart upload: no UploadId in the answer".into())
            })?;
        let res = self.parts(key, &upload_id, f, total, cancel, progress);
        match res {
            Ok(xml) => {
                let q = [("uploadId", upload_id.as_str())];
                let resp = self.send(
                    "complete multipart upload",
                    Method::Post,
                    at,
                    &q,
                    xml.into_bytes(),
                    &[],
                )?;
                Self::ok("complete multipart upload", resp).map(|_| ())
            }
            Err(e) => {
                let q = [("uploadId", upload_id.as_str())];
                match self.send(
                    "abort multipart upload",
                    Method::Delete,
                    at,
                    &q,
                    Vec::new(),
                    &[],
                ) {
                    Ok(r) if r.is_success() => Err(e),
                    Ok(r) => Err(Error::Upload(format!(
                        "{e}; abort also failed: HTTP {}",
                        r.status.as_u16()
                    ))),
                    Err(a) => Err(Error::Upload(format!("{e}; abort also failed: {a}"))),
                }
            }
        }
    }

    fn parts(
        &self,
        key: &str,
        upload_id: &str,
        f: &mut File,
        total: u64,
        cancel: &CancelToken,
        progress: Progress<'_>,
    ) -> Result<String, Error> {
        let mut xml = String::from("<CompleteMultipartUpload>");
        let mut buf = vec![0u8; PART];
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
            let num = n.to_string();
            let q = [("partNumber", num.as_str()), ("uploadId", upload_id)];
            let what = format!("upload part {n}");
            let resp = self.send(
                &what,
                Method::Put,
                (&self.bucket, key),
                &q,
                buf[..len].to_vec(),
                &[],
            )?;
            let resp = Self::ok(&what, resp)?;
            let etag = resp
                .headers
                .get("etag")
                .ok_or_else(|| Error::Upload(format!("S3 {what}: no ETag in the answer")))?;
            xml.push_str(&format!(
                "<Part><PartNumber>{n}</PartNumber><ETag>{etag}</ETag></Part>"
            ));
            sent += len as u64;
            progress(sent, total);
        }
        xml.push_str("</CompleteMultipartUpload>");
        Ok(xml)
    }
}

impl Uploader for S3 {
    fn upload(
        &self,
        file: &Path,
        cancel: &CancelToken,
        progress: Progress<'_>,
    ) -> Result<String, Error> {
        let key = self.key_for(&base_name(file)?);
        let mut f =
            File::open(file).map_err(|e| Error::Upload(format!("open backup file: {e}")))?;
        let total = f
            .metadata()
            .map(|m| m.len())
            .map_err(|e| Error::Upload(format!("stat backup file: {e}")))?;
        if total <= PART as u64 {
            let mut body = vec![0u8; total as usize];
            let n = read_full(&mut f, &mut body)?;
            body.truncate(n);
            let ct = [("content-type", "application/octet-stream")];
            let resp = self.send(
                "put object",
                Method::Put,
                (&self.bucket, &key),
                &[],
                body,
                &ct,
            )?;
            Self::ok("put object", resp)?;
            progress(total, total);
        } else {
            self.multipart(&key, &mut f, total, cancel, progress)?;
        }
        Ok(format!("s3://{}/{key}", self.bucket))
    }

    fn download(&self, cloud_path: &str, dest: &Path, cancel: &CancelToken) -> Result<u64, Error> {
        let (bucket, key) = parse_cloud_path(cloud_path)?;
        super::ranged_download(dest, PART as u64, cancel, |a, b| {
            let range = format!("bytes={a}-{b}");
            let resp = self.send(
                "get object",
                Method::Get,
                (&bucket, &key),
                &[],
                Vec::new(),
                &[("range", &range)],
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
                _ => Err(http_error("S3 get object", &resp)),
            }
        })
    }

    fn delete(&self, cloud_path: &str) -> Result<(), Error> {
        let (bucket, key) = parse_cloud_path(cloud_path)?;
        let resp = self.send(
            "delete object",
            Method::Delete,
            (&bucket, &key),
            &[],
            Vec::new(),
            &[],
        )?;
        if resp.is_success() || resp.status.as_u16() == 404 {
            Ok(())
        } else {
            Err(http_error("S3 delete object", &resp))
        }
    }
}

#[cfg(test)]
#[path = "s3_tests.rs"]
mod tests;
