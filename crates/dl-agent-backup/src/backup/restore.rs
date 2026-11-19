//! Go `backup.Restore` and `backup.RestoreFromRecord`.
//!
//! Differences from Go, on purpose:
//! - The database name, file path and file moves are statement parameters.
//! - Restoring to a database that does not exist yet moves its files to the server's
//!   default data and log directories under the new name (`<db>.mdf`, `<db>_log.ldf`), so a
//!   copy next to the original does not collide with the original's files. Restoring over
//!   an existing database keeps Go's statement (`WITH REPLACE`, the backup's own paths).
//! - A cloud copy is downloaded into a directory the SQL Server service can read: the
//!   configured backup directory, else the instance's backup directory when the server is
//!   on this machine, else the agent's temp directory (Go always used the temp directory).
//! - A downloaded copy sealed under a tenant backup key ([`crate::crypto::envelope`]) is
//!   opened with the key its header names (asked of the hub, which gives an agent only its
//!   own tenant's keys) before `RESTORE`; a plaintext copy (every backup made before
//!   encryption) is restored as it is.

use std::path::{Path, PathBuf};
use std::time::Instant;

use dl_tds::CancelToken;

use super::{Steps, file_safe};
use crate::Error;
use crate::crypto::envelope::{self, BackupKeys, NoKeys};
use crate::sql::{Connector, SqlConn, Target, quote};
use crate::upload::{Destination, Uploaders};

/// One restore.
#[derive(Debug, Clone)]
pub struct RestoreParams {
    /// The server to restore on.
    pub target: Target,
    /// The database to create or replace.
    pub target_database: String,
    /// `WITH REPLACE` (the hub's restores always set it).
    pub with_replace: bool,
    /// A server-side `.bak` / `.trn` path.
    pub backup_file: String,
}

/// The stored record a restore starts from.
#[derive(Debug, Clone, Default)]
pub struct Source {
    /// The file the server wrote, if it is still there.
    pub local_file_path: String,
    /// The destination's copy.
    pub cloud_file_path: String,
    /// A tag for the downloaded file's name (the restore request id).
    pub tag: String,
    /// The destination that holds the cloud copy (`None` when it has no credentials).
    pub destination: Option<Destination>,
}

/// Restores `p.backup_file`.
pub fn restore(
    sql: &dyn Connector,
    p: &RestoreParams,
    cancel: &CancelToken,
    steps: &mut Steps,
) -> Result<(), Error> {
    validate(p)?;
    let t0 = Instant::now();
    steps.set("restore", "running", "Connecting to SQL Server", None);
    let mut conn = connect(sql, p, cancel, steps, t0)?;
    finish(conn.as_mut(), p, steps, t0)
}

/// [`restore_from_record_with`] without backup keys (a sealed copy cannot be opened).
pub fn restore_from_record(
    sql: &dyn Connector,
    uploaders: &dyn Uploaders,
    src: &Source,
    p: RestoreParams,
    backup_dir: &str,
    cancel: &CancelToken,
    steps: &mut Steps,
) -> Result<(), Error> {
    let io = RecordIo {
        uploaders,
        keys: &NoKeys,
        backup_dir,
    };
    restore_from_record_with(sql, &io, src, p, cancel, steps)
}

/// Where a restore's file comes from.
pub struct RecordIo<'a> {
    /// The destinations.
    pub uploaders: &'a dyn Uploaders,
    /// The tenant's backup keys, for a sealed cloud copy.
    pub keys: &'a dyn BackupKeys,
    /// The configured server-side backup directory (may be empty).
    pub backup_dir: &'a str,
}

