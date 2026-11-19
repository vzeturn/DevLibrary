//! Resend by key (ADR 0128): rows `sync.verify` found different are sent again, or
//! tombstoned when the store lost them. Port of `Strategies/KeyRepairTests.cs`, plus the
//! phase-B acceptance on a fake cloud: a row deleted in SMILE_POS while no agent ran
//! (fresh capture state, no shadow) is tombstoned within one verify cycle.

mod capture_runner_support;
mod verify_support;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use capture_runner_support::{
    FakeRuntimeConfig, FakeVendorSource, RecordingBlobStore, RunningCapture, allow, fast_config,
    invoice, row, runner,
};
use dl_agent_pda_sync::capture::key_repair::cursor_of;
use dl_agent_pda_sync::capture::shadow_manifest;
use dl_agent_pda_sync::capture::{
    BatchSink, BatchStore, CaptureEngine, CaptureOptions, CaptureState, InMemoryCaptureState,
    KeyRepairList, KeyRepairTarget, OutboxBatchStore, Result, SqliteCaptureState, StateFactory,
};
use dl_agent_pda_sync::commands::verify::skip_reasons;
use dl_agent_pda_sync::config::{RuntimeConfig, SystemClock};
use dl_agent_pda_sync::contracts::catalog::SyncCatalog;
use dl_agent_pda_sync::contracts::hashing::row_key;
use dl_agent_pda_sync::contracts::map::StrMap;
use dl_agent_pda_sync::contracts::mapping::RawValue;
use dl_agent_pda_sync::contracts::time::TimeZone;
use dl_agent_pda_sync::contracts::wire::{SyncBatch, sync_verify_repair as repair};
use dl_agent_pda_sync::outbox::{BlobEnqueueResult, SqliteOutbox};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use verify_support::{FakeCloud, handler, lock, run};

const TABLE: &str = "DefRVCList";

fn key_of(rvc: i32) -> String {
    let mut k = StrMap::new();
    k.insert("RVCNo", Some(rvc.to_string()));
    row_key::of(&k)
}

#[derive(Default)]
struct RecordingSink(Mutex<Vec<SyncBatch>>);

impl BatchSink for RecordingSink {
    fn send(&self, batch: &SyncBatch, _ctx: &Ctx) -> Result<()> {
        lock(&self.0).push(batch.clone());
        Ok(())
    }
}

impl RecordingSink {
    fn batches(&self) -> Vec<SyncBatch> {
        lock(&self.0).clone()
    }

    fn clear(&self) {
        lock(&self.0).clear();
    }
}

#[derive(Default)]
struct FakeRepairList {
    next: Mutex<i64>,
    items: Mutex<Vec<KeyRepairTarget>>,
}

impl FakeRepairList {
    fn add(&self, table: &str, keys: &[String]) {
        for k in keys {
            let mut n = lock(&self.next);
            *n += 1;
            lock(&self.items).push(KeyRepairTarget::new(*n, table, k));
        }
    }

    fn items(&self) -> Vec<KeyRepairTarget> {
        lock(&self.items).clone()
    }
}

impl KeyRepairList for FakeRepairList {
    fn pending(&self, _ctx: &Ctx) -> Result<Vec<KeyRepairTarget>> {
        Ok(self.items())
    }

    fn complete(&self, ids: &[i64], _ctx: &Ctx) -> Result<()> {
        lock(&self.items).retain(|t| !ids.contains(&t.id));
        Ok(())
    }
}

struct Harness {
    source: Arc<FakeVendorSource>,
    state: Arc<InMemoryCaptureState>,
    sink: Arc<RecordingSink>,
}

impl Harness {
    fn site(rows: i32) -> Harness {
        let source = Arc::new(FakeVendorSource::default());
        source.add(
            TABLE,
            (1..=rows)
                .map(|i| {
                    row(&[
                        ("RVCNo", RawValue::I32(i)),
                        ("RVCName", RawValue::String(format!("R{i}"))),
                        ("Picture", RawValue::Null),
                    ])
                })
                .collect(),
        );
        Harness {
            source,
            state: Arc::new(InMemoryCaptureState::new()),
            sink: Arc::new(RecordingSink::default()),
        }
    }

