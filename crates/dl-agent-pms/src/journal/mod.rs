//! The local op journal: `request_key -> result frame` for every keyed write the agent
//! applied, so a `replay: true` op is answered without running anything (SMILE-only
//! contract, ADR 2026-11-19 section 2). It lives in the agent state directory, never in a
//! database: no object is created on a customer database.
//!
//! One file per key under `<state dir>/op-journal/`, named by the SHA-256 of the key (a
//! key may hold `:`, which Windows refuses in a file name). Line 1 is a header
//! `{"v":1,"op":"<name>","at":<unix ms>}`, line 2 the result frame as it was sent. A
//! file is written to a temporary name in the same directory, flushed to disk and renamed,
//! so a crash leaves either the whole entry or none (a stale temporary file is removed on
//! the next open). Entries are kept [`RETENTION`] (at least the contract's 7 days) and
//! pruned by age; past [`MAX_ENTRIES`] or [`MAX_BYTES`] the journal refuses new entries
//! and the caller refuses the keyed write instead of applying it unrecorded.
//!
//! Request keys and results are never logged.

pub mod exec;

use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dl_json::Value;

use crate::json;
use crate::wire::{self, Frame, OpResult};

/// The journal directory inside the state directory.
pub const DIR: &str = "op-journal";
/// How long an entry is kept: the contract's 7 days plus a day of clock slack.
pub const RETENTION: Duration = Duration::from_secs(8 * 24 * 3600);
/// Entries kept at most.
pub const MAX_ENTRIES: u64 = 100_000;
/// Bytes kept at most.
pub const MAX_BYTES: u64 = 256 * 1024 * 1024;
/// How often a running journal prunes expired entries.
pub const PRUNE_EVERY: Duration = Duration::from_secs(3600);

const VERSION: i64 = 1;
const ENTRY_EXT: &str = "json";
const TMP_MARK: &str = ".tmp-";

/// The journal's bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Entry lifetime.
    pub retention: Duration,
    /// Entries kept at most.
    pub max_entries: u64,
    /// Bytes kept at most.
    pub max_bytes: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            retention: RETENTION,
            max_entries: MAX_ENTRIES,
            max_bytes: MAX_BYTES,
        }
    }
}

/// A journal failure. Messages name the directory, never a key or a result.
#[derive(Debug)]
pub enum JournalError {
    /// The cap is reached; nothing is recorded until entries expire.
    Full {
        /// Entries now.
        entries: u64,
        /// Bytes now.
        bytes: u64,
    },
    /// A stored entry does not parse.
    Corrupt(String),
    /// The file system.
    Io(String),
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalError::Full { entries, bytes } => write!(
                f,
                "op journal full ({entries} entries, {bytes} bytes): keyed writes are refused until entries expire"
            ),
            JournalError::Corrupt(m) => write!(f, "op journal entry corrupt: {m}"),
            JournalError::Io(m) => write!(f, "op journal: {m}"),
        }
    }
}

impl std::error::Error for JournalError {}

/// One recorded op.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The op name the key was applied for.
    pub op: String,
    /// When it was recorded (unix ms).
    pub at_ms: i64,
    /// The result frame as sent.
    pub result: OpResult,
}

/// The journal's state for `status` and the self-test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    /// The directory.
    pub dir: PathBuf,
    /// Entries kept.
    pub entries: u64,
    /// Bytes kept.
    pub bytes: u64,
    /// The bounds.
    pub limits: Limits,
    /// The last write or prune failure, if the last attempt failed.
    pub last_error: Option<String>,
}

impl Health {
    /// The cap is reached.
    pub fn full(&self) -> bool {
        self.entries >= self.limits.max_entries || self.bytes >= self.limits.max_bytes
    }

    /// The health as JSON (no key, no result).
    pub fn to_json(&self) -> Value {
        dl_json::json!({
            "dir": self.dir.display().to_string(),
            "entries": self.entries,
            "bytes": self.bytes,
            "max_entries": self.limits.max_entries,
            "max_bytes": self.limits.max_bytes,
            "retention_days": self.limits.retention.as_secs() / 86_400,
            "full": self.full(),
            "last_error": self.last_error.clone(),
        })
    }
}

