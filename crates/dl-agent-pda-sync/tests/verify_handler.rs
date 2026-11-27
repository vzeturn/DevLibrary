//! `sync.verify` (ADR 0126/0128): the agent's shadow against the cloud manifest, per table,
//! with skips and repair. Port of `Commands/VerifyCommandHandlerTests.cs`.

mod capture_runner_support;
mod commands_support;
mod support;
mod verify_support;

use std::sync::Arc;
use std::time::Duration;

use capture_runner_support::FakeRuntimeConfig;
use dl_agent_pda_sync::capture::{CaptureState, InMemoryCaptureState, StateFactory};
use dl_agent_pda_sync::commands::verify::{
    BUDGET, REPAIR_COOLDOWN, SYNC_MANIFEST_KEYS_LIMIT, differing, skip_reasons as skip,
};
use dl_agent_pda_sync::commands::{SiteCommandExecutor, VerifyCommandHandler, reasons};
use dl_agent_pda_sync::config::{RuntimeConfig, codes};
use dl_agent_pda_sync::contracts::time::DateTimeOffset;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, SyncBatch, SyncMode, SyncVerifyPayload, SyncVerifySkippedTable,
    sync_verify_repair as repair, wire_version,
};
use dl_agent_pda_sync::outbox::{EnqueueResult, RECOVERY_RESNAPSHOT_COOLDOWN, SqliteOutbox};
use dl_agent_pda_sync::reporting::{Level, Logger, Value};
use dl_agent_pda_sync::util::ctx::Ctx;
use support::TestEnv;
use verify_support::*;

struct Kit {
    env: TestEnv,
    outbox: Arc<SqliteOutbox>,
    state: Arc<InMemoryCaptureState>,
    cloud: Arc<FakeCloud>,
}

impl Kit {
    fn new() -> Kit {
        let env = TestEnv::new();
        let outbox =
            Arc::new(SqliteOutbox::open(env.paths.outbox_db(), i64::MAX, &Logger::none()).unwrap());
        Kit {
            env,
            outbox,
            state: Arc::new(InMemoryCaptureState::new()),
            cloud: FakeCloud::new(),
        }
    }

    fn now(&self) -> DateTimeOffset {
        DateTimeOffset::from_unix_nanos(
            dl_agent_pda_sync::config::Clock::now(&*self.env.time).unix_nanos(),
            0,
        )
        .unwrap()
    }

    fn handler_with(&self, config: Option<RuntimeConfig>) -> VerifyCommandHandler {
        let state: Arc<dyn CaptureState> = self.state.clone();
        let factory: StateFactory = Arc::new(move || Ok(state.clone()));
        handler(
            &self.outbox,
            factory,
            self.cloud.clone(),
            self.env.time.clone(),
            test_catalog(),
            config.map(|c| Arc::new(FakeRuntimeConfig::new(c)) as _),
            &self.env.log,
        )
    }

    fn handler(&self) -> VerifyCommandHandler {
        self.handler_with(None)
    }

    /// The same rows in the shadow and in the cloud.
    fn both(&self, table: &str, rows: &[(String, String)]) {
        seed(&*self.state, table, rows);
        self.cloud.set_rows(table, rows);
    }

    fn enqueue(&self, stream: &str) {
        let r = self
            .outbox
            .enqueue(
                stream,
                |seq| SyncBatch {
                    agent_id: "agent".into(),
                    stream: stream.into(),
                    seq,
                    mapper_ver: wire_version::MAPPER,
                    table: stream.into(),
                    mode: SyncMode::Snapshot,
                    rows: Vec::new(),
                    tombstones: Vec::new(),
                    watermark: None,
                    hops: Default::default(),
                },
                true,
            )
            .unwrap();
        assert_eq!(r, EnqueueResult::Queued);
    }

    fn pending_resnapshot(&self) -> Vec<String> {
        self.outbox.pending_stream_resnapshot().unwrap()
    }
}

fn skipped(table: &str, reason: &str) -> SyncVerifySkippedTable {
    SyncVerifySkippedTable::new(table, reason)
}

fn sorted(mut v: Vec<SyncVerifySkippedTable>) -> Vec<SyncVerifySkippedTable> {
    v.sort_by(|a, b| a.table.cmp(&b.table));
    v
}

