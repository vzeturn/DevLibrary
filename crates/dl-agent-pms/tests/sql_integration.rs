//! Against a real SQL Server (2017 and 2022 are the supported floor and ceiling): set
//! `PMS_AGENT_TEST_SQL` to a connection URL with rights to create a database, e.g.
//! `sqlserver://sa:<pw>@127.0.0.1:14333?encrypt=disable`. Without it every test returns
//! early. The test databases are `pms_agent_it_*`, never a real SMILE_* name.
//!
//! - the golden op/result pairs of testdata/ops, served by procedures generated from each
//!   fixture's `db.calls` (binds are checked inside the procedure);
//! - `fo.hotelDate.get` against a SMILE_FO fixture whose `HotelParameter` table and
//!   `GetHotelDate1` procedure are the vendor's (`Select HotelDate From HotelParameter`);
//! - the op journal in front of the SQL executor: a keyed write runs once, its replay is
//!   answered from the journal, an unknown key is refused; SQL errors, timeouts, the
//!   result caps and output parameters.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use dl_agent_pms::ctx::Ctx;
use dl_agent_pms::db::{self, Pool};
use dl_agent_pms::exec::{self, Executor, Limits, SqlExecutor};
use dl_agent_pms::journal::exec::Journaled;
use dl_agent_pms::journal::{self, Journal};
use dl_agent_pms::json;
use dl_agent_pms::log::Logger;
use dl_agent_pms::ops::Manifest;
use dl_agent_pms::wire::{self, Frame, Op};
use dl_json::Value;

/// The SMILE_FO stand-in.
const DB_FO: &str = "pms_agent_it_fo";
/// The SMILE_POS stand-in (empty: only routing is checked against it).
const DB_POS: &str = "pms_agent_it_pos";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn server() -> Option<dl_tds::Config> {
    let url = std::env::var("PMS_AGENT_TEST_SQL").ok()?;
    Some(
        url.parse()
            .expect("PMS_AGENT_TEST_SQL is a valid connection URL"),
    )
}

/// The SMILE_FO and SMILE_POS stand-ins, created once (empty).
fn pools() -> Option<db::Pools> {
    static POOLS: OnceLock<Option<db::Pools>> = OnceLock::new();
    POOLS
        .get_or_init(|| {
            let cfg = server()?;
            let mut master = dl_tds::Client::connect(&cfg).expect("connect master");
            for name in [DB_FO, DB_POS] {
                master
                    .batch(&format!(
                        "IF DB_ID('{name}') IS NOT NULL BEGIN ALTER DATABASE {name} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {name}; END; CREATE DATABASE {name};"
                    ))
                    .expect("create database");
            }
            master.close();
            let mut dbs = db::Pools::new();
            for (key, name) in [(db::DB_FO, DB_FO), (db::DB_POS, DB_POS)] {
                dbs.insert(
                    key.to_owned(),
                    Arc::new(Pool::new(key, cfg.clone().with_database(name), 4)),
                );
            }
            Some(dbs)
        })
        .clone()
}

fn fo(dbs: &db::Pools) -> Arc<Pool> {
    dbs[db::DB_FO].clone()
}

/// The fixtures share procedure names: one test at a time redefines them.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static M: Mutex<()> = Mutex::new(());
    M.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn exec_sql(p: &Pool, sql: &str) {
    let ctx = Ctx::background();
    p.get(&ctx)
        .unwrap()
        .batch(&ctx, sql)
        .unwrap_or_else(|e| panic!("{e}\n{sql}"));
}

fn scalar(p: &Pool, sql: &str) -> i64 {
    let ctx = Ctx::background();
    let r = p.get(&ctx).unwrap().batch(&ctx, sql).unwrap();
    r.rows()[0].get::<i64>(0).unwrap()
}

fn lit(s: &str) -> String {
    format!("N'{}'", s.replace('\'', "''"))
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => json::to_string(other),
    }
}

