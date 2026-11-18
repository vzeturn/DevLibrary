//! The durable local queue (a port of `outbox.go`).
//!
//! The Go agent keeps it in a bbolt file; this agent keeps the same two collections in one
//! JSON file rewritten atomically (temporary file, fsync, rename) on every change. It is
//! small: the push cycle drains it every poll interval.
//!
//! It never opens the Go agent's `outbox.db`. Items still queued there when the Go agent
//! is replaced are read again from SQL on the next cycle, because their source rows were
//! not marked collected; a write-back command whose "applied" mark is lost is applied once
//! more, which writes the same invoice number again.

use std::path::{Path, PathBuf};

use dl_json::Value;

use crate::agent_config::write_atomic;
use crate::gojson::{self, Obj, array};

/// Applied write-back ids kept for idempotency (the oldest are dropped beyond this).
const MAX_APPLIED: usize = 50_000;

/// One pending push.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The bill's source reference (`FO:<folio>` / `POS:<check>`), the queue key.
    pub source_ref: String,
    /// The bill id in SMILE.
    pub bill_id: String,
    /// `FO` or `POS`.
    pub bill_type: String,
    /// Unix nanoseconds; fixes drain order (oldest first).
    pub enqueued_at: i128,
    /// The PushItem JSON, sent as is.
    pub item_json: String,
}

/// The queue file and its contents.
pub struct Outbox {
    path: PathBuf,
    entries: Vec<Entry>,
    applied: Vec<String>,
}

fn now_nanos() -> i128 {
    dl_time::OffsetDateTime::now_utc().unix_nanos()
}

impl Outbox {
    /// The file this agent uses next to the configured (Go) outbox path:
    /// `outbox.db` → `outbox.rs.json`.
    pub fn path_for(configured: &str) -> PathBuf {
        Path::new(configured).with_extension("rs.json")
    }

    /// Opens (or creates) the queue.
    pub fn open(path: &Path) -> Result<Outbox, String> {
        let mut ob = Outbox {
            path: path.to_owned(),
            entries: Vec::new(),
            applied: Vec::new(),
        };
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let v = dl_json::parse(&text).map_err(|e| format!("agent: open outbox: {e}"))?;
                for e in v
                    .get("outbox")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                {
                    let s = |k: &str| {
                        e.get(k)
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned()
                    };
                    let at = e
                        .get("enqueued_at")
                        .and_then(Value::as_str)
                        .and_then(|t| t.parse().ok())
                        .unwrap_or(0);
                    ob.entries.push(Entry {
                        source_ref: s("source_ref"),
                        bill_id: s("bill_id"),
                        bill_type: s("bill_type"),
                        enqueued_at: at,
                        item_json: s("item"),
                    });
                }
                for a in v
                    .get("applied")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                {
                    if let Some(id) = a.as_str() {
                        ob.applied.push(id.to_owned());
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| format!("agent: open outbox: {e}"))?;
                }
                ob.flush()?;
            }
            Err(e) => return Err(format!("agent: open outbox: {e}")),
        }
        Ok(ob)
    }

    fn flush(&self) -> Result<(), String> {
        let mut out = String::new();
        Obj::new(&mut out)
            .int("version", 1)
            .raw("outbox", |o| {
                array(o, Some(&self.entries), |o, e| {
                    Obj::new(o)
                        .str("source_ref", &e.source_ref)
                        .str("bill_id", &e.bill_id)
                        .str("bill_type", &e.bill_type)
                        .str("enqueued_at", &e.enqueued_at.to_string())
                        .str("item", &e.item_json)
                        .end();
                })
            })
            .raw("applied", |o| {
                array(o, Some(&self.applied), |o, s| gojson::string(o, s))
            })
            .end();
        write_atomic(&self.path, out.as_bytes()).map_err(|e| format!("agent: write outbox: {e}"))
    }

    /// `EnqueuePush`: last writer wins per source ref.
    pub fn enqueue(
        &mut self,
        source_ref: &str,
        item_json: String,
        bill_id: &str,
        bill_type: &str,
    ) -> Result<(), String> {
        let e = Entry {
            source_ref: source_ref.to_owned(),
            bill_id: bill_id.to_owned(),
            bill_type: bill_type.to_owned(),
            enqueued_at: now_nanos(),
            item_json,
        };
        match self.entries.iter_mut().find(|x| x.source_ref == source_ref) {
            Some(x) => *x = e,
            None => self.entries.push(e),
        }
        self.flush()
    }

    /// `DrainPush`: up to `n` entries (all when `n <= 0`), oldest first; not removed.
    pub fn drain(&self, n: i64) -> Vec<Entry> {
        let mut v = self.entries.clone();
        v.sort_by_key(|e| e.enqueued_at);
        if n > 0 && v.len() as i64 > n {
            v.truncate(n as usize);
        }
        v
    }

    /// `RemovePush`.
    pub fn remove(&mut self, source_ref: &str) -> Result<(), String> {
        let before = self.entries.len();
        self.entries.retain(|e| e.source_ref != source_ref);
        if self.entries.len() == before {
            Ok(())
        } else {
            self.flush()
        }
    }

    /// `MarkApplied`.
    pub fn mark_applied(&mut self, cmd_id: &str) -> Result<(), String> {
        if self.is_applied(cmd_id) {
            return Ok(());
        }
        self.applied.push(cmd_id.to_owned());
        if self.applied.len() > MAX_APPLIED {
            let drop = self.applied.len() - MAX_APPLIED;
            self.applied.drain(..drop);
        }
        self.flush()
    }

    /// `IsApplied`.
    pub fn is_applied(&self, cmd_id: &str) -> bool {
        self.applied.iter().any(|a| a == cmd_id)
    }

    /// Pending entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is pending.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_queue() {
        let dir = std::env::temp_dir().join(format!("mmei-outbox-{}", std::process::id()));
        let path = Outbox::path_for(&dir.join("outbox.db").to_string_lossy());
        assert!(path.to_string_lossy().ends_with("outbox.rs.json"));
        let _ = std::fs::remove_file(&path);
        let mut ob = Outbox::open(&path).unwrap();
        ob.enqueue("FO:1", r#"{"a":"<1>"}"#.into(), "1", "FO")
            .unwrap();
        ob.enqueue("POS:2", "{}".into(), "2", "POS").unwrap();
        ob.enqueue("FO:1", r#"{"a":2}"#.into(), "1", "FO").unwrap();
        ob.mark_applied("c1").unwrap();
        drop(ob);
        let mut ob = Outbox::open(&path).unwrap();
        let d = ob.drain(0);
        assert_eq!(d.len(), 2);
        assert_eq!(
            d.iter().find(|e| e.source_ref == "FO:1").unwrap().item_json,
            r#"{"a":2}"#
        );
        assert_eq!(d[0].source_ref, "POS:2", "re-enqueued FO:1 is newer");
        assert_eq!(ob.drain(1).len(), 1);
        assert!(ob.is_applied("c1") && !ob.is_applied("c2"));
        ob.remove("POS:2").unwrap();
        assert_eq!(Outbox::open(&path).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
