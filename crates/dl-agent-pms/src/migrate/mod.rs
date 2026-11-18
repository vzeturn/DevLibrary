//! Applies a verified MM_PMS migration bundle to the site database: keeps the
//! MM_SchemaVersion ledger, checks its hash chain against the bundle before anything
//! runs, applies pending files in order (one transaction per file where SQL Server
//! allows it), gates data-rewriting files behind a fresh backup, and stops for an
//! operator instead of guessing after a crash inside a file that could not run in a
//! transaction.
//!
//! Nothing here builds SQL from bundle text: batches are sent as they are, every other
//! statement is a constant with parameters (the lock timeout is a signed, range-checked
//! integer). Logs and reports carry versions, names, SQL error numbers and lines only.

pub mod backup;
pub mod ledger;
mod runner;
pub mod split;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_tds::{Param, ParamValue};
use dl_time::OffsetDateTime;

use crate::attrs;
use crate::bundle::{Bundle, Migration};
use crate::ctx::Ctx;
use crate::db::{Conn, DbError, Pool};
use crate::json::{self, Obj};
use crate::log::Logger;
use backup::{BackupGate, BackupProof, RefuseBackup};
use ledger::{LedgerState, Row};
pub use split::{Batch, Finding, runs_in_tx, scan, split_batches};

/// The bundle references an object no migration may reach.
pub const CODE_BUNDLE_FORBIDDEN: &str = "BUNDLE_FORBIDDEN";
/// The bundle needs a newer agent.
pub const CODE_AGENT_TOO_OLD: &str = "AGENT_TOO_OLD";
/// The ledger's own chain or version sequence is broken.
pub const CODE_LEDGER_CORRUPT: &str = "LEDGER_CORRUPT";
/// Ledger and bundle disagree on a version both hold.
pub const CODE_LEDGER_DIVERGED: &str = "LEDGER_DIVERGED";
/// The bundle starts after the ledger head.
pub const CODE_LEDGER_BEHIND: &str = "LEDGER_BEHIND";
/// A crash left a non-transactional file half applied, or the history cannot be adopted.
pub const CODE_NEEDS_OPERATOR: &str = "NEEDS_OPERATOR";
/// A file needs a backup and the gate has no fresh proof.
pub const CODE_BACKUP_REQUIRED: &str = "BACKUP_REQUIRED";
/// A file failed and was rolled back (or stopped half way outside a transaction).
pub const CODE_MIGRATION_FAILED: &str = "MIGRATION_FAILED";
/// Another runner holds the migration lock.
pub const CODE_LOCK_BUSY: &str = "LOCK_BUSY";
/// The database could not be reached or answered unexpectedly.
pub const CODE_DB: &str = "DB_UNAVAILABLE";

/// The application lock migrate.mjs takes too.
pub const LOCK_RESOURCE: &str = "MM_PMS_migrate";

const LOCK_SQL: &str = "DECLARE @rc int;
EXEC @rc = sp_getapplock @Resource = @Resource, @LockMode = 'Exclusive', @LockOwner = 'Session', @LockTimeout = @Wait;
SELECT CAST(@rc AS bigint);";
const UNLOCK_SQL: &str = "EXEC sp_releaseapplock @Resource = @Resource, @LockOwner = 'Session';";
const SESSION_CONTEXT_SQL: &str =
    "EXEC sp_set_session_context @key = N'LegacyFoDb', @value = @FoDb;
EXEC sp_set_session_context @key = N'LegacyPosDb', @value = @PosDb;
EXEC sp_set_session_context @key = N'LegacyBoDb', @value = @BoDb;";
const PERMS_SQL: &str = "SELECT HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'CREATE TABLE'),
       HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'CREATE PROCEDURE'),
       HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'ALTER'),
       HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'BACKUP DATABASE');";
const PERM_NAMES: [&str; 4] = [
    "CREATE TABLE",
    "CREATE PROCEDURE",
    "ALTER",
    "BACKUP DATABASE",
];

/// A refusal or failure with a stable code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MErr {
    /// The code.
    pub code: &'static str,
    /// What happened (versions, files, rules, SQL numbers only).
    pub msg: String,
    /// The failed version (0: none).
    pub seq: i64,
    /// The SQL Server error number (0: none).
    pub sql_number: i32,
    /// Its line.
    pub line: i32,
}

impl MErr {
    /// A plain refusal.
    pub fn new(code: &'static str, msg: impl Into<String>) -> MErr {
        MErr {
            code,
            msg: msg.into(),
            seq: 0,
            sql_number: 0,
            line: 0,
        }
    }
}

impl fmt::Display for MErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.msg)
    }
}

impl std::error::Error for MErr {}

