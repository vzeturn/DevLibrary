//! The local record of finished backups (the Go `jobhistory` store): restore finds the file
//! by the hub history id, retention lists a schedule's backups newest first and forgets the
//! ones it expired. A bounded ring of recent job outcomes backs `backup.status`.

use std::collections::VecDeque;
use std::path::PathBuf;

use dl_json::{Map, Value};

use super::{JsonFile, s, u};
use crate::Error;

/// Outcomes kept for `backup.status`.
pub const RECENT_CAP: usize = 50;

/// One successful backup.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BackupRecord {
    /// The schedule that made it.
    pub schedule_id: String,
    /// The hub history id it was reported under.
    pub hub_history_id: String,
    /// The database it holds.
    pub database_name: String,
    /// Where SQL Server wrote it (empty once uploaded and removed).
    pub local_file_path: String,
    /// The destination path (`s3://…`, a blob URL, a folder path), empty without upload.
    pub cloud_file_path: String,
    /// The destination provider, needed to delete or download it later.
    pub provider: String,
    /// When it finished (ms).
    pub created_at: u64,
}

impl BackupRecord {
    fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("scheduleId", self.schedule_id.as_str());
        m.insert("hubHistoryId", self.hub_history_id.as_str());
        m.insert("databaseName", self.database_name.as_str());
        m.insert("localFilePath", self.local_file_path.as_str());
        m.insert("cloudFilePath", self.cloud_file_path.as_str());
        m.insert("provider", self.provider.as_str());
        m.insert("createdAt", self.created_at);
        Value::Object(m)
    }

    fn from_json(v: &Value) -> BackupRecord {
        BackupRecord {
            schedule_id: s(v, "scheduleId"),
            hub_history_id: s(v, "hubHistoryId"),
            database_name: s(v, "databaseName"),
            local_file_path: s(v, "localFilePath"),
            cloud_file_path: s(v, "cloudFilePath"),
            provider: s(v, "provider"),
            created_at: u(v, "createdAt"),
        }
    }
}

/// One finished job, kept in memory for `backup.status`.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// Hub history id.
    pub history_id: String,
    /// Schedule id.
    pub schedule_id: String,
    /// Database name.
    pub database: String,
    /// Whether it succeeded.
    pub ok: bool,
    /// The failure, if any.
    pub error: String,
    /// Bytes written.
    pub bytes: u64,
    /// When it finished (ms).
    pub finished_at: u64,
}

/// The history file plus the in-memory ring.
#[derive(Debug)]
pub struct History {
    file: JsonFile,
    records: Vec<BackupRecord>,
    recent: VecDeque<Outcome>,
}

impl History {
    /// Opens (or starts) the history at `path`; entries without a schedule id are skipped.
    pub fn open(path: PathBuf) -> Result<History, Error> {
        let file = JsonFile::new(path);
        let records = match file.load()? {
            Some(doc) => doc
                .get("records")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(BackupRecord::from_json)
                        .filter(|r| !r.schedule_id.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            None => Vec::new(),
        };
        Ok(History {
            file,
            records,
            recent: VecDeque::new(),
        })
    }

    fn persist(&self) -> Result<(), Error> {
        let list: Vec<Value> = self.records.iter().map(BackupRecord::to_json).collect();
        let mut m = Map::new();
        m.insert("records", list);
        self.file.store(&Value::Object(m))
    }

    /// Records a successful backup.
    pub fn record(&mut self, r: BackupRecord) -> Result<(), Error> {
        if r.schedule_id.is_empty() {
            return Err(Error::Io(
                "history: cannot record a backup with no schedule id".into(),
            ));
        }
        self.records.push(r);
        self.persist()
    }

    /// A schedule's backups, newest first; an unknown schedule has none.
    pub fn for_schedule(&self, schedule_id: &str) -> Vec<BackupRecord> {
        let mut out: Vec<BackupRecord> = self
            .records
            .iter()
            .filter(|r| r.schedule_id == schedule_id)
            .cloned()
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        out
    }

    /// The backup reported under `hub_history_id`.
    pub fn find(&self, hub_history_id: &str) -> Option<BackupRecord> {
        self.records
            .iter()
            .find(|r| r.hub_history_id == hub_history_id)
            .cloned()
    }

    /// Forgets one backup; forgetting twice is fine.
    pub fn forget(&mut self, hub_history_id: &str) -> Result<(), Error> {
        let before = self.records.len();
        self.records.retain(|r| r.hub_history_id != hub_history_id);
        if before != self.records.len() {
            self.persist()?;
        }
        Ok(())
    }

    /// Adds a job outcome to the ring, dropping the oldest past [`RECENT_CAP`].
    pub fn push_outcome(&mut self, o: Outcome) {
        self.recent.push_front(o);
        self.recent.truncate(RECENT_CAP);
    }

    /// Up to `n` outcomes, newest first.
    pub fn recent(&self, n: usize) -> Vec<Outcome> {
        self.recent.iter().take(n).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::temp_dir;

    fn rec(schedule: &str, hub: &str, at: u64) -> BackupRecord {
        BackupRecord {
            schedule_id: schedule.into(),
            hub_history_id: hub.into(),
            created_at: at,
            ..Default::default()
        }
    }

    fn outcome(id: &str) -> Outcome {
        Outcome {
            history_id: id.into(),
            schedule_id: "s".into(),
            database: "db".into(),
            ok: true,
            error: String::new(),
            bytes: 1,
            finished_at: 0,
        }
    }

    #[test]
    fn record_and_query_by_schedule_newest_first() {
        let dir = temp_dir("history");
        let mut h = History::open(dir.join("h.enc")).unwrap();
        h.record(rec("s1", "a", 1)).unwrap();
        h.record(rec("s1", "b", 3)).unwrap();
        h.record(rec("s2", "c", 2)).unwrap();
        let ids: Vec<String> = h
            .for_schedule("s1")
            .into_iter()
            .map(|r| r.hub_history_id)
            .collect();
        assert_eq!(ids, ["b", "a"]);
        assert!(h.for_schedule("unknown").is_empty());
        assert!(h.record(rec("", "x", 1)).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn store_survives_reopen_and_finds_by_hub_id() {
        let dir = temp_dir("history-reopen");
        let mut h = History::open(dir.join("h.enc")).unwrap();
        h.record(rec("s1", "a", 1)).unwrap();
        let h2 = History::open(dir.join("h.enc")).unwrap();
        assert_eq!(h2.find("a").unwrap().schedule_id, "s1");
        assert!(h2.find("zzz").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn forget_removes_one_record_and_is_idempotent() {
        let dir = temp_dir("history-forget");
        let mut h = History::open(dir.join("h.enc")).unwrap();
        h.record(rec("s1", "a", 1)).unwrap();
        h.record(rec("s1", "b", 2)).unwrap();
        h.forget("a").unwrap();
        h.forget("a").unwrap();
        assert_eq!(h.for_schedule("s1").len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recent_ring_is_newest_first_and_capped() {
        let dir = temp_dir("history-ring");
        let mut h = History::open(dir.join("h.enc")).unwrap();
        assert!(h.recent(5).is_empty());
        for i in 0..(RECENT_CAP + 5) {
            h.push_outcome(outcome(&i.to_string()));
        }
        let r = h.recent(3);
        assert_eq!(r[0].history_id, (RECENT_CAP + 4).to_string());
        assert_eq!(h.recent(1000).len(), RECENT_CAP);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