#[test]
fn every_mirrored_table_matches_and_open_check_tables_are_listed_as_skipped() {
    let k = Kit::new();
    k.both(LEDGER, &rows(&["1", "2", "3"]));
    k.both(MASTER, &rows(&["a", "b"]));

    let r = run(&k.handler(), &[]);

    let tables: Vec<_> = r
        .tables
        .iter()
        .map(|t| {
            (
                t.table.as_str(),
                t.store_rows,
                t.cloud_rows,
                t.mismatched_buckets.len(),
                t.repair_scheduled,
            )
        })
        .collect();
    assert_eq!(tables, [(LEDGER, 3, 3, 0, false), (MASTER, 2, 2, 0, false)]);
    assert_eq!(r.skipped, [skipped(OPEN_TABLE, skip::OPEN_CHECK)]);
    assert_eq!(
        (
            r.buckets,
            r.store_rows,
            r.cloud_rows,
            r.mismatched_tables,
            r.mismatched_buckets,
            r.repair.as_str()
        ),
        (256, 5, 5, 0, 0, repair::NONE)
    );
    assert_eq!(*lock(&k.cloud.calls), [LEDGER, MASTER]);
    assert!(k.pending_resnapshot().is_empty());
}

#[test]
fn a_row_the_cloud_holds_differently_or_lacks_is_reported_by_bucket_and_its_stream_resnapshotted() {
    let k = Kit::new();
    *lock(&k.cloud.keys_error) = Some("http_404".into());
    seed(&*k.state, LEDGER, &rows(&["1", "2", "3"]));
    k.cloud.set_rows(
        LEDGER,
        &[(key("1"), "h-1".into()), (key("2"), "stale".into())],
    );
    k.both(MASTER, &rows(&["a"]));

    let r = run(&k.handler(), &[]);

    let invoice = r.tables.iter().find(|t| t.table == LEDGER).unwrap();
    let mut want = vec![bucket_of("2"), bucket_of("3")];
    want.sort_unstable();
    want.dedup();
    assert_eq!(invoice.mismatched_buckets, want);
    assert_eq!(
        (
            invoice.store_rows,
            invoice.cloud_rows,
            invoice.repair_scheduled
        ),
        (3, 2, true)
    );
    assert!(
        !r.tables
            .iter()
            .find(|t| t.table == MASTER)
            .unwrap()
            .repair_scheduled
    );
    assert_eq!(
        (r.mismatched_tables, r.mismatched_buckets, r.repair.as_str()),
        (
            1,
            invoice.mismatched_buckets.len() as i32,
            repair::SCHEDULED
        )
    );
    assert_eq!(k.pending_resnapshot(), [LEDGER]);
    let warnings: Vec<_> = k
        .env
        .sink
        .events()
        .into_iter()
        .filter(|e| {
            e.level == Level::Warning
                && e.property("Code") == Some(&Value::from(codes::SYNC_STREAM_RESNAPSHOT))
        })
        .collect();
    assert_eq!(warnings.len(), 1);

    let again = run(&k.handler(), &[LEDGER]);
    assert!(
        again.tables.is_empty(),
        "the resnapshot it asked for is now in flight"
    );
    assert_eq!(again.skipped, [skipped(LEDGER, skip::STREAM_BUSY)]);
}

#[test]
fn a_mismatch_is_narrowed_to_row_keys_and_queued_for_a_resend_instead_of_a_resnapshot() {
    let k = Kit::new();
    seed(&*k.state, LEDGER, &rows(&["1", "2", "3"]));
    k.cloud.set_rows(
        LEDGER,
        &[
            (key("1"), "h-1".into()),
            (key("2"), "stale".into()),
            (key("4"), "h-4".into()),
        ],
    );

    let r = run(&k.handler(), &[LEDGER]);

    let invoice = &r.tables[0];
    assert_eq!(
        (
            invoice.repair_keys,
            invoice.repair_scheduled,
            r.repair.as_str()
        ),
        (3, true, repair::KEYS_QUEUED)
    );
    let calls = lock(&k.cloud.key_calls).clone();
    assert_eq!(calls.len(), 1);
    let mut asked = calls[0].clone();
    asked.sort_unstable();
    assert_eq!(asked, invoice.mismatched_buckets);
    let mut queued: Vec<String> = k
        .outbox
        .pending_key_repairs()
        .unwrap()
        .into_iter()
        .map(|t| t.row_key)
        .collect();
    queued.sort();
    let mut want: Vec<String> = ["2", "3", "4"].iter().map(|v| row_key_of(v)).collect();
    want.sort();
    assert_eq!(queued, want);
    assert!(
        k.pending_resnapshot().is_empty(),
        "rows only the cloud holds are tombstoned by key, not resnapshotted"
    );

    assert_eq!(
        run(&k.handler(), &[LEDGER]).repair,
        repair::KEYS_QUEUED,
        "queueing the same keys again is harmless"
    );
    assert_eq!(k.outbox.key_repairs_pending(LEDGER).unwrap(), 3);
}

