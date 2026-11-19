//! One backup job and one restore, ported from Go `internal/backup`: the pipeline
//! (connect → backup → checksum → upload → cleanup) in [`pipeline`] and restore in
//! [`restore`]. Both record their steps in [`Steps`], the list the hub stores with the
//! backup history row.

pub mod pipeline;
pub mod restore;
#[cfg(test)]
mod sql_tests;

use std::time::Instant;

use dl_json::{Value, json};

use crate::sql::Target;
use crate::upload::Destination;

/// What kind of backup a schedule takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupType {
    /// `BACKUP DATABASE`.
    Full,
    /// `BACKUP DATABASE … WITH DIFFERENTIAL`.
    Differential,
    /// `BACKUP LOG`.
    TransactionLog,
}

impl BackupType {
    /// The hub's `backupType` (Go `mapBackupType`): `incremental` and `differential` are
    /// differential, `log` / `transactionlog` a log backup, anything else full.
    pub fn from_hub(kind: &str) -> BackupType {
        match kind.trim().to_ascii_lowercase().as_str() {
            "incremental" | "differential" => BackupType::Differential,
            "log" | "transactionlog" | "transaction_log" => BackupType::TransactionLog,
            _ => BackupType::Full,
        }
    }

    /// The Go name (`Full`, `Differential`, `TransactionLog`).
    pub fn as_str(self) -> &'static str {
        match self {
            BackupType::Full => "Full",
            BackupType::Differential => "Differential",
            BackupType::TransactionLog => "TransactionLog",
        }
    }
}

/// One entry of a job's step list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// `connect`, `backup`, `checksum`, `upload`, `cleanup`, `restore`, …
    pub step: String,
    /// `running`, `done`, `error` or `skipped`.
    pub status: String,
    /// What happened, for people.
    pub detail: String,
    /// How long the step took.
    pub duration_ms: u64,
}

/// The steps of one job, one entry per step holding its latest status (Go reported every
/// transition; the hub only keeps the list it is sent at the end).
#[derive(Debug, Clone, Default)]
pub struct Steps {
    list: Vec<Step>,
}

impl Steps {
    /// An empty list.
    pub fn new() -> Steps {
        Steps::default()
    }

    /// Records `step` as `status`, replacing an earlier entry of the same step.
    /// `started` gives the duration; `None` records 0.
    pub fn set(
        &mut self,
        step: &str,
        status: &str,
        detail: impl Into<String>,
        started: Option<Instant>,
    ) {
        let entry = Step {
            step: step.to_owned(),
            status: status.to_owned(),
            detail: detail.into(),
            duration_ms: started.map_or(0, |t| {
                u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
            }),
        };
        match self.list.iter_mut().find(|s| s.step == step) {
            Some(s) => *s = entry,
            None => self.list.push(entry),
        }
    }

    /// The entry for `step`.
    pub fn get(&self, step: &str) -> Option<&Step> {
        self.list.iter().find(|s| s.step == step)
    }

    /// Every entry, in the order the steps started.
    pub fn list(&self) -> &[Step] {
        &self.list
    }

    /// The list as the hub stores it.
    pub fn to_json(&self) -> Vec<Value> {
        self.list
            .iter()
            .map(|s| {
                json!({
                    "step": s.step.as_str(),
                    "status": s.status.as_str(),
                    "detail": s.detail.as_str(),
                    "durationMs": s.duration_ms,
                })
            })
            .collect()
    }
}

/// Everything one queued backup needs. It is stored in the (sealed) queue file, so it may
/// carry the SQL password and the destination's tokens.
#[derive(Debug, Clone)]
pub struct Params {
    /// The hub's backup history id (a UUID the agent makes when it enqueues).
    pub history_id: String,
    /// The schedule that made the job.
    pub schedule_id: String,
    /// The database to back up.
    pub database_name: String,
    /// Full, differential or log.
    pub backup_type: BackupType,
    /// The server.
    pub target: Target,
    /// Where the file goes; `None` keeps it on the server.
    pub destination: Option<Destination>,
    /// A server-side directory for the file; empty uses the instance's backup directory.
    pub backup_dir: String,
}