    fn engine(&self, repairs: Option<Arc<dyn KeyRepairList>>) -> CaptureEngine {
        CaptureEngine::new(
            CaptureOptions::new("agent-test", Arc::new(TimeZone::utc())),
            SyncCatalog::load().unwrap(),
            self.source.clone(),
            self.state.clone(),
            self.sink.clone(),
            None,
            None,
            Some(SystemClock::shared()),
        )
        .unwrap()
        .with_key_repairs(repairs)
    }

    fn rows(&self, table: &str) -> Vec<StrMap> {
        self.sink
            .batches()
            .iter()
            .filter(|b| b.table == table)
            .flat_map(|b| b.rows.iter().map(|r| r.key.clone()))
            .collect()
    }

    fn tombstones(&self, table: &str) -> Vec<StrMap> {
        self.sink
            .batches()
            .iter()
            .filter(|b| b.table == table)
            .flat_map(|b| b.tombstones.iter().map(|t| t.key.clone()))
            .collect()
    }
}

fn rvc(k: &StrMap) -> Option<&str> {
    k.get("RVCNo").and_then(|v| v.as_deref())
}

#[test]
fn a_queued_row_is_sent_again_and_a_key_the_store_lacks_is_tombstoned() {
    let h = Harness::site(7);
    let queue = Arc::new(FakeRepairList::default());
    let engine = h.engine(Some(queue.clone()));
    let ctx = Ctx::background();
    engine.run_masters(&ctx).unwrap();
    h.sink.clear();
    queue.add(TABLE, &[key_of(6), key_of(42)]);

    engine.run_key_repairs(&ctx).unwrap();

    let rows = h.rows(TABLE);
    assert_eq!(
        rows.len(),
        1,
        "the shadow already held it, a key repair sends it anyway"
    );
    assert_eq!(rvc(&rows[0]), Some("6"));
    let tombs = h.tombstones(TABLE);
    assert_eq!(tombs.len(), 1);
    assert_eq!(rvc(&tombs[0]), Some("42"));
    assert!(queue.items().is_empty());
    assert_eq!(h.state.get_cursor(&cursor_of(TABLE), &ctx).unwrap(), None);
}

#[test]
fn a_scan_cut_into_pages_resumes_from_its_cursor_and_settles_every_key() {
    let h = Harness::site(7);
    let queue = Arc::new(FakeRepairList::default());
    let mut options = CaptureOptions::new("agent-test", Arc::new(TimeZone::utc()));
    options.page_rows = 2;
    let engine = CaptureEngine::new(
        options,
        SyncCatalog::load().unwrap(),
        h.source.clone(),
        h.state.clone(),
        h.sink.clone(),
        None,
        None,
        None,
    )
    .unwrap()
    .with_key_repairs(Some(queue.clone()));
    queue.add(TABLE, &[key_of(1), key_of(7), key_of(9)]);

    engine.run_key_repairs(&Ctx::background()).unwrap();

    let mut sent: Vec<String> = h
        .rows(TABLE)
        .iter()
        .filter_map(|k| rvc(k).map(str::to_owned))
        .collect();
    sent.sort();
    assert_eq!(sent, ["1", "7"]);
    assert_eq!(
        h.tombstones(TABLE)
            .iter()
            .filter_map(rvc)
            .collect::<Vec<_>>(),
        ["9"]
    );
    assert!(queue.items().is_empty());
}

#[test]
fn nothing_queued_sends_nothing() {
    let h = Harness::site(2);
    let engine = h.engine(Some(Arc::new(FakeRepairList::default())));
    engine.run_masters(&Ctx::background()).unwrap();
    h.sink.clear();

    assert_eq!(
        engine.run_key_repairs(&Ctx::background()).unwrap().batches,
        0
    );
    assert!(h.sink.batches().is_empty());
}

#[test]
fn a_key_of_an_open_check_or_unknown_table_is_dropped_without_reading_the_store() {
    let h = Harness::site(1);
    let queue = Arc::new(FakeRepairList::default());
    queue.add("TmpInvoice", &["CheckNo=1".into()]);
    queue.add("NoSuchTable", &["x=1".into()]);
    let before = h.source.queries();

    h.engine(Some(queue.clone()))
        .run_key_repairs(&Ctx::background())
        .unwrap();

    assert!(queue.items().is_empty());
    assert!(h.sink.batches().is_empty());
    assert_eq!(h.source.queries(), before);
}