/// Go `RestoreFromRecord`: the local file when the agent can see it, else a download of the
/// cloud copy, opened with the tenant key it names when it is sealed.
pub fn restore_from_record_with(
    sql: &dyn Connector,
    io: &RecordIo<'_>,
    src: &Source,
    mut p: RestoreParams,
    cancel: &CancelToken,
    steps: &mut Steps,
) -> Result<(), Error> {
    let (uploaders, backup_dir) = (io.uploaders, io.backup_dir);
    if !src.local_file_path.is_empty() && Path::new(&src.local_file_path).exists() {
        p.backup_file = src.local_file_path.clone();
        return restore(sql, &p, cancel, steps);
    }
    if src.cloud_file_path.is_empty() {
        return Err(Error::Sql(
            "restore: backup has neither a local file nor a cloud file path".into(),
        ));
    }
    let Some(dest) = src.destination.as_ref() else {
        return Err(Error::Upload(format!(
            "restore: cloud file path {:?} set but no cloud credentials provided",
            src.cloud_file_path
        )));
    };
    if p.target_database.is_empty() {
        return Err(Error::Sql("targetDatabase is required".into()));
    }
    let t0 = Instant::now();
    steps.set("restore", "running", "Connecting to SQL Server", None);
    let mut conn = connect(sql, &p, cancel, steps, t0)?;
    let dir = download_dir(conn.as_mut(), &p.target, backup_dir)?;
    let file = dir.join(format!("restore_{}.bak", file_safe(&src.tag)));
    steps.set(
        "download",
        "running",
        format!("Downloading {}", src.cloud_file_path),
        None,
    );
    let t1 = Instant::now();
    let got = uploaders
        .open(dest)
        .and_then(|u| u.download(&src.cloud_file_path, &file, cancel));
    if let Err(e) = got {
        steps.set("download", "error", e.to_string(), Some(t1));
        remove_quietly(&file, steps);
        return Err(match e {
            Error::Upload(m) => Error::Upload(format!("restore: download backup from cloud: {m}")),
            other => other,
        });
    }
    steps.set(
        "download",
        "done",
        format!("Downloaded to {}", file.display()),
        Some(t1),
    );
    let file = match unseal(&file, io.keys, cancel, steps) {
        Ok(f) => f,
        Err(e) => {
            remove_quietly(&file, steps);
            return Err(e);
        }
    };
    p.backup_file = file.display().to_string();
    let res = validate(&p).and_then(|()| finish(conn.as_mut(), &p, steps, t0));
    remove_quietly(&file, steps);
    res
}

/// Opens a sealed download next to it (`…_plain.bak`) and removes the sealed file; a
/// plaintext download is returned as it is.
fn unseal(
    file: &Path,
    keys: &dyn BackupKeys,
    cancel: &CancelToken,
    steps: &mut Steps,
) -> Result<PathBuf, Error> {
    let t0 = Instant::now();
    let key_id = match envelope::key_id_of(file) {
        Ok(Some(id)) => id,
        Ok(None) => {
            steps.set(
                "decrypt",
                "skipped",
                "The backup is not encrypted (made before tenant keys)",
                None,
            );
            return Ok(file.to_path_buf());
        }
        Err(e) => {
            steps.set("decrypt", "error", e.to_string(), Some(t0));
            return Err(e);
        }
    };
    steps.set(
        "decrypt",
        "running",
        format!("Decrypting with tenant backup key {key_id}"),
        None,
    );
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned());
    let plain = file.with_file_name(format!("{}_plain.bak", stem.unwrap_or_default()));
    match envelope::open_file(file, &plain, keys, cancel) {
        Ok(id) => {
            steps.set(
                "decrypt",
                "done",
                format!("Decrypted with tenant backup key {id}"),
                Some(t0),
            );
            remove_quietly(file, steps);
            Ok(plain)
        }
        Err(e) => {
            steps.set("decrypt", "error", e.to_string(), Some(t0));
            Err(e)
        }
    }
}

fn validate(p: &RestoreParams) -> Result<(), Error> {
    if p.target_database.is_empty() {
        return Err(Error::Sql("targetDatabase is required".into()));
    }
    quote::check_identifier(&p.target_database)?;
    quote::check_restore_path(&p.backup_file)?;
    Ok(())
}

