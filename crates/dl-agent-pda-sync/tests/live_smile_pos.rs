//! The phase-A acceptance run on a real SQL Server (2017 and 2022): the SMILE_POS fixture
//! of the PDAOrder side-by-side run (`live_support`), the bootstrap scripts 010–090 as an
//! administrator, then everything as the `pda_sync` login they create: the hub
//! `capture_dry_run` pass, the production capture pipeline into a SQLite outbox, and one
//! `kds.line_state` command delivered twice that changes SMILE_POS once.
//!
//! Gated on `PDA_SYNC_TEST_SQL` (a dl-tds URL of a login that may create databases and
//! logins); run with `--test-threads=1` (the `pda_sync` login is server-wide). Skipped
//! when the server already has a `pda_sync` login.

mod capture_runner_support;
mod live_support;
mod support;

use std::str::FromStr;
use std::sync::Arc;

use capture_runner_support::{
    FakeRuntimeConfig, RecordingBlobStore, RunningCapture, fast_config, keys, runner_with,
    should_be_contiguous_per_stream,
};
use dl_agent_pda_sync::bootstrap::admin::SqlServerAdmin;
use dl_agent_pda_sync::bootstrap::discovery::LocalSqlDiscovery;
use dl_agent_pda_sync::bootstrap::{
    AgentLoginProbe, OrderGrantsProbe, SchemaFingerprint, SqlAgentLoginProbe, SqlBootstrapper,
    SqlInstance, SqlOrderGrantsProbe,
};
use dl_agent_pda_sync::capture::{
    BatchStore, CaptureSource, CaptureState, OutboxBatchStore, SqlServerCaptureSource,
    SqlServerSourceOptions, SqliteCaptureState, StateFactory,
};
use dl_agent_pda_sync::commands::{ConfigSource, SiteCommandExecutor, SqlSiteCommandStore};
use dl_agent_pda_sync::config::SystemClock;
use dl_agent_pda_sync::config::sql::agent_connection;
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::catalog::SyncCatalog;
use dl_agent_pda_sync::contracts::time::TimeZone;
use dl_agent_pda_sync::contracts::wire::{
    CheckStateFingerprint, CheckStateHeader, CheckStateLine, CommandStatus, KdsLineStatePayload,
    SyncBatch, SyncCommand, check_aggregate, kds_line_actions, site_command_types,
};
use dl_agent_pda_sync::module::dry_run;
use dl_agent_pda_sync::outbox::{BlobEnqueueResult, CaptureStatus, SqliteOutbox};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::security::names;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_decimal::Decimal;
use dl_json::Value;
use live_support::{SmilePos, admin_url};
use support::TestEnv;

/// The bootstrap steps, in order (`assets/sql/010..090`).
const STEPS: usize = 9;

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

/// Open check 1001 of `smile_pos_seed.sql` as the PDA saw it.
fn seen_1001() -> String {
    CheckStateFingerprint::compute(
        &CheckStateHeader {
            check_no: 1001,
            table_no: Some("T01".into()),
            guests: 2,
            sub_total: dec("60000.5"),
            total_paid: Decimal::ZERO,
        },
        &[
            CheckStateLine {
                line_seq: 5001,
                transaction_code: Some("100".into()),
                item_code: Some("CF01".into()),
                qty: dec("2"),
                price: dec("25000"),
            },
            CheckStateLine {
                line_seq: 5002,
                transaction_code: Some("100".into()),
                item_code: Some("CK01".into()),
                qty: dec("1"),
                price: dec("10000.5"),
            },
        ],
    )
}

