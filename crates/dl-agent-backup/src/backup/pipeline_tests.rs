//! Ports of Go `internal/backup` pipeline tests (`TestRunWithFakeDB_*`, `TestSHA256HexFile_*`)
//! plus the compression and recovery-model fixes.

use dl_json::json;

use super::*;
use crate::backup::Params;
use crate::fakes::{DestState, FakeDest, FakeSql, SqlState};
use crate::sql::{ServerInfo, Target};
use crate::testutil::temp_dir;
use crate::upload::Destination;

fn params(dir: &Path, kind: BackupType, dest: Option<&str>) -> Params {
    Params {
        history_id: "4b1d".into(),
        schedule_id: "sch-1".into(),
        database_name: "Sales".into(),
        backup_type: kind,
        target: Target {
            host: "localhost".into(),
            port: 1433,
            auth_type: "sql".into(),
            username: "sa".into(),
            password: "pw".into(),
        },
        destination: dest.map(|p| Destination {
            provider: p.into(),
            tokens: json!({}),
        }),
        backup_dir: dir.display().to_string(),
    }
}

fn sql() -> FakeSql {
    FakeSql::new(SqlState::default())
}

fn statuses(steps: &Steps) -> Vec<(String, String)> {
    steps
        .list()
        .iter()
        .map(|s| (s.step.clone(), s.status.clone()))
        .collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

#[test]
fn full_pipeline_backs_up_uploads_and_cleans_up() {
    let dir = temp_dir("pipe-full");
    let (db, dest) = (sql(), FakeDest::default());
    let mut steps = Steps::new();
    let r = run(
        &db,
        &dest,
        &params(&dir, BackupType::Full, Some("s3")),
        &CancelToken::new(),
        &mut steps,
    )
    .unwrap();

    assert_eq!(
        statuses(&steps),
        pairs(&[
            ("connect", "done"),
            ("backup", "done"),
            ("checksum", "done"),
            ("encrypt", "skipped"),
            ("upload", "done"),
            ("cleanup", "done")
        ])
    );
    let name = Path::new(&r.local_file_path)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with("srbackup_Sales_") && name.ends_with("_4b1d.bak"),
        "{name}"
    );
    assert_eq!(r.cloud_file_path, format!("fake://s3/{name}"));
    assert_eq!(r.bytes, b"fake backup bytes".len() as u64);
    assert_eq!(r.checksum.len(), 64);
    assert!(
        !Path::new(&r.local_file_path).exists(),
        "uploaded file is removed"
    );
    assert_eq!(dest.state().uploaded, vec![name]);

    let s = db.state();
    let (stmt, args) = &s.statements[0];
    assert_eq!(
        stmt,
        "BACKUP DATABASE @db TO DISK = @path WITH COMPRESSION, CHECKSUM, STATS = 10;"
    );
    assert_eq!(args[0], ("db".to_owned(), "Sales".to_owned()));
    assert!(steps.get("backup").unwrap().detail.contains("COMPRESSION"));
}

#[test]
fn differential_backup_adds_differential() {
    let dir = temp_dir("pipe-diff");
    let db = sql();
    let mut steps = Steps::new();
    run(
        &db,
        &FakeDest::default(),
        &params(&dir, BackupType::Differential, None),
        &CancelToken::new(),
        &mut steps,
    )
    .unwrap();
    assert!(
        db.state().statements[0]
            .0
            .starts_with("BACKUP DATABASE @db TO DISK = @path WITH DIFFERENTIAL, ")
    );
}

#[test]
fn transaction_log_backup_uses_backup_log() {
    let dir = temp_dir("pipe-log");
    let db = sql();
    db.state().recovery.insert("Sales".into(), "FULL".into());
    run(
        &db,
        &FakeDest::default(),
        &params(&dir, BackupType::TransactionLog, None),
        &CancelToken::new(),
        &mut Steps::new(),
    )
    .unwrap();
    assert!(
        db.state().statements[0]
            .0
            .starts_with("BACKUP LOG @db TO DISK = @path WITH ")
    );
}

#[test]
fn log_backup_in_simple_recovery_is_a_recovery_model_error() {
    let dir = temp_dir("pipe-simple");
    let db = sql();
    db.state().recovery.insert("Sales".into(), "SIMPLE".into());
    let mut steps = Steps::new();
    let err = run(
        &db,
        &FakeDest::default(),
        &params(&dir, BackupType::TransactionLog, None),
        &CancelToken::new(),
        &mut steps,
    )
    .unwrap_err();
    assert_eq!(err.code(), "backup_recovery_model");
    assert!(err.to_string().contains("SIMPLE"));
    assert_eq!(steps.get("backup").unwrap().status, "error");
    assert!(db.state().statements.is_empty(), "SQL Server is not asked");
}

