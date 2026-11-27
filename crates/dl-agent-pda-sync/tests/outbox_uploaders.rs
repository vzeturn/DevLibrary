//! Uploaders against the fake cloud: the uploader cases of TransportTests,
//! ParentPendingUploadTests and BlobUploaderTests.

mod outbox_support;
mod support;

use std::sync::Arc;
use std::time::Duration;

use dl_agent_pda_sync::config::AgentIdentity;
use dl_agent_pda_sync::config::codes;
use dl_agent_pda_sync::config::time::SharedClock;
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::{
    SyncBatch, SyncBlobRef, SyncMode, SyncRow, sync_streams, wire_version,
};
use dl_agent_pda_sync::contracts::{StrMap, SyncCatalog};
use dl_agent_pda_sync::outbox::{
    BATCH_CATEGORY, BatchUploader, BlobUploader, EnqueueResult, ParentPendingTracker,
    RECOVERY_RESNAPSHOT_COOLDOWN, SqliteBlobOutbox, SqliteOutbox, UploadCycleResult,
};
use dl_agent_pda_sync::reporting::{Event, Level, Value};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_hash::{Digest, Sha256};
use outbox_support::{Agent, FakeCloud, agent};
use support::TestEnv;

const CHILD: &str = "Trn";
const PARENT: &str = "Invoice";
const STEP: Duration = ParentPendingTracker::MAX_BACKOFF;

struct Fx {
    env: TestEnv,
    cloud: FakeCloud,
    agent: Agent,
    outbox: Arc<SqliteOutbox>,
    uploader: BatchUploader,
    agent_id: String,
}

impl Fx {
    fn new() -> Fx {
        Fx::with_cap(i64::MAX)
    }

    /// A fixture whose outbox refuses batches beyond `cap_bytes` stored payload bytes.
    fn with_cap(cap_bytes: i64) -> Fx {
        let env = TestEnv::new();
        let cloud = FakeCloud::start();
        let agent = agent(&env, &cloud);
        let outbox =
            Arc::new(SqliteOutbox::open(env.paths.outbox_db(), cap_bytes, &env.log).unwrap());
        let uploader = BatchUploader::new(
            outbox.clone(),
            agent.http.clone(),
            agent.tokens.clone(),
            &env.log,
            Some(env.time.clone() as SharedClock),
            Some(15),
        );
        let agent_id = cloud.agent_id.to_string();
        Fx {
            env,
            cloud,
            agent,
            outbox,
            uploader,
            agent_id,
        }
    }

    fn batch(&self, seq: i64, stream: &str, table: &str, watermark: Option<&str>) -> SyncBatch {
        let key: StrMap = [("id".to_owned(), Some(seq.to_string()))]
            .into_iter()
            .collect();
        SyncBatch {
            agent_id: self.agent_id.clone(),
            stream: stream.into(),
            seq,
            mapper_ver: wire_version::MAPPER,
            table: table.into(),
            mode: SyncMode::Delta,
            rows: vec![SyncRow {
                key,
                columns: StrMap::new(),
                hash: "h".into(),
                src_ver: seq,
            }],
            tombstones: vec![],
            watermark: watermark.map(str::to_owned),
        }
    }

    /// TransportTests.Enqueue: stream `open_checks`, table `Invoice`.
    fn enqueue(&self) {
        self.enqueue_to("open_checks", "Invoice");
    }

    /// ParentPendingUploadTests.Enqueue: the table is the stream.
    fn enqueue_stream(&self, stream: &str) {
        self.enqueue_to(stream, stream);
    }

    fn enqueue_to(&self, stream: &str, table: &str) {
        let r = self
            .outbox
            .enqueue(stream, |seq| self.batch(seq, stream, table, None), true)
            .unwrap();
        assert_eq!(r, EnqueueResult::Queued);
    }

    fn cycle(&mut self) -> UploadCycleResult {
        self.uploader.run_cycle(10, &Ctx::background()).unwrap()
    }

    fn advance(&self, d: Duration) {
        self.env.time.advance(d);
    }

    fn now(&self) -> dl_agent_pda_sync::contracts::time::DateTimeOffset {
        use dl_agent_pda_sync::config::Clock;
        let t = self.env.time.now();
        dl_agent_pda_sync::contracts::time::DateTimeOffset::from_unix_nanos(t.unix_nanos(), 0)
            .unwrap()
    }

    /// Retries the parked stream every [`STEP`] until `until` has passed.
    fn wait_parked(&mut self, until: Duration) {
        let mut waited = Duration::ZERO;
        while waited < until {
            self.advance(STEP);
            self.cycle();
            waited += STEP;
        }
    }

    fn uploader_events(&self) -> Vec<Event> {
        self.env
            .sink
            .events()
            .into_iter()
            .filter(|e| e.property("SourceContext") == Some(&Value::from(BATCH_CATEGORY)))
            .collect()
    }