/// A typed SQL literal for one fixture cell.
fn cell_sql(typ: &str, v: &Value) -> String {
    let sql_type = match typ {
        "BIGINT" => "bigint".to_owned(),
        "INT" => "int".to_owned(),
        "SMALLINT" => "smallint".to_owned(),
        "TINYINT" => "tinyint".to_owned(),
        "BIT" => "bit".to_owned(),
        "VARCHAR" => "varchar(400)".to_owned(),
        "NVARCHAR" => "nvarchar(400)".to_owned(),
        "DATETIME" => "datetime".to_owned(),
        "SMALLDATETIME" => "smalldatetime".to_owned(),
        "DATE" => "date".to_owned(),
        "DATETIME2" => "datetime2(7)".to_owned(),
        "DATETIMEOFFSET" => "datetimeoffset(7)".to_owned(),
        "FLOAT" => "float".to_owned(),
        "REAL" => "real".to_owned(),
        "MONEY" => "money".to_owned(),
        "SMALLMONEY" => "smallmoney".to_owned(),
        "UNIQUEIDENTIFIER" => "uniqueidentifier".to_owned(),
        "CHAR" => "char(10)".to_owned(),
        "NCHAR" => "nchar(10)".to_owned(),
        // rowversion cannot be cast to; binary(8) encodes the same (base64).
        "TIMESTAMP" | "BINARY" | "VARBINARY" | "IMAGE" => "varbinary(400)".to_owned(),
        "NUMERIC" | "DECIMAL" => {
            let t = text_of(v);
            let scale = t.split_once('.').map_or(0, |(_, f)| f.len());
            format!("numeric(38, {scale})")
        }
        other => panic!("fixture type {other} not handled"),
    };
    let value = match v {
        Value::Null => "NULL".to_owned(),
        Value::Bool(b) => u8::from(*b).to_string(),
        Value::Number(_) => json::to_string(v),
        // The fixture writes UTC instants; types without an offset take the wall clock.
        Value::String(s) if matches!(typ, "DATETIME" | "SMALLDATETIME" | "DATE" | "DATETIME2") => {
            lit(s.trim_end_matches('Z'))
        }
        Value::String(s) if matches!(typ, "TIMESTAMP" | "BINARY" | "VARBINARY" | "IMAGE") => {
            let b = dl_encoding::base64::STANDARD.decode(s).unwrap();
            format!("0x{}", dl_encoding::hex::encode(b))
        }
        Value::String(s) => lit(s),
        other => panic!("cell {other:?}"),
    };
    format!("CAST({value} AS {sql_type})")
}

/// `CREATE OR ALTER PROCEDURE` serving one fixture call, checking its binds.
/// The batches that make a fixture call happen on the server: setup, then the proc.
fn proc_sql(manifest: &Manifest, op: &str, call: &Value) -> Vec<String> {
    let name = call["proc"].as_str().unwrap();
    let spec = manifest
        .lookup(op)
        .unwrap()
        .calls
        .iter()
        .find(|c| c.proc_name == name)
        .unwrap();
    let params: Vec<String> = spec
        .params
        .iter()
        .map(|p| format!("@{} {}", p.name, p.typ))
        .collect();
    let mut batches = Vec::new();
    let mut body = String::from("SET NOCOUNT ON;\n");
    if let Some(Value::Object(binds)) = call.get("binds").filter(|b| !is_empty_object(b)) {
        let conds: Vec<String> = binds
            .iter()
            .map(|(k, v)| match v {
                Value::Null => format!("@{k} IS NULL"),
                Value::String(s) => format!("@{k} = {}", lit(s)),
                other => format!("@{k} = {}", json::to_string(other)),
            })
            .collect();
        body.push_str(&format!(
            "IF NOT ({}) THROW 50999, 'binds differ', 1;\n",
            conds.join(" AND ")
        ));
    }
    if let Some(e) = call.get("error").filter(|e| !e.is_null()) {
        let (setup, raise) = raise_sql(e);
        batches.extend(setup);
        body.push_str(&raise);
    }
    for set in call
        .get("sets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let cols = set["columns"].as_array().unwrap();
        let rows = set["rows"].as_array().unwrap();
        let select = |row: Option<&Vec<Value>>| {
            let items: Vec<String> = cols
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let v = row.map_or(Value::Null, |r| r[i].clone());
                    format!(
                        "{} AS [{}]",
                        cell_sql(c["type"].as_str().unwrap(), &v),
                        c["name"].as_str().unwrap()
                    )
                })
                .collect();
            items.join(", ")
        };
        if rows.is_empty() {
            body.push_str(&format!("SELECT TOP 0 {};\n", select(None)));
        } else {
            let parts: Vec<String> = rows
                .iter()
                .map(|r| format!("SELECT {}", select(r.as_array())))
                .collect();
            body.push_str(&format!("{};\n", parts.join("\nUNION ALL ")));
        }
    }
    let rs = call
        .get("return_status")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    body.push_str(&format!("RETURN {rs};\n"));
    batches.push(format!(
        "CREATE OR ALTER PROCEDURE dbo.{name} {}\nAS\nBEGIN\n{body}END",
        params.join(", ")
    ));
    batches
}