#[test]
fn a_truncated_key_answer_falls_back_to_a_resnapshot() {
    let k = Kit::new();
    seed(&*k.state, LEDGER, &rows(&["1"]));
    k.cloud
        .keys_truncated
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let r = run(&k.handler(), &[LEDGER]);

    assert_eq!(
        (r.tables[0].repair_keys, r.repair.as_str()),
        (0, repair::SCHEDULED)
    );
    assert_eq!(k.pending_resnapshot(), [LEDGER]);
    assert!(k.outbox.pending_key_repairs().unwrap().is_empty());
}

#[test]
fn too_many_mismatched_buckets_skip_the_key_request_and_resnapshot() {
    let k = Kit::new();
    let values: Vec<String> = (0..400).map(|i| i.to_string()).collect();
    let refs: Vec<&str> = values.iter().map(String::as_str).collect();
    seed(&*k.state, LEDGER, &rows(&refs));

    let r = run(&k.handler(), &[LEDGER]);

    assert!(r.tables[0].mismatched_buckets.len() > SYNC_MANIFEST_KEYS_LIMIT);
    assert_eq!(r.repair, repair::SCHEDULED);
    assert!(lock(&k.cloud.key_calls).is_empty());
}

#[test]
fn differing_keys_are_those_on_one_side_only_or_with_another_hash() {
    let m = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    };
    let shadow = m(&[("a", "1"), ("b", "2"), ("c", "3")]);
    let cloud = m(&[("a", "1"), ("b", "x"), ("d", "4")]);

    assert_eq!(differing(&shadow, &cloud), ["b", "c", "d"]);
}

#[test]
fn repair_waits_out_the_recovery_resnapshot_cooldown() {
    let k = Kit::new();
    *lock(&k.cloud.keys_error) = Some("http_404".into());
    seed(&*k.state, LEDGER, &rows(&["1"]));
    let hour_ago = k.now().add_seconds(-3600.0);
    k.outbox
        .request_recovery_resnapshot("test", hour_ago)
        .unwrap();
    k.outbox.clear_needs_resnapshot().unwrap();

    let cooling = run(&k.handler(), &[LEDGER]);

    assert_eq!(cooling.repair, repair::COOLDOWN);
    assert_eq!(cooling.tables[0].mismatched_buckets.len(), 1);
    assert!(!cooling.tables[0].repair_scheduled);
    assert!(k.pending_resnapshot().is_empty());

    k.env.time.advance(RECOVERY_RESNAPSHOT_COOLDOWN);
    k.outbox.defer_recovery_resnapshot().unwrap();
    assert_eq!(
        run(&k.handler(), &[LEDGER]).repair,
        repair::COOLDOWN,
        "a deferred recovery resnapshot is still owed"
    );
    assert!(k.pending_resnapshot().is_empty());
}

#[test]
fn a_stream_resnapshotted_within_a_week_is_not_resnapshotted_again() {
    let k = Kit::new();
    *lock(&k.cloud.keys_error) = Some("http_404".into());
    seed(&*k.state, LEDGER, &rows(&["1"]));
    k.outbox
        .request_stream_resnapshot(&[LEDGER], "earlier", k.now())
        .unwrap();
    k.outbox
        .begin_stream_resnapshot(&[LEDGER], k.now())
        .unwrap();
    k.enqueue(LEDGER);
    k.outbox.stream_resnapshot_captured().unwrap();
    for item in k.outbox.peek::<&str>(10, &[]).unwrap() {
        k.outbox.ack(item.id).unwrap();
    }
    assert_eq!(
        k.outbox.finished_stream_resnapshots(k.now()).unwrap().len(),
        1
    );

    k.env.time.advance(Duration::from_secs(24 * 3600));
    assert_eq!(run(&k.handler(), &[LEDGER]).repair, repair::COOLDOWN);
    assert!(k.pending_resnapshot().is_empty());

    k.env.time.advance(REPAIR_COOLDOWN);
    assert_eq!(run(&k.handler(), &[LEDGER]).repair, repair::SCHEDULED);
    assert_eq!(k.pending_resnapshot(), [LEDGER]);
}

