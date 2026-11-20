use std::cell::RefCell;

use super::*;
use crate::store::testdir::TestDir;

/// A server with some databases; `deny` lists databases the login cannot open.
struct Fake {
    present: Vec<&'static str>,
    deny: Vec<&'static str>,
    login_missing: bool,
    /// The rung a test login reaches.
    rung: Rung,
    /// A server without TLS 1.2 on another computer: a SQL login fails as
    /// `sql_tls_unavailable` unless `clear`, then works unencrypted.
    old_remote: bool,
    seen: RefCell<Vec<String>>,
}

fn tls_unavailable() -> Failure {
    Failure {
        message: "sql_tls_unavailable: tls handshake failed".into(),
        number: None,
    }
}

fn fake(present: &[&'static str]) -> Fake {
    Fake {
        present: present.to_vec(),
        deny: Vec::new(),
        login_missing: false,
        rung: Rung::AsConfigured,
        old_remote: false,
        seen: RefCell::new(Vec::new()),
    }
}

fn login_failed() -> Failure {
    Failure {
        message: "Login failed for user 'NT AUTHORITY\\SYSTEM'.".into(),
        number: Some(18456),
    }
}

impl Sql for Fake {
    fn present(
        &self,
        connection: &str,
        wanted: &[&str],
        clear: bool,
    ) -> Result<Vec<String>, Failure> {
        self.seen.borrow_mut().push(connection.to_string());
        if self.login_missing {
            return Err(login_failed());
        }
        if self.old_remote && !clear {
            return Err(tls_unavailable());
        }
        Ok(wanted
            .iter()
            .filter(|w| self.present.contains(w))
            .map(|w| w.to_string())
            .collect())
    }

    fn test(&self, connection: &str, clear: bool) -> Result<Rung, Failure> {
        self.seen.borrow_mut().push(connection.to_string());
        if self.old_remote {
            return if clear {
                Ok(Rung::Unencrypted)
            } else {
                Err(tls_unavailable())
            };
        }
        let c: dl_tds::Config = connection.parse().unwrap();
        if self.deny.contains(&c.database()) {
            return Err(Failure {
                message: format!(
                    "Cannot open database \"{}\" that was requested by the login. Using the user default database \"master\" instead.",
                    c.database()
                ),
                // What SQL Server 2017/2022 answer (seen live); older servers say 4060.
                number: Some(4063),
            });
        }
        Ok(self.rung)
    }
}

fn windows(server: &str) -> Request {
    Request {
        server: server.into(),
        auth: AuthMode::Windows,
        user: String::new(),
        password: String::new(),
        databases: Vec::new(),
        allow_unencrypted: false,
    }
}

fn sql_login(password: &str) -> Request {
    Request {
        server: ".\\SQLEXPRESS".into(),
        auth: AuthMode::Sql,
        user: "smile".into(),
        password: password.into(),
        databases: Vec::new(),
        allow_unencrypted: false,
    }
}

#[test]
fn connection_strings_parse_for_both_modes_and_quote_values() {
    let c: dl_tds::Config = connection(&windows(".\\SQLEXPRESS"), "SMILE_FO")
        .parse()
        .unwrap();
    assert!(c.integrated_auth());
    assert_eq!(c.instance(), Some("SQLEXPRESS"));
    assert_eq!(c.database(), "SMILE_FO");
    let c: dl_tds::Config = connection(&sql_login("p;w\"d"), "SMILE_POS")
        .parse()
        .unwrap();
    assert!(!c.integrated_auth());
    assert_eq!((c.user(), c.password()), ("smile", "p;w\"d"));
    let c: dl_tds::Config = connection(&windows("db01,1500"), "SMILE_BO")
        .parse()
        .unwrap();
    assert_eq!((c.host(), c.port()), ("db01", Some(1500)));
}

#[test]
fn local_system_login_depends_on_where_the_server_is() {
    for s in [
        ".",
        ".\\SQLEXPRESS",
        "(local)",
        "localhost,1433",
        "POS01\\SQL2008",
        "tcp:127.0.0.1",
    ] {
        assert_eq!(
            service_login(s, "POS01", "HOTEL"),
            "NT AUTHORITY\\SYSTEM",
            "{s}"
        );
    }
    assert_eq!(
        service_login("db01\\SMILE", "POS01", "HOTEL"),
        "HOTEL\\POS01$"
    );
    assert_eq!(service_login("db01", "POS01", "POS01"), "POS01$");
    assert_eq!(service_login("db01", "POS01", ""), "POS01$");
}

#[test]
fn every_present_smile_database_is_tested() {
    let f = fake(&["SMILE_FO", "SMILE_BO"]);
    let out = run(&windows("."), &f);
    assert!(out.ok, "{:?}", out);
    assert_eq!(out.login, "NT AUTHORITY\\SYSTEM");
    let names: Vec<&str> = out.databases.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["SMILE_FO", "SMILE_BO"]);
    assert!(f.seen.borrow()[0].contains("Database=\"master\""));
    assert!(out.hint.is_none());
    let v = out.to_json();
    assert_eq!(v["auth"].as_str(), Some("windows"));
    assert_eq!(v["databases"][1]["ok"].as_bool(), Some(true));
}

