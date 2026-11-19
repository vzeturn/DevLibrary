//! Google Drive, following Go's `up_gdrive.go`: OAuth refresh (`clientId`, `clientSecret`,
//! `refreshToken`, scope `drive.file`), the destination folder `remoteFolderPath` resolved
//! (and created) segment by segment, files under 5 MiB in one multipart request and larger
//! ones as a resumable upload. The cloud path is `gdrive://<remoteFolderPath>/<file name>`
//! exactly as Go stored it (the folder path is not normalized: the hub and restores key on it).
//!
//! Differences from Go: 32 MiB chunks (Go 64 MiB; fewer bytes in memory), a dropped or failed
//! chunk is resumed by asking the session how much arrived, and download, delete and lookup
//! never create folders (a missing folder or file is "not found"; delete treats that as done).

use std::fs::File;
use std::path::Path;

use dl_http::{Method, encode_query_component};
use dl_json::{Map, Value, json};
use dl_tds::CancelToken;

use super::oauth::{OAuth, Pace, json_error, read_at, request};
use super::{Progress, Uploader, base_name, ranged_download};
use crate::Error;

/// Files below this go up in one request.
const SIMPLE_LIMIT: u64 = 5 * 1024 * 1024;
/// Resumable chunk: a multiple of 256 KiB (Drive's rule).
const CHUNK: usize = 32 * 1024 * 1024;
/// Failed attempts in a row on one chunk before the upload fails.
const MAX_FAILURES: u32 = 5;
const BASE: &str = "https://www.googleapis.com";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const LABEL: &str = "Google Drive";

/// One Google Drive destination.
#[derive(Debug)]
pub struct GDrive {
    api: OAuth,
    folder: String,
    base: String,
    chunk: usize,
}

/// Go's `parseGDriveCloudPath`: `gdrive://<folder path>/<file name>`.
pub fn parse_cloud_path(path: &str) -> Result<(String, String), Error> {
    let rest = path.strip_prefix("gdrive://").ok_or_else(|| {
        Error::Upload(format!(
            "invalid Drive cloud path {path:?}: missing \"gdrive://\" prefix"
        ))
    })?;
    let (folder, name) = rest.rsplit_once('/').unwrap_or(("", rest));
    if name.is_empty() {
        return Err(Error::Upload(format!(
            "invalid Drive cloud path {path:?}: missing file name"
        )));
    }
    Ok((folder.to_owned(), name.to_owned()))
}