fn is_empty_object(v: &Value) -> bool {
    matches!(v, Value::Object(m) if m.is_empty())
}

/// The statement that raises a fixture's SQL error. THROW only takes numbers
/// from 50000 up, so a system error is raised by doing what causes it.
fn raise_sql(e: &Value) -> (Option<String>, String) {
    let number = e["number"].as_i64().unwrap();
    let message = e["message"].as_str().unwrap();
    if number >= 50000 {
        return (None, format!("THROW {number}, {}, 1;\n", lit(message)));
    }
    match number {
        208 => {
            let name = message
                .strip_prefix("Invalid object name '")
                .and_then(|m| m.strip_suffix("'."))
                .unwrap_or_else(|| panic!("error 208 message not understood: {message}"));
            (
                Some(format!("DROP TABLE IF EXISTS dbo.[{name}];")),
                format!("SELECT * FROM [{name}];\n"),
            )
        }
        other => panic!("fixture SQL error {other}: add the statement that raises it"),
    }
}

/// Object keys sorted recursively, numbers by value: JSON equality like Go's DeepEqual.
fn semantic(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut items: Vec<(String, String)> =
                m.iter().map(|(k, v)| (k.clone(), semantic(v))).collect();
            items.sort();
            let parts: Vec<String> = items
                .into_iter()
                .map(|(k, v)| format!("{k:?}:{v}"))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(semantic).collect::<Vec<_>>().join(",")),
        Value::Number(n) => format!("{}", n.as_f64().unwrap_or(f64::NAN)),
        other => json::to_string(other),
    }
}

fn op_of(v: &Value) -> Op {
    match wire::decode_json(json::to_string(v).as_bytes()).unwrap() {
        Frame::Op(op) => op,
        other => panic!("{other:?}"),
    }
}

fn executor(dbs: &db::Pools, manifest: Manifest, limits: Limits) -> SqlExecutor {
    SqlExecutor {
        manifest: Arc::new(manifest),
        dbs: dbs.clone(),
        limits,
        log: Logger::discard(),
    }
}

/// Runs `op` and checks its result frame validates.
fn run(e: &dyn Executor, op: &Op) -> wire::OpResult {
    let res = e.execute(&Ctx::background(), op);
    wire::encode(&Frame::Result(res.clone()))
        .unwrap_or_else(|err| panic!("{}: frame does not validate: {err}", op.op));
    res
}

#[test]
fn golden_op_fixtures_match_the_cloud_contract() {
    let Some(dbs) = pools() else { return };
    let _g = serial();
    let p = fo(&dbs);
    let manifest = Manifest::load().unwrap();
    let dir = root().join("ops");
    for name in ["fo-hotel-date-get.json", "fo-hotel-date-get-sql-error.json"] {
        let fx = json::parse(&std::fs::read(dir.join(name)).unwrap()).unwrap();
        let op = op_of(&fx["op"]);
        let calls = fx["db"]["calls"].as_array().unwrap();
        let spec = manifest.lookup(&op.op).unwrap();
        for c in &spec.calls {
            let call = calls
                .iter()
                .find(|x| x["proc"].as_str() == Some(c.proc_name.as_str()))
                .unwrap_or_else(|| panic!("{name}: no fixture call for {}", c.proc_name));
            for sql in proc_sql(&manifest, &op.op, call) {
                exec_sql(&p, &sql);
            }
        }
        let e = executor(&dbs, manifest.clone(), Limits::default());
        let frame = Frame::Result(run(&e, &op));
        let got = json::parse(wire::to_json(&frame).as_bytes()).unwrap();
        assert_eq!(
            semantic(&got),
            semantic(&fx["result"]),
            "{name}\n got: {}",
            wire::to_json(&frame)
        );
    }
}