/// Keeps the SQL Server error number and line, never the message (it can quote data).
pub fn sql_failure(code: &'static str, seq: i64, err: &DbError) -> MErr {
    match err {
        DbError::Tds(dl_tds::Error::Server { number, line, .. }) => MErr {
            code,
            msg: format!("SQL Server error {number} at line {line}"),
            seq,
            sql_number: *number,
            line: *line,
        },
        DbError::Done(_) | DbError::Tds(dl_tds::Error::Timeout) => MErr {
            code,
            msg: "cancelled or timed out".into(),
            seq,
            sql_number: 0,
            line: 0,
        },
        DbError::Tds(_) => MErr {
            code: CODE_DB,
            msg: "database unavailable".into(),
            seq,
            sql_number: 0,
            line: 0,
        },
    }
}

/// A runner statement's failure (not a migration batch).
pub fn db_err(err: DbError) -> MErr {
    sql_failure(CODE_DB, 0, &err)
}

/// The legacy database names published in SESSION_CONTEXT for the synonym files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegacyDbs {
    /// FO (default SMILE_FO).
    pub fo: String,
    /// POS (default SMILE_POS).
    pub pos: String,
    /// BO (default SMILE_BO).
    pub bo: String,
}

impl LegacyDbs {
    fn with_defaults(mut self) -> LegacyDbs {
        for (v, d) in [
            (&mut self.fo, "SMILE_FO"),
            (&mut self.pos, "SMILE_POS"),
            (&mut self.bo, "SMILE_BO"),
        ] {
            if v.is_empty() {
                *v = d.to_owned();
            }
        }
        self
    }
}

/// One progress step: no SQL text, no data.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Event {
    /// Bundle id.
    pub bundle_id: String,
    /// Version.
    pub seq: i64,
    /// File name.
    pub name: String,
    /// adopt, backup, start, applied or failed.
    pub phase: String,
    /// Runs in a transaction.
    pub in_tx: bool,
    /// Step duration.
    pub duration_ms: i64,
    /// Error code of a failed step.
    pub code: String,
}

/// Runner settings.
pub struct Options {
    /// The MM_PMS pool.
    pub pool: Arc<Pool>,
    /// This build's version.
    pub agent_version: String,
    /// The backup gate.
    pub backup: Box<dyn BackupGate>,
    /// Legacy database names.
    pub legacy: LegacyDbs,
    /// Approval to re-run the half-applied file at this version (0: none).
    pub rerun_partial: i64,
    /// Wait for the migration lock.
    pub lock_wait: Duration,
    /// Logger.
    pub log: Logger,
    /// The clock.
    pub now: fn() -> OffsetDateTime,
    /// Progress sink (must not block for long).
    pub progress: Box<dyn Fn(Event) + Send + Sync>,
}

impl Options {
    /// Defaults: refuse backups, 30 s lock wait, no progress.
    pub fn new(pool: Arc<Pool>, agent_version: &str) -> Options {
        Options {
            pool,
            agent_version: agent_version.to_owned(),
            backup: Box::new(RefuseBackup),
            legacy: LegacyDbs::default(),
            rerun_partial: 0,
            lock_wait: Duration::from_secs(30),
            log: Logger::discard(),
            now: OffsetDateTime::now_utc,
            progress: Box::new(|_| {}),
        }
    }
}

/// A file the runner would apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    /// Version.
    pub seq: i64,
    /// File name.
    pub name: String,
    /// Runs in a transaction.
    pub in_tx: bool,
    /// Needs a backup.
    pub requires_backup: bool,
    /// Lock timeout.
    pub lock_timeout_ms: i64,
    /// Online index build.
    pub online_index: bool,
}

/// What a run would do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// Bundle id.
    pub bundle_id: String,
    /// from_version.
    pub from: i64,
    /// to_version.
    pub to: i64,
    /// The ledger head before the run (after adoption when `adopt` > 0).
    pub head: i64,
    /// Files adopted from MM_MigrationHistory.
    pub adopt: usize,
    /// The half-applied version.
    pub partial: i64,
    /// `partial` will be re-run on the operator's approval.
    pub rerun: bool,
    /// The ledger table exists.
    pub ledger_exists: bool,
    /// Files to apply.
    pub pending: Vec<Pending>,
    /// The gate's answer when a pending file needs a backup it cannot provide now.
    pub backup_refusal: String,
    /// Database permissions the run would lack.
    pub missing_permissions: Vec<String>,
}

