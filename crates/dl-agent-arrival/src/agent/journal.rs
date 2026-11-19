//! `IdempotencyLog`: executed PMS writes keyed by idempotency key, kept 48 h. A retried write
//! (after a timeout or a dropped connection) gets the first outcome back instead of writing
//! to the PMS twice. JSON lines, the same file the .NET agent kept, so the 48 h survive the
//! switch to this module.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use dl_json::Value;
use dl_time::OffsetDateTime;

use crate::agent::clock::Clock;
use crate::agent::vault::tmp_path;
use crate::contracts::PmsResponse;
use crate::error::Error;
use crate::stj;

/// How long a write is remembered.
pub const RETENTION_MS: i64 = 48 * 3600 * 1000;

#[derive(Debug, Clone)]
struct Entry {
    at: OffsetDateTime,
    response: PmsResponse,
}

#[derive(Debug)]
struct Inner {
    entries: HashMap<String, Entry>,
    file: Option<File>,
}

/// The journal.
pub struct Journal {
    path: PathBuf,
    clock: Arc<dyn Clock>,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// One stored line: `{"key":…,"at":…,"response":{…}}` (`SyncJson.Options`).
pub fn entry_line(key: &str, at: &OffsetDateTime, response: &PmsResponse) -> String {
    let mut out = String::from("{\"key\":");
    stj::escape_into(&mut out, key);
    out.push_str(",\"at\":");
    // A typed DateTimeOffset is written raw by System.Text.Json (no `\u002B` for `+`).
    out.push('"');
    out.push_str(&stj::num::format_dto(at));
    out.push('"');
    out.push_str(",\"response\":");
    response.write(&mut out);
    out.push('}');
    out
}

fn parse_line(line: &str) -> Option<(String, Entry)> {
    let v = stj::parse(line).ok()?;
    let m = v.as_object()?;
    let key = match stj::get_ci(m, "key") {
        Some(Value::String(s)) => s.clone(),
        _ => return None,
    };
    let at = match stj::get_ci(m, "at") {
        Some(Value::String(s)) => OffsetDateTime::parse_rfc3339(s).ok()?,
        _ => return None,
    };
    let response = PmsResponse::from_json(stj::get_ci(m, "response")?).ok()?;
    Some((key, Entry { at, response }))
}

impl Journal {
    /// Loads the journal: blank and torn lines are skipped, entries older than 48 h dropped,
    /// the last line of a key wins; then the file is rewritten without them.
    ///
    /// # Errors
    /// [`Error::Io`] when the file cannot be read or rewritten.
    pub fn open(path: &Path, clock: Arc<dyn Clock>) -> Result<Journal, Error> {
        let mut entries = HashMap::new();
        let mut skipped = 0usize;
        let mut order: Vec<String> = Vec::new();
        match std::fs::read(path) {
            Ok(bytes) => {
                let cutoff = clock.now_ms() - RETENTION_MS;
                for line in String::from_utf8_lossy(&bytes).lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match parse_line(line) {
                        Some((key, e)) if e.at.unix_millis() > cutoff => {
                            if !entries.contains_key(&key) {
                                order.push(key.clone());
                            }
                            entries.insert(key, e);
                        }
                        Some(_) => {}
                        None => skipped += 1,
                    }
                }
                if skipped > 0 {
                    dl_log::warn!("skipped unreadable idempotency journal lines"; count = skipped);
                }
                let mut text = String::new();
                for key in &order {
                    if let Some(e) = entries.get(key) {
                        text.push_str(&entry_line(key, &e.at, &e.response));
                        text.push_str(if cfg!(windows) { "\r\n" } else { "\n" });
                    }
                }
                let tmp = tmp_path(path);
                std::fs::write(&tmp, text).map_err(|e| Error::io("idempotency journal", e))?;
                std::fs::rename(&tmp, path).map_err(|e| Error::io("idempotency journal", e))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io("idempotency journal", e)),
        }
        Ok(Journal {
            path: path.to_path_buf(),
            clock,
            inner: Mutex::new(Inner {
                entries,
                file: None,
            }),
        })
    }

    /// `TryGet`: the remembered response for `key` (ordinal).
    pub fn get(&self, key: &str) -> Option<PmsResponse> {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.entries.get(key).map(|e| e.response.clone())
    }

    /// Number of remembered writes.
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .len()
    }

    /// Whether nothing is remembered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `AppendAsync`: remembers `response` under `key` and appends its line.
    ///
    /// # Errors
    /// [`Error::Io`] when the line cannot be written (the entry is still kept in memory).
    pub fn append(&self, key: &str, response: &PmsResponse) -> Result<(), Error> {
        let at = self.clock.now();
        let line = entry_line(key, &at, response) + "\n";
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.entries.insert(
            key.to_string(),
            Entry {
                at,
                response: response.clone(),
            },
        );
        if inner.file.is_none() {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| Error::io("idempotency journal", e))?;
            }
            let f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .map_err(|e| Error::io("idempotency journal", e))?;
            inner.file = Some(f);
        }
        let result = match inner.file.as_mut() {
            Some(f) => f.write_all(line.as_bytes()).and_then(|()| f.flush()),
            None => Ok(()),
        };
        if let Err(e) = result {
            inner.file = None;
            return Err(Error::io("idempotency journal", e));
        }
        Ok(())
    }

    /// Forces appended lines to disk (on stop).
    ///
    /// # Errors
    /// [`Error::Io`].
    pub fn flush(&self) -> Result<(), Error> {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        match &inner.file {
            Some(f) => f
                .sync_all()
                .map_err(|e| Error::io("idempotency journal", e)),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::clock::SystemClock;
    use crate::contracts::Status;

    #[test]
    fn replays_survive_a_reload_and_old_or_torn_lines_are_dropped() {
        let dir = std::env::temp_dir().join(format!("dl-arrival-journal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-idempotency.jsonl");
        let old =
            OffsetDateTime::from_unix_millis(SystemClock.now_ms() - RETENTION_MS - 1000).unwrap();
        let stale = PmsResponse::bare("r0", Status::Ok, None);
        let seed = format!("{}\n\n{{torn\n", entry_line("old", &old, &stale));
        std::fs::write(&path, seed).unwrap();
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let j = Journal::open(&path, clock.clone()).unwrap();
        assert!(j.is_empty());
        let first = PmsResponse::bare("r1", Status::Ok, None);
        j.append("k1", &first).unwrap();
        j.append("k1", &PmsResponse::bare("r2", Status::Ok, None))
            .unwrap();
        j.flush().unwrap();
        let j2 = Journal::open(&path, clock).unwrap();
        assert_eq!(j2.len(), 1);
        assert_eq!(j2.get("k1").unwrap().request_id, "r2");
        assert!(j2.get("K1").is_none(), "keys are ordinal");
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