struct State {
    entries: u64,
    bytes: u64,
    pruned: Instant,
    last_error: Option<String>,
}

/// The op journal of one state directory.
pub struct Journal {
    dir: PathBuf,
    limits: Limits,
    st: Mutex<State>,
}

impl fmt::Debug for Journal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Journal").field("dir", &self.dir).finish()
    }
}

fn io(what: &str, path: &Path, e: &std::io::Error) -> JournalError {
    JournalError::Io(format!("{what} {}: {e}", path.display()))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The file name of `key`'s entry.
pub fn entry_name(key: &str) -> String {
    format!("{}.{ENTRY_EXT}", crate::sha256_hex(key.as_bytes()))
}

/// What a directory scan found.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Scan {
    /// Entries kept.
    pub entries: u64,
    /// Bytes kept.
    pub bytes: u64,
    /// Expired entries removed.
    pub expired: u64,
    /// Stale temporary files removed.
    pub stale: u64,
}

/// Counts the entries of `dir`; with `clean`, removes expired entries and stale
/// temporary files (`temp_too`: also temporaries, safe only before any write starts).
fn scan(
    dir: &Path,
    retention: Duration,
    clean: bool,
    temp_too: bool,
) -> Result<Scan, JournalError> {
    let mut s = Scan::default();
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(s),
        Err(e) => return Err(io("read", dir, &e)),
    };
    let now = SystemTime::now();
    for item in rd {
        let item = item.map_err(|e| io("read", dir, &e))?;
        let name = item.file_name().to_string_lossy().into_owned();
        let path = item.path();
        let Ok(meta) = item.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        if name.contains(TMP_MARK) {
            if clean && temp_too {
                fs::remove_file(&path).map_err(|e| io("remove", &path, &e))?;
                s.stale += 1;
            }
            continue;
        }
        if !name.ends_with(&format!(".{ENTRY_EXT}")) {
            continue;
        }
        let age = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .unwrap_or_default();
        if clean && age > retention {
            fs::remove_file(&path).map_err(|e| io("remove", &path, &e))?;
            s.expired += 1;
            continue;
        }
        s.entries += 1;
        s.bytes += meta.len();
    }
    Ok(s)
}

/// The journal of `state_dir` as it is on disk, without creating or changing anything
/// (the self-test, which may run beside the service).
pub fn inspect(state_dir: &Path) -> Result<Health, JournalError> {
    let dir = state_dir.join(DIR);
    let s = scan(&dir, RETENTION, false, false)?;
    Ok(Health {
        dir,
        entries: s.entries,
        bytes: s.bytes,
        limits: Limits::default(),
        last_error: None,
    })
}

/// Writes `data` to `path` so a crash leaves either the old file or the whole new one.
fn write_atomic(path: &Path, data: &[u8]) -> Result<(), JournalError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!(
        "{name}{TMP_MARK}{}-{}",
        std::process::id(),
        dl_rand::range(0..u64::MAX)
    ));
    let res = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| io("create", &tmp, &e))?;
        f.write_all(data).map_err(|e| io("write", &tmp, &e))?;
        f.sync_all().map_err(|e| io("flush", &tmp, &e))?;
        drop(f);
        fs::rename(&tmp, path).map_err(|e| io("rename", path, &e))?;
        sync_dir(dir)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp); // best effort: a leftover is removed on the next open
    }
    res
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<(), JournalError> {
    fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| io("flush", dir, &e))
}

#[cfg(not(unix))]
fn sync_dir(_: &Path) -> Result<(), JournalError> {
    // NTFS commits the rename in its own log; a directory cannot be opened for flushing.
    Ok(())
}

impl Journal {
    /// Opens (creating) the journal of `state_dir` with the default bounds, removing
    /// expired entries and stale temporary files.
    pub fn open(state_dir: &Path) -> Result<Journal, JournalError> {
        Journal::open_dir(&state_dir.join(DIR), Limits::default())
    }