    fn codes(&self) -> Vec<String> {
        self.uploader_events()
            .iter()
            .filter_map(|e| {
                e.property("Code")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect()
    }

    fn episode_reports(&self) -> Vec<Event> {
        self.uploader_events()
            .into_iter()
            .filter(|e| {
                e.level >= Level::Warning
                    && e.property("Code") == Some(&Value::from(codes::SYNC_PARENT_PENDING))
            })
            .collect()
    }

    fn errors(&self) -> Vec<Event> {
        self.uploader_events()
            .into_iter()
            .filter(|e| e.level == Level::Error)
            .collect()
    }
}

fn s(v: &str) -> Value {
    Value::from(v)
}

// ---- TransportTests ----

#[test]
fn uploader_sends_queued_batches_in_order_and_empties_the_outbox() {
    let mut fx = Fx::new();
    fx.enqueue();
    fx.enqueue();
    fx.enqueue();
    assert_eq!(fx.cycle(), UploadCycleResult::Progress);
    assert_eq!(fx.cloud.applied_seqs(), [1, 2, 3]);
    assert_eq!(fx.outbox.depth().unwrap(), 0);
    assert_eq!(fx.cycle(), UploadCycleResult::Idle);
    assert!(fx.cloud.with(|c| c.rejections.is_empty()));
}

#[test]
fn outbox_survives_a_restart_and_the_uploader_continues() {
    let mut fx = Fx::new();
    for w in ["inv:100", "inv:200"] {
        fx.outbox
            .enqueue(
                "closed_ledger",
                |seq| fx.batch(seq, "closed_ledger", "Invoice", Some(w)),
                true,
            )
            .unwrap();
    }
    let reopened =
        Arc::new(SqliteOutbox::open(fx.env.paths.outbox_db(), i64::MAX, &fx.env.log).unwrap());
    fx.outbox = reopened.clone();
    fx.outbox
        .enqueue(
            "closed_ledger",
            |seq| fx.batch(seq, "closed_ledger", "Invoice", None),
            true,
        )
        .unwrap();
    fx.uploader = BatchUploader::new(
        reopened,
        fx.agent.http.clone(),
        fx.agent.tokens.clone(),
        &fx.env.log,
        Some(fx.env.time.clone() as SharedClock),
        None,
    );
    fx.cycle();
    assert_eq!(fx.cloud.applied_seqs(), [1, 2, 3]);
    assert_eq!(
        fx.outbox.watermark("closed_ledger").unwrap().as_deref(),
        Some("inv:200")
    );
}

#[test]
fn transient_failures_retry_with_growing_backoff_then_succeed() {
    let mut fx = Fx::new();
    fx.enqueue();
    fx.cloud.with(|c| c.fail_next_batches = 2);
    assert_eq!(fx.cycle(), UploadCycleResult::Progress);
    assert_eq!(fx.cloud.applied_seqs().len(), 1);
    let sleeps = fx.env.sleeps();
    assert_eq!(sleeps.len(), 2);
    assert!(sleeps[1] > sleeps[0]);
    assert!(
        !fx.cloud
            .with(|c| c.rejections.contains(&"replay".to_owned()))
    );
}

#[test]
fn persistent_outage_keeps_the_batch_queued_and_reports_blocked() {
    let mut fx = Fx::new();
    fx.enqueue();
    fx.cloud.with(|c| c.fail_next_batches = 100);
    assert_eq!(fx.cycle(), UploadCycleResult::Blocked);
    assert_eq!(fx.outbox.depth().unwrap(), 1);
    assert!(fx.cloud.applied_seqs().is_empty());
}

#[test]
fn duplicate_seq_is_acked_by_the_cloud_and_removed_locally() {
    let mut fx = Fx::new();
    fx.cloud
        .with(|c| c.last_seq.insert("open_checks".into(), 5));
    fx.enqueue(); // seq 1 <= cloud 5 -> duplicate
    fx.cycle();
    assert_eq!(fx.outbox.depth().unwrap(), 0);
    assert!(fx.cloud.applied_seqs().is_empty());
}

#[test]
fn seq_gap_realigns_queue_and_requests_resnapshot_when_expected_batch_is_gone() {
    let mut fx = Fx::new();
    fx.enqueue();
    fx.enqueue();
    fx.outbox.ack_below("open_checks", 2).unwrap(); // the agent lost seq 1 while the cloud still expects it
    fx.cycle();
    assert!(fx.outbox.needs_resnapshot().unwrap());
    fx.cycle();
    assert_eq!(
        fx.cloud.applied_seqs(),
        [1],
        "the renumbered batch goes as #1"
    );
}

#[test]
fn permanently_rejected_batch_is_quarantined_not_dropped() {
    let mut fx = Fx::new();
    fx.enqueue();
    fx.cloud.with(|c| {
        c.fail_next_batches = 1;
        c.fail_status = 422;
    });
    fx.cycle();
    assert_eq!(fx.outbox.quarantined_count().unwrap(), 1);
    assert!(fx.outbox.needs_resnapshot().unwrap());
}

#[test]
fn permanently_rejected_batch_reports_the_cloud_code_reason_and_table() {
    let mut fx = Fx::new();
    fx.enqueue();
    fx.cloud.with(|c| {
        c.fail_next_batches = 1;
        c.fail_status = 400;
        c.fail_body = Some(
            r#"{"result":400,"code":"VALIDATION_FAILED","message":"Unknown column 'x_col' for app.invoices."}"#.into(),
        );
    });
    fx.cycle();
    let errors = fx.errors();
    assert_eq!(errors.len(), 1);
    let e = &errors[0];
    assert_eq!(e.property("Code"), Some(&s(codes::SYNC_BATCH_QUARANTINED)));
    assert_eq!(e.property("RejectCode"), Some(&s("VALIDATION_FAILED")));
    assert_eq!(
        e.property("Reason"),
        Some(&s("Unknown column 'x_col' for app.invoices."))
    );
    assert_eq!(e.property("Table"), Some(&s("Invoice")));
    assert_eq!(e.property("Stream"), Some(&s("open_checks")));
    assert_eq!(e.property("Seq"), Some(&Value::Int(1)));
    assert_eq!(e.property("Status"), Some(&Value::Int(400)));
}

#[test]
fn a_gap_at_a_quarantined_batch_heals_by_renumbering_the_stream() {
    let mut fx = Fx::new();
    fx.enqueue();
    fx.enqueue();
    fx.enqueue();
    fx.cloud.with(|c| {
        c.fail_next_batches = 1;
        c.fail_status = 400;
    });
    fx.cycle(); // #1 quarantined, #2 answers 409 expected 1: renumbered
    fx.cycle(); // old #2 and #3 go as #1 and #2

    let applied: Vec<(i64, String)> = fx.cloud.with(|c| {
        c.applied
            .iter()
            .map(|b| (b.seq, b.rows[0].key.get("id").cloned().flatten().unwrap()))
            .collect()
    });
    assert_eq!(applied, [(1, "2".to_owned()), (2, "3".to_owned())]);
    assert_eq!(fx.outbox.depth().unwrap(), 0);
    assert_eq!(
        fx.outbox.quarantined_count().unwrap(),
        1,
        "the rejected batch is kept for diagnostics"
    );
    assert_eq!(fx.outbox.last_seq("open_checks").unwrap(), 2);
    fx.enqueue();
    fx.cycle();
    assert_eq!(
        fx.cloud.applied_seqs().last(),
        Some(&3),
        "new batches continue the renumbered sequence"
    );

    let codes = fx.codes();
    let pos = |c: &str| codes.iter().position(|x| x == c).unwrap();
    assert!(pos(codes::SYNC_BATCH_QUARANTINED) < pos(codes::SYNC_STREAM_GAP));
    assert!(pos(codes::SYNC_STREAM_GAP) < pos(codes::SYNC_STREAM_RECOVERED));
    assert_eq!(
        codes
            .iter()
            .filter(|c| *c == codes::SYNC_STREAM_GAP)
            .count(),
        1,
        "one gap, healed once"
    );
    assert!(
        fx.outbox.needs_resnapshot().unwrap(),
        "the quarantined rows come back through a resnapshot"
    );
}

#[test]
fn recovery_resnapshots_are_paced_by_the_cooldown() {
    let mut fx = Fx::new();
    fx.enqueue();
    fx.cloud.with(|c| {
        c.fail_next_batches = 1;
        c.fail_status = 400;
    });
    fx.cycle();
    assert!(fx.outbox.needs_resnapshot().unwrap());
    fx.outbox.clear_needs_resnapshot().unwrap(); // the capture runner took it

    fx.enqueue();
    fx.cloud.with(|c| c.fail_next_batches = 1);
    fx.cycle(); // the rejection persists: a second quarantine within the cooldown
    assert!(
        !fx.outbox.needs_resnapshot().unwrap(),
        "back-to-back full re-reads of SMILE_POS are held back"
    );
    assert!(fx.outbox.recovery_resnapshot_deferred().unwrap());

    fx.advance(RECOVERY_RESNAPSHOT_COOLDOWN);
    fx.cycle();
    assert!(
        fx.outbox.needs_resnapshot().unwrap(),
        "the deferred resnapshot is requested once the cooldown has passed"
    );
    assert!(!fx.outbox.recovery_resnapshot_deferred().unwrap());
}

#[test]
fn expired_access_token_is_refreshed_transparently_during_upload() {
    let mut fx = Fx::new();
    fx.cloud.with(|c| c.token_lifetime_seconds = 120);
    fx.enqueue();
    fx.cycle();
    fx.advance(Duration::from_secs(300));
    fx.enqueue();
    fx.cycle();
    assert!(fx.cloud.with(|c| c.token_counter) > 1);
    assert_eq!(fx.cloud.applied_seqs(), [1, 2]);
}

// ---- ParentPendingUploadTests ----

#[test]
fn a_parent_pending_answer_parks_the_stream_while_the_other_streams_flow() {
    let mut fx = Fx::new();
    fx.cloud
        .with(|c| c.parent_pending.insert(CHILD.into(), PARENT.into()));
    fx.enqueue_stream(CHILD);
    fx.enqueue_stream(CHILD);
    fx.enqueue_stream(PARENT);

    assert_eq!(fx.cycle(), UploadCycleResult::Progress);

    assert_eq!(
        fx.cloud.with(|c| c
            .applied
            .iter()
            .map(|b| b.stream.clone())
            .collect::<Vec<_>>()),
        [PARENT]
    );
    assert_eq!(
        fx.cloud.with(|c| c.parent_pending_answers),
        1,
        "the stream's later batch is not sent past the one that waits"
    );
    assert_eq!(fx.outbox.depth().unwrap(), 2);
    assert_eq!(fx.outbox.quarantined_count().unwrap(), 0);
    assert!(!fx.outbox.needs_resnapshot().unwrap());
    assert_eq!(
        fx.cycle(),
        UploadCycleResult::Idle,
        "the parked stream waits out its backoff"
    );
    assert_eq!(fx.cloud.with(|c| c.parent_pending_answers), 1);

    fx.cloud.with(|c| c.parent_pending.clear());
    fx.advance(ParentPendingTracker::FIRST_BACKOFF);
    assert_eq!(fx.cycle(), UploadCycleResult::Progress);

    let child: Vec<i64> = fx.cloud.with(|c| {
        c.applied
            .iter()
            .filter(|b| b.stream == CHILD)
            .map(|b| b.seq)
            .collect()
    });
    assert_eq!(child, [1, 2]);
    assert_eq!(fx.outbox.depth().unwrap(), 0);
    let reports = fx.episode_reports();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].property("Outcome"), Some(&s("resolved")));
    assert_eq!(reports[0].property("ParentStream"), Some(&s(PARENT)));
    assert_eq!(reports[0].property("Attempts"), Some(&Value::Int(1)));
    assert!(fx.outbox.pending_stream_resnapshot().unwrap().is_empty());
}

