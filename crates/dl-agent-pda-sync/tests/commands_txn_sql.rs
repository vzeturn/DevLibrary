//! `txn.snapshot` on a real SQL Server (ADR 0159, RT-0849; port of
//! `SqlTxnSnapshotIntegrationTests`): open then close a bill, replay, stale versions, a key
//! clash with another bill, the hash capture computes, and the narrow `pda_sync` grants.
//! Gated on `PDA_SYNC_TEST_SQL` (a dl-tds URL of a login that may create databases on the
//! server holding `CT_SMILE_POS`); each test creates its own `CT_RT0849_RS_*` database with
//! the six bill tables cloned from `CT_SMILE_POS` (columns, defaults and primary keys, no
//! rows), the vendor `dbo.Unicode2TCVN`, the agent scripts 030/040/050/120/130 and a
//! `pda_sync` user without login, and drops it afterwards. Synthetic data only.

use std::sync::Arc;

use dl_agent_pda_sync::capture::plan;
use dl_agent_pda_sync::capture::{
    CaptureSource, SourceQuery, SqlServerCaptureSource, SqlServerSourceOptions,
};
use dl_agent_pda_sync::commands::{
    ConfigSource, SiteCommandExecutor, SqlSiteCommandStore, TXN_COLUMNS_SQL, TXN_LOCK_SQL,
    TXN_SAVE_SQL, TxnInsertValue, TxnLiveColumn, TxnValue, txn_aggregate_hash, txn_aggregate_line,
    txn_delete_sql, txn_insert_sql, txn_param, txn_select_sql,
};
use dl_agent_pda_sync::config::SystemClock;
use dl_agent_pda_sync::contracts::map::StrMap;
use dl_agent_pda_sync::contracts::mapping::{CatalogRowMapper, MapperOptions, RawValue};
use dl_agent_pda_sync::contracts::time::TimeZone;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, SyncCommand, SyncCommandResult, TxnSnapshotResult, txn_command_types,
    txn_outcomes, txn_reasons, txn_scopes,
};
use dl_agent_pda_sync::contracts::{SyncCatalog, WireJson};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_tds::Param;
use dl_uuid::Uuid;

const SOURCE: &str = "CT_SMILE_POS";
const SCRIPTS: [&str; 5] = [
    include_str!("../assets/sql/030_command_log.sql"),
    include_str!("../assets/sql/040_sync_state.sql"),
    include_str!("../assets/sql/050_grants.sql"),
    include_str!("../assets/sql/130_txn_applied.sql"),
    include_str!("../assets/sql/140_txn_grants.sql"),
];

