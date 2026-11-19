//! Live check of the `ItemDef.IName1` lookup query. Needs a SQL Server sysadmin URL in
//! `MMEI_TEST_MSSQL_URL` (`sqlserver://sa:...@host:port`, no `database`); skipped otherwise.
//! Creates and drops its own database.

use std::time::Duration;

use dl_agent_einvoice::pos_names::{ITEM_NAMES_QUERY, collect_names};
use dl_tds::{Client, Config, Param};

const DB: &str = "mmei_posnames_live_rs";

fn connect(cfg: &Config) -> Client {
    let mut c = Client::connect(&cfg.clone().with_connect_timeout(Duration::from_secs(30)))
        .expect("connect");
    c.set_timeout(Some(Duration::from_secs(60))).unwrap();
    c
}

fn exec(c: &mut Client, sql: &str) {
    if let Err(e) = c.batch(sql) {
        panic!("{}: {e}", sql.trim().lines().next().unwrap_or(""));
    }
}

fn names(db: &mut Client, check: i64) -> std::collections::HashMap<String, String> {
    let rs = db
        .query(ITEM_NAMES_QUERY, &[Param::new("CheckNo", check)])
        .expect("lookup query");
    let mut rows = Vec::new();
    for set in rs.sets() {
        for r in set.rows() {
            rows.push((r.get::<String>(0).unwrap(), r.get::<String>(1).unwrap()));
        }
    }
    collect_names(rows)
}

#[test]
fn iname1_lookup_with_and_without_the_column() {
    let Some(base) = std::env::var("MMEI_TEST_MSSQL_URL")
        .ok()
        .filter(|u| !u.is_empty())
    else {
        eprintln!("note: skipping, set MMEI_TEST_MSSQL_URL to run against a real SQL Server");
        return;
    };
    let mut master = connect(&base.parse().expect("MMEI_TEST_MSSQL_URL"));
    let drop_db = format!(
        "IF DB_ID('{DB}') IS NOT NULL BEGIN ALTER DATABASE {DB} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {DB} END"
    );
    exec(&mut master, &drop_db);
    exec(&mut master, &format!("CREATE DATABASE {DB}"));
    let sep = if base.contains('?') { '&' } else { '?' };
    let mut db = connect(&format!("{base}{sep}database={DB}").parse().unwrap());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        exec(
            &mut db,
            "CREATE TABLE dbo.Trn (CheckNo numeric, TrnCode int, ItemCode varchar(20));
             CREATE TABLE dbo.ItemDef (ICode varchar(20));
             INSERT dbo.Trn VALUES (5, 100, 'A1 '), (5, 100, 'B2'), (5, 110, 'D'), (6, 100, 'A1');
             INSERT dbo.ItemDef VALUES ('A1');",
        );
        // No IName1 column: the batch must run and return nothing.
        assert!(
            names(&mut db, 5).is_empty(),
            "missing column keeps till names"
        );
        // The ALTER runs alone: SQL Server compiles a batch before running it, so
        // an INSERT in the same batch would still see the one-column table.
        exec(
            &mut db,
            "ALTER TABLE dbo.ItemDef ADD IName1 nvarchar(100) NULL;",
        );
        exec(
            &mut db,
            "INSERT dbo.ItemDef VALUES ('B2', N'');
             UPDATE dbo.ItemDef SET IName1 = N'Tôm' WHERE ICode = 'A1';",
        );
        let m = names(&mut db, 5);
        assert_eq!(m.get("A1").map(String::as_str), Some("Tôm"));
        assert!(!m.contains_key("B2"), "blank name keeps the till name");
        assert!(!m.contains_key("D"), "TrnCode 110 is not looked up");
    }));
    if let Err(e) = master.batch(&drop_db) {
        eprintln!("drop test database: {e}");
    }
    if let Err(p) = result {
        std::panic::resume_unwind(p);
    }
}