#[test]
fn missing_windows_login_yields_dba_script_and_no_grant() {
    let mut f = fake(&["SMILE_FO"]);
    f.login_missing = true;
    let out = run(&windows("."), &f);
    assert!(!out.ok);
    assert_eq!(out.error.as_ref().unwrap().number, Some(18456));
    let hint = out.hint.unwrap();
    assert!(
        hint.contains("CREATE LOGIN [NT AUTHORITY\\SYSTEM] FROM WINDOWS"),
        "{hint}"
    );
    assert!(hint.contains("USE [SMILE_POS]"));
    assert!(hint.contains("sp_addrolemember N'db_datareader'"));
    // The only calls were the listing; nothing that could grant anything.
    assert_eq!(f.seen.borrow().len(), 1);
}

#[test]
fn a_denied_database_fails_the_run_with_the_exact_error() {
    let mut f = fake(&["SMILE_FO", "SMILE_POS"]);
    f.deny = vec!["SMILE_POS"];
    let out = run(&windows("."), &f);
    assert!(!out.ok);
    let pos = &out.databases[1];
    assert!(
        pos.error
            .as_ref()
            .unwrap()
            .message
            .contains("Cannot open database \"SMILE_POS\"")
    );
    let hint = out.hint.unwrap();
    assert!(hint.contains("USE [SMILE_POS]") && !hint.contains("USE [SMILE_FO]"));
    // SQL logins get the error but no Windows grant script.
    let out = run(&sql_login("x"), &f);
    assert!(!out.ok && out.hint.is_none());
}

#[test]
fn no_smile_database_is_an_error() {
    let out = run(&windows("."), &fake(&[]));
    assert!(!out.ok);
    assert!(out.error.unwrap().message.contains("no SMILE database"));
}

#[test]
fn explicit_databases_skip_the_listing() {
    let f = fake(&[]);
    let mut req = windows(".");
    req.databases = vec!["SMILE_FO".into()];
    let out = run(&req, &f);
    assert!(out.ok);
    assert_eq!(f.seen.borrow().len(), 1);
}

#[test]
fn save_seals_the_password_keeps_limits_and_round_trips() {
    let d = TestDir::new("sql-setup-save");
    std::fs::write(
        d.0.join(SQL_FILE),
        r#"{"databases": {"SMILE_FO": {"connection": "Server=.;Database=SMILE_FO;Integrated Security=SSPI", "max_connections": 9, "transport": "unencrypted"}}}"#,
    )
    .unwrap();
    let req = sql_login("Pa;ss\"1");
    let mut f = fake(&["SMILE_FO", "SMILE_POS"]);
    f.rung = Rung::Tls10;
    let out = run(&req, &f);
    assert_eq!(out.databases[0].transport, Some(Rung::Tls10));
    save(&d.0, &req, &out).unwrap();
    let text = std::fs::read_to_string(d.0.join(SQL_FILE)).unwrap();
    assert!(!text.contains("Pa;ss"), "{text}");
    let doc = dl_json::parse(&text).unwrap();
    let fo = &doc["databases"]["SMILE_FO"];
    assert_eq!(fo["auth"].as_str(), Some("sql"));
    assert_eq!(fo["max_connections"].as_u64(), Some(9));
    // The rung setup proved replaces an older pin.
    assert_eq!(fo["transport"].as_str(), Some("tls1.0"));
    assert_eq!(
        doc["databases"]["SMILE_POS"]["transport"].as_str(),
        Some("tls1.0")
    );
    assert!(fo["password_sealed"].as_str().is_some());
    assert_eq!(
        doc["databases"]["SMILE_POS"]["max_connections"].as_u64(),
        Some(4)
    );
    let (full, mode) = secret::resolve(fo).unwrap();
    assert_eq!(mode, AuthMode::Sql);
    let c: dl_tds::Config = full.parse().unwrap();
    assert_eq!(c.password(), "Pa;ss\"1");
    // The pools load, and the installer default comes back without the password.
    let site = crate::sql::Site::load(&d.0);
    assert!(site.load_error.is_none(), "{:?}", site.load_error);
    let Saved {
        auth,
        server,
        user,
        databases: names,
    } = saved(&d.0).unwrap().unwrap();
    assert_eq!(
        (auth, server.as_str(), user.as_str()),
        (AuthMode::Sql, "localhost\\SQLEXPRESS", "smile")
    );
    assert_eq!(names, ["SMILE_FO", "SMILE_POS"]);
    assert_eq!(
        saved_password(&d.0, "SMILE").unwrap().as_deref(),
        Some("Pa;ss\"1")
    );
    assert_eq!(saved_password(&d.0, "other").unwrap(), None);
    // Switching to Windows drops the sealed password.
    let req = windows(".");
    let out = run(&req, &fake(&["SMILE_FO"]));
    save(&d.0, &req, &out).unwrap();
    let doc = dl_json::parse(&std::fs::read_to_string(d.0.join(SQL_FILE)).unwrap()).unwrap();
    assert!(
        doc["databases"]["SMILE_FO"]
            .get("password_sealed")
            .is_none()
    );
    assert_eq!(
        doc["databases"]["SMILE_FO"]["auth"].as_str(),
        Some("windows")
    );
    assert!(saved(&TestDir::new("sql-setup-empty").0).unwrap().is_none());
}