#[test]
fn log_backup_of_a_missing_database_says_so() {
    let dir = temp_dir("pipe-missing");
    let err = run(
        &sql(),
        &FakeDest::default(),
        &params(&dir, BackupType::TransactionLog, None),
        &CancelToken::new(),
        &mut Steps::new(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");
}

#[test]
fn express_gets_no_compression_and_the_reason_is_recorded() {
    let dir = temp_dir("pipe-express");
    let db = sql();
    db.state().info = ServerInfo {
        engine_edition: 4,
        edition: "Express Edition (64-bit)".into(),
        major: 15,
        minor: 0,
        transport: "as-configured".into(),
    };
    let mut steps = Steps::new();
    let r = run(
        &db,
        &FakeDest::default(),
        &params(&dir, BackupType::Full, None),
        &CancelToken::new(),
        &mut steps,
    )
    .unwrap();
    assert_eq!(
        db.state().statements[0].0,
        "BACKUP DATABASE @db TO DISK = @path WITH CHECKSUM, STATS = 10;"
    );
    assert!(r.options.contains("no COMPRESSION"));
    assert!(
        steps
            .get("backup")
            .unwrap()
            .detail
            .contains("no COMPRESSION")
    );
}

#[test]
fn no_destination_skips_upload_and_keeps_the_file() {
    let dir = temp_dir("pipe-skip");
    let mut steps = Steps::new();
    let r = run(
        &sql(),
        &FakeDest::default(),
        &params(&dir, BackupType::Full, None),
        &CancelToken::new(),
        &mut steps,
    )
    .unwrap();
    assert_eq!(steps.get("upload").unwrap().status, "skipped");
    assert_eq!(
        steps.get("upload").unwrap().detail,
        "No cloud destination configured"
    );
    assert_eq!(steps.get("cleanup").unwrap().status, "skipped");
    assert!(r.cloud_file_path.is_empty());
    assert!(Path::new(&r.local_file_path).exists());
}

#[test]
fn unsupported_provider_fails_the_upload_step() {
    let dir = temp_dir("pipe-gdrive");
    let mut steps = Steps::new();
    let err = run(
        &sql(),
        &FakeDest::default(),
        &params(&dir, BackupType::Full, Some("dropbox")),
        &CancelToken::new(),
        &mut steps,
    )
    .unwrap_err();
    assert_eq!(err.code(), "backup_destination_unsupported");
    assert_eq!(steps.get("upload").unwrap().status, "error");
}

#[test]
fn missing_file_fails_the_checksum_step() {
    let dir = temp_dir("pipe-nofile");
    let db = sql();
    db.state().write_file = false;
    let mut steps = Steps::new();
    let err = run(
        &db,
        &FakeDest::default(),
        &params(&dir, BackupType::Full, None),
        &CancelToken::new(),
        &mut steps,
    )
    .unwrap_err();
    assert!(err.to_string().contains("checksum: stat"), "{err}");
    assert_eq!(steps.get("checksum").unwrap().status, "error");
}

#[test]
fn remote_server_file_is_not_reachable_from_the_agent() {
    let dir = temp_dir("pipe-remote");
    let db = sql();
    db.state().write_file = false;
    let mut p = params(&dir, BackupType::Full, None);
    p.target.host = "db-server-07\\SQL2019".into();
    let err = run(
        &db,
        &FakeDest::default(),
        &p,
        &CancelToken::new(),
        &mut Steps::new(),
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains(
            "was written by SQL Server on db-server-07 but is not reachable from the agent"
        ),
        "{msg}"
    );
}

#[test]
fn default_backup_path_comes_from_the_server_then_go_fallback() {
    let dir = temp_dir("pipe-default");
    let db = sql();
    db.state().backup_dir = Some(dir.display().to_string());
    let mut p = params(&dir, BackupType::Full, None);
    p.backup_dir.clear();
    let r = run(
        &db,
        &FakeDest::default(),
        &p,
        &CancelToken::new(),
        &mut Steps::new(),
    )
    .unwrap();
    assert!(r.local_file_path.starts_with(&dir.display().to_string()));

    let db = sql();
    db.state().write_file = false;
    let _ = run(
        &db,
        &FakeDest::default(),
        &p,
        &CancelToken::new(),
        &mut Steps::new(),
    );
    let path = &db.state().statements[0].1[1].1;
    assert!(
        path.starts_with("C:\\Windows\\Temp\\srbackup_Sales_"),
        "{path}"
    );
}

#[test]
fn connect_error_is_reported_in_the_connect_step() {
    let dir = temp_dir("pipe-conn");
    let db = sql();
    db.state().connect_error = Some(Error::Sql("connect to localhost: login failed".into()));
    let mut steps = Steps::new();
    let err = run(
        &db,
        &FakeDest::default(),
        &params(&dir, BackupType::Full, None),
        &CancelToken::new(),
        &mut steps,
    )
    .unwrap_err();
    assert_eq!(err.code(), "backup_sql_failed");
    assert_eq!(statuses(&steps), pairs(&[("connect", "error")]));
}

#[test]
fn backup_exec_error_is_reported_in_the_backup_step() {
    let dir = temp_dir("pipe-exec");
    let db = sql();
    db.state().run_error = Some(Error::Sql(
        "statement failed: 3201 cannot open backup device".into(),
    ));
    let mut steps = Steps::new();
    run(
        &db,
        &FakeDest::default(),
        &params(&dir, BackupType::Full, None),
        &CancelToken::new(),
        &mut steps,
    )
    .unwrap_err();
    let s = steps.get("backup").unwrap();
    assert_eq!(s.status, "error");
    assert!(s.detail.contains("3201"));
}

#[test]
fn upload_error_fails_the_job_and_keeps_the_file() {
    let dir = temp_dir("pipe-uperr");
    let dest = FakeDest(std::sync::Arc::new(std::sync::Mutex::new(DestState {
        upload_error: Some(Error::Upload("S3 upload part 1: HTTP 403".into())),
        ..DestState::default()
    })));
    let mut steps = Steps::new();
    let err = run(
        &sql(),
        &dest,
        &params(&dir, BackupType::Full, Some("s3")),
        &CancelToken::new(),
        &mut steps,
    )
    .unwrap_err();
    assert_eq!(err.code(), "backup_upload_failed");
    assert_eq!(steps.get("upload").unwrap().status, "error");
    assert!(steps.get("cleanup").is_none());
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        1,
        "the file stays for the next attempt"
    );
}

#[test]
fn empty_and_bad_database_names_are_refused() {
    let dir = temp_dir("pipe-name");
    let mut p = params(&dir, BackupType::Full, None);
    p.database_name.clear();
    let err = run(
        &sql(),
        &FakeDest::default(),
        &p,
        &CancelToken::new(),
        &mut Steps::new(),
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "databaseName is required");
    p.database_name = "a\u{0}b".into();
    assert!(
        run(
            &sql(),
            &FakeDest::default(),
            &p,
            &CancelToken::new(),
            &mut Steps::new()
        )
        .is_err()
    );
}

#[test]
fn odd_database_names_stay_parameters() {
    let dir = temp_dir("pipe-odd");
    let db = sql();
    let mut p = params(&dir, BackupType::Full, None);
    p.database_name = "Bán]'; DROP--".into();
    let r = run(
        &db,
        &FakeDest::default(),
        &p,
        &CancelToken::new(),
        &mut Steps::new(),
    )
    .unwrap();
    let s = db.state();
    assert!(!s.statements[0].0.contains("DROP"));
    assert_eq!(s.statements[0].1[0].1, "Bán]'; DROP--");
    assert!(r.local_file_path.contains("srbackup_Bán]'; DROP--_"));
}

#[test]
fn sha256_of_known_empty_and_missing_files() {
    let dir = temp_dir("pipe-sha");
    let f = dir.join("abc.bak");
    std::fs::write(&f, b"abc").unwrap();
    assert_eq!(
        sha256_file(&f, &CancelToken::new()).unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    std::fs::write(&f, b"").unwrap();
    assert_eq!(
        sha256_file(&f, &CancelToken::new()).unwrap(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert!(sha256_file(&dir.join("nope.bak"), &CancelToken::new()).is_err());
    let c = CancelToken::new();
    c.cancel();
    assert_eq!(sha256_file(&f, &c).unwrap_err(), Error::Cancelled);
}

#[test]
fn utc_stamp_matches_go_layout() {
    assert_eq!(utc_stamp(1_795_045_847), "20261118_235047");
    assert_eq!(utc_stamp(0), "19700101_000000");
}
