//! `SqliteOutbox::adopt_agent_id`: queued batches follow a re-enrolled identity (RT-0450).

mod outbox_support;
mod support;

use std::sync::Arc;

use dl_agent_pda_sync::config::time::SharedClock;
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::time::DateTimeOffset;
use dl_agent_pda_sync::contracts::wire::{SyncBatch, SyncMode, wire_version};
use dl_agent_pda_sync::outbox::{
    BatchUploader, EnqueueResult, RECOVERY_RESNAPSHOT_COOLDOWN, SqliteOutbox, UploadCycleResult,
};
use dl_agent_pda_sync::util::ctx::Ctx;
use outbox_support::{FakeCloud, agent};
use support::TestEnv;

fn batch(agent_id: &str, stream: &str, seq: i64) -> SyncBatch {
    SyncBatch {
        agent_id: agent_id.into(),
        stream: stream.into(),
        seq,
        mapper_ver: wire_version::MAPPER,
        table: "Invoice".into(),
        mode: SyncMode::Delta,
        rows: vec![],
        tombstones: vec![],
        watermark: None,
        hops: Default::default(),
    }
}

fn enqueue(outbox: &SqliteOutbox, agent_id: &str, stream: &str) {
    let r = outbox
        .enqueue(stream, |seq| batch(agent_id, stream, seq), true)
        .unwrap();
    assert_eq!(r, EnqueueResult::Queued);
}

fn at() -> DateTimeOffset {
    DateTimeOffset::utc(2026, 11, 21, 3, 0, 0).unwrap()
}

fn raw(env: &TestEnv) -> dl_sqlite::Connection {
    dl_sqlite::Connection::open(env.paths.outbox_db(), dl_sqlite::OpenFlags::READ_WRITE).unwrap()
}

fn open(env: &TestEnv) -> SqliteOutbox {
    SqliteOutbox::open(env.paths.outbox_db(), i64::MAX, &env.log).unwrap()
}

#[test]
fn queued_batches_take_the_new_agent_id_and_quarantined_ones_stay() {
    let env = TestEnv::new();
    let outbox = open(&env);
    enqueue(&outbox, "old", "s1");
    enqueue(&outbox, "old", "s1");
    enqueue(&outbox, "old", "s2");
    let first = outbox.peek::<&str>(10, &[]).unwrap()[0].id;
    outbox.quarantine(first, "bad", false).unwrap();

    let r = outbox.adopt_agent_id("new", at()).unwrap();
    assert_eq!(r.restamped, 2);
    assert!(r.changed);

    let items = outbox.peek::<&str>(10, &[]).unwrap();
    assert_eq!(items.len(), 2);
    for item in &items {
        let b = SyncBatch::from_json(std::str::from_utf8(&item.payload).unwrap()).unwrap();
        assert_eq!(b.agent_id, "new");
        assert_eq!(b.seq, item.seq);
        assert_eq!(b.stream, item.stream);
        assert_eq!(b.table, "Invoice");
    }
    assert_eq!(outbox.quarantined_count().unwrap(), 1);
    // The stored byte count follows the rewritten payload.
    let db = raw(&env);
    let mismatched: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM outbox WHERE bytes<>length(payload)",
            &[],
            |r| r.get::<i64>(0),
        )
        .unwrap();
    assert_eq!(mismatched, 0);
}

#[test]
fn adopting_the_same_id_twice_restamps_nothing_the_second_time() {
    let env = TestEnv::new();
    let outbox = open(&env);
    enqueue(&outbox, "old", "s1");
    assert_eq!(outbox.adopt_agent_id("new", at()).unwrap().restamped, 1);
    let again = outbox.adopt_agent_id("new", at()).unwrap();
    assert_eq!(again.restamped, 0);
    assert!(!again.changed);
}

#[test]
fn identity_change_clears_the_recovery_cooldown_and_requests_the_deferred_resnapshot() {
    let env = TestEnv::new();
    let outbox = open(&env);
    assert!(!outbox.adopt_agent_id("old", at()).unwrap().changed);
    let hour_ago = at().checked_add_ticks(-36_000_000_000).unwrap();
    outbox
        .request_recovery_resnapshot("batch quarantined", hour_ago)
        .unwrap();
    outbox.clear_needs_resnapshot().unwrap();
    outbox.defer_recovery_resnapshot().unwrap();
    assert!(RECOVERY_RESNAPSHOT_COOLDOWN.as_secs() > 3600);

    // Same id: nothing changes, the cooldown stays.
    let same = outbox.adopt_agent_id("old", at()).unwrap();
    assert_eq!((same.restamped, same.changed), (0, false));
    assert!(!outbox.needs_resnapshot().unwrap());
    assert!(outbox.recovery_resnapshot_deferred().unwrap());
    assert_eq!(outbox.last_recovery_resnapshot().unwrap(), Some(hour_ago));

    let r = outbox.adopt_agent_id("new", at()).unwrap();
    assert!(r.changed);
    assert!(outbox.needs_resnapshot().unwrap());
    assert!(!outbox.recovery_resnapshot_deferred().unwrap());
    assert_eq!(outbox.last_recovery_resnapshot().unwrap(), Some(at()));
}

#[test]
fn identity_change_without_a_deferred_resnapshot_only_drops_the_cooldown() {
    let env = TestEnv::new();
    let outbox = open(&env);
    outbox.adopt_agent_id("old", at()).unwrap();
    outbox
        .request_recovery_resnapshot("batch quarantined", at())
        .unwrap();
    outbox.clear_needs_resnapshot().unwrap();
    assert!(outbox.adopt_agent_id("new", at()).unwrap().changed);
    assert!(!outbox.needs_resnapshot().unwrap());
    assert_eq!(outbox.last_recovery_resnapshot().unwrap(), None);
}

#[test]
fn a_corrupt_queued_payload_is_an_error_not_skipped() {
    let env = TestEnv::new();
    let outbox = open(&env);
    let r = outbox.enqueue("s1", |_| batch("old", "s1", 1), true);
    assert!(r.is_ok());
    drop(outbox);
    // Corrupt the stored payload behind the outbox's back.
    let db = raw(&env);
    db.execute("UPDATE outbox SET payload=x'7b'", &[]).unwrap();
    drop(db);
    let outbox = open(&env);
    assert!(outbox.adopt_agent_id("new", at()).is_err());
}

#[test]
fn re_enrolled_agent_uploads_what_was_queued_under_the_old_id() {
    let env = TestEnv::new();
    let cloud = FakeCloud::start();
    let agent = agent(&env, &cloud);
    let outbox = Arc::new(open(&env));
    enqueue(&outbox, "old", "open_checks");
    enqueue(&outbox, "old", "open_checks");
    let new_id = cloud.agent_id.to_string();
    assert_eq!(outbox.adopt_agent_id(&new_id, at()).unwrap().restamped, 2);

    let mut uploader = BatchUploader::new(
        outbox.clone(),
        agent.http.clone(),
        agent.tokens.clone(),
        &env.log,
        Some(env.time.clone() as SharedClock),
        Some(15),
    );
    let result = uploader.run_cycle(10, &Ctx::background()).unwrap();
    assert_eq!(result, UploadCycleResult::Progress);
    assert!(outbox.peek::<&str>(10, &[]).unwrap().is_empty());
    assert_eq!(outbox.quarantined_count().unwrap(), 0);
    cloud.with(|s| {
        assert_eq!(s.applied.len(), 2);
        assert!(s.applied.iter().all(|b| b.agent_id == new_id));
    });
}
