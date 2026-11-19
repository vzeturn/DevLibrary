//! The module's durable state under `<module data dir>/backup/`: the job queue, the report
//! outbox, the local backup history and the last run of each schedule. Every file is sealed
//! ([`crate::seal`]) and replaced atomically, so a crash leaves the old or the new version.
//! The Go agent's bbolt files are not read: production had no agents when this module
//! shipped, so it starts clean.

pub mod history;
pub mod outbox;
pub mod queue;
pub mod runs;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use dl_json::Value;

use crate::Error;

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The directory holding the state files: `<module data dir>/backup`.
pub fn state_dir(module_data_dir: &Path) -> PathBuf {
    module_data_dir.join("backup")
}

/// A sealed JSON document on disk.
#[derive(Debug, Clone)]
pub struct JsonFile {
    path: PathBuf,
}

impl JsonFile {
    /// The file at `path`; its directory is created on the first write.
    pub fn new(path: PathBuf) -> JsonFile {
        JsonFile { path }
    }

    /// Where it lives.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The stored document, or `None` before the first write.
    pub fn load(&self) -> Result<Option<Value>, Error> {
        crate::seal::read_json(&self.path)
    }

    /// Replaces the stored document.
    pub fn store(&self, doc: &Value) -> Result<(), Error> {
        crate::seal::write_json(&self.path, doc)
    }
}

/// Reads a string field.
pub(crate) fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Reads an unsigned field.
pub(crate) fn u(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

#[cfg(test)]
pub(crate) fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dlbk-{tag}-{}", dl_uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