/// A Drive query string literal: single quotes and backslashes escaped.
fn q_str(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

impl GDrive {
    /// Reads `clientId`, `clientSecret`, `refreshToken`, `accessToken`, `tokenExpiresAt` and
    /// `remoteFolderPath`.
    pub fn from_tokens(t: &Value) -> Result<GDrive, Error> {
        Self::build(t, BASE, TOKEN_URL, CHUNK, Pace::default())
    }

    /// Test constructor: other endpoints, chunk size and pacing (never reachable from hub
    /// configuration).
    #[cfg(test)]
    pub fn for_test(t: &Value, server: &str, chunk: usize, pace: Pace) -> Result<GDrive, Error> {
        Self::build(t, server, &format!("{server}/token"), chunk, pace)
    }

    fn build(
        t: &Value,
        base: &str,
        token_url: &str,
        chunk: usize,
        pace: Pace,
    ) -> Result<GDrive, Error> {
        Ok(GDrive {
            api: OAuth::from_tokens(LABEL, t, token_url, pace)?,
            folder: super::text(t, "remoteFolderPath"),
            base: base.to_owned(),
            chunk,
        })
    }

    /// The cloud path Go stores for `name`.
    pub fn cloud_path(&self, name: &str) -> String {
        format!("gdrive://{}/{name}", self.folder)
    }

    fn call(
        &self,
        method: Method,
        url: &str,
        headers: &[(&str, &str)],
        body: impl Into<Vec<u8>>,
        cancel: &CancelToken,
        what: &str,
    ) -> Result<dl_http::Response, Error> {
        let req = request(method, url, headers, body)?;
        self.api.send(req, cancel, &format!("{LABEL} {what}"))
    }

    /// The id of the first non-trashed child of `parent` named `name` (a folder when
    /// `folder`).
    fn find(
        &self,
        parent: &str,
        name: &str,
        folder: bool,
        cancel: &CancelToken,
    ) -> Result<Option<String>, Error> {
        let mut q = format!(
            "name = {} and {} in parents and trashed = false",
            q_str(name),
            q_str(parent)
        );
        if folder {
            q.push_str(&format!(" and mimeType = {}", q_str(FOLDER_MIME)));
        }
        let url = format!(
            "{}/drive/v3/files?q={}&fields={}&pageSize=1",
            self.base,
            encode_query_component(&q),
            encode_query_component("files(id)")
        );
        let what = if folder { "list folder" } else { "find file" };
        let resp = self.call(Method::Get, &url, &[], Vec::new(), cancel, what)?;
        if !resp.is_success() {
            return Err(json_error(&format!("{LABEL} {what}"), &resp));
        }
        let body = parse_body(&resp, what)?;
        Ok(body
            .get("files")
            .and_then(|f| f.get_index(0))
            .and_then(|f| f.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned))
    }

    /// Resolves `path` to a folder id, creating missing folders when `create`.
    fn folder_id(
        &self,
        path: &str,
        create: bool,
        cancel: &CancelToken,
    ) -> Result<Option<String>, Error> {
        let mut parent = "root".to_owned();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            if let Some(id) = self.find(&parent, part, true, cancel)? {
                parent = id;
                continue;
            }
            if !create {
                return Ok(None);
            }
            let meta = json!({"name": part, "mimeType": FOLDER_MIME, "parents": [parent.as_str()]});
            let url = format!("{}/drive/v3/files?fields=id", self.base);
            let json_ct = [("content-type", "application/json")];
            let resp = self.call(
                Method::Post,
                &url,
                &json_ct,
                dl_json::to_string(&meta),
                cancel,
                "create folder",
            )?;
            if !resp.is_success() {
                return Err(json_error(&format!("{LABEL} create folder"), &resp));
            }
            parent = id_of(&parse_body(&resp, "create folder")?, "create folder")?;
        }
        Ok(Some(parent))
    }

    fn metadata(name: &str, parent: &str) -> String {
        let mut m = Map::new();
        m.insert("name", name);
        m.insert("parents", json!([parent]));
        dl_json::to_string(&Value::Object(m))
    }

    fn simple_upload(
        &self,
        f: &mut File,
        total: u64,
        name: &str,
        parent: &str,
        cancel: &CancelToken,
    ) -> Result<(), Error> {
        let data = read_at(f, 0, total as usize)?;
        let boundary = format!("dl-{}", dl_uuid::Uuid::new_v4().simple());
        let mut body = format!(
            "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{}\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n",
            Self::metadata(name, parent)
        )
        .into_bytes();
        body.extend_from_slice(&data);
        body.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());
        let url = format!(
            "{}/upload/drive/v3/files?uploadType=multipart&fields=id,name,size",
            self.base
        );
        let ct = format!("multipart/related; boundary={boundary}");
        let resp = self.call(
            Method::Post,
            &url,
            &[("content-type", &ct)],
            body,
            cancel,
            "upload",
        )?;
        if resp.is_success() {
            Ok(())
        } else {
            Err(json_error(&format!("{LABEL} upload"), &resp))
        }
    }

    fn resumable_upload(
        &self,
        f: &mut File,
        total: u64,
        name: &str,
        parent: &str,
        cancel: &CancelToken,
        progress: Progress<'_>,
    ) -> Result<(), Error> {
        let url = format!(
            "{}/upload/drive/v3/files?uploadType=resumable&fields=id,name,size",
            self.base
        );
        let len = total.to_string();
        let resp = self.call(
            Method::Post,
            &url,
            &[
                ("content-type", "application/json; charset=UTF-8"),
                ("x-upload-content-type", "application/octet-stream"),
                ("x-upload-content-length", &len),
            ],
            Self::metadata(name, parent),
            cancel,
            "start upload",
        )?;
        if !resp.is_success() {
            return Err(json_error(&format!("{LABEL} start upload"), &resp));
        }
        // The session URL is a bearer capability: used, never logged or reported.
        let session = resp
            .headers
            .get("location")
            .filter(|l| !l.is_empty())
            .ok_or_else(|| Error::Upload(format!("{LABEL} start upload: no session URL")))?
            .to_owned();
        self.chunks(&session, f, total, cancel, progress)
    }

    fn chunks(
        &self,
        session: &str,
        f: &mut File,
        total: u64,
        cancel: &CancelToken,
        progress: Progress<'_>,
    ) -> Result<(), Error> {
        let pace = self.api.pace();
        let (mut offset, mut failures) = (0u64, 0u32);
        while offset < total {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let data = read_at(f, offset, self.chunk.min((total - offset) as usize))?;
            if data.is_empty() {
                return Err(Error::Upload(
                    "backup file ended before its recorded size".into(),
                ));
            }
            let end = offset + data.len() as u64;
            let range = format!("bytes {offset}-{}/{total}", end - 1);
            let sent = request(Method::Put, session, &[("content-range", &range)], data)
                .map(|r| self.api.http().request(&r));
            let wait = match sent? {
                Ok(resp) => match resp.status.as_u16() {
                    200 | 201 => {
                        progress(total, total);
                        return Ok(());
                    }
                    308 => {
                        offset = persisted(&resp, end)?;
                        failures = 0;
                        progress(offset, total);
                        continue;
                    }
                    429 | 500..=599 => pace.wait_for(Some(&resp), failures),
                    _ => return Err(json_error(&format!("{LABEL} upload chunk"), &resp)),
                },
                Err(_) => pace.wait_for(None, failures),
            };
            failures += 1;
            if failures >= MAX_FAILURES {
                return Err(Error::Upload(format!(
                    "{LABEL} upload chunk at {offset} failed {MAX_FAILURES} times in a row"
                )));
            }
            pace.sleep(wait, cancel)?;
            // Ask the session how much arrived and carry on from there.
            match self.status(session, total) {
                Ok(Some(at)) => offset = at,
                Ok(None) => {
                    progress(total, total);
                    return Ok(());
                }
                Err(_) => {}
            }
        }
        Ok(())
    }

    /// `Some(bytes the session holds)`, or `None` when the upload already completed.
    fn status(&self, session: &str, total: u64) -> Result<Option<u64>, Error> {
        let range = format!("bytes */{total}");
        let req = request(
            Method::Put,
            session,
            &[("content-range", &range)],
            Vec::new(),
        )?;
        let resp = self
            .api
            .http()
            .request(&req)
            .map_err(|e| Error::Upload(format!("{LABEL} upload status: {e}")))?;
        match resp.status.as_u16() {
            200 | 201 => Ok(None),
            308 => persisted(&resp, total).map(Some),
            _ => Err(json_error(&format!("{LABEL} upload status"), &resp)),
        }
    }
}

