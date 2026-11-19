//! The backup and restore pipelines against a real SQL Server, enabled by
//! `BACKUP_AGENT_TEST_SQL` (`server=host,port;user id=…;password=…`, a SQL login with
//! sysadmin; add `encrypt=disable` where the client has no TLS backend, macOS and Linux). `BACKUP_AGENT_TEST_DIR` must be a directory the server and the test see at the
//! same path (a bind mount for a container): SQL Server writes the `.bak` there and the
//! pipeline reads it back. Every test makes its own databases and drops them afterwards.

use std::path::Path;

use dl_tds::{CancelToken, Encrypt};

use super::pipeline::run;
use super::restore::{RestoreParams, restore};
use super::{BackupResult, BackupType, Params, Steps};
use crate::Error;
use crate::sql::{BackupFile, Connector, ServerInfo, SqlConn, Target, Tds, open};
use crate::upload::Real;

struct Env {
    target: Target,
    dir: String,
    sql: TestTds,
}

/// [`Tds`], with the encryption mode the test connection string asks for.
#[derive(Debug, Clone, Copy)]
struct TestTds(Option<Encrypt>);

impl Connector for TestTds {
    fn connect(&self, target: &Target) -> Result<Box<dyn SqlConn>, Error> {
        match self.0 {
            Some(mode) => open(target, &target.config().with_encrypt(mode)),
            None => Tds.connect(target),
        }
    }
}

fn env() -> Option<Env> {
    let conn = std::env::var("BACKUP_AGENT_TEST_SQL").unwrap_or_default();
    if conn.is_empty() {
        eprintln!("note: BACKUP_AGENT_TEST_SQL is not set; skipping the SQL Server tests");
        return None;
    }
    let dir = std::env::var("BACKUP_AGENT_TEST_DIR")
        .expect("BACKUP_AGENT_TEST_DIR (a directory shared with the server) is required");
    let mut target = Target {
        auth_type: "sql".into(),
        ..Target::default()
    };
    let mut encrypt = None;
    for part in conn.split(';') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        match k.trim().to_ascii_lowercase().as_str() {
            "server" => match v.trim().split_once(',') {
                Some((h, p)) => {
                    target.host = h.trim().into();
                    target.port = p.trim().parse().expect("the port in BACKUP_AGENT_TEST_SQL");
                }
                None => target.host = v.trim().into(),
            },
            "user id" | "uid" => target.username = v.trim().into(),
            "password" | "pwd" => target.password = v.into(),
            "encrypt" if v.trim().eq_ignore_ascii_case("disable") => {
                encrypt = Some(Encrypt::Disabled);
            }
            _ => {}
        }
    }
    Some(Env {
        target,
        dir,
        sql: TestTds(encrypt),
    })
}

/// A database name unique to this test run.
fn name(tag: &str) -> String {
    format!("bk_{tag}_{}", std::process::id())
}

/// Runs `statement` with `@db` (and `@path`) bound; panics with the server's message.
fn exec(e: &Env, statement: &str, db: &str, path: &str) {
    let mut c = e
        .sql
        .connect(&e.target)
        .unwrap_or_else(|e| panic!("connect: {e}"));
    let args = [
        ("db".to_owned(), db.to_owned()),
        ("path".to_owned(), path.to_owned()),
    ];
    c.run(statement, &args)
        .unwrap_or_else(|e| panic!("{statement}: {e}"));
}

/// Drops the listed databases when the test ends, also when it fails.
struct Dbs<'a>(&'a Env, Vec<String>);

impl Drop for Dbs<'_> {
    fn drop(&mut self) {
        let drop_db = "IF DB_ID(@db) IS NOT NULL BEGIN \
                       DECLARE @s nvarchar(max) = N'ALTER DATABASE ' + QUOTENAME(@db) \
                       + N' SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE ' + QUOTENAME(@db); \
                       EXEC(@s); END;";
        for db in &self.1 {
            let res = (self.0.sql)
                .connect(&self.0.target)
                .and_then(|mut c| c.run(drop_db, &[("db".to_owned(), db.clone())]));
            if let Err(e) = res {
                eprintln!("warning: could not drop test database {db}: {e}");
            }
        }
    }
}

/// A database holding table `t` with one row (42), in `recovery` (`FULL` / `SIMPLE`).
fn create(e: &Env, db: &str, recovery: &str) {
    exec(
        e,
        &format!(
            "DECLARE @q nvarchar(258) = QUOTENAME(@db); \
             DECLARE @s nvarchar(max) = N'CREATE DATABASE ' + @q; EXEC(@s); \
             SET @s = N'ALTER DATABASE ' + @q + N' SET RECOVERY {recovery}'; EXEC(@s); \
             SET @s = N'CREATE TABLE ' + @q + N'.dbo.t (v int); \
                        INSERT INTO ' + @q + N'.dbo.t VALUES (42)'; EXEC(@s);"
        ),
        db,
        "",
    );
}

fn backup(
    sql: &dyn Connector,
    e: &Env,
    db: &str,
    kind: BackupType,
    id: &str,
) -> Result<BackupResult, Error> {
    let p = Params {
        history_id: id.into(),
        schedule_id: "sql-test".into(),
        database_name: db.into(),
        backup_type: kind,
        target: e.target.clone(),
        destination: None,
        backup_dir: e.dir.clone(),
    };
    let mut steps = Steps::default();
    let res = run(sql, &Real, &p, &CancelToken::new(), &mut steps);
    if let Ok(r) = &res {
        let detail = steps
            .list()
            .iter()
            .find(|s| s.step == "backup")
            .map(|s| s.detail.clone());
        assert!(
            detail.unwrap_or_default().contains(&r.options),
            "the step records the options"
        );
    }
    res
}

