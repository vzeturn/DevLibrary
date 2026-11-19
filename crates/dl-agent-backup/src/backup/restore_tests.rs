//! Ports of Go `restore_test.go` and `TestRestoreWithFakeDB_*` / `TestRestoreFromRecord_*`.

use dl_json::json;

use super::*;
use crate::fakes::{FakeDest, FakeSql, SqlState};
use crate::sql::BackupFile;
use crate::testutil::temp_dir;

fn target() -> Target {
    Target {
        host: "localhost".into(),
        port: 1433,
        auth_type: "sql".into(),
        username: "sa".into(),
        password: "pw".into(),
    }
}

fn params(file: &str) -> RestoreParams {
    RestoreParams {
        target: target(),
        target_database: "Sales_copy".into(),
        with_replace: true,
        backup_file: file.into(),
    }
}

fn sql() -> FakeSql {
    FakeSql::new(SqlState::default())
}

fn dest() -> Option<Destination> {
    Some(Destination {
        provider: "s3".into(),
        tokens: json!({}),
    })
}

#[test]
fn empty_target_and_file_are_refused() {
    let mut p = params("/x/a.bak");
    p.target_database.clear();
    let err = restore(&sql(), &p, &CancelToken::new(), &mut Steps::new()).unwrap_err();
    assert_eq!(err.to_string(), "targetDatabase is required");
    let err = restore(&sql(), &params(""), &CancelToken::new(), &mut Steps::new()).unwrap_err();
    assert_eq!(err.to_string(), "backupFilePath is required");
}

#[test]
fn extensions_and_unc_paths_are_checked_like_go() {
    for bad in [
        "/x/a.txt",
        "/x/a",
        "\\\\server\\share\\a.bak",
        "//server/share/a.bak",
    ] {
        assert!(
            restore(&sql(), &params(bad), &CancelToken::new(), &mut Steps::new()).is_err(),
            "{bad}"
        );
    }
    for good in ["/x/a.bak", "/x/a.TRN", "C:\\b\\c.BAK"] {
        restore(
            &sql(),
            &params(good),
            &CancelToken::new(),
            &mut Steps::new(),
        )
        .unwrap();
    }
}

#[test]
fn connect_error_is_reported_in_the_restore_step() {
    let db = sql();
    db.state().connect_error = Some(Error::Sql("login failed".into()));
    let mut steps = Steps::new();
    restore(&db, &params("/x/a.bak"), &CancelToken::new(), &mut steps).unwrap_err();
    assert_eq!(steps.get("restore").unwrap().status, "error");
}

#[test]
fn new_database_gets_its_files_moved_under_the_new_name() {
    let db = sql();
    let mut steps = Steps::new();
    restore(&db, &params("/x/a.bak"), &CancelToken::new(), &mut steps).unwrap();
    let s = db.state();
    let (stmt, args) = &s.statements[0];
    assert_eq!(
        stmt,
        "RESTORE DATABASE @db FROM DISK = @path WITH MOVE @l0 TO @p0, MOVE @l1 TO @p1, REPLACE, STATS = 10;"
    );
    let arg = |k: &str| {
        args.iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
            .unwrap()
    };
    assert_eq!(arg("db"), "Sales_copy");
    assert_eq!(arg("l0"), "Sales");
    assert_eq!(arg("p0"), "/var/opt/mssql/data/Sales_copy.mdf");
    assert_eq!(arg("l1"), "Sales_log");
    assert_eq!(arg("p1"), "/var/opt/mssql/log/Sales_copy_log.ldf");
    let step = steps.get("restore").unwrap();
    assert_eq!(step.status, "done");
    assert_eq!(step.detail, "Restored Sales_copy from /x/a.bak");
}

#[test]
fn existing_database_is_replaced_in_place_like_go() {
    let db = sql();
    db.state()
        .recovery
        .insert("Sales_copy".into(), "FULL".into());
    restore(
        &db,
        &params("/x/a.bak"),
        &CancelToken::new(),
        &mut Steps::new(),
    )
    .unwrap();
    assert_eq!(
        db.state().statements[0].0,
        "RESTORE DATABASE @db FROM DISK = @path WITH REPLACE, STATS = 10;"
    );

    let db = sql();
    db.state()
        .recovery
        .insert("Sales_copy".into(), "FULL".into());
    let mut p = params("/x/a.bak");
    p.with_replace = false;
    restore(&db, &p, &CancelToken::new(), &mut Steps::new()).unwrap();
    assert_eq!(
        db.state().statements[0].0,
        "RESTORE DATABASE @db FROM DISK = @path WITH STATS = 10;"
    );
}