#[test]
fn failed_outcome_is_never_saved() {
    let d = TestDir::new("sql-setup-fail");
    let mut f = fake(&["SMILE_FO"]);
    f.login_missing = true;
    let req = windows(".");
    let out = run(&req, &f);
    assert!(save(&d.0, &req, &out).is_err());
    assert!(!d.0.join(SQL_FILE).exists());
}

#[test]
fn request_debug_hides_the_password() {
    let s = format!("{:?}", sql_login("hunter2"));
    assert!(!s.contains("hunter2"));
}

#[test]
fn outcome_survives_the_child_round_trip() {
    let mut f = fake(&["SMILE_FO", "SMILE_POS"]);
    f.deny = vec!["SMILE_POS"];
    f.rung = Rung::Tls10;
    let out = run(&windows("."), &f);
    assert_eq!(out.databases[0].transport, Some(Rung::Tls10));
    assert_eq!(out.databases[1].transport, None);
    assert_eq!(Outcome::from_json(&out.to_json()).unwrap(), out);
}

#[test]
fn an_old_remote_server_needs_the_explicit_opt_in_and_is_pinned_unencrypted() {
    let d = TestDir::new("sql-setup-old-remote");
    let mut req = sql_login("pw");
    req.server = "fo-server\\SQLEXPRESS".into();
    let mut f = fake(&["SMILE_FO"]);
    f.old_remote = true;
    let refused = run(&req, &f);
    assert!(!refused.ok);
    assert!(
        refused
            .error
            .as_ref()
            .is_some_and(|e| e.message.starts_with("sql_tls_unavailable:")),
        "{refused:?}"
    );
    assert!(save(&d.0, &req, &refused).is_err());
    req.allow_unencrypted = true;
    let accepted = run(&req, &f);
    assert!(accepted.ok, "{accepted:?}");
    save(&d.0, &req, &accepted).unwrap();
    let doc = dl_json::parse(&std::fs::read_to_string(d.0.join(SQL_FILE)).unwrap()).unwrap();
    assert_eq!(
        doc["databases"]["SMILE_FO"]["transport"].as_str(),
        Some("unencrypted")
    );
}

#[test]
fn the_source_of_a_chosen_login_is_saved_without_secrets_and_loads() {
    let d = TestDir::new("sql-setup-source");
    let req = sql_login("Hunter#2");
    let out = run(&req, &fake(&["SMILE_FO"]));
    save(&d.0, &req, &out).unwrap();
    let src = dl_json::json!({"kind": "legacy_agent", "product": "einvoice",
                              "location": "bench_old", "file": "C:/ProgramData/MM-EInvoice.bench-old-1/config.enc"});
    set_source(&d.0, Some(src)).unwrap();
    let site = crate::sql::Site::load(&d.0);
    assert!(site.load_error.is_none(), "{:?}", site.load_error);
    let s = site.source().unwrap();
    assert_eq!(s["product"].as_str(), Some("einvoice"));
    assert!(s["chosen_at"].as_u64().is_some());
    assert!(!dl_json::to_string(s).contains("Hunter"));
    set_source(&d.0, None).unwrap();
    let site = crate::sql::Site::load(&d.0);
    assert_eq!(site.source().unwrap()["kind"].as_str(), Some("operator"));
}
