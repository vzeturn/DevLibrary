//! RT-0759 (ADR 0153) against a real SQL Server: the signature query of
//! `SqlServerCaptureSource`, and the open-check rows read per cycle before (a full read
//! every cycle, `open_full_read_every = 1`) and after (two-phase). Gated on
//! `PDA_SYNC_TEST_SQL` (a dl-tds URL of a login that may create databases); the test
//! creates its own `pda_sync_2p_*` database and drops it afterwards.
//!
//! `cargo test --test capture_open_two_phase_sql -- --nocapture` prints the numbers.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dl_agent_pda_sync::capture::{
    BatchSink, CaptureEngine, CaptureOptions, CaptureReport, CaptureSource, InMemoryCaptureState,
    SqlServerCaptureSource, SqlServerSourceOptions,
};
use dl_agent_pda_sync::contracts::wire::SyncBatch;
use dl_agent_pda_sync::contracts::{SyncCatalog, TimeZone};
use dl_agent_pda_sync::util::ctx::Ctx;

const CHECKS: i32 = 300;
const LINES: i32 = 10;
const IDLE_CYCLES: u32 = 5;

fn admin_url() -> Option<String> {
    std::env::var("PDA_SYNC_TEST_SQL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

struct Db {
    admin: dl_tds::Config,
    name: String,
}

impl Db {
    fn create(url: &str) -> Db {
        let admin: dl_tds::Config = url.parse().expect("PDA_SYNC_TEST_SQL is a dl-tds URL");
        let tag = dl_uuid::Uuid::new_v4().simple().to_string();
        let db = Db {
            admin: admin.with_database("master"),
            name: format!("pda_sync_2p_{}", &tag[..12]),
        };
        db.exec_in("master", &format!("CREATE DATABASE [{}];", db.name));
        db.exec(
            "CREATE TABLE dbo.TmpInvoice (CheckNo int NOT NULL PRIMARY KEY, OpenTime datetime NULL, SubTotal money NULL);
CREATE TABLE dbo.TmpTrn (TrnSeq int NOT NULL PRIMARY KEY, CheckNo int NOT NULL, ItemPrice money NULL);
CREATE TABLE dbo.Invoice (CheckNo int NOT NULL PRIMARY KEY, CloseTime datetime NULL, SubTotal money NULL);",
        );
        db.exec(&format!(
            "DECLARE @now datetime = DATEADD(minute, -5, GETUTCDATE());
WITH n AS (SELECT TOP ({CHECKS}) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS c FROM sys.all_objects)
INSERT dbo.TmpInvoice SELECT c, @now, 50000 FROM n;
WITH n AS (SELECT TOP ({CHECKS}) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS c FROM sys.all_objects),
     l AS (SELECT TOP ({LINES}) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS l FROM sys.all_objects)
INSERT dbo.TmpTrn SELECT n.c * 100 + l.l, n.c, 1000 * l.l FROM n CROSS JOIN l;"
        ));
        db
    }

    fn exec_in(&self, database: &str, sql: &str) {
        let mut c = dl_tds::Client::connect(&self.admin.clone().with_database(database)).unwrap();
        c.batch(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        c.close();
    }

    fn exec(&self, sql: &str) {
        self.exec_in(&self.name, sql);
    }

    fn source(&self) -> Arc<SqlServerCaptureSource> {
        Arc::new(SqlServerCaptureSource::new(SqlServerSourceOptions::new(
            self.admin.clone().with_database(&self.name),
        )))
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        if let Ok(mut c) = dl_tds::Client::connect(&self.admin) {
            let _ = c.batch(&format!(
                "IF DB_ID(N'{db}') IS NOT NULL BEGIN ALTER DATABASE [{db}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{db}]; END;",
                db = self.name
            ));
            c.close();
        }
    }
}

#[derive(Default)]
struct CountingSink {
    rows: Mutex<usize>,
}

impl BatchSink for CountingSink {
    fn send(&self, batch: &SyncBatch, _ctx: &Ctx) -> dl_agent_pda_sync::capture::Result<()> {
        *self.rows.lock().unwrap() += batch.rows.len();
        Ok(())
    }
}

fn engine(
    source: Arc<SqlServerCaptureSource>,
    full_every: i32,
) -> (CaptureEngine, Arc<CountingSink>) {
    let mut options = CaptureOptions::new("agent-test", Arc::new(TimeZone::utc()));
    options.open_full_read_every = full_every;
    let sink = Arc::new(CountingSink::default());
    let e = CaptureEngine::new(
        options,
        SyncCatalog::load().unwrap(),
        source,
        Arc::new(InMemoryCaptureState::new()),
        sink.clone(),
        None,
        None,
        None,
    )
    .unwrap();
    (e, sink)
}

/// Rows read and mean time of `IDLE_CYCLES` cycles after the first one.
fn idle(e: &CaptureEngine) -> (i32, Duration, Vec<CaptureReport>) {
    let ctx = Ctx::background();
    let mut rows = 0;
    let mut reports = Vec::new();
    let started = Instant::now();
    for _ in 0..IDLE_CYCLES {
        let r = e.run_open_checks(&ctx).unwrap();
        rows += r.open_rows_read;
        reports.push(r);
    }
    (rows, started.elapsed() / IDLE_CYCLES, reports)
}

#[test]
fn signatures_come_from_sql_server_and_move_only_for_the_changed_check() {
    let Some(url) = admin_url() else {
        eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
        return;
    };
    let db = Db::create(&url);
    let source = db.source();
    let ctx = Ctx::background();

    let before = source
        .check_signatures("TmpTrn", "CheckNo", &ctx)
        .unwrap()
        .expect("SQL Server computes signatures");
    assert_eq!(before.len(), CHECKS as usize);
    let sig = |d: &dl_agent_pda_sync::capture::util::DotNetDict<
        dl_agent_pda_sync::capture::source::CheckSignature,
    >,
               k: &str| d.get(k).unwrap().signature.clone();
    assert!(
        sig(&before, "7").starts_with(&format!("{LINES}:")),
        "{}",
        sig(&before, "7")
    );

    db.exec("UPDATE dbo.TmpTrn SET ItemPrice = 9999 WHERE TrnSeq = 703;");
    let after = source
        .check_signatures("TmpTrn", "CheckNo", &ctx)
        .unwrap()
        .unwrap();
    assert_ne!(sig(&before, "7"), sig(&after, "7"));
    assert_eq!(sig(&before, "8"), sig(&after, "8"));

    // A missing column is a schema fault of that table, like a row read.
    let e = source
        .check_signatures("TmpTrn", "NoSuchColumn", &ctx)
        .unwrap_err();
    assert!(e.is_schema_fault(), "{e:?}");
    assert_eq!(e.table.as_deref(), Some("TmpTrn"));
}

#[test]
fn idle_open_cycles_read_no_rows_with_two_phase_detection() {
    let Some(url) = admin_url() else {
        eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
        return;
    };
    let db = Db::create(&url);
    let ctx = Ctx::background();
    let all = CHECKS + CHECKS * LINES;

    let (old, old_sink) = engine(db.source(), 1);
    let first_old = old.run_open_checks(&ctx).unwrap();
    let (old_rows, old_time, _) = idle(&old);

    let (new, new_sink) = engine(db.source(), 30);
    let first_new = new.run_open_checks(&ctx).unwrap();
    let (new_rows, new_time, reports) = idle(&new);

    assert_eq!(first_old.open_rows_read, all);
    assert_eq!(first_new.open_rows_read, all);
    assert_eq!(
        *old_sink.rows.lock().unwrap(),
        *new_sink.rows.lock().unwrap()
    );
    assert_eq!(old_rows, all * IDLE_CYCLES as i32);
    assert_eq!(new_rows, 0);
    assert!(reports.iter().all(|r| !r.open_full_read && r.batches == 0));

    // One changed line: only that check is read again, and only its line is sent.
    db.exec("UPDATE dbo.TmpTrn SET ItemPrice = 9999 WHERE TrnSeq = 703;");
    let sent = *new_sink.rows.lock().unwrap();
    let one = new.run_open_checks(&ctx).unwrap();
    assert_eq!(one.open_rows_read, 1 + LINES);
    assert_eq!(*new_sink.rows.lock().unwrap() - sent, 1);

    eprintln!(
        "RT-0759 open checks ({CHECKS} checks x {LINES} lines): idle cycle before {} rows / {:?}, after {} rows / {:?}; one changed check reads {} rows",
        all,
        old_time,
        new_rows / IDLE_CYCLES as i32,
        new_time,
        one.open_rows_read
    );
}
