//! Select calls through the SQL executor (ADR 0007): binds only, rows through the same
//! set builder as a procedure call, against the scripted TDS server.

use super::*;
use dl_tds::fake::{FakeServer, Script};

const MANIFEST: &str = r#"{"manifest_version":2,"allowlist_hash":"x","procs":["P"],
 "tables":[{"name":"Room","columns":["RoomNo","RoomType","Status"]}],
 "excluded_proc_patterns":[],
 "ops":[{"name":"fo.room.list","contract_ver":1,"kind":"read","target":"FO","target_db":"SMILE_FO",
  "timeout_ms":5000,"idempotent":true,"calls":[{"select":{"table":"Room","columns":["RoomNo","Status"],
  "where":[{"column":"RoomType","op":"=","param":{"name":"roomType","type":"varchar","arg":"roomType","size":40,"nullable":false}}],
  "order_by":["RoomNo"],"max_rows":2000}}]}]}"#;

fn manifest() -> Manifest {
    Manifest::parse(MANIFEST.as_bytes()).unwrap()
}

fn args(s: &str) -> J {
    json::parse(s.as_bytes()).unwrap()
}

#[test]
fn arg_values_are_bound_never_spliced() {
    let m = manifest();
    let op = m.lookup("fo.room.list").unwrap();
    let evil = "DLX'; DROP TABLE Room; --";
    let calls = bind_args(
        op,
        &args(&format!(r#"{{"roomType":{}}}"#, json::quote(evil))),
    )
    .unwrap();
    assert_eq!(calls.len(), 1);
    let sql = calls[0].sql.as_deref().unwrap();
    assert_eq!(
        sql,
        "SELECT TOP (2000) [RoomNo], [Status] FROM [dbo].[Room] WHERE [RoomType] = @roomType ORDER BY [RoomNo]"
    );
    assert!(!sql.contains("DLX") && !sql.contains("DROP"), "{sql}");
    assert_eq!(calls[0].params.len(), 1);
    assert_eq!(calls[0].params[0].name(), "roomType");
    assert!(calls[0].proc_name.is_empty() && calls[0].outputs.is_empty());
    // The bind rules of a proc param apply: required, typed, size, no extra args; an error
    // names the arg and never its value.
    for (bad, want) in [
        ("{}", "invalid_args: roomType: required"),
        (
            r#"{"roomType":5}"#,
            "invalid_args: roomType: varchar expects a string",
        ),
        (
            r#"{"roomType":"SECRET-0123456789-0123456789-0123456789-0123"}"#,
            "invalid_args: roomType: varchar longer than 40 characters",
        ),
        (
            r#"{"roomType":"DLX","x":"SECRET"}"#,
            "invalid_args: x: not an argument of this op",
        ),
    ] {
        assert_eq!(bind_args(op, &args(bad)).err().unwrap(), want);
    }
}

/// The token stream of `SELECT RoomNo (nvarchar), Status (int)` with two rows.
fn two_rows() -> Vec<u8> {
    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }
    let mut t = vec![0x81, 2, 0]; // COLMETADATA, 2 columns
    // usertype, flags (nullable), NVARCHAR(20) with a Latin1 collation, name.
    t.extend_from_slice(&[0, 0, 0, 0, 1, 0, 0xE7, 40, 0, 9, 4, 0xD0, 0, 52, 6]);
    t.extend_from_slice(&utf16("RoomNo"));
    t.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0x38, 6]); // INT4
    t.extend_from_slice(&utf16("Status"));
    for (room, status) in [("101", 1u32), ("102", 2)] {
        t.push(0xD1); // ROW
        t.extend_from_slice(&((room.len() * 2) as u16).to_le_bytes());
        t.extend_from_slice(&utf16(room));
        t.extend_from_slice(&status.to_le_bytes());
    }
    t.extend_from_slice(&[0xFD, 0x10, 0, 0xC1, 0]); // DONE with a count
    t.extend_from_slice(&2u64.to_le_bytes());
    t
}

fn executor(srv: &FakeServer) -> SqlExecutor {
    let port = srv.addr().port();
    let cfg: dl_tds::Config =
        format!("Server=127.0.0.1,{port};Encrypt=false;Connect Timeout=5;User ID=u;Password=p")
            .parse()
            .unwrap();
    let mut dbs = db::Pools::new();
    dbs.insert("SMILE_FO".into(), Arc::new(Pool::new("SMILE_FO", cfg, 1)));
    SqlExecutor {
        manifest: Arc::new(manifest()),
        dbs,
        limits: Limits::default(),
        log: Logger::discard(),
    }
}

fn op_frame(args_json: &str) -> Op {
    Op {
        id: "0b7f6c1e-1d2a-4c3b-9e2f-1a2b3c4d5e6f".into(),
        op: "fo.room.list".into(),
        contract_ver: 1,
        target_db: "SMILE_FO".into(),
        args: args(args_json),
        request_key: None,
        expected_stamp: None,
        deadline_ms: 5000,
        replay: None,
    }
}

#[test]
fn a_select_op_returns_rows() {
    let srv = FakeServer::start(Script {
        never_encrypt: true,
        // BEGIN TRANSACTION, then the select; the rollback that ends the read gets DONE.
        responses: vec![Vec::new(), two_rows()],
        ..Script::default()
    })
    .unwrap();
    let ex = executor(&srv);
    let res = ex.execute(&Ctx::background(), &op_frame(r#"{"roomType":"DLX"}"#));
    assert_eq!(res.status, wire::STATUS_APPLIED, "{:?}", res.error);
    assert_eq!(res.rowcounts, [2]);
    assert_eq!(res.sets.len(), 1);
    let rows = &res.sets[0];
    assert_eq!(rows[0].get("RoomNo"), Some(&J::String("101".into())));
    assert_eq!(rows[1].get("RoomNo"), Some(&J::String("102".into())));
    assert_eq!(json::to_string(rows[1].get("Status").unwrap()), "2");
    assert!(res.outputs.is_empty(), "a select has no ReturnValue");
    let a = &srv.attempts()[0];
    assert!(a.logged_in);
    assert_eq!(a.requests, 3, "begin, the select, rollback");
    // A bad arg is refused before any connection is made.
    let res = ex.execute(&Ctx::background(), &op_frame(r#"{"roomType":7}"#));
    assert_eq!(res.status, wire::STATUS_REJECTED);
    assert_eq!(srv.attempts().len(), 1);
}

#[test]
fn the_embedded_manifest_still_loads() {
    let m = Manifest::load().unwrap();
    assert!(m.tables.is_empty(), "SMILE-PMS has not declared tables yet");
    assert!(!m.implemented_ops().is_empty());
}