#[test]
fn an_episode_that_outlives_the_bound_resends_the_parent_then_the_stream_and_is_reported_once() {
    let mut fx = Fx::new();
    fx.cloud
        .with(|c| c.parent_pending.insert(CHILD.into(), PARENT.into()));
    fx.enqueue_stream(CHILD);
    fx.cycle();

    fx.wait_parked(Duration::from_secs(14 * 60));
    assert!(
        fx.outbox.pending_stream_resnapshot().unwrap().is_empty(),
        "the episode is younger than the bound"
    );
    assert!(fx.episode_reports().is_empty(), "retries are not reported");

    fx.wait_parked(STEP);
    assert_eq!(
        fx.outbox.pending_stream_resnapshot().unwrap(),
        [PARENT, CHILD]
    );
    fx.wait_parked(Duration::from_secs(10 * 60));
    let reports = fx.episode_reports();
    assert_eq!(reports.len(), 1);
    assert_eq!(
        reports[0].property("Outcome"),
        Some(&s("resnapshot_requested"))
    );
    assert_eq!(reports[0].property("Stream"), Some(&s(CHILD)));
    assert_eq!(
        fx.outbox.quarantined_count().unwrap(),
        0,
        "a wait is never a quarantine"
    );
    assert!(
        !fx.outbox.needs_resnapshot().unwrap(),
        "only the two streams are resent, not the whole store"
    );

    fx.cloud.with(|c| c.parent_pending.clear());
    fx.wait_parked(STEP);
    assert_eq!(
        fx.cloud.with(|c| c
            .applied
            .iter()
            .map(|b| b.stream.clone())
            .collect::<Vec<_>>()),
        [CHILD]
    );
    assert_eq!(
        fx.episode_reports().len(),
        1,
        "the episode was reported when it escalated"
    );
}

