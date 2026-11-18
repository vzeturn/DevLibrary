//! The gate before a file flagged `requires_backup`: a COPY_ONLY backup taken by the
//! runner, or an operator's fresh attestation, or (unconfigured) a refusal.

use std::path::{Path, PathBuf};
use std::time::Duration;

use dl_json::Value;
use dl_time::OffsetDateTime;

use super::{CODE_BACKUP_REQUIRED, MErr, sql_failure};
use crate::config::MigrationBackup;
use crate::ctx::Ctx;
use crate::db::Conn;
use crate::json::{self, Obj};

/// COPY_ONLY full backup to a directory on the SQL Server host.
pub const BACKUP_COPY_ONLY: &str = "copy_only";
/// An operator's marker file younger than the limit.
pub const BACKUP_ATTESTED: &str = "attested";
/// How old an attestation may be by default.
pub const DEFAULT_ATTEST_MAX_AGE: Duration = Duration::from_secs(6 * 3600);
/// The default marker file name in the state directory.
pub const ATTEST_FILE: &str = "backup-attested.json";

/// The only backup statement: the path is a parameter, the database the connection's.
const COPY_ONLY_SQL: &str = "DECLARE @db sysname = DB_NAME();
BACKUP DATABASE @db TO DISK = @Path WITH COPY_ONLY, CHECKSUM, INIT, FORMAT, NAME = N'pms-agent pre-migration';";

/// What the gate accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupProof {
    /// copy_only or attested.
    pub mode: String,
    /// When.
    pub at: OffsetDateTime,
    /// The backup file name (copy_only), without the directory.
    pub file: String,
}

impl BackupProof {
    /// The JSON of the Go struct.
    pub fn to_json(&self) -> String {
        let mut o = Obj::new()
            .str("mode", &self.mode)
            .str("at", &rfc3339_nano(self.at));
        if !self.file.is_empty() {
            o = o.str("file", &self.file);
        }
        o.finish()
    }
}

/// `t.Format(time.RFC3339Nano)`.
pub fn rfc3339_nano(t: OffsetDateTime) -> String {
    let mut s = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        t.year(),
        t.month().number(),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    );
    let ns = t.nanosecond();
    if ns != 0 {
        let f = format!("{ns:09}");
        s.push('.');
        s.push_str(f.trim_end_matches('0'));
    }
    let secs = t.offset().whole_seconds();
    if secs == 0 {
        s.push('Z');
    } else {
        let a = secs.unsigned_abs();
        s.push_str(&format!(
            "{}{:02}:{:02}",
            if secs < 0 { '-' } else { '+' },
            a / 3600,
            (a % 3600) / 60
        ));
    }
    s
}

/// Go's `time.Duration.String` for whole seconds (e.g. `6h0m0s`, `-5m0s`).
pub fn go_duration(secs: i64) -> String {
    if secs == 0 {
        return "0s".into();
    }
    let sign = if secs < 0 { "-" } else { "" };
    let a = secs.unsigned_abs();
    let (h, m, s) = (a / 3600, (a % 3600) / 60, a % 60);
    if h > 0 {
        format!("{sign}{h}h{m}m{s}s")
    } else if m > 0 {
        format!("{sign}{m}m{s}s")
    } else {
        format!("{sign}{s}s")
    }
}

/// Decides whether a `requires_backup` file may run.
pub trait BackupGate: Send + Sync {
    /// Whether [`BackupGate::ensure`] could succeed now, writing nothing (dry run).
    fn check(&self, now: OffsetDateTime) -> Result<(), MErr>;
    /// A fresh proof, taking the backup when the mode does.
    fn ensure(
        &self,
        ctx: &Ctx,
        c: &mut Conn<'_>,
        seq: i64,
        now: OffsetDateTime,
    ) -> Result<BackupProof, MErr>;
}

/// Refuses every file that needs a backup.
pub struct RefuseBackup;

