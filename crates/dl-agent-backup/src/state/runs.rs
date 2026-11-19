//! When each schedule last ran (ms), so a restart does not re-run a schedule that already
//! ran in this period and does not miss one that came due while the agent was down.

use std::collections::BTreeMap;
use std::path::PathBuf;

use dl_json::{Map, Value};

use super::JsonFile;
use crate::Error;

/// The last-run file.
#[derive(Debug)]
pub struct Runs {
    file: JsonFile,
    last: BTreeMap<String, u64>,
}

impl Runs {
    /// Opens (or starts) the file at `path`.
    pub fn open(path: PathBuf) -> Result<Runs, Error> {
        let file = JsonFile::new(path);
        let mut last = BTreeMap::new();
        if let Some(Value::Object(m)) = file.load()? {
            for (k, v) in m.iter() {
                if let Some(at) = v.as_u64() {
                    last.insert(k.to_owned(), at);
                }
            }
        }
        Ok(Runs { file, last })
    }

    /// When `schedule_id` last ran; `None` when it never did.
    pub fn last_run(&self, schedule_id: &str) -> Option<u64> {
        self.last.get(schedule_id).copied()
    }

    /// Records a run.
    pub fn mark(&mut self, schedule_id: &str, at: u64) -> Result<(), Error> {
        self.last.insert(schedule_id.to_owned(), at);
        let mut m = Map::new();
        for (k, v) in &self.last {
            m.insert(k.as_str(), *v);
        }
        self.file.store(&Value::Object(m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::temp_dir;

    #[test]
    fn marks_survive_reopen() {
        let dir = temp_dir("runs");
        let mut r = Runs::open(dir.join("runs.enc")).unwrap();
        assert_eq!(r.last_run("s"), None);
        r.mark("s", 42).unwrap();
        assert_eq!(
            Runs::open(dir.join("runs.enc")).unwrap().last_run("s"),
            Some(42)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