#[test]
fn a_parent_resent_in_full_that_still_lacks_the_rows_turns_the_wait_into_a_rejection() {
    let mut fx = Fx::new();
    fx.cloud
        .with(|c| c.parent_pending.insert(CHILD.into(), PARENT.into()));
    fx.enqueue_stream(CHILD);
    fx.cycle();
    fx.wait_parked(Duration::from_secs(16 * 60));
    assert_eq!(
        fx.outbox.pending_stream_resnapshot().unwrap(),
        [PARENT, CHILD]
    );

    // Capture resends both streams; the parent's batch is applied, so its resnapshot finishes clean.
    fx.outbox
        .begin_stream_resnapshot(&[PARENT, CHILD], fx.now())
        .unwrap();
    fx.enqueue_stream(PARENT);
    fx.outbox.stream_resnapshot_captured().unwrap();
    fx.cycle();
    let done: Vec<(String, i64)> = fx
        .outbox
        .finished_stream_resnapshots(fx.now())
        .unwrap()
        .into_iter()
        .map(|r| (r.stream, r.through_seq))
        .collect();
    assert_eq!(done, [(PARENT.to_owned(), 1)]);

    fx.wait_parked(Duration::from_secs(14 * 60));
    assert_eq!(
        fx.outbox.quarantined_count().unwrap(),
        0,
        "the resent parent may still be on its way"
    );
    fx.wait_parked(STEP);

    assert_eq!(fx.outbox.quarantined_count().unwrap(), 1);
    assert_eq!(fx.outbox.depth().unwrap(), 0);
    let errors = fx.errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(
        errors[0].property("Code"),
        Some(&s(codes::SYNC_BATCH_QUARANTINED))
    );
    assert_eq!(
        errors[0].property("RejectCode"),
        Some(&s("SYNC_PARENT_GONE"))
    );
    assert_eq!(errors[0].property("Status"), Some(&Value::Int(424)));
    assert!(
        !fx.outbox
            .finished_stream_resnapshots(fx.now())
            .unwrap()
            .iter()
            .any(|r| r.stream == CHILD),
        "the child's resnapshot quarantined a batch, so it is not clean"
    );
}

fn parent_resent_and_still_missing(fx: &mut Fx) {
    fx.cycle();
    fx.wait_parked(Duration::from_secs(16 * 60));
    fx.outbox
        .begin_stream_resnapshot(&[PARENT, CHILD], fx.now())
        .unwrap();
    fx.enqueue_stream(PARENT);
    fx.outbox.stream_resnapshot_captured().unwrap();
    fx.cycle();
    fx.outbox.finished_stream_resnapshots(fx.now()).unwrap(); // the heartbeat's call: the parent's resnapshot is done
    fx.wait_parked(Duration::from_secs(16 * 60));
}