#[test]
fn requested_tables_answer_in_catalog_spelling_and_list_what_cannot_be_compared() {
    let k = Kit::new();
    k.both(LEDGER, &rows(&["1"]));

    let r = run(
        &k.handler(),
        &[
            "invoice", "Nope", UNMIRRORED, OPEN_TABLE, SKIPPED, "INVOICE",
        ],
    );

    assert_eq!(
        r.tables
            .iter()
            .map(|t| t.table.as_str())
            .collect::<Vec<_>>(),
        [LEDGER]
    );
    assert_eq!(
        sorted(r.skipped),
        sorted(vec![
            skipped("Nope", skip::UNKNOWN_TABLE),
            skipped(UNMIRRORED, skip::NOT_MIRRORED),
            skipped(SKIPPED, skip::NOT_MIRRORED),
            skipped(OPEN_TABLE, skip::OPEN_CHECK),
        ])
    );
}

#[test]
fn only_tables_the_agent_captures_under_the_allowlist_are_compared() {
    let k = Kit::new();
    k.both(MASTER, &rows(&["a"]));
    let config = || RuntimeConfig {
        table_allowlist: Some(vec![Some(MASTER.to_uppercase())]),
        ..RuntimeConfig::default()
    };

    let all = run(&k.handler_with(Some(config())), &[]);
    let named = run(&k.handler_with(Some(config())), &[LEDGER]);

    assert_eq!(
        all.tables
            .iter()
            .map(|t| t.table.as_str())
            .collect::<Vec<_>>(),
        [MASTER]
    );
    assert!(all.skipped.is_empty());
    assert!(named.tables.is_empty());
    assert_eq!(named.skipped, [skipped(LEDGER, skip::NOT_CAPTURED)]);
}

#[test]
fn a_stream_with_batches_queued_before_or_during_the_comparison_is_left_out() {
    let k = Arc::new(Kit::new());
    k.both(LEDGER, &rows(&["1"]));
    k.both(MASTER, &rows(&["a"]));
    k.enqueue(MASTER);
    let outbox = k.outbox.clone();
    *lock(&k.cloud.on_call) = Some(Box::new(move |table| {
        if table == LEDGER {
            outbox
                .enqueue(
                    LEDGER,
                    |seq| SyncBatch {
                        agent_id: "agent".into(),
                        stream: LEDGER.into(),
                        seq,
                        mapper_ver: wire_version::MAPPER,
                        table: LEDGER.into(),
                        mode: SyncMode::Snapshot,
                        rows: Vec::new(),
                        tombstones: Vec::new(),
                        watermark: None,
                        hops: Default::default(),
                    },
                    true,
                )
                .unwrap();
        }
    }));

    let r = run(&k.handler(), &[]);

    assert!(r.tables.is_empty());
    assert_eq!(
        sorted(r.skipped),
        sorted(vec![
            skipped(LEDGER, skip::STREAM_BUSY),
            skipped(MASTER, skip::STREAM_BUSY),
            skipped(OPEN_TABLE, skip::OPEN_CHECK),
        ])
    );
    assert_eq!(
        *lock(&k.cloud.calls),
        [LEDGER],
        "a stream already busy is not even fetched"
    );
    assert_eq!(r.repair, repair::NONE);
    *lock(&k.cloud.on_call) = None;
}

#[test]
fn every_table_waits_while_a_full_resnapshot_is_owed() {
    let k = Kit::new();
    seed(&*k.state, LEDGER, &rows(&["1"]));
    k.outbox.request_resnapshot("test").unwrap();

    let r = run(&k.handler(), &[LEDGER, MASTER]);

    assert!(r.tables.is_empty());
    assert_eq!(r.skipped.len(), 2);
    assert!(
        r.skipped
            .iter()
            .all(|s| s.reason == skip::RESNAPSHOT_PENDING)
    );
    assert!(lock(&k.cloud.calls).is_empty());
}

