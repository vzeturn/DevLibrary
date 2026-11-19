//! The site command journal and the expected-state guard on a real SQL Server (port of
//! the core cases of `Commands/SqlSiteCommandStoreIntegrationTests.cs`). Gated on
//! `PDA_SYNC_TEST_SQL` (a dl-tds URL of a login that may create databases); each test
//! creates its own `pda_sync_test_*` database and drops it afterwards. The tests run as
//! that login, not as `pda_sync` with the bootstrap grants (the grants are the bootstrap's
//! tests).

use std::sync::Arc;

use dl_agent_pda_sync::commands::{
    ConfigSource, SiteCommandExecutor, SiteCommandStore, SqlSiteCommandStore, reasons,
    schema_columns,
};
use dl_agent_pda_sync::config::SystemClock;
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::{
    CheckStateFingerprint, CheckStateHeader, CheckStateLine, CommandStatus, KdsLineStatePayload,
    SyncCommand, SyncCommandResult, check_aggregate, kds_line_actions, site_command_types,
    table_command_types,
};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_uuid::Uuid;

const RVC: i32 = 3;
const COMMAND_LOG: &str = include_str!("../assets/sql/030_command_log.sql");

fn admin_url() -> Option<String> {
    std::env::var("PDA_SYNC_TEST_SQL")
        .ok()
        .filter(|s| !s.trim().is_empty())
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
            database: format!("pda_sync_test_{}", &tag[..12]),
        };
        s.exec_in("master", &format!("CREATE DATABASE [{}];", s.database));
        s.exec(COMMAND_LOG);
        s.exec(
            "CREATE TABLE dbo.TmpInvoice (CheckNo int NOT NULL PRIMARY KEY, RVC int NULL, TableNo char(6) NULL, NGuest int NULL,
    SubTotal money NULL, TotalPaid money NULL);
CREATE TABLE dbo.TmpTrn (CheckNo int NOT NULL, TrnSeq int NOT NULL, TrnCode int NULL, ItemCode char(10) NULL,
    TrnQty float NULL, ItemPrice money NULL, PRIMARY KEY (CheckNo, TrnSeq));
CREATE TABLE dbo.tmpTrnInfo (CheckNo int NOT NULL, TrnNo int NOT NULL, IsDone bit NULL, DoneTime datetime NULL,
    PRIMARY KEY (CheckNo, TrnNo));
