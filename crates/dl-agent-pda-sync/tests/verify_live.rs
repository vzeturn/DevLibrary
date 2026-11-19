//! The phase-B acceptance of `sync.verify` (ADR 0126/0128) on a real SQL Server: rows
//! deleted in SMILE_POS while no agent ran are tombstoned within one verify cycle by a
//! fresh agent (empty `capture.db`, no shadow).
//!
//! The SMILE_POS fixture of `live_support`, the bootstrap `010`–`090` as an administrator,
//! then capture as `pda_sync`: an "old" agent mirrors the store into a fake cloud (which
//! keeps `sync.row_state` and answers the manifest endpoints as the PDA cloud does), a
//! closed check and a cashier (with its outlet rows) are deleted, a "new" agent snapshots with a fresh capture
//! state, one `sync.verify` narrows the mismatch to keys and the capture's key-repair job
//! tombstones them. A second verify finds nothing.
//!
//! Gated on `PDA_SYNC_TEST_SQL`; run with `--test-threads=1` (the `pda_sync` login is
//! server-wide). Skipped when the server already has a `pda_sync` login.

mod capture_runner_support;
mod live_support;
mod support;
mod verify_support;

use std::collections::HashSet;
use std::sync::Arc;

use capture_runner_support::{
    FakeRuntimeConfig, RecordingBlobStore, RunningCapture, fast_config, runner_with,
};
use dl_agent_pda_sync::bootstrap::admin::SqlServerAdmin;
use dl_agent_pda_sync::bootstrap::discovery::LocalSqlDiscovery;
use dl_agent_pda_sync::bootstrap::{SchemaFingerprint, SqlBootstrapper};
use dl_agent_pda_sync::capture::{
    BatchStore, CaptureSource, CaptureState, OutboxBatchStore, SqlServerCaptureSource,
    SqlServerSourceOptions, SqliteCaptureState, StateFactory,
};
use dl_agent_pda_sync::config::SystemClock;
use dl_agent_pda_sync::config::sql::agent_connection;
use dl_agent_pda_sync::contracts::catalog::SyncCatalog;
use dl_agent_pda_sync::contracts::wire::sync_verify_repair as repair;
use dl_agent_pda_sync::outbox::{BlobEnqueueResult, SqliteOutbox};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::security::names;
use dl_agent_pda_sync::util::ctx::Ctx;
use live_support::{SmilePos, admin_url};
use support::TestEnv;
use verify_support::{FakeCloud, handler, run};

fn has(cloud: &FakeCloud, table: &str, suffix: &str) -> bool {
    cloud.row_keys(table).iter().any(|k| k.ends_with(suffix))
}

/// One agent: its capture runner over `source`, its own capture state and outbox, pumped
/// into `cloud` until `done`.
fn agent(
    source: &dl_tds::Config,
    state: StateFactory,
    outbox: &Arc<SqliteOutbox>,
    cloud: &Arc<FakeCloud>,
    done: impl Fn(&FakeCloud) -> bool,
    what: &str,
) -> RunningCapture {
    let cfg = source.clone();
    let source = Arc::new(move || {
        Ok(
            Arc::new(SqlServerCaptureSource::new(SqlServerSourceOptions::new(
                cfg.clone(),
            ))) as Arc<dyn CaptureSource>,
        )
    });
    let r = runner_with(
        source,
        state,
        Arc::new(RecordingBlobStore::new(BlobEnqueueResult::Queued)),
        None,
    );
    let queue: Arc<dyn BatchStore> = Arc::new(OutboxBatchStore::new(outbox.clone(), None));
    let run = RunningCapture::start(&r, queue, Arc::new(FakeRuntimeConfig::new(fast_config())));
    run.until(
        || {
            cloud.pump(outbox);
            done(cloud)
        },
        what,
    );
    run
}

/// Waits until nothing was queued for a second: every stream is settled.
fn settle(run: &RunningCapture, cloud: &FakeCloud, outbox: &SqliteOutbox, what: &str) {
    let quiet = std::cell::Cell::new(0);
    run.until(
        || {
            let idle = cloud.pump(outbox) == 0 && outbox.depth().unwrap() == 0;
            quiet.set(if idle { quiet.get() + 1 } else { 0 });
            quiet.get() >= 50
        },
        what,
    );
}

