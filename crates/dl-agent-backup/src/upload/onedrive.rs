//! OneDrive through Microsoft Graph, following Go's `up_onedrive.go`: refresh-token flow
//! against the `common` authority (scopes `Files.ReadWrite offline_access`), items addressed
//! by path under the drive root (Graph creates missing folders itself), files under 4 MiB in
//! one `PUT .../content`, up to 20 MiB in one upload session, and larger files as up to four
//! parallel sessions, one per part (`<name>.p0`, `<name>.p1`, ...) plus a `<name>.odmanifest`
//! JSON that is the stored cloud path. The cloud path is `onedrive://<graph path>`.
//!
//! Differences from Go: 10 MiB session chunks (Go 60 MiB; four sessions at once would hold
//! 240 MiB), a failed chunk is resumed from the session's `nextExpectedRanges`, a part and the
//! whole file are hashed in separate passes, the manifest is compact JSON, and delete treats a
//! missing item as done. Chunk requests and the pre-authenticated download URL never carry
//! the bearer token.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use dl_encoding::hex;
use dl_hash::{Digest, Sha256};
use dl_http::{Method, encode_path_segment};
use dl_json::{Map, Value};
use dl_tds::CancelToken;

use super::oauth::{OAuth, Pace, json_error, read_at, request};
use super::{Progress, Uploader, base_name};
use crate::Error;

const SMALL_LIMIT: u64 = 4 * 1024 * 1024;
const PARALLEL_MIN: u64 = 20 * 1024 * 1024;
/// Target size of one part of a parallel upload (Go's chunk constant drove the split).
const PART_TARGET: u64 = 60 * 1024 * 1024;
const MAX_SESSIONS: u64 = 4;
/// Session chunk: a multiple of 320 KiB (Graph's rule).
const CHUNK: usize = 10 * 1024 * 1024;
const MAX_FAILURES: u32 = 5;
const GRAPH: &str = "https://graph.microsoft.com/v1.0";
const TOKEN_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";
const MANIFEST_SUFFIX: &str = ".odmanifest";
const LABEL: &str = "OneDrive";

/// One OneDrive destination.
#[derive(Debug)]
pub struct OneDrive {
    api: OAuth,
    folder: String,
    base: String,
    chunk: usize,
    part_target: u64,
}

/// Cancellation by the agent or by a sibling part that failed.
struct Stop<'a> {
    cancel: &'a CancelToken,
    abort: &'a AtomicBool,
}

impl Stop<'_> {
    fn check(&self) -> Result<(), Error> {
        if self.cancel.is_cancelled() || self.abort.load(Ordering::Acquire) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// One part of a large file, as the `.odmanifest` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Part {
    index: u64,
    name: String,
    size: u64,
    sha256: String,
}

/// Go's `parseOneDriveCloudPath`: the Graph item path after `onedrive://`.
pub fn parse_cloud_path(path: &str) -> Result<String, Error> {
    let rest = path.strip_prefix("onedrive://").ok_or_else(|| {
        Error::Upload(format!(
            "invalid OneDrive cloud path {path:?}: missing \"onedrive://\" prefix"
        ))
    })?;
    if rest.is_empty() {
        return Err(Error::Upload(format!(
            "invalid OneDrive cloud path {path:?}: missing item path"
        )));
    }
    Ok(rest.to_owned())
}

fn sha_hex(h: Sha256) -> String {
    hex::encode(h.finalize())
}

