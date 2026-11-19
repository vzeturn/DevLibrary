//! Port of Go `config_test.go` (the template, UI and update cases are dropped with them)
//! plus the enrollment writer.

use super::*;

fn temp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("dl-hl-cfg-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join("config.json")
}

#[test]
fn load_config_seed_once_encrypts() {
    let path = temp("seed");
    let plain = r#"{
        "hubUrl": "https://hub.smile.vn",
        "hotelId": "h1",
        "token": "tok-plaintext",
        "ui": {"enabled": false, "port": 9000},
        "sql": {"server": "localhost", "database": "SMILE_FO", "user": "sa", "password": "pass-plaintext"}
    }"#;
    std::fs::write(&path, plain).expect("write");
    let cfg = load(&path).expect("load").expect("present");
    assert_eq!(cfg.token, "tok-plaintext");
    assert_eq!(cfg.sql.password, "pass-plaintext");

    let raw = std::fs::read_to_string(&path).expect("read");
    let disk = dl_json::parse(&raw).expect("json");
    let tok = disk.get("token").and_then(Value::as_str).expect("token");
    let pw = disk
        .pointer("/sql/password")
        .and_then(Value::as_str)
        .expect("password");
    assert!(
        tok.starts_with(ENC_PREFIX) && pw.starts_with(ENC_PREFIX),
        "{raw}"
    );
    assert!(!raw.contains("plaintext"), "no plaintext on disk: {raw}");
    assert_eq!(
        disk.pointer("/ui/port").and_then(Value::as_i64),
        Some(9000),
        "unknown fields survive the rewrite"
    );

    let again = load(&path).expect("reload").expect("present");
    assert_eq!(again, cfg);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        raw,
        "no rewrite"
    );
}

#[test]
fn load_config_strips_utf8_bom() {
    let path = temp("bom");
    let mut raw = vec![0xEF, 0xBB, 0xBF];
    raw.extend_from_slice(
        br#"{"hubUrl": "https://hub.smile.vn", "hotelId": "h1", "token": "tok-plaintext", "sql": {"server": ".", "database": "SMILE_FO", "trustedConnection": true}}"#,
    );
    std::fs::write(&path, raw).expect("write");
    let cfg = load(&path).expect("load").expect("present");
    assert_eq!(cfg.hotel_id, "h1");
    assert_eq!(cfg.token, "tok-plaintext");
    assert!(cfg.sql.trusted_connection);
}

#[test]
fn missing_config_is_none_and_not_created() {
    let path = temp("missing");
    assert_eq!(load(&path).expect("load"), None);
    assert!(!path.exists(), "the module never writes a template");
}

#[test]
fn broken_config_is_an_error() {
    let path = temp("broken");
    std::fs::write(&path, "{not json").expect("write");
    assert!(load(&path).is_err());
    std::fs::write(&path, r#"{"token": "enc:%%%"}"#).expect("write");
    let e = load(&path).expect_err("bad base64");
    assert!(e.contains("token"), "{e}");
}

#[test]
fn validate() {
    let good = LocalConfig {
        hub_url: "https://hub.smile.vn".into(),
        hotel_id: "h1".into(),
        token: "t".into(),
        sql: SqlConfig {
            server: "localhost".into(),
            database: "SMILE_FO".into(),
            user: "sa".into(),
            ..SqlConfig::default()
        },
        ..LocalConfig::default()
    };
    assert_eq!(good.problem(), None);
    let mut bad = good.clone();
    bad.token.clear();
    assert!(bad.problem().is_some(), "no token");
    let mut example = good.clone();
    example.hub_url = "https://hub.example.com".into();
    assert!(example.problem().is_some(), "template hub URL");
    let mut trusted = good.clone();
    trusted.sql.user.clear();
    trusted.sql.trusted_connection = true;
    assert_eq!(trusted.problem(), None, "trusted connection needs no user");
    let mut over = good;
    over.sql = SqlConfig {
        connection_string_override: "server=x".into(),
        ..SqlConfig::default()
    };
    assert_eq!(over.problem(), None);
}

#[test]
fn conn_string() {
    let mut s = SqlConfig {
        server: "srv".into(),
        database: "SMILE_FO".into(),
        user: "sa".into(),
        password: "pw".into(),
        ..SqlConfig::default()
    };
    let got = s.conn_string();
    for want in [
        "server=srv",
        "database=SMILE_FO",
        "user id=sa",
        "password=pw",
        "encrypt=disable",
        "TrustServerCertificate=true",
    ] {
        assert!(got.contains(want), "{want} missing in {got}");
    }
    dl_agent::sql::TdsConnector::parse(&got).expect("dl-tds reads the Go string");
    s.trusted_connection = true;
    let got = s.conn_string();
    assert!(got.contains("trusted_connection=yes") && !got.contains("user id="));
    s.connection_string_override = "server=x;database=y".into();
    assert_eq!(s.conn_string(), "server=x;database=y");
}

#[test]
fn debug_hides_secrets() {
    let cfg = LocalConfig {
        token: "SECRET-TOKEN".into(),
        sql: SqlConfig {
            password: "SECRET-PW".into(),
            connection_string_override: "SECRET-CS".into(),
            ..SqlConfig::default()
        },
        ..LocalConfig::default()
    };
    let d = format!("{cfg:?}");
    assert!(!d.contains("SECRET"), "{d}");
}

#[test]
fn enrollment_writes_install_script_shape() {
    let path = temp("enroll");
    std::fs::write(
        &path,
        r#"{"hubUrl":"https://old","configToken":"keep","sql":{"server":"x"}}"#,
    )
    .expect("write");
    let e = Enrollment {
        hub_url: "https://hub.smile.vn/".into(),
        hotel_id: "h1".into(),
        token: "tok".into(),
        instance_id: "inst-1".into(),
        sql_user: "smile".into(),
        sql_password: "pw".into(),
    };
    write_enrollment(&path, &e).expect("enroll");
    let raw = std::fs::read_to_string(&path).expect("read");
    assert!(!raw.contains("\"tok\"") && !raw.contains("\"pw\""), "{raw}");
    let disk = dl_json::parse(&raw).expect("json");
    assert_eq!(
        disk.get("configToken").and_then(Value::as_str),
        Some("keep")
    );
    let cfg = load(&path).expect("load").expect("present");
    assert_eq!(cfg.hub_url, "https://hub.smile.vn");
    assert_eq!(cfg.token, "tok");
    assert_eq!(cfg.instance_id, "inst-1");
    assert_eq!(cfg.sql.server, ".");
    assert_eq!(cfg.sql.database, "SMILE_FO");
    assert!(!cfg.sql.trusted_connection);
    assert_eq!(
        (cfg.sql.user.as_str(), cfg.sql.password.as_str()),
        ("smile", "pw")
    );
    assert_eq!(cfg.problem(), None);

    let windows_auth = Enrollment {
        sql_user: String::new(),
        ..e
    };
    write_enrollment(&path, &windows_auth).expect("enroll");
    let cfg = load(&path).expect("load").expect("present");
    assert!(cfg.sql.trusted_connection);
    assert!(cfg.sql.password.is_empty());
}