#[test]
fn a_parent_gone_batch_is_sent_once_more_skipping_orphans_and_is_not_quarantined() {
    let mut fx = Fx::new();
    fx.cloud.with(|c| {
        c.parent_pending.insert(CHILD.into(), PARENT.into());
        c.honor_skip_orphans = true;
    });
    fx.enqueue_stream(CHILD);

    parent_resent_and_still_missing(&mut fx);

    assert_eq!(fx.outbox.quarantined_count().unwrap(), 0);
    assert_eq!(fx.outbox.depth().unwrap(), 0);
    let skips: Vec<(String, i64)> = fx.cloud.with(|c| {
        c.orphan_skips
            .iter()
            .map(|b| (b.stream.clone(), b.seq))
            .collect()
    });
    assert_eq!(skips, [(CHILD.to_owned(), 1)]);
    let skipped = fx
        .uploader_events()
        .into_iter()
        .filter(|e| {
            e.level == Level::Warning
                && e.property("Code") == Some(&s(sync_streams::ORPHANS_SKIPPED_CODE))
        })
        .count();
    assert_eq!(skipped, 1);
    assert!(fx.errors().is_empty());
}

fn orphan_skip_seqs(fx: &Fx) -> Vec<i64> {
    fx.cloud
        .with(|c| c.orphan_skips.iter().map(|b| b.seq).collect())
}

#[test]
fn a_full_outbox_that_cannot_take_the_parent_resend_skips_the_orphans_after_the_escalation() {
    let probe = Fx::new();
    let cap = i64::try_from(probe.batch(1, CHILD, CHILD, None).to_json().len()).unwrap();
    let mut fx = Fx::with_cap(cap);
    fx.cloud.with(|c| {
        c.parent_pending.insert(CHILD.into(), PARENT.into());
        c.honor_skip_orphans = true;
    });
    fx.enqueue_stream(CHILD);
    assert!(fx.outbox.is_full().unwrap());
    fx.cycle();

    fx.wait_parked(Duration::from_secs(14 * 60));
    assert!(orphan_skip_seqs(&fx).is_empty(), "not escalated yet");
    // The escalation asks for the resend; the parent never finishes (capture is refused), so the next attempt skips.
    fx.wait_parked(STEP * 3);

    assert_eq!(orphan_skip_seqs(&fx), [1]);
    assert_eq!(fx.outbox.depth().unwrap(), 0);
    assert_eq!(fx.outbox.quarantined_count().unwrap(), 0);
    assert!(fx.errors().is_empty());
    assert!(!fx.outbox.is_full().unwrap());
}

#[test]
fn an_outbox_a_few_bytes_under_the_cap_after_a_refusal_skips_the_orphans_at_the_next_attempt() {
    let probe = Fx::new();
    let len = i64::try_from(probe.batch(1, CHILD, CHILD, None).to_json().len()).unwrap();
    let mut fx = Fx::with_cap(len + 3);
    fx.cloud.with(|c| {
        c.parent_pending.insert(CHILD.into(), PARENT.into());
        c.honor_skip_orphans = true;
    });
    fx.enqueue_stream(CHILD);
    assert!(!fx.outbox.is_full().unwrap(), "a few bytes under the cap");
    let refused = fx
        .outbox
        .enqueue(PARENT, |seq| fx.batch(seq, PARENT, PARENT, None), false)
        .unwrap();
    assert_eq!(refused, EnqueueResult::CapReached);
    assert!(fx.outbox.is_full().unwrap());
    fx.cycle();

    fx.wait_parked(Duration::from_secs(14 * 60));
    assert!(orphan_skip_seqs(&fx).is_empty(), "not escalated yet");
    fx.wait_parked(STEP * 3);

    assert_eq!(orphan_skip_seqs(&fx), [1]);
    assert_eq!(fx.outbox.depth().unwrap(), 0);
    assert!(fx.errors().is_empty());
}

#[test]
fn an_escalated_stream_with_room_in_the_outbox_skips_orphans_only_after_twice_the_bound() {
    let mut fx = Fx::new();
    fx.cloud.with(|c| {
        c.parent_pending.insert(CHILD.into(), PARENT.into());
        c.honor_skip_orphans = true;
    });
    fx.enqueue_stream(CHILD);
    fx.cycle();
    fx.wait_parked(Duration::from_secs(16 * 60)); // escalated, the parent resnapshot never finishes
    assert_eq!(
        fx.outbox.pending_stream_resnapshot().unwrap(),
        [PARENT, CHILD]
    );

    fx.wait_parked(Duration::from_secs(28 * 60));
    assert!(orphan_skip_seqs(&fx).is_empty(), "inside 2 x the bound");
    assert_eq!(fx.outbox.depth().unwrap(), 1);

    fx.wait_parked(STEP * 2);
    assert_eq!(orphan_skip_seqs(&fx), [1]);
    assert_eq!(fx.outbox.depth().unwrap(), 0);
    assert_eq!(fx.outbox.quarantined_count().unwrap(), 0);
}

#[test]
fn a_stream_whose_orphans_were_skipped_skips_them_again_after_a_short_wait() {
    let mut fx = Fx::new();
    fx.cloud.with(|c| {
        c.parent_pending.insert(CHILD.into(), PARENT.into());
        c.honor_skip_orphans = true;
    });
    fx.enqueue_stream(CHILD);
    parent_resent_and_still_missing(&mut fx);

    // The next resend of the stream carries the same orphans: no second escalation, no second parent resend.
    fx.enqueue_stream(CHILD);
    fx.cycle();
    fx.wait_parked(STEP * 2);

    let seqs: Vec<i64> = fx
        .cloud
        .with(|c| c.orphan_skips.iter().map(|b| b.seq).collect());
    assert_eq!(seqs, [1, 2]);
    assert!(fx.outbox.pending_stream_resnapshot().unwrap().is_empty());
    assert_eq!(fx.outbox.quarantined_count().unwrap(), 0);
}