/// The bytes a 308 says the session holds (`Range: bytes=0-N`, none → 0), never more than
/// `sent`.
fn persisted(resp: &dl_http::Response, sent: u64) -> Result<u64, Error> {
    let held = match resp.headers.get("range") {
        None => 0,
        Some(r) => r
            .rsplit('-')
            .next()
            .and_then(|n| n.trim().parse::<u64>().ok())
            .map(|n| n + 1)
            .ok_or_else(|| Error::Upload(format!("{LABEL} upload: unreadable Range answer")))?,
    };
    if held > sent {
        return Err(Error::Upload(format!(
            "{LABEL} upload: session reports {held} bytes, only {sent} were sent"
        )));
    }
    Ok(held)
}

fn parse_body(resp: &dl_http::Response, what: &str) -> Result<Value, Error> {
    dl_json::from_str::<Value>(&String::from_utf8_lossy(&resp.body))
        .map_err(|_| Error::Upload(format!("{LABEL} {what}: the answer is not JSON")))
}

fn id_of(v: &Value, what: &str) -> Result<String, Error> {
    v.get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::Upload(format!("{LABEL} {what}: no id in the answer")))
}

impl Uploader for GDrive {
    fn upload(
        &self,
        file: &Path,
        cancel: &CancelToken,
        progress: Progress<'_>,
    ) -> Result<String, Error> {
        let name = base_name(file)?;
        let mut f =
            File::open(file).map_err(|e| Error::Upload(format!("open backup file: {e}")))?;
        let total = f
            .metadata()
            .map(|m| m.len())
            .map_err(|e| Error::Upload(format!("stat backup file: {e}")))?;
        let parent = self
            .folder_id(&self.folder, true, cancel)?
            .unwrap_or_else(|| "root".to_owned());
        if total < SIMPLE_LIMIT {
            self.simple_upload(&mut f, total, &name, &parent, cancel)?;
            progress(total, total);
        } else {
            self.resumable_upload(&mut f, total, &name, &parent, cancel, progress)?;
        }
        Ok(self.cloud_path(&name))
    }

    fn download(&self, cloud_path: &str, dest: &Path, cancel: &CancelToken) -> Result<u64, Error> {
        let (folder, name) = parse_cloud_path(cloud_path)?;
        let id = self
            .folder_id(&folder, false, cancel)?
            .map(|p| self.find(&p, &name, false, cancel))
            .transpose()?
            .flatten()
            .ok_or_else(|| {
                Error::Upload(format!(
                    "Drive download: file {name:?} not found in folder {folder:?}"
                ))
            })?;
        let url = format!(
            "{}/drive/v3/files/{}?alt=media",
            self.base,
            encode_query_component(&id)
        );
        let chunk = self.chunk as u64;
        ranged_download(dest, chunk, cancel, |a, b| {
            let range = format!("bytes={a}-{b}");
            let resp = self.call(
                Method::Get,
                &url,
                &[("range", &range)],
                Vec::new(),
                cancel,
                "download",
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
                _ => Err(json_error(&format!("{LABEL} download"), &resp)),
            }
        })
    }

    fn delete(&self, cloud_path: &str) -> Result<(), Error> {
        let cancel = CancelToken::new();
        let (folder, name) = parse_cloud_path(cloud_path)?;
        let Some(parent) = self.folder_id(&folder, false, &cancel)? else {
            return Ok(());
        };
        let Some(id) = self.find(&parent, &name, false, &cancel)? else {
            return Ok(());
        };
        let url = format!(
            "{}/drive/v3/files/{}",
            self.base,
            encode_query_component(&id)
        );
        let resp = self.call(Method::Delete, &url, &[], Vec::new(), &cancel, "delete")?;
        if resp.is_success() || resp.status.as_u16() == 404 {
            Ok(())
        } else {
            Err(json_error(&format!("{LABEL} delete"), &resp))
        }
    }
}

#[cfg(test)]
#[path = "gdrive_tests.rs"]
mod tests;
