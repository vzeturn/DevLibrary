//! Reports waiting for the hub. A report is added before it is posted and acknowledged
//! after the hub took it, so a hub outage or a restart loses nothing; the sync loop drains
//! it at start and every two minutes (the Go `outbox` package).

use std::path::PathBuf;

use dl_json::{Map, Value};

use super::{JsonFile, s};
use crate::Error;

/// The most reports kept; past it the oldest is dropped (and counted).
pub const CAP: usize = 1000;

/// One undelivered report.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// `backup:<historyId>`, `restore:<requestId>` or `command:<commandId>`.
    pub key: String,
    /// The `POST /api/agent/report` body.
    pub body: Value,
}

/// The outbox file.
#[derive(Debug)]
pub struct Outbox {
    file: JsonFile,
    entries: Vec<Entry>,
    dropped: u64,
}

impl Outbox {
    /// Opens (or starts) the outbox at `path`.
    pub fn open(path: PathBuf) -> Result<Outbox, Error> {
        let file = JsonFile::new(path);
        let entries = match file.load()? {
            Some(doc) => doc
                .get("entries")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|e| Entry {
                            key: s(e, "key"),
                            body: e.get("body").cloned().unwrap_or(Value::Null),
                        })
                        .filter(|e| !e.key.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            None => Vec::new(),
        };
        Ok(Outbox {
            file,
            entries,
            dropped: 0,
        })
    }

    fn persist(&self) -> Result<(), Error> {
        let list: Vec<Value> = self
            .entries
            .iter()
            .map(|e| {
                let mut m = Map::new();
                m.insert("key", e.key.as_str());
                m.insert("body", e.body.clone());
                Value::Object(m)
            })
            .collect();
        let mut m = Map::new();
        m.insert("entries", list);
        self.file.store(&Value::Object(m))
    }

    /// Stores a report under `key`, replacing an older one with the same key.
    pub fn add(&mut self, key: &str, body: Value) -> Result<(), Error> {
        self.entries.retain(|e| e.key != key);
        self.entries.push(Entry {
            key: key.to_owned(),
            body,
        });
        if self.entries.len() > CAP {
            let over = self.entries.len() - CAP;
            self.entries.drain(..over);
            self.dropped += over as u64;
        }
        self.persist()
    }

    /// The hub took the report.
    pub fn ack(&mut self, key: &str) -> Result<(), Error> {
        let before = self.entries.len();
        self.entries.retain(|e| e.key != key);
        if before != self.entries.len() {
            self.persist()?;
        }
        Ok(())
    }

    /// The hub took `body` under `key`; a newer report stored under the same key since then
    /// stays.
    pub fn ack_sent(&mut self, key: &str, body: &Value) -> Result<(), Error> {
        let before = self.entries.len();
        self.entries.retain(|e| e.key != key || &e.body != body);
        if before != self.entries.len() {
            self.persist()?;
        }
        Ok(())
    }

    /// Undelivered reports, oldest first.
    pub fn pending(&self) -> Vec<Entry> {
        self.entries.clone()
    }

    /// How many reports wait.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing waits.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Reports dropped since start because the outbox was full.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::temp_dir;
    use dl_json::json;

    #[test]
    fn add_ack_and_survive_reopen() {
        let dir = temp_dir("outbox");
        let path = dir.join("outbox.enc");
        let mut o = Outbox::open(path.clone()).unwrap();
        o.add("backup:h1", json!({"historyId": "h1"})).unwrap();
        o.add("backup:h2", json!({"historyId": "h2"})).unwrap();
        o.add("backup:h1", json!({"historyId": "h1", "v": 2}))
            .unwrap();
        let o2 = Outbox::open(path.clone()).unwrap();
        let keys: Vec<String> = o2.pending().into_iter().map(|e| e.key).collect();
        assert_eq!(keys, ["backup:h2", "backup:h1"]);
        o.ack("backup:h2").unwrap();
        o.ack("unknown").unwrap();
        assert_eq!(Outbox::open(path).unwrap().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cap_drops_the_oldest() {
        let dir = temp_dir("outbox-cap");
        let mut o = Outbox::open(dir.join("o.enc")).unwrap();
        o.entries = (0..CAP)
            .map(|i| Entry {
                key: format!("k{i}"),
                body: Value::Null,
            })
            .collect();
        o.add("new", Value::Null).unwrap();
        assert_eq!(o.len(), CAP);
        assert_eq!(o.pending()[0].key, "k1");
        assert_eq!(o.dropped(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