/// Fails unless msdb recorded the backup at `path` with checksums and `compressed`.
fn check_backupset(e: &Env, path: &str, compressed: bool) {
    let op = if compressed { "<" } else { "=" };
    exec(
        e,
        &format!(
            "IF NOT EXISTS (SELECT 1 FROM msdb.dbo.backupset b \
               JOIN msdb.dbo.backupmediafamily f ON f.media_set_id = b.media_set_id \
               WHERE f.physical_device_name = @path AND b.has_backup_checksums = 1 \
                 AND b.compressed_backup_size {op} b.backup_size) \
             THROW 50000, 'the backup set does not have the expected options', 1;"
        ),
        "",
        path,
    );
}

fn remove(path: &str) {
    if let Err(e) = std::fs::remove_file(path) {
        eprintln!("warning: could not remove {path}: {e}");
    }
}

/// The real connection, but the server says it is Express (no backup compression).
struct AsExpress(TestTds);

struct Express {
    inner: Box<dyn SqlConn>,
    info: ServerInfo,
}

impl Connector for AsExpress {
    fn connect(&self, target: &Target) -> Result<Box<dyn SqlConn>, Error> {
        let inner = self.0.connect(target)?;
        let info = ServerInfo {
            engine_edition: 4,
            edition: "Express Edition (64-bit)".into(),
            ..inner.info().clone()
        };
        Ok(Box::new(Express { inner, info }))
    }
}

impl SqlConn for Express {
    fn info(&self) -> &ServerInfo {
        &self.info
    }
    fn backup_dir(&mut self) -> Result<Option<String>, Error> {
        self.inner.backup_dir()
    }
    fn recovery_model(&mut self, database: &str) -> Result<Option<String>, Error> {
        self.inner.recovery_model(database)
    }
    fn run(&mut self, statement: &str, params: &[(String, String)]) -> Result<(), Error> {
        self.inner.run(statement, params)
    }
    fn file_list(&mut self, path: &str) -> Result<Vec<BackupFile>, Error> {
        self.inner.file_list(path)
    }
    fn data_dirs(&mut self) -> Result<(String, String), Error> {
        self.inner.data_dirs()
    }
    fn list_databases(&mut self) -> Result<Vec<String>, Error> {
        self.inner.list_databases()
    }
    fn set_cancel(&mut self, token: Option<CancelToken>) {
        self.inner.set_cancel(token);
    }
}

#[test]
fn full_backups_with_and_without_compression_then_a_log_backup() {
    let Some(e) = env() else { return };
    let db = name("full");
    let _dbs = Dbs(&e, vec![db.clone()]);
    create(&e, &db, "FULL");

    let r = backup(&e.sql, &e, &db, BackupType::Full, "h-compressed").unwrap();
    assert!(
        r.options.contains(": COMPRESSION, CHECKSUM"),
        "{}",
        r.options
    );
    assert!(Path::new(&r.local_file_path).is_file());
    assert!(r.bytes > 0 && r.checksum.len() == 64);
    assert!(r.cloud_file_path.is_empty());
    check_backupset(&e, &r.local_file_path, true);
    remove(&r.local_file_path);

    let r = backup(&AsExpress(e.sql), &e, &db, BackupType::Full, "h-plain").unwrap();
    assert!(r.options.contains("no COMPRESSION"), "{}", r.options);
    check_backupset(&e, &r.local_file_path, false);
    remove(&r.local_file_path);

    let r = backup(&e.sql, &e, &db, BackupType::TransactionLog, "h-log").unwrap();
    assert!(Path::new(&r.local_file_path).is_file());
    remove(&r.local_file_path);
}

#[test]
fn a_log_backup_on_simple_recovery_fails_with_its_code() {
    let Some(e) = env() else { return };
    let db = name("simple");
    let _dbs = Dbs(&e, vec![db.clone()]);
    create(&e, &db, "SIMPLE");

    let err = backup(&e.sql, &e, &db, BackupType::TransactionLog, "h-simple").unwrap_err();
    assert!(matches!(err, Error::RecoveryModel(_)), "{err}");
    assert_eq!(err.code(), "backup_recovery_model");
    assert!(err.to_string().contains("SIMPLE"), "{err}");
}

#[test]
fn restore_with_replace_to_a_new_name() {
    let Some(e) = env() else { return };
    let (src, copy) = (name("source"), name("restored"));
    let _dbs = Dbs(&e, vec![src.clone(), copy.clone()]);
    create(&e, &src, "FULL");
    let r = backup(&e.sql, &e, &src, BackupType::Full, "h-restore").unwrap();

    let p = RestoreParams {
        target: e.target.clone(),
        target_database: copy.clone(),
        with_replace: true,
        backup_file: r.local_file_path.clone(),
    };
    let row = "DECLARE @n int; \
               DECLARE @q nvarchar(400) = N'SELECT @n = COUNT(*) FROM ' + QUOTENAME(@db) + N'.dbo.t WHERE v = 42'; \
               EXEC sp_executesql @q, N'@n int OUTPUT', @n OUTPUT; \
               IF @n <> 1 THROW 50000, 'the restored row is missing', 1;";
    // A new database next to its still-online source (the files are moved), then over itself.
    for _ in 0..2 {
        let mut steps = Steps::default();
        restore(&e.sql, &p, &CancelToken::new(), &mut steps).unwrap();
        exec(&e, row, &copy, "");
        exec(&e, row, &src, "");
    }
    remove(&r.local_file_path);
}

#[path = "replace_tests.rs"]
mod replace;
