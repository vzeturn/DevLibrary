//! Live check against a real SQL Server (`SA_TEST_MSSQL_URL`, sysadmin, no database);
//! skipped when unset. Creates and drops its own database.

use dl_tds::{Client, Config};

const DB: &str = "sa_shared_live_rs";
const FIXTURE: &str = include_str!("../sql/test_fixture.sql");

#[test]
fn resolve_handles_blank_padded_duplicate_and_mistyped_subs() {
    let Some(url) = std::env::var("SA_TEST_MSSQL_URL")
        .ok()
        .filter(|u| !u.is_empty())
    else {
        eprintln!("note: skipping, set SA_TEST_MSSQL_URL to run against a real SQL Server");
        return;
    };
    let cfg: Config = url.parse().expect("SA_TEST_MSSQL_URL");
    let mut c = Client::connect(&cfg).expect("connect");
    c.batch(&format!("IF DB_ID('{DB}') IS NOT NULL BEGIN ALTER DATABASE {DB} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {DB} END; CREATE DATABASE {DB}"))
        .expect("create db");
    c.batch(&format!("USE {DB}")).expect("use");
    c.batch(FIXTURE).expect("fixture");
    dl_agent_smile_shared::install_into(&mut c).expect("install");
    dl_agent_smile_shared::install_into(&mut c).expect("install is idempotent");
    // (code, sub, fallback_sub0, fallback_code) -> expected Description (None = no row)
    let cases: &[(&str, &str, u8, u8, Option<&str>)] = &[
        ("110", "1", 0, 0, Some("Room Plus")),
        ("110", "", 0, 0, Some("Room")),
        ("110", "00", 0, 0, Some("Room")),
        ("120", "0", 0, 0, Some("Food")),
        ("150", "7", 0, 0, Some("Padded")),
        ("160", "2", 0, 0, Some("Dup B")),
        ("170", "0", 0, 0, Some("Blank sub")),
        ("180", "0", 0, 0, None),
        ("180", "0", 1, 0, None),
        ("180", "0", 0, 1, Some("Only sub1")),
        ("180", "5", 1, 1, Some("Only sub1")),
        ("190", " ab ", 0, 0, Some("Text sub")),
        ("110", "2", 1, 0, Some("Room")),
        ("999", "0", 1, 1, None),
    ];
    for (code, sub, f0, fc, want) in cases {
        let sql = format!(
            "SELECT Description FROM dbo.SA_TrnCode_Resolve('{code}', '{sub}', {f0}, {fc})"
        );
        let rs = c.query(&sql, &[]).expect("query");
        let got: Option<String> = rs.rows().first().map(|r| r.get::<String>(0).expect("col"));
        assert_eq!(got.as_deref(), *want, "case {code}/{sub}/{f0}/{fc}");
    }
    c.batch("USE master").ok();
    c.batch(&format!(
        "ALTER DATABASE {DB} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {DB}"
    ))
    .ok();
}