#[test]
fn an_engine_without_a_queue_has_nothing_to_repair() {
    let h = Harness::site(1);
    assert_eq!(
        h.engine(None)
            .run_key_repairs(&Ctx::background())
            .unwrap()
            .batches,
        0
    );
}

// ---- acceptance: a fresh agent (no shadow) heals rows deleted while no agent ran

const TABLES: [&str; 7] = [
    "TmpInvoice",
    "TmpTrn",
    "Invoice",
    "InvoiceCxl",
    "VoidTrn",
    "ReOpenLog",
    "DefRVCList",
];

/// A small store: open check 100, closed checks 1..3, outlets 1 and 2.
fn store(deleted_in_gap: bool) -> Arc<FakeVendorSource> {
    let s = Arc::new(FakeVendorSource::default());
    let open = !deleted_in_gap;
    s.add(
        "TmpInvoice",
        if open {
            vec![row(&[
                ("CheckNo", RawValue::I32(100)),
                ("OpenTime", capture_runner_support::local(0)),
                ("SubTotal", capture_runner_support::dec("50000")),
            ])]
        } else {
            Vec::new()
        },
    );
    s.add(
        "TmpTrn",
        if open {
            vec![row(&[
                ("TrnSeq", RawValue::I32(1)),
                ("CheckNo", RawValue::I32(100)),
                ("ItemPrice", capture_runner_support::dec("50000")),
            ])]
        } else {
            Vec::new()
        },
    );
    let mut checks = vec![invoice(1, 0, "10"), invoice(2, 1, "20")];
    if deleted_in_gap {
        // Check 100 was closed while no agent ran; check 3 was deleted.
        checks.push(invoice(100, 0, "50000"));
    } else {
        checks.push(invoice(3, 2, "30"));
    }
    s.add("Invoice", checks);
    s.add("InvoiceCxl", vec![])
        .add("VoidTrn", vec![])
        .add("ReOpenLog", vec![]);
    let mut outlets = vec![row(&[
        ("RVCNo", RawValue::I32(1)),
        ("RVCName", RawValue::String("Cafe".into())),
        ("Picture", RawValue::Null),
    ])];
    if !deleted_in_gap {
        outlets.push(row(&[
            ("RVCNo", RawValue::I32(2)),
            ("RVCName", RawValue::String("Bar".into())),
            ("Picture", RawValue::Null),
        ]));
    }
    s.add("DefRVCList", outlets);
    s
}

fn config() -> RuntimeConfig {
    RuntimeConfig {
        table_allowlist: allow(&TABLES),
        ..fast_config()
    }
}

/// One agent's capture into its own outbox, pumped into `cloud` until `done`.
fn capture_until(
    site: &Arc<FakeVendorSource>,
    state: StateFactory,
    outbox: &Arc<SqliteOutbox>,
    cloud: &Arc<FakeCloud>,
    done: impl Fn(&FakeCloud) -> bool,
    what: &str,
) -> RunningCapture {
    let r = runner(
        site,
        state,
        Arc::new(RecordingBlobStore::new(BlobEnqueueResult::Queued)),
    );
    let queue: Arc<dyn BatchStore> = Arc::new(OutboxBatchStore::new(outbox.clone(), None));
    let run = RunningCapture::start(&r, queue, Arc::new(FakeRuntimeConfig::new(config())));
    run.until(
        || {
            cloud.pump(outbox);
            done(cloud)
        },
        what,
    );
    run
}

fn has(cloud: &FakeCloud, table: &str, needle: &str) -> bool {
    cloud.row_keys(table).iter().any(|k| k.ends_with(needle))
}

