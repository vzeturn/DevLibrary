//! Select calls (ADR 0007) against a real SQL Server: set `PMS_AGENT_TEST_SQL` (as for
//! `sql_integration.rs`). Without it the test returns early. The database is
//! `PMS_AGENT_TEST_SQL_SELECT_DB` (default `pms_agent_it_select`), created and dropped here.

use std::sync::Arc;

use dl_agent_pms::ctx::Ctx;
use dl_agent_pms::db::{self, Pool};
use dl_agent_pms::exec::{Executor, Limits, SqlExecutor};
use dl_agent_pms::json;
use dl_agent_pms::log::Logger;
use dl_agent_pms::ops::Manifest;
use dl_agent_pms::wire::{self, Frame, Op};
use dl_json::Value as J;

const MANIFEST: &str = r#"{"manifest_version":2,"allowlist_hash":"","procs":[],"excluded_proc_patterns":[],
 "tables":[{"name":"Room","columns":["RoomNo","RoomType","Status"]}],
 "ops":[{"name":"it.room.list","contract_ver":1,"kind":"read","target":"FO","target_db":"SMILE_FO",
  "timeout_ms":15000,"idempotent":true,"calls":[{"select":{"table":"Room","columns":["RoomNo","RoomType","Status"],
  "where":[{"column":"RoomType","op":"=","param":{"name":"roomType","type":"varchar","arg":"roomType","size":10}}],
  "order_by":["RoomNo"],"max_rows":2}}]}]}"#;

fn op(room_type: &str) -> Op {
    let v = json::parse(
        format!(
            r#"{{"type":"op","id":"6f1c2b7e-8d4a-4c3b-9e2f-1a2b3c4d5e6f","op":"it.room.list","contract_ver":1,"target_db":"SMILE_FO","args":{{"roomType":{}}},"request_key":null,"replay":false,"deadline_ms":15000}}"#,
            json::quote(room_type)
        )
        .as_bytes(),
    )
    .unwrap();
    match wire::decode_json(json::to_string(&v).as_bytes()).unwrap() {
        Frame::Op(op) => op,
        other => panic!("{other:?}"),
    }
}

fn batch(cfg: &dl_tds::Config, sql: &str) {
    let mut c = dl_tds::Client::connect(cfg).expect("connect");
    c.batch(sql).unwrap_or_else(|e| panic!("{e}\n{sql}"));
    c.close();
}

#[test]
fn select_reads_only_bound_rows_from_a_declared_table() {
    let Ok(url) = std::env::var("PMS_AGENT_TEST_SQL") else {
        return;
    };
    let cfg: dl_tds::Config = url.parse().expect("PMS_AGENT_TEST_SQL is a valid URL");
    let name = std::env::var("PMS_AGENT_TEST_SQL_SELECT_DB")
        .unwrap_or_else(|_| "pms_agent_it_select".to_owned());
    assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    let drop_db = format!(
        "IF DB_ID('{name}') IS NOT NULL BEGIN ALTER DATABASE [{name}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{name}]; END;"
    );
    batch(&cfg, &format!("{drop_db} CREATE DATABASE [{name}];"));
    let db_cfg = cfg.clone().with_database(&name);
    batch(
        &db_cfg,
        "CREATE TABLE dbo.Room (RoomNo varchar(10) NOT NULL, RoomType varchar(10) NOT NULL, Status varchar(5) NOT NULL, Secret varchar(20) NULL);
         INSERT dbo.Room VALUES ('101','EO-D','VC','x'),('102','EO-D','OC','x'),('103','EO-D','VD','x'),('201','S-TPL','VC','x');",
    );

    let mut dbs = db::Pools::new();
    dbs.insert(
        db::DB_FO.to_owned(),
        Arc::new(Pool::new(db::DB_FO, db_cfg, 2)),
    );
    let e = SqlExecutor {
        manifest: Arc::new(Manifest::parse(MANIFEST.as_bytes()).unwrap()),
        dbs,
        limits: Limits::default(),
        log: Logger::discard(),
    };
    let ctx = Ctx::background();

    // A room type longer than two characters (the vendor proc's SQL 8152 case); TOP (2).
    let r = e.execute(&ctx, &op("EO-D"));
    assert_eq!(r.status, "applied", "{r:?}");
    let rows = &r.sets[0];
    let nos: Vec<_> = rows.iter().map(|row| row.get("RoomNo").cloned()).collect();
    assert_eq!(
        nos,
        vec![Some(J::String("101".into())), Some(J::String("102".into()))]
    );
    assert!(rows.iter().all(|row| row.get("Secret").is_none()));

    // An injection-shaped value is only a bound value: no rows, table intact.
    let r = e.execute(&ctx, &op("' OR 1=1--"));
    assert_eq!(r.status, "applied", "{r:?}");
    assert!(r.sets[0].is_empty());
    // Past the declared size the bind refuses before any SQL runs.
    let r = e.execute(&ctx, &op("x'; DROP TABLE dbo.Room;--"));
    assert_eq!(r.status, "rejected", "{r:?}");
    let r = e.execute(&ctx, &op("S-TPL"));
    assert_eq!(r.sets[0].len(), 1);

    batch(&cfg, &drop_db);
}