#[test]
fn a_424_without_the_parent_pending_code_is_still_quarantined() {
    assert!(
        !dl_agent_pda_sync::transport::retry::is_transient(424),
        "a 424 is never retried in place"
    );
    let mut fx = Fx::new();
    fx.enqueue_stream(CHILD);
    fx.cloud.with(|c| {
        c.fail_next_batches = 1;
        c.fail_status = 424;
        c.fail_body = Some(r#"{"result":424,"code":"SOMETHING_ELSE"}"#.into());
    });
    fx.cycle();
    assert_eq!(fx.outbox.quarantined_count().unwrap(), 1);
    assert!(fx.episode_reports().is_empty());
}

// ---- BlobUploaderTests ----

fn sha(content: &[u8]) -> String {
    dl_encoding::hex::encode(Sha256::digest(content))
}

fn picture(rvc: &str, content: &[u8]) -> SyncBlobRef {
    let key: StrMap = [("RVCNo".to_owned(), Some(rvc.to_owned()))]
        .into_iter()
        .collect();
    SyncBlobRef {
        table: "DefRVCList".into(),
        key,
        column: "Picture".into(),
        sha256: sha(content),
    }
}

struct BlobFx {
    env: TestEnv,
    cloud: FakeCloud,
    queue: Arc<SqliteBlobOutbox>,
    uploader: BlobUploader,
}

impl BlobFx {
    fn new() -> BlobFx {
        let env = TestEnv::new();
        let cloud = FakeCloud::start();
        let agent = agent(&env, &cloud);
        let queue = Arc::new(
            SqliteBlobOutbox::open(
                env.paths.blob_queue_db(),
                i64::MAX,
                SyncCatalog::load().unwrap(),
                &env.log,
            )
            .unwrap(),
        );
        let identity = AgentIdentity {
            agent_id: cloud.agent_id,
            tenant_id: "tenant-1".into(),
            cloud_url: cloud.base.clone(),
            machine_id: "TEST-PC".into(),
            sql_instance: None,
            sql_database: None,
        };
        let uploader =
            BlobUploader::new(queue.clone(), &identity, agent.http, agent.tokens, &env.log);
        BlobFx {
            env,
            cloud,
            queue,
            uploader,
        }
    }

    fn enqueue(&self, rvc: &str, content: &[u8]) {
        self.queue.enqueue(&picture(rvc, content), content).unwrap();
    }

    fn cycle(&self) -> UploadCycleResult {
        self.uploader.run_cycle(10, &Ctx::background()).unwrap()
    }

    fn refs(&self) -> Vec<(String, String, String, String)> {
        self.cloud.with(|c| {
            c.blob_refs
                .iter()
                .map(|r| {
                    (
                        r.table.clone(),
                        r.key.get("RVCNo").cloned().flatten().unwrap(),
                        r.column.clone(),
                        r.sha256.clone(),
                    )
                })
                .collect()
        })
    }
}

#[test]
fn queued_blobs_are_put_by_hash_then_referenced_and_leave_the_queue() {
    let fx = BlobFx::new();
    fx.enqueue("1", &[1, 2, 3]);
    fx.enqueue("2", &[4, 5]);

    assert_eq!(fx.cycle(), UploadCycleResult::Progress);

    assert_eq!(
        fx.cloud.with(|c| c.blobs.get(&sha(&[1, 2, 3])).cloned()),
        Some(vec![1, 2, 3])
    );
    assert_eq!(
        fx.cloud.with(|c| c.blobs.get(&sha(&[4, 5])).cloned()),
        Some(vec![4, 5])
    );
    let d = |s: &str| s.to_owned();
    assert_eq!(
        fx.refs(),
        [
            (d("DefRVCList"), d("1"), d("Picture"), sha(&[1, 2, 3])),
            (d("DefRVCList"), d("2"), d("Picture"), sha(&[4, 5]))
        ]
    );
    assert_eq!(fx.queue.depth().unwrap(), 0);
    assert_eq!(fx.cycle(), UploadCycleResult::Idle);
    assert!(fx.cloud.with(|c| c.rejections.is_empty()));
    let _ = &fx.env;
}

#[test]
fn content_the_cloud_already_holds_is_still_referenced_by_the_new_row() {
    let fx = BlobFx::new();
    fx.enqueue("1", &[9, 9]);
    fx.cycle();
    fx.enqueue("2", &[9, 9]);

    assert_eq!(fx.cycle(), UploadCycleResult::Progress);

    assert_eq!(fx.cloud.with(|c| c.blobs.len()), 1);
    let rvcs: Vec<String> = fx.refs().into_iter().map(|r| r.1).collect();
    assert_eq!(rvcs, ["1", "2"]);
    assert_eq!(fx.queue.depth().unwrap(), 0);
}

#[test]
fn content_reported_missing_by_the_refs_call_stays_queued() {
    let fx = BlobFx::new();
    fx.enqueue("1", &[1]);
    fx.cloud.with(|c| c.forget_blobs = true);

    assert_eq!(fx.cycle(), UploadCycleResult::Blocked);

    assert_eq!(fx.queue.depth().unwrap(), 1);
    assert!(fx.refs().is_empty());
    fx.cloud.with(|c| c.forget_blobs = false);
    assert_eq!(fx.cycle(), UploadCycleResult::Progress);
    assert_eq!(fx.refs().len(), 1);
    assert_eq!(fx.queue.depth().unwrap(), 0);
}

#[test]
fn persistent_blob_outage_keeps_the_blob_queued_and_reports_blocked() {
    let fx = BlobFx::new();
    fx.enqueue("1", &[1]);
    fx.enqueue("2", &[2]);
    fx.cloud.with(|c| c.fail_next_blob_puts = 100);

    assert_eq!(fx.cycle(), UploadCycleResult::Blocked);

    assert_eq!(
        fx.queue.depth().unwrap(),
        2,
        "nothing is dropped and the second blob waits behind the first"
    );
    assert!(fx.cloud.with(|c| c.blobs.is_empty()));
}

#[test]
fn permanently_refused_blob_is_quarantined_and_the_next_one_still_uploads() {
    let fx = BlobFx::new();
    fx.enqueue("1", &[1]);
    fx.enqueue("2", &[2]);
    fx.cloud.with(|c| {
        c.fail_next_blob_puts = 1;
        c.blob_fail_status = 400;
    });

    assert_eq!(fx.cycle(), UploadCycleResult::Progress);

    assert_eq!(fx.queue.quarantined_count().unwrap(), 1);
    assert_eq!(fx.queue.depth().unwrap(), 0);
    let rvcs: Vec<String> = fx.refs().into_iter().map(|r| r.1).collect();
    assert_eq!(rvcs, ["2"]);
}

#[test]
fn rejected_access_token_is_dropped_and_the_retry_uses_a_fresh_one() {
    let fx = BlobFx::new();
    fx.enqueue("1", &[1]);
    fx.cloud.with(|c| {
        c.fail_next_blob_puts = 1;
        c.blob_fail_status = 401;
    });

    assert_eq!(fx.cycle(), UploadCycleResult::Blocked);
    let exchanges = fx.cloud.with(|c| c.token_counter);
    assert_eq!(fx.cycle(), UploadCycleResult::Progress);

    assert_eq!(
        fx.cloud.with(|c| c.token_counter),
        exchanges + 1,
        "the refused token was invalidated, not reused"
    );
    assert_eq!(fx.refs().len(), 1);
    assert_eq!(fx.queue.depth().unwrap(), 0);
}

// ---- RuntimeTests: heartbeat ----

mod heartbeat {
    use super::*;
    use dl_agent_pda_sync::config::{AgentHealth, AgentStatus, Clock, RuntimeConfigStore, health};
    use dl_agent_pda_sync::contracts::wire::StreamLag;
    use dl_agent_pda_sync::outbox::{CaptureStatus, HeartbeatParts, HeartbeatSender};
    use dl_agent_pda_sync::transport::SyncHttpClient;
    use std::sync::Mutex;

    struct StubCapture {
        sql_reachable: Mutex<bool>,
        error: Option<String>,
    }

    impl CaptureStatus for StubCapture {
        fn sql_reachable(&self) -> bool {
            *self.sql_reachable.lock().unwrap()
        }
        fn streams(&self) -> Vec<StreamLag> {
            vec![StreamLag {
                stream: "open_checks".into(),
                lag_rows: 3,
                lag_seconds: 1.5,
            }]
        }
        fn capture_error(&self) -> Option<String> {
            self.error.clone()
        }
    }

    struct HbFx {
        fx: Fx,
        blobs: Arc<SqliteBlobOutbox>,
        config: Arc<RuntimeConfigStore>,
        capture: Arc<StubCapture>,
    }

    impl HbFx {
        fn new(error: Option<&str>) -> HbFx {
            let fx = Fx::new();
            let blobs = Arc::new(
                SqliteBlobOutbox::open(
                    fx.env.paths.blob_queue_db(),
                    i64::MAX,
                    SyncCatalog::load().unwrap(),
                    &fx.env.log,
                )
                .unwrap(),
            );
            let config =
                Arc::new(RuntimeConfigStore::new(fx.env.paths.clone(), &fx.env.log).unwrap());
            let capture = Arc::new(StubCapture {
                sql_reachable: Mutex::new(true),
                error: error.map(str::to_owned),
            });
            AgentStatus::write(
                &fx.env.paths,
                &AgentStatus::new("enrolled", None, None, fx.env.time.now()),
            )
            .unwrap();
            HbFx {
                fx,
                blobs,
                config,
                capture,
            }
        }

        fn sender(&self, http: Option<Arc<SyncHttpClient>>) -> HeartbeatSender {
            let env = &self.fx.env;
            HeartbeatSender::new(
                HeartbeatParts {
                    identity: AgentIdentity {
                        agent_id: self.fx.cloud.agent_id,
                        tenant_id: "tenant-1".into(),
                        cloud_url: self.fx.cloud.base.clone(),
                        machine_id: "TEST-PC".into(),
                        sql_instance: None,
                        sql_database: None,
                    },
                    outbox: self.fx.outbox.clone(),
                    blobs: self.blobs.clone(),
                    capture: self.capture.clone(),
                    http: http.unwrap_or_else(|| self.fx.agent.http.clone()),
                    tokens: self.fx.agent.tokens.clone(),
                    config: self.config.clone(),
                    clock: env.time.clone(),
                    health: Arc::new(AgentHealth::new(
                        env.paths.clone(),
                        env.time.clone(),
                        &env.log,
                    )),
                    agent_version: "3.30.0".into(),
                },
                &env.log,
            )
        }

        fn status(&self) -> AgentStatus {
            AgentStatus::try_read(&self.fx.env.paths).unwrap().unwrap()
        }
    }

    #[test]
    fn heartbeat_reports_queue_depth_and_lag_and_applies_cloud_config() {
        let hb = HbFx::new(Some("SQL_LOGIN_FAILED"));
        hb.fx.enqueue();
        hb.fx.cloud.with(|c| {
            c.config_json = Some(
                r#"{"openCheckPollMs":5000,"closedRescanDays":5,"tableAllowlist":["Invoice"]}"#
                    .into(),
            )
        });
        assert_eq!(
            hb.blobs
                .enqueue(&picture("1", &[1, 2, 3]), &[1, 2, 3])
                .unwrap(),
            dl_agent_pda_sync::outbox::BlobEnqueueResult::Queued
        );

        assert!(hb.sender(None).send(&Ctx::background()).unwrap());

        let beat = hb.fx.cloud.with(|c| c.heartbeats.clone());
        assert_eq!(beat.len(), 1);
        let beat = &beat[0];
        assert_eq!(beat.queue_depth, 1);
        assert_eq!(
            beat.blob_queue_depth,
            Some(1),
            "the image backlog is reported next to the batch backlog"
        );
        assert!(beat.sql_reachable);
        assert_eq!(beat.streams[0].lag_rows, 3);
        assert_eq!(beat.capture_error.as_deref(), Some("SQL_LOGIN_FAILED"));
        assert_eq!(beat.agent_id, hb.fx.cloud.agent_id.to_string());
        assert_eq!(beat.resnapshots, None);
        // RT-1327: the command types this agent runs, so the PDA offers order.room_charge only to a store that can.
        let commands = beat
            .commands
            .as_deref()
            .expect("the heartbeat lists its commands");
        for verb in [
            "order.room_charge",
            "order.tender_add",
            "order.check_close",
            "order.check_open",
        ] {
            assert!(commands.iter().any(|c| c == verb), "{verb}");
        }
        let st = hb.status();
        assert_eq!(
            (st.state.as_str(), st.error_code.as_deref()),
            (health::DEGRADED, Some("SQL_LOGIN_FAILED"))
        );
        assert_eq!(hb.config.current().open_check_poll_ms, 5000);
        let again = RuntimeConfigStore::new(hb.fx.env.paths.clone(), &Default::default()).unwrap();
        assert_eq!(
            again.current().closed_rescan_days,
            5,
            "config persists across restarts"
        );
    }

    #[test]
    fn heartbeat_writes_healthy_then_degraded_on_401_and_refreshes_the_token() {
        let hb = HbFx::new(None);
        let sender = hb.sender(None);
        assert!(sender.send(&Ctx::background()).unwrap());
        assert_eq!(hb.status().state, health::HEALTHY);

        // Another token exchange makes the cloud refuse the sender's cached access token once.
        let other = agent(&hb.fx.env, &hb.fx.cloud);
        other.tokens.get_access_token(&Ctx::background()).unwrap();
        hb.fx.advance(Duration::from_secs(15));
        assert!(!sender.send(&Ctx::background()).unwrap());
        let st = hb.status();
        assert_eq!(
            (st.state.as_str(), st.error_code.as_deref()),
            (health::DEGRADED, Some("HEARTBEAT_HTTP_401"))
        );

        *hb.capture.sql_reachable.lock().unwrap() = false;
        assert!(
            sender.send(&Ctx::background()).unwrap(),
            "the token was refreshed"
        );
        assert_eq!(
            hb.status().error_code.as_deref(),
            Some(codes::SQL_UNREACHABLE)
        );
    }

    #[test]
    fn heartbeat_carries_finished_resnapshots_until_accepted() {
        let hb = HbFx::new(None);
        hb.fx
            .outbox
            .begin_stream_resnapshot(&["open_checks"], hb.fx.now())
            .unwrap();
        hb.fx.outbox.stream_resnapshot_captured().unwrap();
        assert!(hb.sender(None).send(&Ctx::background()).unwrap());
        let sent = hb
            .fx
            .cloud
            .with(|c| c.heartbeats[0].resnapshots.clone())
            .unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            (sent[0].stream.as_str(), sent[0].through_seq),
            ("open_checks", 0)
        );
        assert!(
            hb.fx
                .outbox
                .finished_stream_resnapshots(hb.fx.now())
                .unwrap()
                .is_empty(),
            "marked reported"
        );
    }

    #[test]
    fn heartbeat_that_cannot_reach_the_cloud_is_recorded_as_degraded_and_still_fails() {
        let hb = HbFx::new(None);
        hb.fx
            .agent
            .tokens
            .get_access_token(&Ctx::background())
            .unwrap();
        let (key, _) =
            dl_agent_pda_sync::security::AgentKey::load_or_create(hb.fx.env.secrets.as_ref())
                .unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let nowhere = Arc::new(
            hb.fx
                .env
                .http_for(&format!("http://127.0.0.1:{port}/"), Arc::new(key)),
        );

        let err = hb
            .sender(Some(nowhere))
            .send(&Ctx::background())
            .unwrap_err();

        let dl_agent_pda_sync::outbox::UploadError::Transport(t) = &err else {
            panic!("{err}")
        };
        assert_eq!(t.code(), Some(codes::CLOUD_UNREACHABLE));
        let st = hb.status();
        assert_eq!(
            (st.state.as_str(), st.error_code.as_deref()),
            (health::DEGRADED, Some(codes::CLOUD_UNREACHABLE))
        );
    }
}