/// `kds.line_state` done on line 5001 of check 1001 (RVC 1).
fn kds_done_5001() -> SyncCommand {
    let p = KdsLineStatePayload {
        rvc_no: 1,
        check_no: 1001,
        line_seqs: vec![5001],
        action: kds_line_actions::DONE.into(),
        cashier: "AN".into(),
    };
    SyncCommand {
        command_id: dl_uuid::Uuid::new_v4(),
        type_: site_command_types::KDS_LINE_STATE.into(),
        aggregate_key: check_aggregate::key(1, 1001),
        expected_hash: Some(seen_1001()),
        payload: p.to_element(),
        created_by: "AN".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

fn drain(outbox: &SqliteOutbox) -> Vec<SyncBatch> {
    outbox
        .peek::<&str>(100_000, &[])
        .unwrap()
        .iter()
        .map(|i| SyncBatch::from_json(std::str::from_utf8(&i.payload).unwrap()).unwrap())
        .collect()
}

fn has_table(batches: &[SyncBatch], table: &str) -> bool {
    batches.iter().any(|b| b.table.eq_ignore_ascii_case(table))
}

fn table_batches(batches: &[SyncBatch], table: &str) -> usize {
    batches
        .iter()
        .filter(|b| b.table.eq_ignore_ascii_case(table))
        .count()
}

/// The `streams` entry of a dry pass whose name names `table`.
fn stream<'a>(streams: &'a dl_json::Map, table: &str) -> &'a Value {
    let t = table.to_lowercase();
    streams
        .iter()
        .find(|(k, _)| k.to_lowercase() == t || k.to_lowercase().ends_with(&format!(".{t}")))
        .map(|(_, v)| v)
        .unwrap_or_else(|| {
            panic!(
                "no {table} stream in {:?}",
                streams.keys().collect::<Vec<_>>()
            )
        })
}

fn rows(v: &Value) -> i64 {
    v.get("rows").and_then(Value::as_i64).unwrap()
}