#[test]
fn rows_deleted_while_no_agent_ran_are_tombstoned_within_one_verify_cycle() {
    let Some(url) = admin_url() else {
        eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
        return;
    };
    let Some(pos) = SmilePos::create(&url) else {
        return;
    };
    let major = pos.major();
    let ctx = Ctx::background();
    let env = TestEnv::new();

    // 010–090 as the administrator; everything after as pda_sync.
    let bootstrapper = SqlBootstrapper::new(
        Arc::new(LocalSqlDiscovery::new(&Logger::none())),
        Arc::new(SqlServerAdmin::new(pos.admin_credential())),
        SchemaFingerprint::new(SyncCatalog::load().unwrap(), 0.85),
        env.secrets.clone(),
        &env.log,
    );
    bootstrapper
        .run(false, Some(&pos.instance), Some(&pos.database), &ctx)
        .unwrap_or_else(|e| panic!("bootstrap on SQL {major}: {e}"));
    let pw = String::from_utf8(env.secrets.load(names::SQL_PASSWORD).unwrap().unwrap()).unwrap();
    let cfg = agent_connection(&pos.instance, &pos.database, &pw);
    let cloud = FakeCloud::new();
    let open = |name: &str| {
        Arc::new(SqliteOutbox::open(env.dir.join(name), i64::MAX, &Logger::none()).unwrap())
    };

    // The old agent mirrors SMILE_POS into the cloud, then is uninstalled.
    let old_outbox = open("old-outbox.db");
    let old_path = env.dir.join("old-capture.db");
    let old_state: StateFactory = Arc::new(move || {
        Ok(Arc::new(SqliteCaptureState::open(&old_path)?) as Arc<dyn CaptureState>)
    });
    let old_agent = agent(
        &cfg,
        old_state,
        &old_outbox,
        &cloud,
        |c| {
            ["=901", "=902", "=903"]
                .iter()
                .all(|k| has(c, "Invoice", k))
                && has(c, "CashierDef", "=BINH")
                && c.row_keys("CashierDef").len() == 3
        },
        "the old agent's mirror",
    );
    settle(
        &old_agent,
        &cloud,
        &old_outbox,
        "the old agent's mirror uploaded",
    );
    old_agent.stop();
    assert_eq!(cloud.row_keys("CashierRVC").len(), 3);

    // While no agent runs, closed check 903 and cashier BINH are deleted.
    pos.exec("DELETE dbo.Invoice WHERE CheckNo = 903; DELETE dbo.CashierRVC WHERE Cashier = 'BINH'; DELETE dbo.CashierDef WHERE Initial = 'BINH';");

    // The new agent starts from an empty capture.db: its snapshot cannot tombstone them.
    let outbox = open("outbox.db");
    let state_path = env.dir.join("capture.db");
    let p = state_path.clone();
    let state: StateFactory =
        Arc::new(move || Ok(Arc::new(SqliteCaptureState::open(&p)?) as Arc<dyn CaptureState>));
    let shadow_rows = |table: &str| {
        SqliteCaptureState::open(&state_path)
            .unwrap()
            .count_shadow(table, &Ctx::background())
            .unwrap()
    };
    let new_agent = agent(
        &cfg,
        state.clone(),
        &outbox,
        &cloud,
        |_| shadow_rows("Invoice") == 2 && shadow_rows("CashierDef") == 2,
        "the new agent's snapshot",
    );
    settle(
        &new_agent,
        &cloud,
        &outbox,
        "the new agent's snapshot uploaded",
    );
    assert!(
        has(&cloud, "Invoice", "=903"),
        "the snapshot alone leaves the deleted check"
    );
    assert!(
        has(&cloud, "CashierDef", "=BINH"),
        "and the deleted cashier"
    );
    assert_eq!(
        cloud.row_keys("CashierRVC").len(),
        3,
        "and the cashier's outlets"
    );

    // One verify cycle over every table.
    let h = handler(
        &outbox,
        state,
        cloud.clone(),
        SystemClock::shared(),
        Arc::new(SyncCatalog::load().unwrap().clone()),
        Some(Arc::new(FakeRuntimeConfig::new(fast_config())) as _),
        &Logger::none(),
    );
    let first = run(&h, &[]);
    let mismatched: HashSet<&str> = first
        .tables
        .iter()
        .filter(|t| !t.mismatched_buckets.is_empty())
        .map(|t| t.table.as_str())
        .collect();
    // CashierRVC: the two outlet rows of BINH went with the cashier.
    assert_eq!(
        mismatched,
        HashSet::from(["Invoice", "CashierDef", "CashierRVC"]),
        "{first:?}"
    );
    assert_eq!(first.repair, repair::KEYS_QUEUED, "{first:?}");
    eprintln!(
        "SQL Server {major}: {} tables compared, {} store rows; skipped {:?}",
        first.tables.len(),
        first.store_rows,
        first.skipped
    );

    new_agent.until(
        || {
            cloud.pump(&outbox);
            !has(&cloud, "Invoice", "=903")
                && !has(&cloud, "CashierDef", "=BINH")
                && outbox.pending_key_repairs().unwrap().is_empty()
        },
        "the key repair tombstones",
    );
    new_agent.until(
        || cloud.pump(&outbox) == 0 && outbox.depth().unwrap() == 0,
        "the repair uploaded",
    );
    new_agent.stop();
    assert!(has(&cloud, "Invoice", "=901") && has(&cloud, "CashierDef", "=AN"));

    // Parity limit (C# `KeyRepairCapture`): a table that tolerates duplicate rows
    // (`CashierRVC`, logical-keys.json) is not repaired by key, so the outlet rows of the
    // deleted cashier stay in the cloud and every verify queues them again.
    assert_eq!(cloud.row_keys("CashierRVC").len(), 3);
    let second = run(&h, &[]);
    let still: Vec<&str> = second
        .tables
        .iter()
        .filter(|t| !t.mismatched_buckets.is_empty())
        .map(|t| t.table.as_str())
        .collect();
    assert_eq!(still, ["CashierRVC"], "{second:?}");
}