/// SHA-256 of `size` bytes of `path` from `start`.
fn hash_range(path: &Path, start: u64, size: u64, stop: &Stop<'_>) -> Result<String, Error> {
    let mut f = File::open(path).map_err(|e| Error::Upload(format!("open backup file: {e}")))?;
    f.seek(SeekFrom::Start(start))
        .map_err(|e| Error::Upload(format!("seek backup file: {e}")))?;
    let (mut h, mut left) = (Sha256::default(), size);
    let mut buf = vec![0u8; 1024 * 1024];
    while left > 0 {
        stop.check()?;
        let want = buf.len().min(left as usize);
        let n = f
            .read(&mut buf[..want])
            .map_err(|e| Error::Upload(format!("read backup file: {e}")))?;
        if n == 0 {
            return Err(Error::Upload(
                "backup file ended before its recorded size".into(),
            ));
        }
        h.update(&buf[..n]);
        left -= n as u64;
    }
    Ok(sha_hex(h))
}

impl OneDrive {
    /// Reads `clientId`, `clientSecret`, `refreshToken`, `accessToken`, `tokenExpiresAt` and
    /// `remoteFolderPath`.
    pub fn from_tokens(t: &Value) -> Result<OneDrive, Error> {
        Self::build(t, GRAPH, TOKEN_URL, CHUNK, PART_TARGET, Pace::default())
    }

    /// Test constructor: other endpoints, chunk and part sizes, pacing (never reachable from
    /// hub configuration).
    #[cfg(test)]
    pub fn for_test(
        t: &Value,
        server: &str,
        chunk: usize,
        part_target: u64,
        pace: Pace,
    ) -> Result<OneDrive, Error> {
        Self::build(
            t,
            &format!("{server}/v1.0"),
            &format!("{server}/token"),
            chunk,
            part_target,
            pace,
        )
    }

    fn build(
        t: &Value,
        base: &str,
        token_url: &str,
        chunk: usize,
        part_target: u64,
        pace: Pace,
    ) -> Result<OneDrive, Error> {
        Ok(OneDrive {
            api: OAuth::from_tokens(LABEL, t, token_url, pace)?,
            folder: super::text(t, "remoteFolderPath")
                .trim_matches('/')
                .to_owned(),
            base: base.to_owned(),
            chunk,
            part_target,
        })
    }

    fn graph_path(&self, name: &str) -> String {
        if self.folder.is_empty() {
            name.to_owned()
        } else {
            format!("{}/{name}", self.folder)
        }
    }