fn admin_url() -> Option<String> {
    std::env::var("PDA_SYNC_TEST_SQL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn mapper() -> Arc<CatalogRowMapper> {
    Arc::new(CatalogRowMapper::new(
        SyncCatalog::load().unwrap().clone(),
        MapperOptions::new(TimeZone::fixed("UTC", 0)),
    ))
}

fn table_list() -> String {
    txn_scopes::BILL_TABLES
        .iter()
        .map(|t| format!("N'{t}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The primary keys (one column each in SMILE_POS) and the column defaults of the six
/// source tables, recreated here.
fn clone_keys_and_defaults_sql() -> String {
    let list = table_list();
    format!(
        "DECLARE @sql nvarchar(max) = N'';
SELECT @sql += N'ALTER TABLE dbo.' + QUOTENAME(t.name) + N' ADD CONSTRAINT ' + QUOTENAME(N'PK_' + t.name) + N' PRIMARY KEY NONCLUSTERED ('
    + QUOTENAME(c.name) + N');'
  FROM {SOURCE}.sys.indexes i JOIN {SOURCE}.sys.tables t ON t.object_id = i.object_id
  JOIN {SOURCE}.sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
  JOIN {SOURCE}.sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
 WHERE i.is_primary_key = 1 AND t.name IN ({list});
SELECT @sql += N'ALTER TABLE dbo.' + QUOTENAME(t.name) + N' ADD DEFAULT ' + d.definition + N' FOR ' + QUOTENAME(c.name) + N';'
  FROM {SOURCE}.sys.default_constraints d JOIN {SOURCE}.sys.tables t ON t.object_id = d.parent_object_id
  JOIN {SOURCE}.sys.columns c ON c.object_id = d.parent_object_id AND c.column_id = d.parent_column_id
 WHERE t.name IN ({list});
EXEC sys.sp_executesql @sql;"
    )
}

struct Store {
    admin: dl_tds::Config,
    database: String,
}

impl Store {
    fn create(url: &str) -> Store {
        let admin: dl_tds::Config = url.parse().expect("PDA_SYNC_TEST_SQL is a dl-tds URL");
        let tag = Uuid::new_v4().simple().to_string();
        let s = Store {
            admin: admin.with_database("master"),
            database: format!("CT_RT0849_RS_{}", &tag[..10]),
        };
        s.create_database();
        for table in txn_scopes::BILL_TABLES {
            s.exec(&format!(
                "SELECT * INTO dbo.[{table}] FROM {SOURCE}.dbo.[{table}] WHERE 1 = 0;"
            ));
        }
        s.exec(&clone_keys_and_defaults_sql());
        s.exec(&format!(
            "DECLARE @d nvarchar(max) = (SELECT definition FROM {SOURCE}.sys.sql_modules WHERE object_id = OBJECT_ID(N'{SOURCE}.dbo.Unicode2TCVN')); EXEC sys.sp_executesql @d;"
        ));
        s.exec("CREATE USER [pda_sync] WITHOUT LOGIN;");
        for script in SCRIPTS {
            s.exec(script);
        }
        // 070 grants the order procedures too, which this scratch database does not have.
        s.exec("GRANT EXECUTE ON OBJECT::dbo.Unicode2TCVN TO [pda_sync];");
        s
    }

    /// Parallel tests creating databases at once can hit SQL Server 1807 (no exclusive lock on
    /// `model`); a failed CREATE DATABASE writes nothing, so it is retried a few times.
    fn create_database(&self) {
        let sql = format!("CREATE DATABASE [{}];", self.database);
        let mut attempt = 0u64;
        loop {
            let mut c = dl_tds::Client::connect(&self.admin).unwrap();
            let outcome = c.batch(&sql);
            c.close();
            match outcome {
                Ok(_) => return,
                Err(e) if attempt < 5 => {
                    attempt += 1;
                    eprintln!("{sql} attempt {attempt}: {e}; retrying");
                    std::thread::sleep(std::time::Duration::from_secs(3 * attempt));
                }
                Err(e) => panic!("{sql}: {e}"),
            }
        }
    }

    fn exec_in(&self, database: &str, sql: &str) {
        let mut c = dl_tds::Client::connect(&self.admin.clone().with_database(database)).unwrap();
        // The statement text is synthetic DDL; it never carries a secret.
        c.batch(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        c.close();
    }

    fn exec(&self, sql: &str) {
        self.exec_in(&self.database, sql);
    }

    fn config(&self) -> dl_tds::Config {
        self.admin.clone().with_database(&self.database)
    }

    fn scalar(&self, sql: &str, params: &[Param]) -> Option<i64> {
        let mut c = dl_tds::Client::connect(&self.config()).unwrap();
        let r = c.query(sql, params).unwrap();
        let v = r.rows().first().and_then(|row| row.get::<i64>(0).ok());
        c.close();
        v
    }

    fn count(&self, table: &str, check: i64) -> i64 {
        self.scalar(
            &format!("SELECT CAST(COUNT(*) AS bigint) FROM dbo.[{table}] WHERE CheckNo = @c"),
            &[Param::new("c", check)],
        )
        .unwrap()
    }

    fn executor(&self) -> SiteCommandExecutor {
        let cfg = self.config();
        let source: ConfigSource = Arc::new(move || Ok(cfg.clone()));
        SiteCommandExecutor::new(
            Arc::new(SqlSiteCommandStore::new(source)),
            SystemClock::shared(),
            &Logger::none(),
        )
        .with_mapper(mapper())
    }

    fn run(&self, c: &SyncCommand) -> SyncCommandResult {
        self.executor().execute(c, &Ctx::background()).unwrap()
    }

    /// The aggregate hash from what capture reads (its own source and the mapper),
    /// independent of the executor's read, and the canonical rows per table.
    fn capture(&self, check: i64) -> (String, Vec<(String, Vec<StrMap>)>) {
        let m = mapper();
        let source = SqlServerCaptureSource::new(SqlServerSourceOptions::new(self.config()));
        let mut lines = Vec::new();
        let mut rows = Vec::new();
        for name in txn_scopes::BILL_TABLES {
            let table = m.catalog().find(name).unwrap();
            let mut q = SourceQuery::new(name);
            q.exclude_columns = Some(plan::exclude_of(m.catalog(), table));
            q.bytes_columns = table
                .transforms
                .as_ref()
                .and_then(|t| t.tcvn3_decode.clone());
            q.where_ = Some(vec![("CheckNo".into(), RawValue::I64(check))]);
            let mut canon = Vec::new();
            for row in source.query(&q, &Ctx::background()).unwrap() {
                let c = m.canonicalize(name, &row).unwrap();
                lines.push(txn_aggregate_line(name, &c.hash));
                canon.push(c.columns);
            }
            rows.push((name.to_owned(), canon));
        }
        (txn_aggregate_hash(lines), rows)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if let Ok(mut c) = dl_tds::Client::connect(&self.admin) {
            let _ = c.batch(&format!(
                "IF DB_ID(N'{db}') IS NOT NULL BEGIN ALTER DATABASE [{db}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{db}]; END;",
                db = self.database
            ));
            c.close();
        }
    }
}

macro_rules! store {
    () => {{
        let Some(url) = admin_url() else {
            eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
            return;
        };
        Store::create(&url)
    }};
}

fn bill(check: i64, version: i64, tables: [String; 6]) -> String {
    let list: Vec<String> = txn_scopes::BILL_TABLES
        .iter()
        .zip(tables)
        .map(|(n, rows)| format!(r#"{{"table":"{n}","rows":{rows}}}"#))
        .collect();
    format!(
        r#"{{"scope":"bill","scope_key":{check},"version":{version},"tables":[{}],"actor":"cashier-1"}}"#,
        list.join(",")
    )
}

fn empty() -> String {
    "[]".into()
}

fn open_header(check: i64, remark: &str) -> String {
    format!(
        r#"[{{"CheckNo":{check},"RVC":4,"OpenTime":"2026-11-23T08:00:00.123","SubTotal":120000,"PONumber":"{remark}"}}]"#
    )
}

fn open_line(check: i64, seq: i64) -> String {
    format!(
        r#"[{{"TrnSeq":{seq},"CheckNo":{check},"RVC":4,"ItemCode":"SPA01","TrnDesc":"Massage 60"}}]"#
    )
}

fn open(check: i64, seq: i64, version: i64) -> String {
    bill(
        check,
        version,
        [
            open_header(check, "Bàn 4"),
            open_line(check, seq),
            empty(),
            empty(),
            empty(),
            empty(),
        ],
    )
}

fn closed(check: i64, seq: i64, version: i64) -> String {
    bill(
        check,
        version,
        [
            empty(),
            empty(),
            empty(),
            format!(
                r#"[{{"CheckNo":{check},"RVC":4,"OpenTime":"2026-11-23T08:00:00.123","CloseTime":"2026-11-23T09:15:00","SubTotal":120000,"PONumber":"Khách lẻ","TaxOmit":false}}]"#
            ),
            format!(
                r#"[{{"TrnSeq":{seq},"CheckNo":{check},"RVC":4,"ItemCode":"SPA01","TrnDesc":"Massage đá nóng"}}]"#
            ),
            format!(r#"[{{"PaymentNo":{seq},"CheckNo":{check},"RVC":4,"PmtRef":"Tiền mặt"}}]"#),
        ],
    )
}

fn snapshot(check: i64, payload: &str) -> SyncCommand {
    snapshot_with(check, payload, Uuid::new_v4())
}

fn snapshot_with(check: i64, payload: &str, id: Uuid) -> SyncCommand {
    SyncCommand {
        command_id: id,
        type_: txn_command_types::SNAPSHOT.into(),
        aggregate_key: txn_scopes::aggregate_key(txn_scopes::BILL, check),
        expected_hash: None,
        payload: stj::parse(payload).unwrap(),
        created_by: "cloud".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

fn applied(r: &SyncCommandResult) -> TxnSnapshotResult {
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    TxnSnapshotResult::from_element(r.result.as_ref().unwrap()).unwrap()
}

fn column<'a>(rows: &'a [(String, Vec<StrMap>)], table: &str, name: &str) -> Option<&'a str> {
    let (_, list) = rows.iter().find(|(t, _)| t == table).unwrap();
    assert_eq!(list.len(), 1, "{table}");
    // The Rust mapper does not drop the trailing spaces of a CHAR(n) value yet (the .NET
    // ADR 0123 `TrimFixedWidth`, a known gap outside this port), so compare without them.
    list[0]
        .get(name)
        .and_then(|v| v.as_deref())
        .map(|v| v.trim_end_matches(' '))
}

#[test]
fn open_then_close_moves_the_bill_and_each_result_hash_is_the_one_capture_computes() {
    let db = store!();
    let (check, seq) = (98_000_010, 98_000_020);

    let opened = applied(&db.run(&snapshot(check, &open(check, seq, 1))));
    assert_eq!(opened.outcome, txn_outcomes::APPLIED);
    assert_eq!(db.count("TmpInvoice", check), 1);
    assert_eq!(db.count("TmpTrn", check), 1);
    assert_eq!(opened.hash, db.capture(check).0);

    let closed = applied(&db.run(&snapshot(check, &closed(check, seq, 2))));
    for t in ["TmpInvoice", "TmpTrn", "TmpPayment"] {
        assert_eq!(db.count(t, check), 0, "{t}");
    }
    for t in ["Invoice", "Trn", "Payment"] {
        assert_eq!(db.count(t, check), 1, "{t}");
    }
    let (hash, rows) = db.capture(check);
    assert_eq!(closed.hash, hash);
    assert_ne!(closed.hash, opened.hash);
    // Unicode2TCVN encodes and capture decodes the same text.
    assert_eq!(column(&rows, "Trn", "TrnDesc"), Some("Massage đá nóng"));
    assert_eq!(column(&rows, "Invoice", "PONumber"), Some("Khách lẻ"));
    assert_eq!(column(&rows, "Payment", "PmtRef"), Some("Tiền mặt"));
    let payment = closed.row_counts.iter().find(|(t, _)| t == "Payment");
    assert_eq!(payment.map(|p| p.1), Some(1));
    assert_eq!(
        db.scalar(
            "SELECT Version FROM dbo.Pda_TxnApplied WHERE AggregateKey = @k",
            &[Param::new("k", format!("bill:{check}"))]
        ),
        Some(2)
    );
}

#[test]
fn a_replayed_command_id_answers_the_logged_result_and_writes_no_duplicate() {
    let db = store!();
    let (check, seq) = (98_000_030, 98_000_040);
    let id = Uuid::new_v4();
    let first = db.run(&snapshot_with(check, &open(check, seq, 1), id));
    let replay = db.run(&snapshot_with(check, &open(check, seq, 1), id));
    assert_eq!(replay, first);
    assert_eq!(db.count("TmpTrn", check), 1);
    assert_eq!(
        db.scalar(
            "SELECT CAST(COUNT(*) AS bigint) FROM dbo.Pda_SyncCommandLog WHERE CommandId = @id",
            &[Param::new("id", id)]
        ),
        Some(1)
    );
}

#[test]
fn an_older_version_after_a_newer_one_is_stale_and_the_same_content_again_hashes_the_same() {
    let db = store!();
    let (check, seq) = (98_000_050, 98_000_060);
    let v2 = applied(&db.run(&snapshot(check, &closed(check, seq, 2))));

    let v1 = applied(&db.run(&snapshot(check, &open(check, seq, 1))));
    assert_eq!(v1.outcome, txn_outcomes::STALE);
    assert_eq!(v1.applied_version, 2);
    assert_eq!(v1.hash, v2.hash);
    assert_eq!(
        db.count("TmpInvoice", check),
        0,
        "the stale open must not come back"
    );

    let v3 = applied(&db.run(&snapshot(check, &closed(check, seq, 3))));
    assert_eq!(v3.outcome, txn_outcomes::APPLIED);
    assert_eq!(v3.hash, v2.hash);
    assert_eq!(db.count("Trn", check), 1);
}

#[test]
fn a_line_key_of_another_bill_is_a_key_conflict_and_nothing_of_the_command_is_written() {
    let db = store!();
    let (a, b, seq) = (98_000_070, 98_000_080, 98_000_090);
    applied(&db.run(&snapshot(a, &open(a, seq, 1))));

    let clash = db.run(&snapshot(b, &open(b, seq, 1)));
    assert_eq!(clash.status, CommandStatus::Rejected);
    assert_eq!(clash.reason.as_deref(), Some(txn_reasons::KEY_CONFLICT));
    // The header insert before the clash rolled back to the savepoint.
    assert_eq!(db.count("TmpInvoice", b), 0);
    assert_eq!(db.count("TmpTrn", a), 1, "the other bill keeps its line");
    assert_eq!(
        db.scalar(
            "SELECT CAST(COUNT(*) AS bigint) FROM dbo.Pda_TxnApplied WHERE AggregateKey = @k",
            &[Param::new("k", format!("bill:{b}"))]
        ),
        Some(0)
    );
    // A store-state refusal rolls back with no log row.
    assert_eq!(
        db.scalar(
            "SELECT CAST(COUNT(*) AS bigint) FROM dbo.Pda_SyncCommandLog WHERE CommandId = @id",
            &[Param::new("id", clash.command_id)]
        ),
        Some(0)
    );

    // The redrive of the same command id runs again once the clash is gone.
    db.exec(&format!("DELETE FROM dbo.TmpTrn WHERE CheckNo = {a};"));
    let mut redrive = snapshot(b, &open(b, seq, 1));
    redrive.command_id = clash.command_id;
    applied(&db.run(&redrive));
    assert_eq!(db.count("TmpInvoice", b), 1);
}

#[test]
fn unknown_columns_and_bad_values_are_refused_before_any_write() {
    let db = store!();
    let (check, seq) = (98_000_100, 98_000_110);
    let with = |header: String, line: String| {
        bill(check, 1, [header, line, empty(), empty(), empty(), empty()])
    };
    let cases = [
        (
            with(
                open_header(check, "x"),
                format!(r#"[{{"TrnSeq":{seq},"CheckNo":{check},"Nope":1}}]"#),
            ),
            txn_reasons::COLUMN_UNKNOWN,
        ),
        // 300 does not fit tinyint.
        (
            with(
                format!(r#"[{{"CheckNo":{check},"RVC":300}}]"#),
                open_line(check, seq),
            ),
            txn_reasons::VALUE_INVALID,
        ),
        // ItemCode is char(12).
        (
            with(
                open_header(check, "x"),
                format!(
                    r#"[{{"TrnSeq":{seq},"CheckNo":{check},"ItemCode":"{}"}}]"#,
                    "x".repeat(13)
                ),
            ),
            txn_reasons::VALUE_INVALID,
        ),
    ];
    for (payload, reason) in cases {
        let r = db.run(&snapshot(check, &payload));
        assert_eq!(r.reason.as_deref(), Some(reason));
    }
    assert_eq!(db.count("TmpInvoice", check), 0);
}

#[test]
fn the_pda_sync_user_can_run_every_txn_statement_and_cannot_update_a_bill_table() {
    let db = store!();
    let (check, seq) = (98_000_120_i64, 98_000_130_i64);
    let mut c = dl_tds::Client::connect(&db.config()).unwrap();
    c.batch("BEGIN TRANSACTION; EXECUTE AS USER = N'pda_sync';")
        .unwrap();

    let cols = c
        .query(
            TXN_COLUMNS_SQL,
            &[Param::new("Table", "dbo.Trn".to_owned())],
        )
        .unwrap();
    let desc = cols
        .rows()
        .iter()
        .find(|r| r.get::<String>(0).ok().as_deref() == Some("TrnDesc"))
        .map(|r| (r.get::<String>(1).unwrap(), r.get::<i16>(2).unwrap()));
    assert_eq!(desc, Some(("varchar".to_owned(), -1)));

    let key = format!("bill:{check}");
    let lock = c
        .query(TXN_LOCK_SQL, &[Param::new("Key", key.clone())])
        .unwrap();
    assert!(lock.rows().is_empty());

    let numeric = |name: &str| TxnLiveColumn::new(name, "numeric", 9, 18, 0);
    let row = [
        TxnInsertValue {
            column: numeric("TrnSeq"),
            value: Some(TxnValue::Decimal(seq.to_string().parse().unwrap())),
            tcvn3: false,
        },
        TxnInsertValue {
            column: numeric("CheckNo"),
            value: Some(TxnValue::Decimal(check.to_string().parse().unwrap())),
            tcvn3: false,
        },
        TxnInsertValue {
            column: TxnLiveColumn::new("TrnDesc", "varchar", -1, 0, 0),
            value: Some(TxnValue::Text("Trà".into())),
            tcvn3: true,
        },
    ];
    let scope = [Param::new("Scope", check)];
    c.query(&txn_delete_sql("Trn", "CheckNo").unwrap(), &scope)
        .unwrap();
    let params: Vec<Param> = row
        .iter()
        .enumerate()
        .map(|(i, v)| txn_param(&format!("p{i}"), v).unwrap())
        .collect();
    c.query(&txn_insert_sql("Trn", &row).unwrap(), &params)
        .unwrap();
    let read = c
        .query(
            &txn_select_sql(
                "Trn",
                "CheckNo",
                "[TrnSeq], CONVERT(varbinary(max), [TrnDesc]) AS [TrnDesc]",
            )
            .unwrap(),
            &scope,
        )
        .unwrap();
    assert_eq!(read.rows().len(), 1);
    assert!(read.rows()[0].get::<Vec<u8>>(1).is_ok());

    c.query(
        TXN_SAVE_SQL,
        &[
            Param::new("Key", key.clone()),
            Param::new("Version", 1_i64),
            Param::new("Hash", "a".repeat(64)),
            Param::new("CommandId", Uuid::new_v4()),
        ],
    )
    .unwrap();
    let lock = c.query(TXN_LOCK_SQL, &[Param::new("Key", key)]).unwrap();
    assert_eq!(lock.rows()[0].get::<i64>(0).ok(), Some(1));

    let err = c
        .query(
            "UPDATE dbo.Trn SET TrnDesc = 'x' WHERE CheckNo = @c",
            &[Param::new("c", check)],
        )
        .expect_err("pda_sync must not update a bill table");
    assert!(err.to_string().contains("permission"), "{err}");
    let _ = c.batch("IF @@TRANCOUNT > 0 ROLLBACK;");
    c.close();
}