    /// Opens (creating) the journal in `dir` with `limits`.
    pub fn open_dir(dir: &Path, limits: Limits) -> Result<Journal, JournalError> {
        fs::create_dir_all(dir).map_err(|e| io("create", dir, &e))?;
        let s = scan(dir, limits.retention, true, true)?;
        Ok(Journal {
            dir: dir.to_path_buf(),
            limits,
            st: Mutex::new(State {
                entries: s.entries,
                bytes: s.bytes,
                pruned: Instant::now(),
                last_error: None,
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The entry for `key`, if one is kept.
    pub fn get(&self, key: &str) -> Result<Option<Entry>, JournalError> {
        let path = self.dir.join(entry_name(key));
        let raw = match fs::read(&path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io("read", &path, &e)),
        };
        parse_entry(&raw).map(Some)
    }

    /// Records `result` (an applied op) for `key`.
    ///
    /// # Errors
    /// [`JournalError::Full`] at the cap (nothing written), or a file-system failure.
    pub fn put(&self, key: &str, op: &str, result: &OpResult) -> Result<(), JournalError> {
        let data = encode_entry(op, now_ms(), result);
        let len = data.len() as u64;
        if self.lock().pruned.elapsed() >= PRUNE_EVERY {
            self.prune()?;
        }
        {
            let st = self.lock();
            if st.entries + 1 > self.limits.max_entries || st.bytes + len > self.limits.max_bytes {
                drop(st);
                self.prune()?;
            }
        }
        let mut st = self.lock();
        if st.entries + 1 > self.limits.max_entries || st.bytes + len > self.limits.max_bytes {
            return Err(JournalError::Full {
                entries: st.entries,
                bytes: st.bytes,
            });
        }
        let path = self.dir.join(entry_name(key));
        let old = fs::metadata(&path).ok().map(|m| m.len());
        match write_atomic(&path, &data) {
            Ok(()) => {
                match old {
                    Some(n) => st.bytes = st.bytes.saturating_sub(n) + len,
                    None => {
                        st.entries += 1;
                        st.bytes += len;
                    }
                }
                st.last_error = None;
                Ok(())
            }
            Err(e) => {
                st.last_error = Some(e.to_string());
                Err(e)
            }
        }
    }

    /// Removes expired entries and recounts; returns how many expired.
    pub fn prune(&self) -> Result<u64, JournalError> {
        let res = scan(&self.dir, self.limits.retention, true, false);
        let mut st = self.lock();
        st.pruned = Instant::now();
        match res {
            Ok(s) => {
                st.entries = s.entries;
                st.bytes = s.bytes;
                Ok(s.expired)
            }
            Err(e) => {
                st.last_error = Some(e.to_string());
                Err(e)
            }
        }
    }

    /// The journal's state.
    pub fn health(&self) -> Health {
        let st = self.lock();
        Health {
            dir: self.dir.clone(),
            entries: st.entries,
            bytes: st.bytes,
            limits: self.limits,
            last_error: st.last_error.clone(),
        }
    }
}

/// The two lines of an entry.
pub fn encode_entry(op: &str, at_ms: i64, result: &OpResult) -> Vec<u8> {
    let head = format!(
        "{{\"v\":{VERSION},\"op\":{},\"at\":{at_ms}}}",
        json::quote(op)
    );
    let body = wire::to_json(&Frame::Result(result.clone()));
    format!("{head}\n{body}\n").into_bytes()
}

/// Parses an entry written by [`encode_entry`].
pub fn parse_entry(raw: &[u8]) -> Result<Entry, JournalError> {
    let bad = |m: &str| JournalError::Corrupt(m.to_owned());
    let text = std::str::from_utf8(raw).map_err(|_| bad("not UTF-8"))?;
    let (head, body) = text.split_once('\n').ok_or_else(|| bad("no result line"))?;
    let h = json::parse_object(head.as_bytes()).ok_or_else(|| bad("bad header"))?;
    if h.get("v").and_then(json::as_int) != Some(VERSION) {
        return Err(bad("unknown version"));
    }
    let op = match h.get("op") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Err(bad("header without op")),
    };
    let at_ms = h
        .get("at")
        .and_then(json::as_int)
        .ok_or_else(|| bad("header without at"))?;
    match wire::decode_json(body.trim_end().as_bytes()) {
        Ok(Frame::Result(result)) => Ok(Entry { op, at_ms, result }),
        Ok(_) => Err(bad("not a result frame")),
        Err(e) => Err(JournalError::Corrupt(e.to_string())),
    }
}

#[cfg(test)]
mod tests;
