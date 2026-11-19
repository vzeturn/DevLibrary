//! `Capture/SqlServerCaptureIntegrationTests.cs`: the production wiring
//! (`SqlServerCaptureSource`, `SqliteCaptureState`, `SqliteOutbox`) against a real SQL
//! Server. Gated on `PDA_SYNC_TEST_SQL` (a dl-tds URL of a login that may create databases
//! and logins); each test creates its own `pda_sync_test_*` database and read-only login
//! (`db_datareader`, as `Bootstrap/Sql/050_grants.sql`) and drops both afterwards.

mod capture_runner_support;

use std::sync::Arc;

use capture_runner_support::*;
use dl_agent_pda_sync::capture::status::OPEN_JOB;
use dl_agent_pda_sync::capture::{
    BatchStore, CaptureSource, CaptureState, OutboxBatchStore, SqlServerCaptureSource,
    SqlServerSourceOptions, SqliteCaptureState, StateFactory,
};
use dl_agent_pda_sync::config::codes;
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::SyncBatch;
use dl_agent_pda_sync::outbox::{BlobEnqueueResult, CaptureStatus, SqliteOutbox};
use dl_agent_pda_sync::reporting::Logger;

const TABLES: [&str; 4] = ["TmpInvoice", "TmpTrn", "Invoice", "DefRVCList"];