#[test]
fn extra_data_and_log_files_get_numbered_names() {
    let files = [
        BackupFile {
            logical: "d1".into(),
            kind: "D".into(),
        },
        BackupFile {
            logical: "d2".into(),
            kind: "D".into(),
        },
        BackupFile {
            logical: "l1".into(),
            kind: "L".into(),
        },
        BackupFile {
            logical: "l2".into(),
            kind: "L".into(),
        },
        BackupFile {
            logical: "fs".into(),
            kind: "S".into(),
        },
    ];
    let m = moves("New:Db", &files, "D:\\Data", "E:\\Log\\");
    let phys: Vec<&str> = m.iter().map(|(_, p)| p.as_str()).collect();
    assert_eq!(
        phys,
        [
            "D:\\Data\\New_Db.mdf",
            "D:\\Data\\New_Db_2.ndf",
            "E:\\Log\\New_Db_log.ldf",
            "E:\\Log\\New_Db_log2.ldf",
            "D:\\Data\\New_Db_fs1"
        ]
    );
}

#[test]
fn exec_error_fails_the_restore_step() {
    let db = sql();
    db.state().run_error = Some(Error::Sql("statement failed: 3154".into()));
    let mut steps = Steps::new();
    let err = restore(&db, &params("/x/a.bak"), &CancelToken::new(), &mut steps).unwrap_err();
    assert!(err.to_string().contains("3154"));
    assert_eq!(steps.get("restore").unwrap().status, "error");
}

#[test]
fn from_record_uses_the_local_file_when_present() {
    let dir = temp_dir("restore-local");
    let local = dir.join("srbackup_Sales_x.bak");
    std::fs::write(&local, b"bak").unwrap();
    let (db, cloud) = (sql(), FakeDest::default());
    let src = Source {
        local_file_path: local.display().to_string(),
        cloud_file_path: "fake://s3/x.bak".into(),
        tag: "r1".into(),
        destination: dest(),
    };
    restore_from_record(
        &db,
        &cloud,
        &src,
        params(""),
        "",
        &CancelToken::new(),
        &mut Steps::new(),
    )
    .unwrap();
    assert!(cloud.state().downloaded.is_empty());
    assert_eq!(db.state().statements[0].1[1].1, local.display().to_string());
}

#[test]
fn from_record_downloads_when_the_local_file_is_gone() {
    let dir = temp_dir("restore-cloud");
    let (db, cloud) = (sql(), FakeDest::default());
    let src = Source {
        local_file_path: dir.join("gone.bak").display().to_string(),
        cloud_file_path: "fake://s3/x.bak".into(),
        tag: "req-7".into(),
        destination: dest(),
    };
    let mut steps = Steps::new();
    let backup_dir = dir.display().to_string();
    restore_from_record(
        &db,
        &cloud,
        &src,
        params(""),
        &backup_dir,
        &CancelToken::new(),
        &mut steps,
    )
    .unwrap();
    let (from, to) = cloud.state().downloaded[0].clone();
    assert_eq!(from, "fake://s3/x.bak");
    assert_eq!(to, dir.join("restore_req-7.bak").display().to_string());
    assert_eq!(db.state().statements[0].1[1].1, to);
    assert!(
        !Path::new(&to).exists(),
        "the downloaded copy is removed afterwards"
    );
    assert_eq!(steps.get("download").unwrap().status, "done");
}

#[test]
fn from_record_without_any_copy_or_credentials_fails_like_go() {
    let src = Source::default();
    let err = restore_from_record(
        &sql(),
        &FakeDest::default(),
        &src,
        params(""),
        "",
        &CancelToken::new(),
        &mut Steps::new(),
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "restore: backup has neither a local file nor a cloud file path"
    );

    let src = Source {
        cloud_file_path: "s3://b/k.bak".into(),
        ..Source::default()
    };
    let err = restore_from_record(
        &sql(),
        &FakeDest::default(),
        &src,
        params(""),
        "",
        &CancelToken::new(),
        &mut Steps::new(),
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "restore: cloud file path \"s3://b/k.bak\" set but no cloud credentials provided"
    );
}

#[test]
fn from_record_download_error_propagates() {
    let dir = temp_dir("restore-dlerr");
    let cloud = FakeDest::default();
    cloud.state().download_error = Some(Error::Upload("S3 GET: HTTP 404".into()));
    let src = Source {
        cloud_file_path: "fake://s3/x.bak".into(),
        destination: dest(),
        ..Source::default()
    };
    let db = sql();
    let mut steps = Steps::new();
    let err = restore_from_record(
        &db,
        &cloud,
        &src,
        params(""),
        &dir.display().to_string(),
        &CancelToken::new(),
        &mut steps,
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "restore: download backup from cloud: S3 GET: HTTP 404"
    );
    assert_eq!(steps.get("download").unwrap().status, "error");
    assert!(db.state().statements.is_empty());
}