fn connect(
    sql: &dyn Connector,
    p: &RestoreParams,
    cancel: &CancelToken,
    steps: &mut Steps,
    t0: Instant,
) -> Result<Box<dyn SqlConn>, Error> {
    match sql.connect(&p.target) {
        Ok(mut c) => {
            c.set_cancel(Some(cancel.clone()));
            Ok(c)
        }
        Err(e) => {
            steps.set("restore", "error", e.to_string(), Some(t0));
            Err(e)
        }
    }
}

fn finish(
    conn: &mut dyn SqlConn,
    p: &RestoreParams,
    steps: &mut Steps,
    t0: Instant,
) -> Result<(), Error> {
    let res = statement(conn, p).and_then(|(sql, args)| conn.run(&sql, &args));
    match res {
        Ok(()) => {
            steps.set(
                "restore",
                "done",
                format!("Restored {} from {}", p.target_database, p.backup_file),
                Some(t0),
            );
            Ok(())
        }
        Err(e) => {
            steps.set("restore", "error", e.to_string(), Some(t0));
            Err(e)
        }
    }
}

/// The `RESTORE DATABASE` statement and its parameters.
pub fn statement(
    conn: &mut dyn SqlConn,
    p: &RestoreParams,
) -> Result<(String, Vec<(String, String)>), Error> {
    let mut args = vec![
        ("db".to_owned(), p.target_database.clone()),
        ("path".to_owned(), p.backup_file.clone()),
    ];
    let mut with: Vec<String> = Vec::new();
    if conn.recovery_model(&p.target_database)?.is_none() {
        let files = conn.file_list(&p.backup_file)?;
        let (data_dir, log_dir) = conn.data_dirs()?;
        for (i, (logical, physical)) in moves(&p.target_database, &files, &data_dir, &log_dir)
            .into_iter()
            .enumerate()
        {
            with.push(format!("MOVE @l{i} TO @p{i}"));
            args.push((format!("l{i}"), logical));
            args.push((format!("p{i}"), physical));
        }
    }
    if p.with_replace {
        with.push("REPLACE".into());
    }
    with.push("STATS = 10".into());
    Ok((
        format!(
            "RESTORE DATABASE @db FROM DISK = @path WITH {};",
            with.join(", ")
        ),
        args,
    ))
}

/// Where each file of the backup goes for a new database `target`.
pub fn moves(
    target: &str,
    files: &[crate::sql::BackupFile],
    data_dir: &str,
    log_dir: &str,
) -> Vec<(String, String)> {
    let base = file_safe(target);
    let (mut data, mut logs, mut other) = (0usize, 0usize, 0usize);
    files
        .iter()
        .map(|f| {
            let physical = match f.kind.as_str() {
                "L" => {
                    logs += 1;
                    let name = if logs == 1 {
                        format!("{base}_log.ldf")
                    } else {
                        format!("{base}_log{logs}.ldf")
                    };
                    quote::join(log_dir, &name)
                }
                "D" => {
                    data += 1;
                    let name = if data == 1 {
                        format!("{base}.mdf")
                    } else {
                        format!("{base}_{data}.ndf")
                    };
                    quote::join(data_dir, &name)
                }
                _ => {
                    other += 1;
                    quote::join(data_dir, &format!("{base}_fs{other}"))
                }
            };
            (f.logical.clone(), physical)
        })
        .collect()
}

fn download_dir(
    conn: &mut dyn SqlConn,
    target: &Target,
    backup_dir: &str,
) -> Result<PathBuf, Error> {
    if !backup_dir.trim().is_empty() {
        return Ok(PathBuf::from(backup_dir.trim()));
    }
    if target.is_local()
        && let Some(d) = conn.backup_dir()?
    {
        return Ok(PathBuf::from(d));
    }
    Ok(std::env::temp_dir())
}

fn remove_quietly(file: &Path, steps: &mut Steps) {
    match std::fs::remove_file(file) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        // The restore's own result stands; the leftover file is recorded, not hidden.
        Err(e) => steps.set(
            "cleanup",
            "error",
            format!("remove {}: {e}", file.display()),
            None,
        ),
    }
}

#[cfg(test)]
#[path = "restore_tests.rs"]
mod tests;
