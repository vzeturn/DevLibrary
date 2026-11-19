//! Live check of `sql-setup` and the selftest SQL rows against a real SQL Server laid out
//! like the SR demo machine: `SMILE_FO` and `SMILE_POS`, no `SMILE_BO`.
//!
//! Skipped unless `DL_AGENT_SQL_LIVE` holds a sysadmin connection string (see `sql_live.rs`).
//! The test creates the two databases when missing and a throw-away SQL login
//! `dl_setup_live`; run it with `--test-threads=1` on a disposable container.

use std::path::{Path, PathBuf};

use dl_agent::cli::App;
use dl_agent::selftest::{Status, Trigger, execute};
use dl_agent::sql::TdsConnector;
use dl_agent::sql::secret::{self, AuthMode};
use dl_agent::sql::setup::{self, Live, Request};
use dl_json::Value;

const LOGIN: &str = "dl_setup_live";
const PASSWORD: &str = "Smile-Setup-9x!";

/// A sysadmin session and the `Server=` value of the live connection string.
fn admin() -> Option<(dl_tds::Client, String)> {
    let conn = std::env::var("DL_AGENT_SQL_LIVE").ok()?;
    let server = conn
        .split(';')
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| {
            let k = k.trim().to_ascii_lowercase();
            k == "server" || k == "data source" || k == "address"
        })
        .map(|(_, v)| v.trim().to_string())
        .expect("DL_AGENT_SQL_LIVE names no Server");
    let c = TdsConnector::parse(&conn).unwrap();
    Some((dl_tds::Client::connect(c.config()).unwrap(), server))
}

fn data_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dl-agent-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The SR demo layout and a SQL login that can open `SMILE_FO` only.
fn fixture(a: &mut dl_tds::Client) {
    a.batch(
        "IF DB_ID(N'SMILE_FO') IS NULL CREATE DATABASE SMILE_FO;\n\
         IF DB_ID(N'SMILE_POS') IS NULL CREATE DATABASE SMILE_POS;\n\
         IF DB_ID(N'SMILE_BO') IS NOT NULL DROP DATABASE SMILE_BO;",
    )
    .unwrap();
    a.batch(&format!(
        "USE SMILE_POS; IF USER_ID(N'{LOGIN}') IS NOT NULL DROP USER [{LOGIN}];\n\
         USE SMILE_FO; IF USER_ID(N'{LOGIN}') IS NOT NULL DROP USER [{LOGIN}];\n\
         USE master; IF SUSER_ID(N'{LOGIN}') IS NOT NULL DROP LOGIN [{LOGIN}];\n\
         CREATE LOGIN [{LOGIN}] WITH PASSWORD = N'{PASSWORD}', CHECK_POLICY = OFF;\n\
         USE SMILE_FO; CREATE USER [{LOGIN}] FOR LOGIN [{LOGIN}];\n\
         EXEC sp_addrolemember N'db_datareader', N'{LOGIN}';\n\
         USE master;"
    ))
    .unwrap();
}

fn request(server: &str, password: &str) -> Request {
    Request {
        server: server.to_string(),
        auth: AuthMode::Sql,
        user: LOGIN.into(),
        password: password.into(),
        databases: Vec::new(),
    }
}

fn sql_json(dir: &Path) -> Value {
    dl_json::parse(&std::fs::read_to_string(dir.join("sql.json")).unwrap()).unwrap()
}

#[test]
fn sql_login_is_tested_on_every_smile_database_before_it_is_saved() {
    let Some((mut a, server)) = admin() else {
        return;
    };
    fixture(&mut a);

    // A wrong password fails on master, before any database is listed.
    let wrong = setup::run(&request(&server, "not-the-password"), &Live);
    assert!(!wrong.ok);
    assert_eq!(
        wrong.error.as_ref().and_then(|f| f.number),
        Some(18456),
        "{wrong:?}"
    );

    // Both SMILE databases are found; the login can open SMILE_FO only.
    let half = setup::run(&request(&server, PASSWORD), &Live);
    let names: Vec<&str> = half.databases.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["SMILE_FO", "SMILE_POS"], "{half:?}");
    assert!(half.databases[0].error.is_none(), "{half:?}");
    let pos = half.databases[1]
        .error
        .as_ref()
        .expect("SMILE_POS must fail");
    assert!(
        matches!(pos.number, Some(4060 | 4063 | 916 | 18456)),
        "SMILE_POS: {pos:?}"
    );
    assert!(!half.ok && half.hint.is_none(), "{half:?}");
    let dir = data_dir("setup-live");
    assert!(setup::save(&dir, &request(&server, PASSWORD), &half).is_err());
    assert!(!dir.join("sql.json").exists());

    // The DBA grants SMILE_POS: now every database passes and the choice is saved.
    a.batch(&format!(
        "USE SMILE_POS; CREATE USER [{LOGIN}] FOR LOGIN [{LOGIN}];\n\
         EXEC sp_addrolemember N'db_datareader', N'{LOGIN}'; USE master;"
    ))
    .unwrap();
    let req = request(&server, PASSWORD);
    let all = setup::run(&req, &Live);
    assert!(all.ok, "{all:?}");
    setup::save(&dir, &req, &all).unwrap();

    let saved = setup::saved(&dir).unwrap().unwrap();
    assert_eq!(saved.auth, AuthMode::Sql);
    assert_eq!(saved.user, LOGIN);
    assert_eq!(saved.databases, ["SMILE_FO", "SMILE_POS"]);
    assert_eq!(
        setup::saved_password(&dir, LOGIN).unwrap().as_deref(),
        Some(PASSWORD)
    );
    let doc = sql_json(&dir);
    for db in ["SMILE_FO", "SMILE_POS"] {
        let entry = &doc["databases"][db];
        let conn = entry["connection"].as_str().unwrap();
        assert!(!conn.contains(PASSWORD), "{conn}");
        assert_eq!(entry["auth"].as_str(), Some("sql"));
        assert!(entry["password_sealed"].as_str().is_some());
        // The saved entry opens the database it names.
        let (full, mode) = secret::resolve(entry).unwrap();
        assert_eq!(mode, AuthMode::Sql);
        let c = TdsConnector::parse(&full).unwrap();
        let mut s = dl_tds::Client::connect(c.config()).unwrap();
        let r = s.query("SELECT DB_NAME() AS db", &[]).unwrap();
        assert_eq!(r.rows()[0].get::<String>(0).unwrap(), db);
    }

    // The selftest rows the hub sees: one per database, with the login and the TLS rung.
    let run = execute(
        &App::new("dl-agent-test", "Test agent"),
        &dir,
        Trigger::Command,
        Some("core"),
        &Value::Null,
    );
    for db in ["SMILE_FO", "SMILE_POS"] {
        let id = format!("sql.{db}");
        let row = run.checks.iter().find(|c| c.id == id).expect(&id);
        println!("{id}: {:?} {}", row.status, row.detail);
        assert!(matches!(row.status, Status::Pass | Status::Warn), "{row:?}");
        assert!(
            row.detail.contains(&format!("sql auth as {LOGIN}")),
            "{row:?}"
        );
        assert!(!row.detail.contains(PASSWORD));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
