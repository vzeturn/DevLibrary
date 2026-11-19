//! Go `backup.Run`: connect → backup → checksum → upload → cleanup.
//!
//! Differences from Go, on purpose:
//! - The database name and the file path are statement parameters, not spliced text.
//! - `COMPRESSION` and `CHECKSUM` are added only where the edition and version support
//!   them ([`BackupOptions`]); the choice is written into the `backup` step.
//! - A log backup of a database in SIMPLE recovery stops before SQL Server is asked, with
//!   [`Error::RecoveryModel`].
//! - When the server is remote and the agent cannot see the file it wrote, the error says
//!   so instead of a bare "stat" failure.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Instant;

use dl_encoding::hex;
use dl_hash::{Digest, Sha256};
use dl_tds::CancelToken;

use super::{BackupResult, BackupType, Params, Steps, file_safe};
use crate::Error;
use crate::sql::{BackupOptions, Connector, SqlConn, quote};
use crate::upload::Uploaders;

/// Go's fallback when the instance's backup directory cannot be read.
pub const FALLBACK_DIR: &str = "C:\\Windows\\Temp";

/// Runs one backup. `steps` gets every step's final status, also on failure.
pub fn run(
    sql: &dyn Connector,
    uploaders: &dyn Uploaders,
    p: &Params,
    cancel: &CancelToken,
    steps: &mut Steps,
) -> Result<BackupResult, Error> {
    quote::check_identifier(&p.database_name)?;

    let t0 = Instant::now();
    steps.set(
        "connect",
        "running",
        format!("Connecting to SQL Server {}", p.target.host),
        None,
    );
    let mut conn = match sql.connect(&p.target) {
        Ok(c) => c,
        Err(e) => {
            steps.set("connect", "error", e.to_string(), Some(t0));
            return Err(e);
        }
    };
    steps.set(
        "connect",
        "done",
        format!("Connected to {}", p.target.host),
        Some(t0),
    );
    conn.set_cancel(Some(cancel.clone()));

    let t0 = Instant::now();
    let (path, options) = match prepare(conn.as_mut(), p) {
        Ok(v) => v,
        Err(e) => {
            steps.set("backup", "error", e.to_string(), Some(t0));
            return Err(e);
        }
    };
    let extra = (p.backup_type == BackupType::Differential).then_some("DIFFERENTIAL");
    let verb = if p.backup_type == BackupType::TransactionLog {
        "BACKUP LOG"
    } else {
        "BACKUP DATABASE"
    };
    let statement = format!("{verb} @db TO DISK = @path {};", options.with_clause(extra));
    steps.set(
        "backup",
        "running",
        format!("Running {} backup → {path}", p.backup_type.as_str()),
        None,
    );
    let args = [
        ("db".to_owned(), p.database_name.clone()),
        ("path".to_owned(), path.clone()),
    ];
    if let Err(e) = conn.run(&statement, &args) {
        steps.set("backup", "error", e.to_string(), Some(t0));
        return Err(e);
    }
    steps.set(
        "backup",
        "done",
        format!("Backup written: {path} ({})", options.reason),
        Some(t0),
    );
    drop(conn);

    let t0 = Instant::now();
    steps.set("checksum", "running", "Computing SHA-256", None);
    let (bytes, checksum) = match checksum(&path, p, cancel) {
        Ok(v) => v,
        Err(e) => {
            steps.set("checksum", "error", e.to_string(), Some(t0));
            return Err(e);
        }
    };
    steps.set(
        "checksum",
        "done",
        format!("sha256={}...", &checksum[..8]),
        Some(t0),
    );

    let t0 = Instant::now();
    let mut cloud_file_path = String::new();
    match &p.destination {
        Some(dest) => {
            steps.set(
                "upload",
                "running",
                format!("Uploading directly to {}", dest.provider),
                None,
            );
            let sent = uploaders
                .open(dest)
                .and_then(|u| u.upload(Path::new(&path), cancel, &mut |_, _| {}));
            match sent {
                Ok(cloud) => {
                    steps.set(
                        "upload",
                        "done",
                        format!("Upload complete: {cloud}"),
                        Some(t0),
                    );
                    cloud_file_path = cloud;
                }
                Err(e) => {
                    steps.set("upload", "error", e.to_string(), Some(t0));
                    return Err(e);
                }
            }
        }
        None => steps.set("upload", "skipped", "No cloud destination configured", None),
    }

    let t0 = Instant::now();
    if cloud_file_path.is_empty() {
        steps.set(
            "cleanup",
            "skipped",
            format!("Keeping local file: {path}"),
            None,
        );
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => steps.set("cleanup", "done", "Temp file removed", Some(t0)),
            // Go: a failed cleanup is reported in the step and does not fail the backup.
            Err(e) => steps.set("cleanup", "error", format!("remove {path}: {e}"), Some(t0)),
        }
    }

    Ok(BackupResult {
        cloud_file_path,
        local_file_path: path,
        bytes,
        checksum,
        options: options.reason,
    })
}

/// Picks the file path and the `WITH` options, and refuses a log backup in SIMPLE recovery.
fn prepare(conn: &mut dyn SqlConn, p: &Params) -> Result<(String, BackupOptions), Error> {
    if p.backup_type == BackupType::TransactionLog {
        match conn.recovery_model(&p.database_name)? {
            None => {
                return Err(Error::Sql(format!(
                    "database {} does not exist on {}",
                    p.database_name, p.target.host
                )));
            }
            Some(m) if m.eq_ignore_ascii_case("SIMPLE") => {
                return Err(Error::RecoveryModel(format!(
                    "database {} uses the SIMPLE recovery model; a transaction log backup needs FULL or BULK_LOGGED",
                    p.database_name
                )));
            }
            Some(_) => {}
        }
    }
    let dir = if p.backup_dir.trim().is_empty() {
        // Go: a server that will not say (no permission on xp_instance_regread) gets the
        // Windows temp directory.
        conn.backup_dir()?
            .unwrap_or_else(|| FALLBACK_DIR.to_owned())
    } else {
        p.backup_dir.trim().to_owned()
    };
    let name = format!(
        "srbackup_{}_{}_{}.bak",
        file_safe(&p.database_name),
        utc_stamp(crate::state::now_ms() / 1000),
        p.history_id
    );
    let path = quote::join(&dir, &name);
    quote::check_path(&path)?;
    Ok((path, BackupOptions::for_server(conn.info())))
}

/// `yyyymmdd_hhmmss` in UTC (Go's `20060102_150405`).
pub fn utc_stamp(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let (y, m, d) = crate::schedule::cron::civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}_{:02}{:02}{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

fn checksum(path: &str, p: &Params, cancel: &CancelToken) -> Result<(u64, String), Error> {
    let meta = std::fs::metadata(path).map_err(|e| {
        if p.target.is_local() {
            Error::io(&format!("checksum: stat {path}"), &e)
        } else {
            Error::Io(format!(
                "backup file {path} was written by SQL Server on {} but is not reachable from the agent: {e}",
                p.target.machine()
            ))
        }
    })?;
    let hash = sha256_file(Path::new(path), cancel)?;
    Ok((meta.len(), hash))
}

/// SHA-256 of a file in lowercase hex, stopping when `cancel` fires.
pub fn sha256_file(path: &Path, cancel: &CancelToken) -> Result<String, Error> {
    let what = format!("checksum: sha256 {}", path.display());
    let mut f = File::open(path).map_err(|e| Error::io(&what, &e))?;
    let mut h = Sha256::default();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let n = f.read(&mut buf).map_err(|e| Error::io(&what, &e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;