impl Params {
    /// The queue's JSON form.
    pub fn to_json(&self) -> Value {
        json!({
            "historyId": self.history_id.as_str(),
            "scheduleId": self.schedule_id.as_str(),
            "databaseName": self.database_name.as_str(),
            "backupType": self.backup_type.as_str(),
            "target": target_json(&self.target),
            "destination": self.destination.as_ref().map_or(Value::Null, Destination::to_json),
            "backupDir": self.backup_dir.as_str(),
        })
    }

    /// Reads [`Params::to_json`] back; `None` when a required field is missing.
    pub fn from_json(v: &Value) -> Option<Params> {
        let text = |v: &Value, k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let t = v.get("target")?;
        let p = Params {
            history_id: text(v, "historyId"),
            schedule_id: text(v, "scheduleId"),
            database_name: text(v, "databaseName"),
            backup_type: BackupType::from_hub(&text(v, "backupType")),
            target: Target {
                host: text(t, "host"),
                port: t
                    .get("port")
                    .and_then(Value::as_u64)
                    .and_then(|p| u16::try_from(p).ok())
                    .unwrap_or(0),
                auth_type: text(t, "authType"),
                username: text(t, "username"),
                password: text(t, "password"),
            },
            destination: v.get("destination").and_then(Destination::from_json),
            backup_dir: text(v, "backupDir"),
        };
        (!p.history_id.is_empty() && !p.database_name.is_empty()).then_some(p)
    }
}

fn target_json(t: &Target) -> Value {
    json!({
        "host": t.host.as_str(),
        "port": t.port,
        "authType": t.auth_type.as_str(),
        "username": t.username.as_str(),
        "password": t.password.as_str(),
    })
}

/// What a finished backup produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupResult {
    /// Where the destination keeps the file; empty when it stayed on the server.
    pub cloud_file_path: String,
    /// The file the server wrote (removed after an upload).
    pub local_file_path: String,
    /// The file's size.
    pub bytes: u64,
    /// SHA-256, lowercase hex.
    pub checksum: String,
    /// The `WITH` options chosen and why (also in the `backup` step).
    pub options: String,
}

/// A name made safe for a file name: characters Windows forbids become `_`.
pub fn file_safe(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_control() || "\\/:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_types_map_like_go() {
        assert_eq!(BackupType::from_hub("Full"), BackupType::Full);
        assert_eq!(
            BackupType::from_hub("incremental"),
            BackupType::Differential
        );
        assert_eq!(
            BackupType::from_hub("Differential"),
            BackupType::Differential
        );
        assert_eq!(
            BackupType::from_hub("TransactionLog"),
            BackupType::TransactionLog
        );
        assert_eq!(BackupType::from_hub("log"), BackupType::TransactionLog);
        assert_eq!(BackupType::from_hub("whatever"), BackupType::Full);
    }

    #[test]
    fn steps_keep_the_latest_status_per_step() {
        let mut s = Steps::new();
        s.set("connect", "running", "Connecting", None);
        s.set("backup", "running", "x", None);
        s.set("connect", "done", "Connected", Some(Instant::now()));
        assert_eq!(s.list().len(), 2);
        assert_eq!(s.list()[0].status, "done");
        let j = s.to_json();
        assert_eq!(j[0].get("step").and_then(Value::as_str), Some("connect"));
        assert!(j[0].get("durationMs").is_some());
    }

    #[test]
    fn params_round_trip_through_json() {
        let p = Params {
            history_id: "h1".into(),
            schedule_id: "s1".into(),
            database_name: "Sales]DB".into(),
            backup_type: BackupType::TransactionLog,
            target: Target {
                host: "db\\SQL2019".into(),
                port: 1433,
                auth_type: "sql".into(),
                username: "sa".into(),
                password: "pw".into(),
            },
            destination: Some(Destination {
                provider: "s3".into(),
                tokens: json!({"bucketName": "b"}),
            }),
            backup_dir: String::new(),
        };
        let back = Params::from_json(&p.to_json()).unwrap();
        assert_eq!(back.database_name, "Sales]DB");
        assert_eq!(back.backup_type, BackupType::TransactionLog);
        assert_eq!(back.target, p.target);
        assert_eq!(back.destination, p.destination);
        assert!(Params::from_json(&json!({"target": {}})).is_none());
    }

    #[test]
    fn file_names_lose_forbidden_characters() {
        assert_eq!(file_safe("a/b:c*d"), "a_b_c_d");
        assert_eq!(file_safe("Bán hàng"), "Bán hàng");
    }
}