impl Plan {
    /// The JSON of the Go struct.
    pub fn to_json(&self) -> String {
        let pending: Vec<String> = self
            .pending
            .iter()
            .map(|p| {
                Obj::new()
                    .int("seq", p.seq)
                    .str("name", &p.name)
                    .bool("in_tx", p.in_tx)
                    .bool("requires_backup", p.requires_backup)
                    .int("lock_timeout_ms", p.lock_timeout_ms)
                    .bool("online_index", p.online_index)
                    .finish()
            })
            .collect();
        let mut o = Obj::new()
            .str("bundle_id", &self.bundle_id)
            .int("from_version", self.from)
            .int("to_version", self.to)
            .int("head", self.head)
            .int("adopt", self.adopt as i64);
        if self.partial != 0 {
            o = o.int("partial", self.partial);
        }
        if self.rerun {
            o = o.bool("rerun", true);
        }
        o = o
            .bool("ledger_exists", self.ledger_exists)
            .raw("pending", &format!("[{}]", pending.join(",")));
        if !self.backup_refusal.is_empty() {
            o = o.str("backup_refusal", &self.backup_refusal);
        }
        if !self.missing_permissions.is_empty() {
            let p: Vec<String> = self
                .missing_permissions
                .iter()
                .map(|s| json::quote(s))
                .collect();
            o = o.raw("missing_permissions", &format!("[{}]", p.join(",")));
        }
        o.finish()
    }
}

/// A file the run applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// Version.
    pub seq: i64,
    /// File name.
    pub name: String,
    /// Ran in a transaction.
    pub in_tx: bool,
    /// Duration.
    pub duration_ms: i64,
}

/// The outcome of [`Runner::apply`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// Bundle id.
    pub bundle_id: String,
    /// Ledger head after the run.
    pub head: i64,
    /// Files adopted.
    pub adopted: usize,
    /// Files applied.
    pub applied: Vec<Applied>,
    /// The backup proof used.
    pub backup: Option<BackupProof>,
}

impl Report {
    /// The JSON of the Go struct.
    pub fn to_json(&self) -> String {
        let applied: Vec<String> = self
            .applied
            .iter()
            .map(|a| {
                Obj::new()
                    .int("seq", a.seq)
                    .str("name", &a.name)
                    .bool("in_tx", a.in_tx)
                    .int("duration_ms", a.duration_ms)
                    .finish()
            })
            .collect();
        let mut o = Obj::new()
            .str("bundle_id", &self.bundle_id)
            .int("head", self.head)
            .int("adopted", self.adopted as i64)
            .raw("applied", &format!("[{}]", applied.join(",")));
        if let Some(b) = &self.backup {
            o = o.raw("backup", &b.to_json());
        }
        o.finish()
    }
}

/// One difference between the database and a bundle (what, never how).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftFinding {
    /// ledger.
    pub kind: String,
    /// The object.
    pub object: String,
    /// What differs.
    pub code: String,
}

/// The ledger's object name, as findings name it.
pub const LEDGER_TABLE: &str = "dbo.MM_SchemaVersion";
/// A version both hold has another name, sha256 or chain link.
pub const DRIFT_CHECKSUM: &str = "CHECKSUM_CHANGED";
/// The ledger link at the bundle's start is not its prev_chain.
pub const DRIFT_PREV_CHAIN: &str = "PREV_CHAIN_CHANGED";
/// The ledger holds a "running" row.
pub const DRIFT_UNFINISHED: &str = "UNFINISHED";

fn ms_since(t: Instant) -> i64 {
    i64::try_from(t.elapsed().as_millis()).unwrap_or(i64::MAX)
}

/// x.y.z comparison; a non-numeric agent version (a dev build) satisfies only 0.0.0.
pub fn version_at_least(have: &str, need: &str) -> bool {
    let Some(n) = parse_semver(need) else {
        return false;
    };
    let Some(h) = parse_semver(have) else {
        return n == [0, 0, 0];
    };
    h >= n
}

fn parse_semver(v: &str) -> Option<[i64; 3]> {
    let v = v.strip_prefix('v').unwrap_or(v);
    let parts: Vec<&str> = v.splitn(3, '.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut out = [0i64; 3];
    for (i, p) in parts.iter().enumerate() {
        let mut p = *p;
        if i == 2
            && let Some(k) = p.find(['-', '+'])
        {
            p = &p[..k];
        }
        let n: i64 = p.parse().ok()?;
        if n < 0 {
            return None;
        }
        out[i] = n;
    }
    Some(out)
}

pub use runner::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(version_at_least("1.2.3", "1.2.3"));
        assert!(version_at_least("v1.10.0", "1.9.9"));
        assert!(!version_at_least("1.2.3", "1.3.0"));
        assert!(version_at_least("1.2.3-rc1", "1.2.3"));
        assert!(!version_at_least("dev", "0.0.1"));
        assert!(version_at_least("dev", "0.0.0"));
        assert!(!version_at_least("1.0.0", "x"));
    }
}
