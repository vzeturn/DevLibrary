//! Crash-safe JSON files in the agent's data directory.
//!
//! Writes go to `<file>.tmp`, are flushed to disk, then renamed over the target, so a power
//! cut leaves either the old or the new document, never half of one.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use dl_json::Value;

use crate::Error;
use crate::error::io;

/// Reads a JSON document; `Ok(None)` when the file does not exist.
pub fn read_json(path: &Path) -> Result<Option<Value>, Error> {
    match fs::read_to_string(path) {
        Ok(text) => dl_json::parse(&text)
            .map(Some)
            .map_err(|e| Error::Json(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io("read state file", e)),
    }
}

/// Writes a JSON document atomically (pretty-printed, for people reading the data dir).
pub fn write_json(path: &Path, value: &Value) -> Result<(), Error> {
    write_bytes(path, dl_json::to_string_pretty(value).as_bytes())
}

/// Writes bytes atomically.
pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        io("create data dir", fs::create_dir_all(dir))?;
    }
    let tmp = tmp_path(path);
    {
        let mut f = io("create temp file", fs::File::create(&tmp))?;
        io("write temp file", f.write_all(bytes))?;
        io("flush temp file", f.sync_all())?;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp); // best effort; the rename error is what matters
        return Err(Error::Io("replace state file", e));
    }
    Ok(())
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

#[cfg(test)]
pub(crate) mod testdir {
    use std::path::PathBuf;

    /// A fresh, empty directory under the system temp dir, removed on drop.
    pub(crate) struct TestDir(pub(crate) PathBuf);

    impl TestDir {
        pub(crate) fn new(tag: &str) -> TestDir {
            let n = dl_rand::u64();
            let p = std::env::temp_dir().join(format!("dl-agent-{tag}-{n:016x}"));
            std::fs::create_dir_all(&p).unwrap();
            TestDir(p)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testdir::TestDir;
    use super::*;
    use dl_json::json;

    #[test]
    fn missing_file_reads_as_none_and_round_trips() {
        let d = TestDir::new("store");
        let p = d.0.join("sub/state.json");
        assert!(read_json(&p).unwrap().is_none());
        write_json(&p, &json!({"a": 1, "b": "hai"})).unwrap();
        let v = read_json(&p).unwrap().unwrap();
        assert_eq!(v.get("b").and_then(Value::as_str), Some("hai"));
        assert!(!tmp_path(&p).exists());
    }

    #[test]
    fn corrupt_file_is_an_error_not_empty() {
        let d = TestDir::new("store-bad");
        let p = d.0.join("bad.json");
        std::fs::write(&p, "{not json").unwrap();
        assert!(matches!(read_json(&p), Err(Error::Json(_))));
    }
}