    /// `<base>/me/drive/root:/<path><suffix>`, each path segment percent-encoded.
    fn item_url(&self, path: &str, suffix: &str) -> String {
        let enc: Vec<String> = path.split('/').map(encode_path_segment).collect();
        format!("{}/me/drive/root:/{}{suffix}", self.base, enc.join("/"))
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

    fn put_small(
        &self,
        path: &str,
        data: Vec<u8>,
        content_type: &str,
        cancel: &CancelToken,
        what: &str,
    ) -> Result<(), Error> {
        let url = self.item_url(path, ":/content");
        let resp = self.call(
            Method::Put,
            &url,
            &[("content-type", content_type)],
            data,
            cancel,
            what,
        )?;
        if resp.is_success() {
            Ok(())
        } else {
            Err(json_error(&format!("{LABEL} {what}"), &resp))
        }
    }

    /// Uploads `size` bytes of `file` from `start` as one item at `path` through an upload
    /// session. `report(bytes confirmed so far)` after each chunk.
    fn session_upload(
        &self,
        path: &str,
        file: &Path,
        start: u64,
        size: u64,
        stop: &Stop<'_>,
        report: &mut dyn FnMut(u64),
    ) -> Result<(), Error> {
        let mut f =
            File::open(file).map_err(|e| Error::Upload(format!("open backup file: {e}")))?;
        let url = self.item_url(path, ":/createUploadSession");
        let body = r#"{"item":{"@microsoft.graph.conflictBehavior":"replace"}}"#;
        let resp = self.call(
            Method::Post,
            &url,
            &[("content-type", "application/json")],
            body,
            stop.cancel,
            "create upload session",
        )?;
        if !resp.is_success() {
            return Err(json_error(&format!("{LABEL} create upload session"), &resp));
        }
        // The upload URL is a bearer capability: used, never logged or reported.
        let session = dl_json::from_str::<Value>(&String::from_utf8_lossy(&resp.body))
            .ok()
            .and_then(|v| {
                v.get("uploadUrl")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|u| !u.is_empty())
            .ok_or_else(|| Error::Upload(format!("{LABEL} create upload session: no uploadUrl")))?;

        let pace = self.api.pace();
        let (mut offset, mut failures) = (0u64, 0u32);
        while offset < size {
            stop.check()?;
            let len = self.chunk.min((size - offset) as usize);
            let data = read_at(&mut f, start + offset, len)?;
            if data.len() != len {
                return Err(Error::Upload(
                    "backup file ended before its recorded size".into(),
                ));
            }
            let range = format!("bytes {offset}-{}/{size}", offset + len as u64 - 1);
            let req = request(Method::Put, &session, &[("content-range", &range)], data)?;
            // No Authorization header: the upload URL is pre-authenticated.
            let wait = match self.api.http().request(&req) {
                Ok(r) => match r.status.as_u16() {
                    200 | 201 => {
                        report(size);
                        return Ok(());
                    }
                    202 => {
                        offset = next_expected(&r, offset + len as u64)?;
                        failures = 0;
                        report(offset);
                        continue;
                    }
                    404 => {
                        return Err(Error::Upload(format!(
                            "{LABEL} upload session expired or was removed"
                        )));
                    }
                    409 | 416 => Duration::ZERO,
                    429 | 500..=599 => pace.wait_for(Some(&r), failures),
                    _ => return Err(json_error(&format!("{LABEL} upload chunk"), &r)),
                },
                Err(_) => pace.wait_for(None, failures),
            };
            failures += 1;
            if failures >= MAX_FAILURES {
                return Err(Error::Upload(format!(
                    "{LABEL} upload chunk at {offset} failed {MAX_FAILURES} times in a row"
                )));
            }
            pace.sleep(wait, stop.cancel)?;
            // Ask the session where to continue.
            let status = request(Method::Get, &session, &[], Vec::new())?;
            if let Ok(r) = self.api.http().request(&status)
                && r.is_success()
            {
                offset = next_expected(&r, offset)?;
            }
        }
        Ok(())
    }

    fn upload_parallel(
        &self,
        file: &Path,
        name: &str,
        total: u64,
        cancel: &CancelToken,
        progress: Progress<'_>,
    ) -> Result<String, Error> {
        let num = total.div_ceil(self.part_target).clamp(1, MAX_SESSIONS);
        let part_size = total.div_ceil(num);
        let abort = AtomicBool::new(false);
        let sent = AtomicU64::new(0);
        let (tx, rx) = mpsc::channel::<(u64, Result<Part, Error>)>();
        let mut parts: Vec<Option<Part>> = vec![None; num as usize];
        let mut failure: Option<Error> = None;
        std::thread::scope(|s| {
            for i in 0..num {
                let (tx, abort, sent) = (tx.clone(), &abort, &sent);
                s.spawn(move || {
                    let stop = Stop { cancel, abort };
                    let start = i * part_size;
                    let size = part_size.min(total - start);
                    let part_name = format!("{name}.p{i}");
                    let mut seen = 0u64;
                    let res = hash_range(file, start, size, &stop).and_then(|sha256| {
                        let mut report = |now: u64| {
                            sent.fetch_add(now.saturating_sub(seen), Ordering::AcqRel);
                            seen = seen.max(now);
                        };
                        self.session_upload(
                            &self.graph_path(&part_name),
                            file,
                            start,
                            size,
                            &stop,
                            &mut report,
                        )?;
                        Ok(Part {
                            index: i,
                            name: part_name,
                            size,
                            sha256,
                        })
                    });
                    if res.is_err() {
                        abort.store(true, Ordering::Release);
                    }
                    // The receiver outlives the workers; a send can only fail after a panic.
                    let _ = tx.send((i, res));
                });
            }
            drop(tx);
            let mut pending = num;
            while pending > 0 {
                match rx.recv_timeout(Duration::from_millis(150)) {
                    Ok((i, Ok(p))) => {
                        parts[i as usize] = Some(p);
                        pending -= 1;
                    }
                    Ok((_, Err(e))) => {
                        pending -= 1;
                        // A real failure beats the Cancelled it caused in sibling parts.
                        if failure.is_none() || matches!(failure, Some(Error::Cancelled)) {
                            failure = Some(e);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                progress(sent.load(Ordering::Acquire).min(total), total);
            }
        });
        if let Some(e) = failure {
            return Err(e);
        }
        let parts: Vec<Part> = parts
            .into_iter()
            .collect::<Option<_>>()
            .ok_or_else(|| Error::Upload(format!("{LABEL} upload: a part did not finish")))?;
        let stop = Stop {
            cancel,
            abort: &AtomicBool::new(false),
        };
        let whole = hash_range(file, 0, total, &stop)?;
        let manifest = manifest_json(name, total, &whole, &parts);
        let manifest_path = format!("{}{MANIFEST_SUFFIX}", self.graph_path(name));
        self.put_small(
            &manifest_path,
            manifest.into_bytes(),
            "application/json",
            cancel,
            "upload manifest",
        )?;
        progress(total, total);
        Ok(format!("onedrive://{manifest_path}"))
    }

    /// Reads the item at `graph_path` into `out` (and `on` for hashing): the item URL answers
    /// with a redirect to a pre-authenticated URL, which is read with ranged GETs and without
    /// the bearer token.
    fn download_to(
        &self,
        graph_path: &str,
        out: &mut dyn Write,
        on: &mut dyn FnMut(&[u8]),
        cancel: &CancelToken,
    ) -> Result<u64, Error> {
        let mut direct = self.resolve(graph_path, cancel)?;
        let (mut at, mut refetched) = (0u64, false);
        let range_len = self.chunk as u64;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let range = format!("bytes={at}-{}", at + range_len - 1);
            let req = request(Method::Get, &direct, &[("range", &range)], Vec::new())?;
            let resp = self
                .api
                .send_plain(req, cancel, &format!("{LABEL} download"))?;
            let (bytes, total) = match resp.status.as_u16() {
                206 => {
                    let t = super::content_range_total(resp.headers.get("content-range"));
                    (resp.body, t)
                }
                200 => {
                    let t = Some(at + resp.body.len() as u64);
                    (resp.body, t)
                }
                416 if at == 0 => (Vec::new(), Some(0)),
                // The pre-authenticated URL expired: ask Graph for a fresh one, once.
                401 | 403 | 404 if !refetched => {
                    refetched = true;
                    direct = self.resolve(graph_path, cancel)?;
                    continue;
                }
                _ => return Err(json_error(&format!("{LABEL} download"), &resp)),
            };
            refetched = false;
            on(&bytes);
            out.write_all(&bytes)
                .map_err(|e| Error::Upload(format!("write download: {e}")))?;
            at += bytes.len() as u64;
            let done = match total {
                Some(t) => at >= t,
                None => (bytes.len() as u64) < range_len,
            };
            if done || bytes.is_empty() {
                return Ok(at);
            }
        }
    }

    /// The pre-authenticated URL of an item (Graph answers 302 with `Location`).
    fn resolve(&self, graph_path: &str, cancel: &CancelToken) -> Result<String, Error> {
        let url = self.item_url(graph_path, ":/content");
        let resp = self.call(Method::Get, &url, &[], Vec::new(), cancel, "download")?;
        match resp.status.as_u16() {
            301 | 302 | 303 | 307 | 308 => resp
                .headers
                .get("location")
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    Error::Upload(format!("{LABEL} download: redirect without Location"))
                }),
            404 => Err(Error::Upload(format!(
                "{LABEL} download: item {graph_path:?} not found"
            ))),
            _ => Err(json_error(&format!("{LABEL} download"), &resp)),
        }
    }

    fn manifest(
        &self,
        graph_path: &str,
        cancel: &CancelToken,
    ) -> Result<(String, Vec<Part>), Error> {
        let mut raw = Vec::new();
        self.download_to(graph_path, &mut raw, &mut |_| {}, cancel)?;
        let (file_name, parts) = parse_manifest(&raw)?;
        let folder = graph_path
            .strip_suffix(&format!("{file_name}{MANIFEST_SUFFIX}"))
            .map(|f| f.trim_end_matches('/'))
            .unwrap_or("")
            .to_owned();
        Ok((folder, parts))
    }

    /// Deletes one item; already gone counts as deleted.
    fn delete_item(&self, graph_path: &str, cancel: &CancelToken) -> Result<(), Error> {
        let url = self.item_url(graph_path, "");
        let resp = self.call(Method::Delete, &url, &[], Vec::new(), cancel, "delete")?;
        if resp.is_success() || resp.status.as_u16() == 404 {
            Ok(())
        } else {
            Err(json_error(&format!("{LABEL} delete"), &resp))
        }
    }
}

/// The start of the first range in a session's `nextExpectedRanges` (`"12345-"`), or
/// `fallback` when the answer has none.
fn next_expected(resp: &dl_http::Response, fallback: u64) -> Result<u64, Error> {
    let v = dl_json::from_str::<Value>(&String::from_utf8_lossy(&resp.body)).ok();
    let first = v
        .as_ref()
        .and_then(|v| v.get("nextExpectedRanges"))
        .and_then(|r| r.get_index(0))
        .and_then(Value::as_str);
    match first {
        None => Ok(fallback),
        Some(r) => r
            .split('-')
            .next()
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| Error::Upload(format!("{LABEL} upload: unreadable session range"))),
    }
}

fn manifest_json(name: &str, total: u64, sha256: &str, parts: &[Part]) -> String {
    let list: Vec<Value> = parts
        .iter()
        .map(|p| {
            let mut m = Map::new();
            m.insert("index", p.index);
            m.insert("name", p.name.as_str());
            m.insert("size", p.size);
            m.insert("sha256", p.sha256.as_str());
            Value::Object(m)
        })
        .collect();
    let mut m = Map::new();
    m.insert("version", 1u64);
    m.insert("file_name", name);
    m.insert("total_size", total);
    m.insert("part_count", parts.len() as u64);
    m.insert("sha256", sha256);
    m.insert("parts", Value::Array(list));
    dl_json::to_string(&Value::Object(m))
}

/// The file name and parts of a Go- or Rust-written `.odmanifest`.
fn parse_manifest(raw: &[u8]) -> Result<(String, Vec<Part>), Error> {
    let bad = || Error::Upload(format!("{LABEL}: the manifest is not valid"));
    let v = dl_json::from_str::<Value>(&String::from_utf8_lossy(raw)).map_err(|_| bad())?;
    let file_name = v.get("file_name").and_then(Value::as_str).ok_or_else(bad)?;
    let mut parts = Vec::new();
    for p in v.get("parts").and_then(Value::as_array).ok_or_else(bad)? {
        parts.push(Part {
            index: p
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or(parts.len() as u64),
            name: p
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(bad)?
                .to_owned(),
            size: p.get("size").and_then(Value::as_u64).unwrap_or(0),
            sha256: p
                .get("sha256")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        });
    }
    // Order by the recorded index, as Go's slice is ordered by it.
    parts.sort_by_key(|p| p.index);
    if parts.iter().any(|p| p.name.contains('/')) {
        return Err(bad());
    }
    Ok((file_name.to_owned(), parts))
}

impl Uploader for OneDrive {
    fn upload(
        &self,
        file: &Path,
        cancel: &CancelToken,
        progress: Progress<'_>,
    ) -> Result<String, Error> {
        let name = base_name(file)?;
        let total = std::fs::metadata(file)
            .map(|m| m.len())
            .map_err(|e| Error::Upload(format!("stat backup file: {e}")))?;
        let path = self.graph_path(&name);
        if total < SMALL_LIMIT {
            let mut f =
                File::open(file).map_err(|e| Error::Upload(format!("open backup file: {e}")))?;
            let data = read_at(&mut f, 0, total as usize)?;
            self.put_small(&path, data, "application/octet-stream", cancel, "upload")?;
            progress(total, total);
        } else if total <= PARALLEL_MIN {
            let abort = AtomicBool::new(false);
            let stop = Stop {
                cancel,
                abort: &abort,
            };
            self.session_upload(&path, file, 0, total, &stop, &mut |n| progress(n, total))?;
        } else {
            return self.upload_parallel(file, &name, total, cancel, progress);
        }
        Ok(format!("onedrive://{path}"))
    }

