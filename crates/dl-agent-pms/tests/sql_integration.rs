//! Against a real SQL Server: set `PMS_AGENT_TEST_SQL` to a connection URL with rights to
//! create a database, e.g. `sqlserver://sa:<pw>@127.0.0.1:14333?encrypt=disable`. Without
//! it every test returns early.
//!
//! - the golden op/result pairs of packages/shared/src/ops/fixtures, served by procedures
//!   generated from each fixture's `db.calls` (binds are checked inside the procedure);
//! - request-key replay, SQL errors, timeouts and the result caps through MM_Op_GetLog /
//!   MM_Op_CreateLog of packages/db/sql/pms/530 and 531;
//! - the golden migration bundle through the runner (dry run, apply, re-apply, drift).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use dl_agent_pms::ctx::Ctx;
use dl_agent_pms::db::{self, Pool};
use dl_agent_pms::exec::{self, Executor, Limits, SqlExecutor};
use dl_agent_pms::log::Logger;
use dl_agent_pms::migrate::{self, backup};
use dl_agent_pms::ops::Manifest;
use dl_agent_pms::wire::{self, Frame, Op};
use dl_agent_pms::{bundle, config, json};
use dl_json::Value;

const DB: &str = "pms_agent_it";

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

/// Creates the test database once and installs the op-log migrations.
fn pool() -> Option<Arc<Pool>> {
    static POOL: OnceLock<Option<Arc<Pool>>> = OnceLock::new();
    POOL.get_or_init(|| {
        let cfg = server()?;
        let mut master = dl_tds::Client::connect(&cfg).expect("connect master");
        master
            .batch(&format!(
                "IF DB_ID('{DB}') IS NOT NULL BEGIN ALTER DATABASE {DB} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {DB}; END; CREATE DATABASE {DB};"
            ))
            .expect("create database");
        master.close();
        let p = Arc::new(Pool::new(db::DB_PMS, cfg.with_database(DB), 4));
        let ctx = Ctx::background();
        let mut c = p.get(&ctx).unwrap();
        for f in ["530_ops_op-log.sql", "531_ops_op-log-retention.sql"] {
            let sql = std::fs::read_to_string(root().join("sql").join(f)).unwrap();
            for b in migrate::split_batches(&sql) {
                c.batch(&ctx, &b.text).unwrap_or_else(|e| panic!("{f}: {e}"));
            }
        }
        drop(c);
        Some(p)
    })
    .clone()
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
fn proc_sql(manifest: &Manifest, op: &str, call: &Value) -> String {
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
    let mut body = String::from("SET NOCOUNT ON;\n");
    if let Some(Value::Object(binds)) = call.get("binds") {
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
        body.push_str(&format!(
            "THROW {}, {}, 1;\n",
            e["number"].as_i64().unwrap(),
            lit(e["message"].as_str().unwrap())
        ));
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
    format!(
        "CREATE OR ALTER PROCEDURE dbo.{name} {}\nAS\nBEGIN\n{body}END",
        params.join(", ")
    )
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

fn executor(p: &Arc<Pool>, manifest: Manifest, limits: Limits) -> SqlExecutor {
    let mut dbs = db::Pools::new();
    dbs.insert(db::DB_PMS.to_owned(), p.clone());
    SqlExecutor {
        manifest: Arc::new(manifest),
        dbs,
        limits,
        log: Logger::discard(),
    }
}

#[test]
fn golden_op_fixtures_match_the_go_agent() {
    let Some(p) = pool() else { return };
    let _g = serial();
    let manifest = Manifest::load().unwrap();
    let dir = root().join("ops");
    for name in [
        "pos-check-version-get.json",
        "pos-check-version-get-shape.json",
        "pos-check-version-get-sql-error.json",
    ] {
        let fx = json::parse(&std::fs::read(dir.join(name)).unwrap()).unwrap();
        let op = op_of(&fx["op"]);
        let calls = fx["db"]["calls"].as_array().unwrap();
        let spec = manifest.lookup(&op.op).unwrap();
        for c in &spec.calls {
            let call = calls
                .iter()
                .find(|x| x["proc"].as_str() == Some(c.proc_name.as_str()));
            let sql = match call {
                Some(call) => proc_sql(&manifest, &op.op, call),
                None => format!(
                    "CREATE OR ALTER PROCEDURE dbo.{} {} AS THROW 50998, 'must not be called', 1;",
                    c.proc_name,
                    c.params
                        .iter()
                        .map(|p| format!("@{} {}", p.name, p.typ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
            exec_sql(&p, &sql);
        }
        let e = executor(&p, manifest.clone(), Limits::default());
        let res = e.execute(&Ctx::background(), &op);
        let frame = Frame::Result(res);
        wire::encode(&frame).unwrap_or_else(|e| panic!("{name}: frame does not validate: {e}"));
        let got = json::parse(wire::to_json(&frame).as_bytes()).unwrap();
        assert_eq!(
            semantic(&got),
            semantic(&fx["result"]),
            "{name}\n got: {}",
            wire::to_json(&frame)
        );
    }
}

fn it_manifest() -> Manifest {
    let op = |name: &str, kind: &str, proc_name: &str, params: &str, timeout: i64| {
        format!(
            r#"{{"name":"{name}","contract_ver":1,"kind":"{kind}","target":"PMS","timeout_ms":{timeout},"idempotent":false,"calls":[{{"proc":"{proc_name}","params":{params}}}]}}"#
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
            r#"{{"manifest_version":1,"allowlist_hash":"","procs":["MM_It_Add","MM_It_Fail","MM_It_Slow","MM_It_List","MM_It_Out"],"excluded_proc_patterns":[],"ops":[{}]}}"#,
            ops.join(",")
        )
        .as_bytes(),
    )
    .unwrap()
}

fn it_op(name: &str, args: &str, key: Option<&str>) -> Op {
    let key = key.map_or("null".to_owned(), json::quote);
    let v = json::parse(
        format!(
            r#"{{"type":"op","id":"6f1c2b7e-8d4a-4c3b-9e2f-1a2b3c4d5e6f","op":"{name}","contract_ver":1,"args":{args},"request_key":{key},"deadline_ms":15000}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    op_of(&v)
}

#[test]
fn writes_replay_by_request_key_and_errors_are_classified() {
    let Some(p) = pool() else { return };
    let _g = serial();
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
    exec_sql(&p, "DELETE dbo.MM_ItCounter; DELETE dbo.MM_OpLog;");
    let e = executor(&p, it_manifest(), Limits::default());
    let ctx = Ctx::background();
    let count = || scalar(&p, "SELECT CAST(COUNT(*) AS bigint) FROM dbo.MM_ItCounter");

    let first = e.execute(&ctx, &it_op("it.counter.add", r#"{"v":5}"#, Some("req-1")));
    assert_eq!(first.status, wire::STATUS_APPLIED, "{first:?}");
    assert_eq!(first.replayed, Some(false));
    assert_eq!(first.outputs["ReturnValue"].as_i64(), Some(7));
    assert_eq!(first.sets[0][0]["Html"].as_str(), Some("<b>&"));
    assert_eq!(count(), 1);
    let again = e.execute(&ctx, &it_op("it.counter.add", r#"{"v":5}"#, Some("req-1")));
    assert_eq!(again.replayed, Some(true));
    assert_eq!(
        (again.sets.clone(), again.outputs.clone()),
        (first.sets.clone(), first.outputs.clone())
    );
    assert_eq!(count(), 1, "a replay never runs the write again");
    let other = e.execute(
        &ctx,
        &it_op("it.counter.addAgain", r#"{"v":5}"#, Some("req-1")),
    );
    assert_eq!(other.error.unwrap().message, "request_key_conflict");
    // The stored row is what the Go agent writes: the envelope and its sha256.
    let stored = scalar(
        &p,
        "SELECT CAST(CASE WHEN ResultJson LIKE N'{\"v\":\"agent/1\",\"sets\":%' AND LEN(ResultHash) = 64 THEN 1 ELSE 0 END AS bigint) FROM dbo.MM_OpLog WHERE RequestKey = 'req-1'",
    );
    assert_eq!(stored, 1);

    let failed = e.execute(&ctx, &it_op("it.counter.fail", r#"{"v":9}"#, None));
    let err = failed.error.unwrap();
    assert_eq!(
        (failed.status.as_str(), err.code.as_str(), err.sql_number),
        ("rejected", exec::CODE_SQL_ERROR, Some(51000))
    );
    assert_eq!(err.message, "refused by the fixture");
    assert_eq!(count(), 1, "a refused write is rolled back");

    let slow = e.execute(&ctx, &it_op("it.counter.slow", "{}", None));
    assert_eq!(slow.error.unwrap().code, wire::ERR_OP_TIMEOUT);
    // The connection survives the cancelled statement.
    let list = e.execute(&ctx, &it_op("it.counter.list", "{}", None));
    assert_eq!(list.rowcounts, vec![2]);

    let capped = executor(
        &p,
        it_manifest(),
        Limits {
            max_rows_per_set: 1,
            ..Limits::default()
        },
    );
    let big = capped.execute(&ctx, &it_op("it.counter.list", "{}", None));
    assert_eq!(big.error.unwrap().code, exec::CODE_RESULT_TOO_LARGE);

    let out = e.execute(&ctx, &it_op("it.counter.out", r#"{"a":42}"#, None));
    assert_eq!(out.status, wire::STATUS_APPLIED, "{out:?}");
    assert_eq!(out.outputs["B"].as_str(), Some("84"));
    assert_eq!(out.outputs["C"].as_str(), Some("đã xong"));
    assert_eq!(out.outputs["D"].as_str(), Some("2026-11-15T10:00:00.500Z"));
}

#[test]
fn golden_bundle_migrates_and_reports_drift() {
    golden_bundle(true);
}

/// A site migrate.mjs never touched has no MM_MigrationHistory: the runner's history
/// statement must then do nothing (it once failed with error 208, invalid object name).
#[test]
fn golden_bundle_migrates_without_migration_history() {
    golden_bundle(false);
}

fn golden_bundle(with_history: bool) {
    let Some(p) = pool() else { return };
    let _g = serial();
    exec_sql(
        &p,
        "IF OBJECT_ID('dbo.MM_SchemaVersion') IS NOT NULL DROP TABLE dbo.MM_SchemaVersion; IF OBJECT_ID('dbo.MM_BundleFixture') IS NOT NULL DROP TABLE dbo.MM_BundleFixture; IF OBJECT_ID('dbo.MM_MigrationHistory') IS NOT NULL DROP TABLE dbo.MM_MigrationHistory;",
    );
    if with_history {
        // migrate.mjs's history table, which an MM_PMS it migrated has (001_migration_history.sql).
        exec_sql(
            &p,
            "CREATE TABLE dbo.MM_MigrationHistory (FileName varchar(200) NOT NULL PRIMARY KEY, Checksum char(64) NOT NULL, DurationMs int NULL, AppliedAt datetime2(3) NOT NULL DEFAULT SYSUTCDATETIME());",
        );
    }
    let td = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/bundle");
    let keys = bundle::parse_keyring(
        &json::parse(&std::fs::read(td.join("dev-keyring.json")).unwrap()).unwrap(),
    )
    .unwrap();
    let b = bundle::open(&td.join("golden"), &keys).unwrap();
    let state = std::env::temp_dir().join(format!("pms-it-state-{}", std::process::id()));
    std::fs::create_dir_all(&state).unwrap();
    let now = dl_time::OffsetDateTime::now_utc();
    std::fs::write(
        state.join(backup::ATTEST_FILE),
        format!(
            r#"{{"attested_at":"{}","by":"it"}}"#,
            backup::rfc3339_nano(now)
        ),
    )
    .unwrap();
    let events: Arc<Mutex<Vec<String>>> = Arc::default();
    let runner = |rerun: i64| {
        let ev = events.clone();
        let gate = backup::new_backup_gate(
            &config::MigrationBackup {
                mode: "attested".into(),
                ..Default::default()
            },
            &state,
        )
        .unwrap();
        let mut o = migrate::Options::new(p.clone(), "0.1.0");
        o.backup = gate;
        o.rerun_partial = rerun;
        o.progress = Box::new(move |e| ev.lock().unwrap().push(format!("{}:{}", e.phase, e.seq)));
        migrate::Runner::new(o)
    };
    let ctx = Ctx::background();
    let r = runner(0);
    let (plan, res) = r.dry_run(&ctx, &b);
    res.unwrap_or_else(|e| panic!("{e:?}\n{plan:?}"));
    assert_eq!(plan.pending.len(), 5);
    assert!(!plan.ledger_exists);
    assert!(
        plan.backup_refusal.is_empty() && plan.missing_permissions.is_empty(),
        "{plan:?}"
    );
    assert!(
        !plan.pending[2].in_tx,
        "an online index runs outside a transaction"
    );

    let (rep, res) = r.apply(&ctx, &b);
    res.unwrap();
    assert_eq!((rep.head, rep.applied.len()), (5, 5));
    assert_eq!(rep.backup.as_ref().unwrap().mode, "attested");
    assert!(events.lock().unwrap().contains(&"backup:5".to_owned()));
    let lv = migrate::ledger::LedgerVersion {
        pool: Some(p.clone()),
    };
    assert_eq!(
        dl_agent_pms::exec::SchemaVersion::schema_version(&lv, &ctx),
        Ok(5)
    );
    assert_eq!(
        scalar(
            &p,
            "SELECT CAST(COUNT(*) AS bigint) FROM sys.indexes WHERE name = 'IX_MM_BundleFixture_Name'"
        ),
        1
    );

    let (rep, res) = r.apply(&ctx, &b);
    res.unwrap();
    assert_eq!((rep.head, rep.applied.len()), (5, 0));
    let history = if with_history {
        "SELECT CAST(COUNT(*) AS bigint) FROM dbo.MM_MigrationHistory"
    } else {
        "SELECT CAST(CASE WHEN OBJECT_ID(N'dbo.MM_MigrationHistory', N'U') IS NULL THEN 0 ELSE -1 END AS bigint)"
    };
    assert_eq!(
        scalar(&p, history),
        if with_history { 5 } else { 0 },
        "MM_MigrationHistory kept in step, never created"
    );
    assert!(r.check_drift(&ctx, &b).unwrap().is_empty());

    exec_sql(
        &p,
        "UPDATE dbo.MM_SchemaVersion SET Sha256 = REPLICATE('0', 64) WHERE Version = 2",
    );
    let drift = r.check_drift(&ctx, &b).unwrap();
    let codes: Vec<&str> = drift.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(
        codes,
        [migrate::CODE_LEDGER_CORRUPT, migrate::DRIFT_CHECKSUM]
    );
    let (_, res) = r.dry_run(&ctx, &b);
    assert_eq!(res.unwrap_err().code, migrate::CODE_LEDGER_CORRUPT);
    let _ = std::fs::remove_dir_all(&state);
}