#[test]
fn smile_pos_bootstraps_captures_and_applies_one_kds_command_exactly_once() {
    let Some(url) = admin_url() else {
        eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
        return;
    };
    let Some(pos) = SmilePos::create(&url) else {
        return;
    };
    let major = pos.major();
    eprintln!(
        "SQL Server major {major}; {} tables generated from the catalog",
        pos.generated
    );
    let ctx = Ctx::background();
    let env = TestEnv::new();

    // 010–090 as the administrator, on the vendor-shaped database.
    let bootstrapper = SqlBootstrapper::new(
        Arc::new(LocalSqlDiscovery::new(&Logger::none())),
        Arc::new(SqlServerAdmin::new(pos.admin_credential())),
        SchemaFingerprint::new(SyncCatalog::load().unwrap(), 0.85),
        env.secrets.clone(),
        &env.log,
    );
    let report = bootstrapper
        .run(false, Some(&pos.instance), Some(&pos.database), &ctx)
        .unwrap_or_else(|e| panic!("bootstrap on SQL {major}: {e}"));
    assert_eq!(report.steps.len(), STEPS, "{:?}", report.steps);
    assert!(
        report.steps.iter().all(|s| s.executed),
        "{:?}",
        report.steps
    );
    assert!(
        report.fingerprint_score >= 0.85,
        "{}",
        report.fingerprint_score
    );
    let pw = String::from_utf8(env.secrets.load(names::SQL_PASSWORD).unwrap().unwrap()).unwrap();
    let instance = SqlInstance::new(pos.instance.clone(), "argument");
    SqlAgentLoginProbe
        .verify(&instance, &pos.database, &pw, &ctx)
        .unwrap_or_else(|e| panic!("pda_sync reads: {e}"));
    assert_eq!(
        SqlOrderGrantsProbe
            .missing(&pos.instance, &pos.database, &pw, &ctx)
            .unwrap(),
        Vec::<String>::new()
    );
    let again = bootstrapper
        .run(false, Some(&pos.instance), Some(&pos.database), &ctx)
        .unwrap();
    assert!(again.steps.iter().all(|s| s.skipped), "{:?}", again.steps);
    let cfg = agent_connection(&pos.instance, &pos.database, &pw);

    // The hub capture_dry_run pass as pda_sync: counts and hashes, nothing queued.
    let zone = Arc::new(TimeZone::fixed("SE Asia Standard Time", 420));
    let dry = || {
        let source: Arc<dyn CaptureSource> = Arc::new(SqlServerCaptureSource::new(
            SqlServerSourceOptions::new(cfg.clone()),
        ));
        dry_run::run(source, zone.clone(), "agent-live", &fast_config(), &ctx)
            .unwrap_or_else(|e| panic!("dry run on SQL {major}: {e}"))
    };
    let first = dry();
    assert_eq!(first.get("snapshot_complete"), Some(&Value::from(true)));
    assert_eq!(first.get("uploaded"), Some(&Value::from(false)));
    let streams = first.get("streams").and_then(Value::as_object).unwrap();
    assert_eq!(rows(stream(streams, "Invoice")), 3, "{streams:?}");
    assert_eq!(rows(stream(streams, "Trn")), 4, "{streams:?}");
    assert_eq!(rows(stream(streams, "CashierDef")), 3, "{streams:?}");
    assert_eq!(
        first
            .get("blobs")
            .and_then(|b| b.get("count"))
            .and_then(Value::as_i64),
        Some(1),
        "the DefRVCList picture: {first:?}"
    );
    let second = dry();
    assert_eq!(
        first.get("streams"),
        second.get("streams"),
        "a dry pass is deterministic"
    );

    // The production pipeline: SQLite capture state and outbox, as pda_sync.
    let source_cfg = cfg.clone();
    let source = Arc::new(move || {
        Ok(
            Arc::new(SqlServerCaptureSource::new(SqlServerSourceOptions::new(
                source_cfg.clone(),
            ))) as Arc<dyn CaptureSource>,
        )
    });
    let state_path = env.dir.join("capture.db");
    let state: StateFactory = Arc::new(move || {
        Ok(Arc::new(SqliteCaptureState::open(&state_path)?) as Arc<dyn CaptureState>)
    });
    let blobs = Arc::new(RecordingBlobStore::new(BlobEnqueueResult::Queued));
    let runner = runner_with(source, state, blobs.clone(), None);
    let outbox =
        Arc::new(SqliteOutbox::open(env.dir.join("outbox.db"), i64::MAX, &Logger::none()).unwrap());
    let queue: Arc<dyn BatchStore> = Arc::new(OutboxBatchStore::new(outbox.clone(), None));
    let run = RunningCapture::start(
        &runner,
        queue,
        Arc::new(FakeRuntimeConfig::new(fast_config())),
    );
    run.until(
        || {
            let b = drain(&outbox);
            let mut k = keys(&b, "Invoice", "check_no");
            k.sort();
            k.dedup();
            k == ["901", "902", "903"]
                && has_table(&b, "TmpTrn")
                && has_table(&b, "tmpTrnInfo")
                && has_table(&b, "CashierDef")
                && blobs.refs().len() == 1
        },
        "the snapshot of SMILE_POS as pda_sync",
    );
    let info_before = table_batches(&drain(&outbox), "tmpTrnInfo");

    // One kds.line_state, delivered twice: one write, one journal row, one answer.
    let store_cfg = cfg.clone();
    let config: ConfigSource = Arc::new(move || Ok(store_cfg.clone()));
    let executor = SiteCommandExecutor::new(
        Arc::new(SqlSiteCommandStore::new(config)),
        SystemClock::shared(),
        &Logger::none(),
    );
    let cmd = kds_done_5001();
    let applied = executor.execute(&cmd, &ctx).unwrap();
    assert_eq!(
        applied.status,
        CommandStatus::Applied,
        "{:?}",
        applied.reason
    );
    let done_sql = "SELECT CONVERT(varchar(30), DoneTime, 126) FROM dbo.tmpTrnInfo WHERE CheckNo = 1001 AND TrnNo = 5001 AND IsDone = 1";
    let done_at = pos.scalar_text(done_sql);
    assert!(done_at.is_some(), "line 5001 is done");
    let redelivered = executor.execute(&cmd, &ctx).unwrap();
    assert_eq!(redelivered, applied, "the journal answers the redelivery");
    assert_eq!(pos.scalar_text(done_sql), done_at, "written once");
    assert_eq!(
        pos.scalar_i64("SELECT CAST(COUNT(*) AS bigint) FROM dbo.Pda_SyncCommandLog"),
        1
    );
    assert_eq!(
        pos.scalar_i64("SELECT CAST(COUNT(*) AS bigint) FROM dbo.tmpTrnInfo WHERE IsDone = 1"),
        1,
        "only the commanded line changed"
    );

    // The change reaches the outbox through capture.
    run.until(
        || table_batches(&drain(&outbox), "tmpTrnInfo") > info_before,
        "the KDS change captured",
    );
    run.stop();
    let batches = drain(&outbox);
    should_be_contiguous_per_stream(&batches);
    assert_eq!(runner.capture_error(), None);
    assert!(runner.sql_reachable());
    eprintln!(
        "SQL Server {major}: {} batches, {} streams",
        batches.len(),
        streams.len()
    );
}