    fn download(&self, cloud_path: &str, dest: &Path, cancel: &CancelToken) -> Result<u64, Error> {
        let graph_path = parse_cloud_path(cloud_path)?;
        if !graph_path.ends_with(MANIFEST_SUFFIX) {
            let mut out = File::create(dest)
                .map_err(|e| Error::Upload(format!("create {}: {e}", dest.display())))?;
            let n = self.download_to(&graph_path, &mut out, &mut |_| {}, cancel)?;
            return finish(out, dest, n);
        }
        let (folder, parts) = self.manifest(&graph_path, cancel)?;
        let mut out = File::create(dest)
            .map_err(|e| Error::Upload(format!("create {}: {e}", dest.display())))?;
        let mut total = 0;
        for part in &parts {
            let path = if folder.is_empty() {
                part.name.clone()
            } else {
                format!("{folder}/{}", part.name)
            };
            let mut hash = Sha256::default();
            total += self
                .download_to(&path, &mut out, &mut |b| hash.update(b), cancel)
                .map_err(|e| Error::Upload(format!("part {} ({}): {e}", part.index, part.name)))?;
            let got = sha_hex(hash);
            if !part.sha256.is_empty() && got != part.sha256 {
                return Err(Error::Upload(format!(
                    "{LABEL} download: part {} checksum mismatch: got {got}, want {}",
                    part.name, part.sha256
                )));
            }
        }
        finish(out, dest, total)
    }

