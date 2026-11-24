//! Against a real SQL Server standing in for the hotel PMS (testdata/pms-fake.sql, copied from
//! SmileCheckin deploy/dev): bundle install, a catalog read with etag and delta, a write, and a
//! lock held by another session failing fast. The port of `AgentSqlServerE2ETests`.
//!
//! Env-gated: `ARRIVAL_AGENT_TEST_SQL` is a server connection string without a database
//! (for example `Server=127.0.0.1,14333;User ID=sa;Password=...;Encrypt=False`). Unset, every
//! test returns at once. One test function, so the fake databases are dropped and created once.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{TempDir, new_tenant_key};
use dl_agent_arrival::agent::clock::SystemClock;
use dl_agent_arrival::agent::executor::Executor;
use dl_agent_arrival::agent::health::{self, Health, RecordingSink};
use dl_agent_arrival::agent::installer;
use dl_agent_arrival::agent::journal::Journal;
use dl_agent_arrival::agent::processor::Processor;
use dl_agent_arrival::agent::secrets::TenantKeyStore;
use dl_agent_arrival::agent::tds::{self, TdsExecutor, TdsOptions};
use dl_agent_arrival::agent::vault::LegalVault;
use dl_agent_arrival::contracts::{Params, PmsRequest, Status, errors};
use dl_json::Value;
use dl_tds::Param;

const ENV: &str = "ARRIVAL_AGENT_TEST_SQL";
const FAKE_PMS: &str = include_str!("../testdata/pms-fake.sql");

fn server() -> Option<String> {
    std::env::var(ENV)
        .ok()
        .map(|s| s.trim().trim_end_matches(';').to_string())
        .filter(|s| !s.is_empty())
}

/// `ApplyFakePmsAsync`: the script's batches (split on lines holding only `GO`) on `master`.
fn apply_fake_pms(server: &str) {
    let mut conn = tds::connect(server, Some("master")).expect("connect to master");
    let mut batch = String::new();
    for line in FAKE_PMS.lines().chain(std::iter::once("GO")) {
        if line.trim().eq_ignore_ascii_case("GO") {
            if !batch.trim().is_empty() {
                conn.batch(&batch).expect("fake PMS batch");
            }
            batch.clear();
        } else {
            batch.push_str(line);
            batch.push('\n');
        }
    }
}

fn request(
    operation: &str,
    params: &[(&str, &str)],
    base_etag: Option<&str>,
    key: Option<&str>,
) -> PmsRequest {
    let mut parameters = Params::new();
    for (k, v) in params {
        parameters.insert(*k, Some((*v).to_string()));
    }
    PmsRequest {
        request_id: format!("r-{:016x}", dl_rand::u64()),
        operation: operation.into(),
        parameters,
        base_etag: base_etag.map(str::to_string),
        idempotency_key: key.map(str::to_string),
        projection: None,
        ..PmsRequest::default()
    }
}

fn processor(dir: &TempDir, executor: Arc<TdsExecutor>) -> Processor {
    let health = Arc::new(Health::new(
        Arc::new(RecordingSink::default()),
        health::REPORT_INTERVAL,
    ));
    let journal = Arc::new(
        Journal::open(&dir.path().join("idem.jsonl"), Arc::new(SystemClock)).expect("journal"),
    );
    let keys = Arc::new(TenantKeyStore::new(
        &new_tenant_key(),
        dir.path().join("tenant.key"),
    ));
    let vault = LegalVault::new(dir.path().join("vault"), Arc::clone(&keys));
    Processor::new(executor, 10, journal, keys, vault, health, 1)
}

fn seed_folio(pms: &str, folio: i64) {
    let mut conn = tds::connect(pms, None).expect("connect");
    conn.execute(
        "DELETE dbo.Folio WHERE FolioNum = @f; INSERT dbo.Folio (FolioNum, FolioSubNum, NumAdult, NumChild) VALUES (@f, 0, 1, 0)",
        &[Param::new("@f", folio)],
    )
    .expect("seed folio");
}

#[test]
fn the_agent_runs_against_sql_server() {
    let Some(server) = server() else {
        eprintln!("{ENV} not set: SQL Server integration skipped");
        return;
    };
    apply_fake_pms(&server);
    let pms = format!("{server};Database=SMILE_FO");

    // Bundle_is_installed_recorded_and_not_reinstalled
    assert!(
        installer::ensure_installed(&pms, true).expect("install"),
        "forced install runs"
    );
    assert!(
        !installer::ensure_installed(&pms, false).expect("recheck"),
        "same bundle is not reinstalled"
    );
    let opts = TdsOptions {
        max_connections: 2,
        command_timeout: Duration::from_secs(30),
        lock_timeout: Duration::from_secs(1),
    };
    let executor = Arc::new(TdsExecutor::new(&pms, opts).expect("executor"));
    assert!(
        executor.schema_fingerprint().is_some(),
        "schema fingerprint"
    );

    // Clerk_list_runs_against_sql_server_and_answers_not_modified_then_delta
    let dir = TempDir::new("sql-e2e");
    let p = processor(&dir, Arc::clone(&executor));
    let first = p.handle(&request("clerk.list", &[], None, None));
    assert_eq!(first.status, Status::Ok, "{:?}", first.error);
    let ids: Vec<Option<&Value>> = first.result_sets[0]
        .rows
        .iter()
        .map(|r| r.get("ClerkID"))
        .collect();
    assert_eq!(
        ids,
        vec![Some(&Value::from("FO01")), Some(&Value::from("FO02"))]
    );
    let etag = first.etag.clone().expect("etag");
    let unchanged = p.handle(&request("clerk.list", &[], Some(&etag), None));
    assert_eq!(unchanged.status, Status::NotModified);
    tds::connect(&pms, None)
        .expect("connect")
        .execute(
            "INSERT dbo.Clerk (ClerkID, UserName) VALUES ('FO03', N'Night Audit')",
            &[],
        )
        .expect("insert clerk");
    let changed = p.handle(&request("clerk.list", &[], Some(&etag), None));
    assert_eq!(changed.status, Status::Delta);

    // Write_procedure_without_a_result_set_answers_ok
    seed_folio(&pms, 990_001);
    let write = |folio: &str, adults: &str| {
        let key = format!("k-{:016x}", dl_rand::u64());
        request(
            "folio.update-guest-count",
            &[("FolioNum", folio), ("NumAdult", adults), ("NumChild", "1")],
            None,
            Some(&key),
        )
    };
    let ok = p.handle(&write("990001", "2"));
    assert_eq!(ok.status, Status::Ok, "{:?}", ok.error);
    let adults = tds::connect(&pms, None)
        .expect("connect")
        .query(
            "SELECT CAST(NumAdult AS INT) FROM dbo.Folio WHERE FolioNum = 990001",
            &[],
        )
        .expect("read back");
    assert_eq!(adults.rows()[0].values()[0], dl_tds::Value::I32(2));

    // Record_locked_by_another_session_fails_fast_with_a_lock_error
    seed_folio(&pms, 990_002);
    let mut desk = tds::connect(&pms, None).expect("desk session");
    desk.batch("BEGIN TRANSACTION; UPDATE dbo.Folio SET NumAdult = 5 WHERE FolioNum = 990002;")
        .expect("hold the row");
    let started = Instant::now();
    let locked = p.handle(&write("990002", "2"));
    assert_eq!(locked.status, Status::Failed);
    assert!(
        errors::is_record_locked(locked.error.as_deref()),
        "{:?}",
        locked.error
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
    // Closing the desk session rolls its transaction back.
    drop(desk);
}