impl BackupGate for RefuseBackup {
    fn check(&self, _: OffsetDateTime) -> Result<(), MErr> {
        Err(MErr::new(
            CODE_BACKUP_REQUIRED,
            "no backup gate is configured (migration.backup.mode)",
        ))
    }
    fn ensure(
        &self,
        _: &Ctx,
        _: &mut Conn<'_>,
        _: i64,
        now: OffsetDateTime,
    ) -> Result<BackupProof, MErr> {
        Err(self.check(now).unwrap_err())
    }
}

/// `C:\…`, `\\server\share\…` and `/…`, whatever the agent's own OS: SQL Server resolves it.
fn is_abs_any_os(p: &str) -> bool {
    let b = p.as_bytes();
    p.starts_with("\\\\")
        || p.starts_with('/')
        || (b.len() >= 3 && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
}

/// Builds the gate for `cfg`; `state_dir` locates the default attestation marker.
pub fn new_backup_gate(
    cfg: &MigrationBackup,
    state_dir: &Path,
) -> Result<Box<dyn BackupGate>, String> {
    match cfg.mode.as_str() {
        "" => Ok(Box::new(RefuseBackup)),
        BACKUP_COPY_ONLY => {
            if !is_abs_any_os(&cfg.dir) {
                return Err(
                    "migration backup: copy_only needs an absolute dir on the SQL Server host"
                        .into(),
                );
            }
            Ok(Box::new(CopyOnly {
                dir: cfg.dir.clone(),
            }))
        }
        BACKUP_ATTESTED => {
            let marker = if cfg.marker_file.is_empty() {
                state_dir.join(ATTEST_FILE)
            } else {
                PathBuf::from(&cfg.marker_file)
            };
            let max_age = if cfg.max_age_minutes > 0 {
                Duration::from_secs(cfg.max_age_minutes.unsigned_abs() * 60)
            } else {
                DEFAULT_ATTEST_MAX_AGE
            };
            Ok(Box::new(Attested { marker, max_age }))
        }
        m => Err(format!(
            "migration backup: unknown mode {} (copy_only, attested)",
            json::quote(m)
        )),
    }
}

struct CopyOnly {
    dir: String,
}

impl BackupGate for CopyOnly {
    fn check(&self, _: OffsetDateTime) -> Result<(), MErr> {
        Ok(())
    }

    fn ensure(
        &self,
        ctx: &Ctx,
        c: &mut Conn<'_>,
        seq: i64,
        now: OffsetDateTime,
    ) -> Result<BackupProof, MErr> {
        let sep = if self.dir.contains('\\') { "\\" } else { "/" };
        let u = now.to_utc();
        let file = format!(
            "MM_PMS_pre_{seq}_{:04}{:02}{:02}T{:02}{:02}{:02}Z.bak",
            u.year(),
            u.month().number(),
            u.day(),
            u.hour(),
            u.minute(),
            u.second()
        );
        let path = format!("{}{sep}{file}", self.dir.trim_end_matches(['\\', '/']));
        if let Err(e) = c.query(ctx, COPY_ONLY_SQL, &[dl_tds::Param::new("Path", path)]) {
            let mut f = sql_failure(CODE_BACKUP_REQUIRED, seq, &e);
            f.msg = format!("COPY_ONLY backup failed: {}", f.msg);
            return Err(f);
        }
        Ok(BackupProof {
            mode: BACKUP_COPY_ONLY.into(),
            at: u,
            file,
        })
    }
}

struct Attested {
    marker: PathBuf,
    max_age: Duration,
}

impl Attested {
    fn read(&self, now: OffsetDateTime) -> Result<OffsetDateTime, MErr> {
        let refuse = |m: String| MErr::new(CODE_BACKUP_REQUIRED, m);
        let name = self
            .marker
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let raw = std::fs::read(&self.marker)
            .map_err(|_| refuse(format!("no backup attestation ({name})")))?;
        let shape = || {
            refuse("backup attestation must be {\"attested_at\": RFC 3339, \"by\": name}".into())
        };
        let o = json::parse_object(&raw).ok_or_else(shape)?;
        if o.keys()
            .any(|k| !k.eq_ignore_ascii_case("attested_at") && !k.eq_ignore_ascii_case("by"))
        {
            return Err(shape());
        }
        let s = |k: &str| match json::field_ci(&o, k) {
            None | Some(Value::Null) => Ok(String::new()),
            Some(Value::String(s)) => Ok(s.clone()),
            Some(_) => Err(shape()),
        };
        let (at, by) = (s("attested_at")?, s("by")?);
        if by.trim().is_empty() {
            return Err(shape());
        }
        let at = OffsetDateTime::parse_rfc3339(&at)
            .map_err(|_| refuse("backup attestation: attested_at is not RFC 3339".into()))?;
        let age_ns = now.unix_nanos() - at.unix_nanos();
        let max_ns = i128::try_from(self.max_age.as_nanos()).unwrap_or(i128::MAX);
        if age_ns < -300_000_000_000 || age_ns > max_ns {
            let mins = (age_ns + 30_000_000_000).div_euclid(60_000_000_000);
            return Err(refuse(format!(
                "backup attestation is {} old (limit {}) or in the future",
                go_duration(i64::try_from(mins * 60).unwrap_or(i64::MAX)),
                go_duration(i64::try_from(self.max_age.as_secs()).unwrap_or(i64::MAX))
            )));
        }
        Ok(at)
    }
}

impl BackupGate for Attested {
    fn check(&self, now: OffsetDateTime) -> Result<(), MErr> {
        self.read(now).map(drop)
    }

    fn ensure(
        &self,
        _: &Ctx,
        _: &mut Conn<'_>,
        _: i64,
        now: OffsetDateTime,
    ) -> Result<BackupProof, MErr> {
        let at = self.read(now)?;
        Ok(BackupProof {
            mode: BACKUP_ATTESTED.into(),
            at: at.to_utc(),
            file: String::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gates() {
        let d = std::env::temp_dir().join(format!("pms-bak-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let now = OffsetDateTime::parse_rfc3339("2026-11-15T10:00:00Z").unwrap();
        let cfg = |mode: &str, dir: &str| MigrationBackup {
            mode: mode.into(),
            dir: dir.into(),
            ..MigrationBackup::default()
        };
        assert!(
            new_backup_gate(&cfg("", ""), &d)
                .unwrap()
                .check(now)
                .is_err()
        );
        assert!(new_backup_gate(&cfg("copy_only", "backups"), &d).is_err());
        assert!(new_backup_gate(&cfg("copy_only", r"D:\bak"), &d).is_ok());
        assert!(new_backup_gate(&cfg("copy_only", "/var/opt/bak"), &d).is_ok());
        assert!(new_backup_gate(&cfg("nightly", ""), &d).is_err());
        let g = new_backup_gate(&cfg("attested", ""), &d).unwrap();
        let err = g.check(now).unwrap_err();
        assert!(err.msg.contains(ATTEST_FILE), "{}", err.msg);
        let marker = d.join(ATTEST_FILE);
        std::fs::write(
            &marker,
            r#"{"attested_at":"2026-11-15T08:00:00Z","by":"ops"}"#,
        )
        .unwrap();
        assert!(g.check(now).is_ok());
        std::fs::write(
            &marker,
            r#"{"attested_at":"2026-11-15T01:00:00Z","by":"ops"}"#,
        )
        .unwrap();
        assert_eq!(
            g.check(now).unwrap_err().msg,
            "backup attestation is 9h0m0s old (limit 6h0m0s) or in the future"
        );
        std::fs::write(
            &marker,
            r#"{"attested_at":"2026-11-15T08:00:00Z","by":" "}"#,
        )
        .unwrap();
        assert!(g.check(now).is_err());
        std::fs::write(
            &marker,
            r#"{"attested_at":"2026-11-15T08:00:00Z","by":"x","n":1}"#,
        )
        .unwrap();
        assert!(g.check(now).is_err());
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(go_duration(-300), "-5m0s");
    }
}