#[test]
fn duplicate_key_occurrences_leave_the_table_out() {
    let k = Kit::new();
    seed(
        &*k.state,
        LEDGER,
        &[
            (key("1"), "h1".into()),
            (format!("{}#1", key("1")), "h1b".into()),
        ],
    );

    let r = run(&k.handler(), &[LEDGER]);

    assert_eq!(r.skipped, [skipped(LEDGER, skip::DUPLICATE_KEYS)]);
    assert!(lock(&k.cloud.calls).is_empty());
}

#[test]
fn a_cloud_refusal_or_outage_is_listed_per_table_and_the_refusal_logged() {
    let k = Kit::new();
    k.both(MASTER, &rows(&["a"]));
    lock(&k.cloud.errors).insert(LEDGER.into(), "http_409".into());
    lock(&k.cloud.errors).insert(MASTER.into(), "unreachable".into());

    let r = run(&k.handler(), &[LEDGER, MASTER]);

    assert_eq!(
        r.skipped,
        [
            skipped(LEDGER, "manifest_http_409"),
            skipped(MASTER, "manifest_unreachable")
        ]
    );
    let warnings = k.env.sink.messages_at(Level::Warning);
    assert_eq!(
        warnings.iter().filter(|m| m.contains("http_409")).count(),
        1,
        "{warnings:?}"
    );
    assert!(
        !warnings.iter().any(|m| m.contains("unreachable")),
        "{warnings:?}"
    );
}

#[test]
fn a_run_over_every_table_stops_at_the_budget_and_the_next_run_starts_where_it_stopped() {
    let k = Kit::new();
    k.both(LEDGER, &rows(&["1"]));
    k.both(MASTER, &rows(&["a"]));
    let time = k.env.time.clone();
    *lock(&k.cloud.on_call) = Some(Box::new(move |_| {
        time.advance(BUDGET + Duration::from_secs(1))
    }));

    let first = run(&k.handler(), &[]);
    let named = run(&k.handler(), &[LEDGER, MASTER]);
    let second = run(&k.handler(), &[]);
    let third = run(&k.handler(), &[]);

    let names = |r: &dl_agent_pda_sync::contracts::wire::SyncVerifyCommandResult| {
        r.tables.iter().map(|t| t.table.clone()).collect::<Vec<_>>()
    };
    assert_eq!(names(&first), [LEDGER]);
    assert!(first.skipped.contains(&skipped(MASTER, skip::TIME_BUDGET)));
    assert_eq!(
        names(&named),
        [LEDGER],
        "a named list keeps its order and leaves the cursor alone"
    );
    assert_eq!(names(&second), [MASTER]);
    assert!(second.skipped.contains(&skipped(LEDGER, skip::TIME_BUDGET)));
    assert_eq!(names(&third), [LEDGER]);
}

#[test]
fn an_invalid_payload_is_rejected_without_reading_anything() {
    for (buckets, reason, problem) in [
        (0, "nightly", "buckets must be 1..256"),
        (257, "nightly", "buckets must be 1..256"),
        (256, " ", "reason is required"),
    ] {
        let k = Kit::new();
        let cmd = verify_command(&SyncVerifyPayload {
            tables: Vec::new(),
            buckets,
            reason: reason.into(),
        });

        let r = k.handler().execute(&cmd, &Ctx::background()).unwrap();

        assert_eq!(r.status, CommandStatus::Rejected);
        assert_eq!(
            r.reason.unwrap(),
            format!("{}: {problem}", reasons::INVALID_PAYLOAD)
        );
        assert!(lock(&k.cloud.calls).is_empty());
    }
}

#[test]
fn the_executor_routes_the_command_to_the_handler_and_rejects_it_without_one() {
    let k = Kit::new();
    let store = commands_support::FakeStore::new();
    let with = SiteCommandExecutor::new(store.clone(), k.env.time.clone(), &Logger::none())
        .with_verify(Arc::new(k.handler()));
    let without = SiteCommandExecutor::new(store, k.env.time.clone(), &Logger::none());
    let ctx = Ctx::background();

    assert_eq!(
        with.execute(&verify(&[LEDGER]), &ctx).unwrap().status,
        CommandStatus::Applied
    );
    let refused = without.execute(&verify(&[LEDGER]), &ctx).unwrap();

    assert_eq!(refused.status, CommandStatus::Rejected);
    assert_eq!(refused.reason.as_deref(), Some(reasons::UNKNOWN_TYPE));
}