#[test]
fn rows_deleted_while_no_agent_ran_are_tombstoned_within_one_verify_cycle() {
    let dir = std::env::temp_dir()
        .join("pda-sync-agent-tests")
        .join(dl_uuid::Uuid::new_v4().simple().to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let cloud = FakeCloud::new();
    let open = |name: &str| {
        Arc::new(SqliteOutbox::open(dir.join(name), i64::MAX, &Logger::none()).unwrap())
    };

    // The old agent mirrors the store into the cloud, then is uninstalled.
    let before = store(false);
    let old_outbox = open("old-outbox.db");
    let old_state: Arc<dyn CaptureState> = Arc::new(InMemoryCaptureState::new());
    capture_until(
        &before,
        Arc::new(move || Ok(old_state.clone())),
        &old_outbox,
        &cloud,
        |c| {
            c.row_keys("Invoice").len() == 3
                && c.row_keys(TABLE).len() == 2
                && c.row_keys("TmpInvoice").len() == 1
        },
        "the old agent's mirror",
    )
    .stop();
    assert!(has(&cloud, "Invoice", "=3") && has(&cloud, TABLE, "=2"));

    // While no agent runs, check 3 and outlet 2 are deleted and check 100 is closed. The
    // new agent starts with an empty capture.db: its snapshot cannot tombstone them.
    let after = store(true);
    let outbox = open("outbox.db");
    let state_path = dir.join("capture.db");
    let p = state_path.clone();
    let state: StateFactory =
        Arc::new(move || Ok(Arc::new(SqliteCaptureState::open(&p)?) as Arc<dyn CaptureState>));
    let agent = capture_until(
        &after,
        state.clone(),
        &outbox,
        &cloud,
        |c| has(c, "Invoice", "=100"),
        "the new agent's snapshot",
    );
    let quiet = std::cell::Cell::new(0);
    agent.until(
        || {
            let idle = cloud.pump(&outbox) == 0 && outbox.depth().unwrap() == 0;
            quiet.set(if idle { quiet.get() + 1 } else { 0 });
            quiet.get() >= 25
        },
        "the snapshot uploaded and capture quiet",
    );
    assert!(
        has(&cloud, "Invoice", "=3"),
        "the snapshot alone leaves the deleted check"
    );
    assert!(has(&cloud, TABLE, "=2"), "and the deleted outlet");

    // One verify cycle: compare, queue the keys, the capture's key-repair job tombstones.
    let catalog = Arc::new(SyncCatalog::load().unwrap().clone());
    let h = handler(
        &outbox,
        state.clone(),
        cloud.clone(),
        SystemClock::shared(),
        catalog,
        Some(Arc::new(FakeRuntimeConfig::new(config())) as _),
        &Logger::none(),
    );
    let first = run(&h, &[]);
    let mismatched: HashSet<&str> = first
        .tables
        .iter()
        .filter(|t| !t.mismatched_buckets.is_empty())
        .map(|t| t.table.as_str())
        .collect();
    assert_eq!(mismatched, HashSet::from(["Invoice", TABLE]), "{first:?}");
    assert_eq!(first.repair, repair::KEYS_QUEUED, "{first:?}");
    assert!(
        first
            .tables
            .iter()
            .filter(|t| !t.mismatched_buckets.is_empty())
            .all(|t| t.repair_keys == 1)
    );
    assert!(
        first
            .skipped
            .iter()
            .any(|s| s.table == "TmpInvoice" && s.reason == skip_reasons::OPEN_CHECK)
    );

    agent.until(
        || {
            cloud.pump(&outbox);
            !has(&cloud, "Invoice", "=3")
                && !has(&cloud, TABLE, "=2")
                && outbox.pending_key_repairs().unwrap().is_empty()
        },
        "the key repair tombstones",
    );
    agent.until(
        || cloud.pump(&outbox) == 0 && outbox.depth().unwrap() == 0,
        "the repair uploaded",
    );
    agent.stop();

    let second = run(&h, &[]);
    assert_eq!(
        (second.mismatched_tables, second.repair.as_str()),
        (0, repair::NONE),
        "{second:?}"
    );
    assert!(
        has(&cloud, "Invoice", "=100"),
        "the closed check is in the mirror"
    );
    // Parity limit (ADR 0126 §3): open-check tables are never compared, so the open-check
    // row of a check closed while no agent ran stays in the cloud's open set.
    assert!(has(&cloud, "TmpInvoice", "=100"));

    // The shadow and the cloud now hold the same rows of every compared table.
    let st = SqliteCaptureState::open(&state_path).unwrap();
    let all: HashSet<i32> = (0..256).collect();
    let mut shadow: Vec<String> =
        shadow_manifest::bucket_rows(&st, "Invoice", 256, &all, &Ctx::background())
            .unwrap()
            .unwrap()
            .into_keys()
            .collect();
    shadow.sort();
    assert_eq!(shadow, cloud.row_keys("Invoice"));
    let _ = std::fs::remove_dir_all(&dir);
}