    fn delete(&self, cloud_path: &str) -> Result<(), Error> {
        let cancel = CancelToken::new();
        let graph_path = parse_cloud_path(cloud_path)?;
        if !graph_path.ends_with(MANIFEST_SUFFIX) {
            return self.delete_item(&graph_path, &cancel);
        }
        // Parts are real items beside the manifest: delete them all, the manifest last.
        let (folder, parts) = match self.manifest(&graph_path, &cancel) {
            Ok(m) => m,
            Err(e) if e.to_string().contains("not found") => return Ok(()),
            Err(e) => return Err(e),
        };
        let mut failed: VecDeque<String> = VecDeque::new();
        for part in &parts {
            let path = if folder.is_empty() {
                part.name.clone()
            } else {
                format!("{folder}/{}", part.name)
            };
            if let Err(e) = self.delete_item(&path, &cancel) {
                failed.push_back(format!("part {}: {e}", part.name));
            }
        }
        if let Err(e) = self.delete_item(&graph_path, &cancel) {
            failed.push_back(format!("manifest: {e}"));
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(Error::Upload(format!(
                "{LABEL} delete: {} of {} items failed: {}",
                failed.len(),
                parts.len() + 1,
                Vec::from(failed).join("; ")
            )))
        }
    }
}

fn finish(out: File, dest: &Path, n: u64) -> Result<u64, Error> {
    out.sync_all()
        .map_err(|e| Error::Upload(format!("flush {}: {e}", dest.display())))?;
    Ok(n)
}

#[cfg(test)]
#[path = "onedrive_tests.rs"]
mod tests;