CREATE TABLE dbo.TableReservation (nID int IDENTITY(1,1) PRIMARY KEY, GuestInfo nvarchar(200) NULL, NumAdult int NULL,
    NumChild int NULL, FromDateTime datetime NULL, ToDateTime datetime NULL, TableNum nvarchar(20) NULL,
    TelNum nvarchar(50) NULL, Booker nvarchar(50) NULL, CompanyName nvarchar(200) NULL, VATCode nvarchar(50) NULL,
    Address nvarchar(200) NULL, RVC nvarchar(10) NULL);",
        );
        s
    }

    fn exec_in(&self, database: &str, sql: &str) {
        let mut c = dl_tds::Client::connect(&self.admin.clone().with_database(database)).unwrap();
        c.batch(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        c.close();
    }

    fn exec(&self, sql: &str) {
        self.exec_in(&self.database, sql);
    }

    fn count(&self, sql: &str) -> i64 {
        let mut c =
            dl_tds::Client::connect(&self.admin.clone().with_database(&self.database)).unwrap();
        let r = c.query(sql, &[]).unwrap();
        let n: i64 = r.rows()[0].get(0).unwrap();
        c.close();
        n
    }

    fn store(&self) -> Arc<SqlSiteCommandStore> {
        let cfg = self.admin.clone().with_database(&self.database);
        let source: ConfigSource = Arc::new(move || Ok(cfg.clone()));
        Arc::new(SqlSiteCommandStore::new(source))
    }

    fn executor(&self) -> SiteCommandExecutor {
        SiteCommandExecutor::new(self.store(), SystemClock::shared(), &Logger::none())
    }

    /// An open check T4 with two guests and one coffee whose KDS line is not done.
    fn seed_check(&self, check: i64) {
        self.exec(&format!(
            "INSERT dbo.TmpInvoice VALUES ({check}, {RVC}, 'T4', 2, 50000, 0);
INSERT dbo.TmpTrn (CheckNo, TrnSeq, TrnCode, ItemCode, TrnQty, ItemPrice) VALUES ({check}, 1, 1, 'CF01', 1, 50000);
INSERT dbo.tmpTrnInfo (CheckNo, TrnNo, IsDone, DoneTime) VALUES ({check}, 1, 0, NULL);"
        ));
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

fn seen(check: i64) -> String {
    CheckStateFingerprint::compute(
        &CheckStateHeader {
            check_no: check,
            table_no: Some("T4".into()),
            guests: 2,
            sub_total: dl_decimal::Decimal::from(50_000),
            total_paid: dl_decimal::Decimal::ZERO,
        },
        &[CheckStateLine {
            line_seq: 1,
            transaction_code: Some("1".into()),
            item_code: Some("CF01".into()),
            qty: dl_decimal::Decimal::ONE,
            price: dl_decimal::Decimal::from(50_000),
        }],
    )
}

fn command(
    type_: &str,
    payload: dl_json::Value,
    aggregate: &str,
    expected: Option<String>,
) -> SyncCommand {
    SyncCommand {
        command_id: Uuid::new_v4(),
        type_: type_.into(),
        aggregate_key: aggregate.into(),
        expected_hash: expected,
        payload,
        created_by: "clerk-7".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

fn kds_done(check: i64, expected: Option<String>) -> SyncCommand {
    let p = KdsLineStatePayload {
        rvc_no: RVC,
        check_no: check,
        line_seqs: vec![1],
        action: kds_line_actions::DONE.into(),
        cashier: "clerk-7".into(),
    };
    command(
        site_command_types::KDS_LINE_STATE,
        p.to_element(),
        &check_aggregate::key(RVC, check),
        expected,
    )
}

fn reservation(guest: &str) -> SyncCommand {
    let payload = stj::parse(&format!(
        r#"{{"rvc_no":{RVC},"guest_info":"{guest}","num_adult":2,"num_child":0,"from_date_time":"2026-11-15T19:00:00","to_date_time":"2026-11-15T21:00:00","table_num":"T4","tel_num":"0901","booker":"host"}}"#
    ))
    .unwrap();
    command(
        site_command_types::RESERVATION_SAVE,
        payload,
        "reservation:3:new",
        None,
    )
}

#[test]
fn same_command_delivered_twice_at_once_writes_once_and_both_answer_the_same() {
    let Some(url) = admin_url() else {
        eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
        return;
    };
    let db = Store::create(&url);
    let cmd = reservation("Ms Lan");
    let (a, b) = std::thread::scope(|scope| {
        let one = scope.spawn(|| db.executor().execute(&cmd, &Ctx::background()).unwrap());
        let two = scope.spawn(|| db.executor().execute(&cmd, &Ctx::background()).unwrap());
        (one.join().unwrap(), two.join().unwrap())
    });

    assert_eq!(a.status, CommandStatus::Applied, "{:?}", a.reason);
    assert_eq!(a, b, "the second delivery reads the first one's log row");
    assert_eq!(
        db.count("SELECT CAST(COUNT(*) AS bigint) FROM dbo.TableReservation"),
        1
    );
    assert_eq!(
        db.count("SELECT CAST(COUNT(*) AS bigint) FROM dbo.Pda_SyncCommandLog"),
        1
    );

    let recorded = db
        .store()
        .find_recorded(cmd.command_id, &Ctx::background())
        .unwrap();
    assert_eq!(recorded, Some(a));
}

#[test]
fn guarded_command_applies_on_the_seen_check_and_is_refused_after_a_pos_win_change() {
    let Some(url) = admin_url() else {
        eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
        return;
    };
    let db = Store::create(&url);
    db.seed_check(7001);
    db.seed_check(7002);

    let ok = db
        .executor()
        .execute(&kds_done(7001, Some(seen(7001))), &Ctx::background())
        .unwrap();
    assert_eq!(ok.status, CommandStatus::Applied, "{:?}", ok.reason);
    assert_eq!(db.count("SELECT CAST(COUNT(*) AS bigint) FROM dbo.tmpTrnInfo WHERE CheckNo = 7001 AND IsDone = 1"), 1);

    // POS WIN adds a line after the PDA read check 7002.
    db.exec("INSERT dbo.TmpTrn (CheckNo, TrnSeq, TrnCode, ItemCode, TrnQty, ItemPrice) VALUES (7002, 2, 1, 'TEA', 1, 30000);");
    let cmd = kds_done(7002, Some(seen(7002)));
    let refused = db.executor().execute(&cmd, &Ctx::background()).unwrap();
    assert_eq!(refused.reason.as_deref(), Some(reasons::CHECK_CHANGED));
    assert_eq!(db.count("SELECT CAST(COUNT(*) AS bigint) FROM dbo.tmpTrnInfo WHERE CheckNo = 7002 AND IsDone = 1"), 0);
    assert_eq!(
        db.executor().execute(&cmd, &Ctx::background()).unwrap(),
        refused,
        "the logged refusal answers the redelivery"
    );
    let missing = db
        .executor()
        .execute(&kds_done(424_242, Some(seen(424_242))), &Ctx::background())
        .unwrap();
    assert_eq!(missing.reason.as_deref(), Some(reasons::CHECK_NOT_FOUND));
}

#[test]
fn a_log_row_without_a_result_reads_back_its_status_and_time() {
    let Some(url) = admin_url() else {
        eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
        return;
    };
    let db = Store::create(&url);
    let id = Uuid::new_v4();
    db.exec(&format!(
        "INSERT dbo.Pda_SyncCommandLog (CommandId, Status, ResultJson, AtUtc) VALUES ('{id}', N'rejected', NULL, '2026-11-15T08:00:00.123');"
    ));
    let r = db
        .store()
        .find_recorded(id, &Ctx::background())
        .unwrap()
        .unwrap();
    assert_eq!(r.status, CommandStatus::Rejected);
    assert_eq!(r.at_utc.to_json_string(), "2026-11-15T08:00:00.123+00:00");
    assert_eq!(r.reason, None);
}

#[test]
fn the_column_probe_names_the_columns_this_store_lacks() {
    let Some(url) = admin_url() else {
        eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
        return;
    };
    let db = Store::create(&url);
    let store = db.store();
    let id = Uuid::new_v4();
    let mut found = Vec::new();
    let r = store
        .run_once(
            id,
            &mut |unit, ctx| {
                found = unit.tables().missing_columns(
                    schema_columns(table_command_types::TABLE_CHANGE).unwrap(),
                    ctx,
                )?;
                Ok(SyncCommandResult {
                    command_id: id,
                    status: CommandStatus::Rejected,
                    reason: Some("probe".into()),
                    result: None,
                    at_utc: dl_agent_pda_sync::contracts::time::DateTimeOffset::now_utc(),
                    exec_ms: None,
                })
            },
            &Ctx::background(),
        )
        .unwrap();
    assert_eq!(r.reason.as_deref(), Some("probe"));
    // The test store has no NChild column and no dbo.tablelist.
    found.sort();
    assert_eq!(
        found,
        [
            "tablelist.RVC",
            "tablelist.TableNum",
            "tablelist.TableStatus",
            "tmpInvoice.NChild"
        ]
    );
}