/// The vendor's objects on SMILE_FO, as SMILE_FO.exe installs them.
const VENDOR_HOTEL_PARAMETER: &str = "IF OBJECT_ID('dbo.HotelParameter') IS NOT NULL DROP TABLE dbo.HotelParameter; CREATE TABLE dbo.HotelParameter (HotelID varchar(30) NULL, HotelDate datetime NULL, AuditorDate datetime NULL, ReportDate datetime NULL, TodayPassword varchar(10) NULL);";
const VENDOR_GET_HOTEL_DATE1: &str =
    "CREATE OR ALTER PROCEDURE dbo.GetHotelDate1 AS Select HotelDate From HotelParameter";

fn hotel_date_op(target_db: &str) -> Op {
    let v = json::parse(
        format!(
            r#"{{"type":"op","id":"3e4f5a6b-7c8d-4e9f-8a0b-1c2d3e4f5a62","op":"fo.hotelDate.get","contract_ver":1,"target_db":"{target_db}","args":{{}},"request_key":null,"deadline_ms":10000}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    op_of(&v)
}

/// What the cloud makes of the result: the date part of the first row's HotelDate.
fn hotel_date(r: &wire::OpResult) -> Option<String> {
    let v = r.sets.first()?.first()?.get("HotelDate")?.as_str()?;
    Some(v.get(..10)?.to_owned())
}

#[test]
fn hotel_date_comes_from_the_vendor_proc_without_a_timezone_shift() {
    let Some(dbs) = pools() else { return };
    let _g = serial();
    let p = fo(&dbs);
    exec_sql(&p, VENDOR_HOTEL_PARAMETER);
    exec_sql(&p, VENDOR_GET_HOTEL_DATE1);
    let e = executor(&dbs, Manifest::load().unwrap(), Limits::default());
    let op = hotel_date_op(db::DB_FO);

    // No row and a NULL HotelDate are failures, never a null date.
    for setup in [
        "DELETE dbo.HotelParameter",
        "DELETE dbo.HotelParameter; INSERT dbo.HotelParameter (HotelID, HotelDate) VALUES ('H1', NULL)",
    ] {
        exec_sql(&p, setup);
        let r = run(&e, &op);
        let err = r.error.as_ref().expect("an error");
        assert_eq!(
            (r.status.as_str(), err.code.as_str(), err.message.as_str()),
            (
                "failed",
                exec::CODE_NO_DATA,
                "GetHotelDate1 returned no HotelDate"
            ),
            "{setup}"
        );
    }

    for (stored, wire_value, date) in [
        (
            "2026-11-19T00:00:00",
            "2026-11-19T00:00:00.000Z",
            "2026-11-19",
        ),
        // Late evening stays on its own day: the stored wall clock is taken as it is.
        (
            "2026-11-19T23:30:00",
            "2026-11-19T23:30:00.000Z",
            "2026-11-19",
        ),
        (
            "2024-02-29T07:15:00",
            "2024-02-29T07:15:00.000Z",
            "2024-02-29",
        ),
    ] {
        exec_sql(
            &p,
            &format!(
                "DELETE dbo.HotelParameter; INSERT dbo.HotelParameter (HotelID, HotelDate, AuditorDate) VALUES ('H1', '{stored}', '2000-01-01')"
            ),
        );
        let r = run(&e, &op);
        assert_eq!(r.status, wire::STATUS_APPLIED, "{r:?}");
        assert_eq!(r.sets.len(), 1);
        assert_eq!(r.sets[0][0]["HotelDate"].as_str(), Some(wire_value));
        assert_eq!(r.sets[0][0].len(), 1, "only the vendor's one column");
        assert_eq!(hotel_date(&r).as_deref(), Some(date));
        assert_eq!(r.outputs["ReturnValue"].as_i64(), Some(0));
    }

    // The op names SMILE_FO; any other target is refused before a connection is used.
    for wrong in [db::DB_POS, db::DB_BO] {
        let r = run(&e, &hotel_date_op(wrong));
        let err = r.error.expect("an error");
        assert_eq!(
            (r.status.as_str(), err.message.as_str()),
            ("rejected", "target_db_mismatch"),
            "{wrong}"
        );
    }
}

fn it_manifest() -> Manifest {
    let op = |name: &str, kind: &str, proc_name: &str, params: &str, timeout: i64| {
        format!(
            r#"{{"name":"{name}","contract_ver":1,"kind":"{kind}","target":"FO","target_db":"SMILE_FO","timeout_ms":{timeout},"idempotent":false,"calls":[{{"proc":"{proc_name}","params":{params}}}]}}"#
        )
    };
    let v = r#"[{"name":"V","type":"int","arg":"v"}]"#;
    let ops = [
        op("it.counter.add", "write", "MM_It_Add", v, 15000),
        op("it.counter.addAgain", "write", "MM_It_Add", v, 15000),
        op("it.counter.fail", "write", "MM_It_Fail", v, 15000),
        op("it.counter.slow", "read", "MM_It_Slow", "[]", 500),
        op("it.counter.list", "read", "MM_It_List", "[]", 15000),
        op(
            "it.counter.out",
            "write",
            "MM_It_Out",
            r#"[{"name":"A","type":"int","arg":"a"},{"name":"B","type":"bigint","output":true},{"name":"C","type":"nvarchar","output":true},{"name":"D","type":"datetime2","output":true}]"#,
            15000,
        ),
    ];
    Manifest::parse(
        format!(
            r#"{{"manifest_version":2,"allowlist_hash":"","procs":["MM_It_Add","MM_It_Fail","MM_It_Slow","MM_It_List","MM_It_Out"],"excluded_proc_patterns":[],"ops":[{}]}}"#,
            ops.join(",")
        )
        .as_bytes(),
    )
    .unwrap()
}

fn it_op(name: &str, args: &str, key: Option<&str>, replay: bool) -> Op {
    let key = key.map_or("null".to_owned(), json::quote);
    let v = json::parse(
        format!(
            r#"{{"type":"op","id":"6f1c2b7e-8d4a-4c3b-9e2f-1a2b3c4d5e6f","op":"{name}","contract_ver":1,"target_db":"SMILE_FO","args":{args},"request_key":{key},"replay":{replay},"deadline_ms":15000}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    op_of(&v)
}

/// A fresh journal directory under the system temp dir.
fn journal_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("pms-it-journal-{}-{nanos}", std::process::id()))
}

#[test]
fn keyed_writes_run_once_and_errors_are_classified() {
    let Some(dbs) = pools() else { return };
    let _g = serial();
    let p = fo(&dbs);
    exec_sql(
        &p,
        "IF OBJECT_ID('dbo.MM_ItCounter') IS NULL CREATE TABLE dbo.MM_ItCounter (Id int IDENTITY PRIMARY KEY, V int NOT NULL)",
    );
    exec_sql(
        &p,
        "CREATE OR ALTER PROCEDURE dbo.MM_It_Add @V int AS BEGIN SET NOCOUNT ON; INSERT dbo.MM_ItCounter (V) VALUES (@V); SELECT CAST(SCOPE_IDENTITY() AS int) AS Id, @V AS V, N'<b>&' AS Html; RETURN 7; END",
    );
    exec_sql(
        &p,
        "CREATE OR ALTER PROCEDURE dbo.MM_It_Fail @V int AS BEGIN SET NOCOUNT ON; INSERT dbo.MM_ItCounter (V) VALUES (@V); THROW 51000, 'refused by the fixture', 1; END",
    );
    exec_sql(
        &p,
        "CREATE OR ALTER PROCEDURE dbo.MM_It_Slow AS BEGIN WAITFOR DELAY '00:00:05'; SELECT 1 AS X; END",
    );
    exec_sql(
        &p,
        "CREATE OR ALTER PROCEDURE dbo.MM_It_List AS BEGIN SELECT 1 AS N UNION ALL SELECT 2; END",
    );
    exec_sql(
        &p,
        "CREATE OR ALTER PROCEDURE dbo.MM_It_Out @A int, @B bigint OUTPUT, @C nvarchar(50) OUTPUT, @D datetime2 OUTPUT AS BEGIN SET @B = @A * 2; SET @C = N'đã xong'; SET @D = '2026-11-15T10:00:00.5'; RETURN 0; END",
    );
    exec_sql(&p, "DELETE dbo.MM_ItCounter;");
    let dir = journal_dir();
    let j = Arc::new(Journal::open_dir(&dir, journal::Limits::default()).unwrap());
    let sql = Arc::new(executor(&dbs, it_manifest(), Limits::default()));
    let e = Journaled::new(sql, Some(j.clone()), Logger::discard(), None);
    let count = || scalar(&p, "SELECT CAST(COUNT(*) AS bigint) FROM dbo.MM_ItCounter");

    let first = run(
        &e,
        &it_op("it.counter.add", r#"{"v":5}"#, Some("req-1"), false),
    );
    assert_eq!(first.status, wire::STATUS_APPLIED, "{first:?}");
    assert_eq!(first.replayed, None);
    assert_eq!(first.outputs["ReturnValue"].as_i64(), Some(7));
    assert_eq!(first.sets[0][0]["Html"].as_str(), Some("<b>&"));
    assert_eq!(count(), 1);
    assert_eq!(j.health().entries, 1);
    for replay in [true, false] {
        let again = run(
            &e,
            &it_op("it.counter.add", r#"{"v":5}"#, Some("req-1"), replay),
        );
        assert_eq!(again.replayed, Some(true));
        assert_eq!(
            (again.sets.clone(), again.outputs.clone()),
            (first.sets.clone(), first.outputs.clone())
        );
    }
    assert_eq!(count(), 1, "a replay never runs the write again");
    let miss = run(
        &e,
        &it_op("it.counter.add", r#"{"v":5}"#, Some("req-2"), true),
    );
    assert_eq!(miss.error.unwrap().code, wire::ERR_REQUEST_KEY_UNKNOWN);
    assert_eq!(count(), 1, "an unknown replay runs nothing");
    let other = run(
        &e,
        &it_op("it.counter.addAgain", r#"{"v":5}"#, Some("req-1"), false),
    );
    assert_eq!(other.error.unwrap().message, "request_key_conflict");

    let failed = run(
        &e,
        &it_op("it.counter.fail", r#"{"v":9}"#, Some("req-3"), false),
    );
    let err = failed.error.unwrap();
    assert_eq!(
        (failed.status.as_str(), err.code.as_str(), err.sql_number),
        ("rejected", exec::CODE_SQL_ERROR, Some(51000))
    );
    assert_eq!(err.message, "refused by the fixture");
    assert_eq!(count(), 1, "a refused write is rolled back");
    assert_eq!(j.health().entries, 1, "only applied writes are journaled");

    let slow = run(&e, &it_op("it.counter.slow", "{}", None, false));
    assert_eq!(slow.error.unwrap().code, wire::ERR_OP_TIMEOUT);
    // The connection survives the cancelled statement.
    let list = run(&e, &it_op("it.counter.list", "{}", None, false));
    assert_eq!(list.rowcounts, vec![2]);

    let capped = executor(
        &dbs,
        it_manifest(),
        Limits {
            max_rows_per_set: 1,
            ..Limits::default()
        },
    );
    let big = run(&capped, &it_op("it.counter.list", "{}", None, false));
    assert_eq!(big.error.unwrap().code, exec::CODE_RESULT_TOO_LARGE);

    let out = run(&e, &it_op("it.counter.out", r#"{"a":42}"#, None, false));
    assert_eq!(out.status, wire::STATUS_APPLIED, "{out:?}");
    assert_eq!(out.outputs["B"].as_str(), Some("84"));
    assert_eq!(out.outputs["C"].as_str(), Some("đã xong"));
    assert_eq!(out.outputs["D"].as_str(), Some("2026-11-15T10:00:00.500Z"));
    std::fs::remove_dir_all(&dir).unwrap();
}