fn admin_url() -> Option<String> {
    std::env::var("PDA_SYNC_TEST_SQL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// A throwaway SMILE_POS-like database and its reader login.
struct SmilePos {
    admin: dl_tds::Config,
    database: String,
    login: String,
    password: String,
    dir: std::path::PathBuf,
}

impl SmilePos {
    fn create(url: &str) -> SmilePos {
        let admin: dl_tds::Config = url.parse().expect("PDA_SYNC_TEST_SQL is a dl-tds URL");
        let tag = dl_uuid::Uuid::new_v4().simple().to_string();
        let pos = SmilePos {
            admin: admin.with_database("master"),
            database: format!("pda_sync_test_{}", &tag[..12]),
            login: format!("pda_sync_test_{}", &tag[..12]),
            password: format!("P{tag}"),
            dir: std::env::temp_dir().join("pda-sync-agent-tests").join(&tag),
        };
        std::fs::create_dir_all(&pos.dir).unwrap();
        // DDL cannot take parameters; names and password are generated alphanumeric.
        pos.exec_in(
            "master",
            &format!(
                "CREATE DATABASE [{db}]; CREATE LOGIN [{l}] WITH PASSWORD = N'{p}', CHECK_POLICY = OFF, CHECK_EXPIRATION = OFF;",
                db = pos.database,
                l = pos.login,
                p = pos.password
            ),
        );
        pos.exec(&format!(
            "CREATE TABLE dbo.TmpInvoice (CheckNo int NOT NULL PRIMARY KEY, OpenTime datetime NULL, SubTotal money NULL);
CREATE TABLE dbo.TmpTrn (TrnSeq int NOT NULL PRIMARY KEY, CheckNo int NOT NULL, ItemPrice money NULL);
CREATE TABLE dbo.Invoice (CheckNo int NOT NULL PRIMARY KEY, CloseTime datetime NULL, SubTotal money NULL);
CREATE TABLE dbo.DefRVCList (RVCNo int NOT NULL PRIMARY KEY, RVCName varchar(50) NULL, Picture image NULL);
CREATE USER [{l}] FOR LOGIN [{l}];
ALTER ROLE [db_datareader] ADD MEMBER [{l}];",
            l = pos.login
        ));
        pos.exec(
            "DECLARE @now datetime = DATEADD(minute, -5, GETUTCDATE());
INSERT dbo.TmpInvoice VALUES (100, @now, 50000);
INSERT dbo.TmpTrn VALUES (1, 100, 50000);
INSERT dbo.Invoice VALUES (1, @now, 10), (2, DATEADD(day, -400, @now), 20);
INSERT dbo.DefRVCList VALUES (1, 'Caf' + CHAR(233), 0x010203);",
        );
        pos
    }

    fn exec_in(&self, database: &str, sql: &str) {
        let mut c = dl_tds::Client::connect(&self.admin.clone().with_database(database)).unwrap();
        c.batch(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        c.close();
    }

    fn exec(&self, sql: &str) {
        self.exec_in(&self.database, sql);
    }

    fn admin_client(&self) -> dl_tds::Client {
        dl_tds::Client::connect(&self.admin.clone().with_database(&self.database)).unwrap()
    }

    fn user_object_count(&self) -> i64 {
        let mut c = self.admin_client();
        let r = c
            .query(
                "SELECT COUNT(*) FROM sys.objects WHERE is_ms_shipped = 0",
                &[],
            )
            .unwrap();
        let n: i64 = r.rows()[0].get(0).unwrap();
        c.close();
        n
    }

    /// The reader's connection, like the agent's (`pda_sync` with `db_datareader`).
    fn reader(&self, password: &str) -> dl_tds::Config {
        self.admin
            .clone()
            .credentials(&self.login, password)
            .with_database(&self.database)
            .with_app_name("PdaSyncAgent")
    }

    fn runner(
        &self,
        password: &str,
        blobs: Arc<RecordingBlobStore>,
    ) -> dl_agent_pda_sync::capture::SqlCaptureRunner {
        let cfg = self.reader(password);
        let source = Arc::new(move || {
            Ok(
                Arc::new(SqlServerCaptureSource::new(SqlServerSourceOptions::new(
                    cfg.clone(),
                ))) as Arc<dyn CaptureSource>,
            )
        });
        let path = self.dir.join("capture.db");
        let state: StateFactory = Arc::new(move || {
            Ok(Arc::new(SqliteCaptureState::open(&path)?) as Arc<dyn CaptureState>)
        });
        runner_with(source, state, blobs, None)
    }
}

impl Drop for SmilePos {
    fn drop(&mut self) {
        if let Ok(mut c) = dl_tds::Client::connect(&self.admin) {
            let _ = c.batch(&format!(
                "IF DB_ID(N'{db}') IS NOT NULL BEGIN ALTER DATABASE [{db}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{db}]; END;
IF SUSER_ID(N'{l}') IS NOT NULL DROP LOGIN [{l}];",
                db = self.database,
                l = self.login
            ));
            c.close();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn drain(outbox: &SqliteOutbox) -> Vec<SyncBatch> {
    outbox
        .peek::<&str>(10_000, &[])
        .unwrap()
        .iter()
        .map(|i| SyncBatch::from_json(std::str::from_utf8(&i.payload).unwrap()).unwrap())
        .collect()
}

fn invoice_keys(batches: &[SyncBatch]) -> Vec<String> {
    keys(batches, "Invoice", "check_no")
}

#[test]
fn captures_a_real_smile_pos_as_the_read_only_login_and_follows_live_changes() {
    let Some(url) = admin_url() else {
        eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
        return;
    };
    let pos = SmilePos::create(&url);
    let blobs = Arc::new(RecordingBlobStore::new(BlobEnqueueResult::Queued));
    let runner = pos.runner(&pos.password, blobs.clone());
    let objects_before = pos.user_object_count();
    let outbox =
        Arc::new(SqliteOutbox::open(pos.dir.join("outbox.db"), i64::MAX, &Logger::none()).unwrap());
    let queue: Arc<dyn BatchStore> = Arc::new(OutboxBatchStore::new(outbox.clone(), None));

    let run = RunningCapture::start(
        &runner,
        queue,
        Arc::new(FakeRuntimeConfig::new(fast_config())),
    );
    run.until(
        || {
            let b = drain(&outbox);
            invoice_keys(&b).contains(&"2".to_owned())
                && blobs.refs().len() == 1
                && b.iter().any(|b| b.table == "TmpTrn")
        },
        "initial load from SQL Server",
    );
    pos.exec(
        "INSERT dbo.Invoice VALUES (3, DATEADD(minute, -5, GETUTCDATE()), 30); UPDATE dbo.TmpInvoice SET SubTotal = 60000 WHERE CheckNo = 100;",
    );
    run.until(
        || {
            let b = drain(&outbox);
            invoice_keys(&b).contains(&"3".to_owned())
                && b.iter().filter(|b| b.table == "TmpInvoice").count() >= 2
        },
        "live changes from SQL Server",
    );
    run.stop();

    let batches = drain(&outbox);
    should_be_contiguous_per_stream(&batches);
    let mut k = invoice_keys(&batches);
    let n = k.len();
    k.sort();
    k.dedup();
    assert_eq!(k.len(), n, "a closed check is sent once");
    for want in ["1", "2", "3"] {
        assert!(k.contains(&want.to_owned()));
    }
    let rvc: Vec<&SyncBatch> = batches.iter().filter(|b| b.table == "DefRVCList").collect();
    assert_eq!(rvc.len(), 1);
    let row = format!("{:?}", rvc[0].rows);
    assert!(row.contains("Café"), "varchar text survives: {row}");
    assert_eq!(runner.capture_error(), None);
    assert!(runner.sql_reachable());
    assert_eq!(
        pos.user_object_count(),
        objects_before,
        "capture creates nothing in the POS database"
    );
}

#[test]
fn a_schema_lock_on_the_pos_tables_reports_sql_busy_and_capture_resumes_when_it_is_released() {
    let Some(url) = admin_url() else {
        eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
        return;
    };
    let pos = SmilePos::create(&url);
    let queue = Arc::new(MemoryBatchStore::default());
    let runner = pos.runner(
        &pos.password,
        Arc::new(RecordingBlobStore::new(BlobEnqueueResult::Queued)),
    );

    let run = RunningCapture::start(
        &runner,
        queue.clone(),
        Arc::new(FakeRuntimeConfig::new(fast_config())),
    );
    run.until(
        || {
            invoice_keys(&queue.batches()).contains(&"2".to_owned())
                && runner.status().runs(OPEN_JOB) > 0
        },
        "initial load",
    );
    {
        let mut blocker = pos.admin_client();
        // An uncommitted ALTER holds a schema-modification lock that even READ UNCOMMITTED reads must wait for.
        let alters: String = TABLES
            .iter()
            .map(|t| format!("ALTER TABLE dbo.{t} ADD PdaTestLock int NULL; "))
            .collect();
        blocker
            .batch(&format!("BEGIN TRANSACTION; {alters}"))
            .unwrap();
        run.until(
            || runner.capture_error().as_deref() == Some(codes::SQL_BUSY),
            "SQL_BUSY while the POS holds the lock",
        );
        assert!(runner.sql_reachable());
        blocker.batch("ROLLBACK TRANSACTION;").unwrap();
        blocker.close();
    }
    pos.exec("INSERT dbo.Invoice VALUES (4, DATEADD(minute, -5, GETUTCDATE()), 40);");
    run.until(
        || {
            runner.capture_error().is_none()
                && invoice_keys(&queue.batches()).contains(&"4".to_owned())
        },
        "resumed after the lock",
    );
    run.stop();
    should_be_contiguous_per_stream(&queue.batches());
}

#[test]
fn a_wrong_password_is_sql_login_failed_and_unreachable() {
    let Some(url) = admin_url() else {
        eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
        return;
    };
    let pos = SmilePos::create(&url);
    let wrong = format!("Wrong{}", dl_uuid::Uuid::new_v4().simple());
    let runner = pos.runner(
        &wrong,
        Arc::new(RecordingBlobStore::new(BlobEnqueueResult::Queued)),
    );

    let run = RunningCapture::start(
        &runner,
        Arc::new(MemoryBatchStore::default()),
        Arc::new(FakeRuntimeConfig::new(fast_config())),
    );
    run.until(
        || runner.capture_error().as_deref() == Some(codes::SQL_LOGIN_FAILED),
        "SQL_LOGIN_FAILED reported",
    );
    assert!(!runner.sql_reachable());
    assert!(!runner.streams().is_empty());
    run.stop();
}
