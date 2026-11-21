//! `outbox.db`: ports of StreamResnapshotTests (outbox cases) and the outbox-only cases
//! of TransportTests (restart, cap, resequence).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use dl_agent_pda_sync::config::codes;
use dl_agent_pda_sync::contracts::time::DateTimeOffset;
use dl_agent_pda_sync::contracts::wire::{
    StreamResnapshot, SyncBatch, SyncMode, SyncRow, wire_version,
};
use dl_agent_pda_sync::contracts::{StrMap, WireJson};
use dl_agent_pda_sync::outbox::{
    EnqueueResult, ResequenceResult, SqliteOutbox, StreamResnapshotRequest,
};
use dl_agent_pda_sync::reporting::{Level, Logger, LoggerConfig, MemorySink, Value};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A fresh directory under the temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("pda-outbox-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        TempDir(p)
    }

    fn outbox_db(&self) -> PathBuf {
        self.0.join("data").join("outbox.db")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn memory_logger() -> (Logger, Arc<MemorySink>) {
    let sink = Arc::new(MemorySink::default());
    let log = Logger::new(LoggerConfig {
        minimum: Level::Verbose,
        sinks: vec![(sink.clone(), Level::Verbose)],
        ..LoggerConfig::default()
    });
    (log, sink)
}

struct Env {
    dir: TempDir,
    outbox: SqliteOutbox,
    sink: Arc<MemorySink>,
    now: DateTimeOffset,
}

impl Env {
    fn new() -> Env {
        Env::with_cap(i64::MAX)
    }

    fn with_cap(cap: i64) -> Env {
        let dir = TempDir::new();
        let (log, sink) = memory_logger();
        let outbox = SqliteOutbox::open(dir.outbox_db(), cap, &log).unwrap();
        Env {
            dir,
            outbox,
            sink,
            now: DateTimeOffset::utc(2026, 11, 4, 3, 0, 0).unwrap(),
        }
    }

    fn advance_minutes(&mut self, m: i64) {
        self.now = self.now.checked_add_ticks(m * 600_000_000).unwrap();
    }

    fn enqueue(&self, stream: &str) {
        let r = self
            .outbox
            .enqueue(
                stream,
                |seq| empty_batch(stream, seq, SyncMode::Snapshot, None),
                true,
            )
            .unwrap();
        assert_eq!(r, EnqueueResult::Queued);
    }

    fn ack_all(&self, stream: &str) {
        for item in self.outbox.peek::<&str>(100, &[]).unwrap() {
            if item.stream == stream {
                self.outbox.ack(item.id).unwrap();
            }
        }
    }

    fn warnings_with_code(&self, code: &str) -> usize {
        self.sink
            .events()
            .iter()
            .filter(|e| e.level == Level::Warning && e.property("Code") == Some(&Value::from(code)))
            .count()
    }
}

fn empty_batch(stream: &str, seq: i64, mode: SyncMode, watermark: Option<&str>) -> SyncBatch {
    SyncBatch {
        agent_id: "agent".into(),
        stream: stream.into(),
        seq,
        mapper_ver: wire_version::MAPPER,
        table: stream.into(),
        mode,
        rows: vec![],
        tombstones: vec![],
        watermark: watermark.map(str::to_owned),
    }
}

/// TransportTests.Batch: one row keyed by the seq, `pad` bytes of padding.
fn padded_batch(stream: &str, seq: i64, watermark: Option<&str>, pad: usize) -> SyncBatch {
    let key: StrMap = [("id".to_owned(), Some(seq.to_string()))]
        .into_iter()
        .collect();
    let columns: StrMap = [("pad".to_owned(), Some("x".repeat(pad)))]
        .into_iter()
        .collect();
    SyncBatch {
        agent_id: "agent".into(),
        stream: stream.into(),
        seq,
        mapper_ver: wire_version::MAPPER,
        table: "Invoice".into(),
        mode: SyncMode::Delta,
        rows: vec![SyncRow {
            key,
            columns,
            hash: "h".into(),
            src_ver: seq,
        }],
        tombstones: vec![],
        watermark: watermark.map(str::to_owned),
    }
}

fn streams_of(env: &Env, skip: &[&str]) -> Vec<String> {
    env.outbox
        .peek(10, skip)
        .unwrap()
        .into_iter()
        .map(|i| i.stream)
        .collect()
}

#[test]
fn peek_leaves_out_the_parked_streams_and_keeps_the_rest_in_order() {
    let env = Env::new();
    env.enqueue("Trn");
    env.enqueue("Invoice");
    env.enqueue("Trn");
    env.enqueue("open");

    assert_eq!(streams_of(&env, &["Trn"]), ["Invoice", "open"]);
    assert_eq!(streams_of(&env, &["Trn", "open"]), ["Invoice"]);
    assert_eq!(streams_of(&env, &[]).len(), 4);
}

#[test]
fn a_clean_stream_resnapshot_is_reported_once_its_batches_left_the_queue() {
    let mut env = Env::new();
    let first = env
        .outbox
        .request_stream_resnapshot(&["Invoice", "Trn"], "test", env.now)
        .unwrap();
    let again = env
        .outbox
        .request_stream_resnapshot(&["Trn"], "test", env.now)
        .unwrap();
    assert_eq!(first.added, ["Invoice", "Trn"]);
    assert_eq!(
        again,
        StreamResnapshotRequest {
            added: vec![],
            already_pending: vec!["Trn".into()]
        }
    );
    assert_eq!(
        env.outbox.pending_stream_resnapshot().unwrap(),
        ["Invoice", "Trn"]
    );

    env.outbox
        .begin_stream_resnapshot(&["Invoice", "Trn"], env.now)
        .unwrap();
    assert!(env.outbox.pending_stream_resnapshot().unwrap().is_empty());
    env.enqueue("Invoice");
    env.enqueue("Invoice");
    env.enqueue("Trn");
    env.outbox.stream_resnapshot_captured().unwrap();
    assert!(
        env.outbox
            .finished_stream_resnapshots(env.now)
            .unwrap()
            .is_empty(),
        "the resent batches are still queued"
    );

    env.ack_all("Invoice");
    env.advance_minutes(1);
    let done = env.outbox.finished_stream_resnapshots(env.now).unwrap();
    assert_eq!(
        done,
        [StreamResnapshot {
            stream: "Invoice".into(),
            through_seq: 2,
            completed_at_utc: env.now
        }]
    );
    assert_eq!(
        env.outbox.stream_resnapshot_finished_at("Invoice").unwrap(),
        Some(env.now)
    );
    assert_eq!(
        env.outbox.stream_resnapshot_finished_at("Trn").unwrap(),
        None
    );
    assert_eq!(
        env.outbox.finished_stream_resnapshots(env.now).unwrap(),
        done,
        "it is carried until a heartbeat is accepted"
    );

    env.outbox.mark_stream_resnapshots_reported(&done).unwrap();
    assert!(
        env.outbox
            .finished_stream_resnapshots(env.now)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        env.outbox.stream_resnapshot_finished_at("Invoice").unwrap(),
        Some(done[0].completed_at_utc),
        "a reported resnapshot still counts as resent"
    );
}

#[test]
fn a_stream_resnapshot_that_quarantined_a_batch_is_never_reported() {
    let env = Env::new();
    env.enqueue("Trn"); // quarantined before the resnapshot: already counted
    let first = env.outbox.peek::<&str>(1, &[]).unwrap()[0].id;
    env.outbox.quarantine(first, "old", false).unwrap();
    env.outbox
        .begin_stream_resnapshot(&["Trn"], env.now)
        .unwrap();
    env.enqueue("Trn");
    env.enqueue("Trn");
    env.outbox.stream_resnapshot_captured().unwrap();
    let items = env.outbox.peek::<&str>(10, &[]).unwrap();
    env.outbox.ack(items[0].id).unwrap();
    env.outbox.quarantine(items[1].id, "new", false).unwrap();

    assert!(
        env.outbox
            .finished_stream_resnapshots(env.now)
            .unwrap()
            .is_empty()
    );

    assert_eq!(env.warnings_with_code(codes::SYNC_STREAM_RESNAPSHOT), 1);
    assert_eq!(
        env.outbox
            .request_stream_resnapshot(&["Trn"], "retry", env.now)
            .unwrap()
            .added,
        ["Trn"]
    );
}

#[test]
fn a_scheduled_stream_resnapshot_survives_a_restart() {
    let env = Env::new();
    env.outbox
        .request_stream_resnapshot(&["Invoice"], "test", env.now)
        .unwrap();
    let Env { dir, outbox, .. } = env;
    drop(outbox);

    let reopened = SqliteOutbox::open(dir.outbox_db(), i64::MAX, &Logger::none()).unwrap();

    assert_eq!(reopened.pending_stream_resnapshot().unwrap(), ["Invoice"]);
}

#[test]
fn recovery_resnapshot_is_recorded_and_deferred_across_restarts() {
    let env = Env::new();
    assert_eq!(env.outbox.last_recovery_resnapshot().unwrap(), None);
    let at = DateTimeOffset::utc(2026, 11, 4, 3, 0, 0)
        .unwrap()
        .to_offset(7 * 60)
        .unwrap();
    env.outbox
        .request_recovery_resnapshot("quarantine", at)
        .unwrap();
    assert!(env.outbox.needs_resnapshot().unwrap());
    env.outbox.clear_needs_resnapshot().unwrap();
    env.outbox.defer_recovery_resnapshot().unwrap();
    let Env { dir, outbox, .. } = env;
    drop(outbox);

    let reopened = SqliteOutbox::open(dir.outbox_db(), i64::MAX, &Logger::none()).unwrap();
    let last = reopened.last_recovery_resnapshot().unwrap().unwrap();
    assert_eq!(last, at);
    assert_eq!(
        last.offset_minutes(),
        7 * 60,
        "stored with its offset (\"O\")"
    );
    assert!(reopened.recovery_resnapshot_deferred().unwrap());
    assert!(!reopened.needs_resnapshot().unwrap());
}

#[test]
fn outbox_survives_a_restart_and_keeps_sequence_and_watermark() {
    let env = Env::new();
    for w in ["inv:100", "inv:200"] {
        env.outbox
            .enqueue(
                "closed_ledger",
                |seq| padded_batch("closed_ledger", seq, Some(w), 0),
                true,
            )
            .unwrap();
    }
    let Env { dir, outbox, .. } = env;
    drop(outbox);

    let outbox = SqliteOutbox::open(dir.outbox_db(), i64::MAX, &Logger::none()).unwrap();
    assert_eq!(outbox.depth().unwrap(), 2);
    assert_eq!(outbox.last_seq("closed_ledger").unwrap(), 2);
    assert_eq!(
        outbox.watermark("closed_ledger").unwrap().as_deref(),
        Some("inv:200")
    );
    let r = outbox
        .enqueue(
            "closed_ledger",
            |seq| padded_batch("closed_ledger", seq, None, 0),
            true,
        )
        .unwrap();
    assert_eq!(r, EnqueueResult::Queued);
    assert_eq!(outbox.last_seq("closed_ledger").unwrap(), 3);
    assert_eq!(
        outbox.watermark("closed_ledger").unwrap().as_deref(),
        Some("inv:200"),
        "a batch without a watermark must not erase the cursor"
    );
    let seqs: Vec<i64> = outbox
        .peek::<&str>(10, &[])
        .unwrap()
        .iter()
        .map(|i| i.seq)
        .collect();
    assert_eq!(seqs, [1, 2, 3]);
}

#[test]
fn cap_reached_sets_needs_resnapshot_without_dropping_queued_data_or_the_cursor() {
    let env = Env::with_cap(4_000);
    let first = env
        .outbox
        .enqueue(
            "closed_ledger",
            |seq| padded_batch("closed_ledger", seq, Some("inv:10"), 500),
            true,
        )
        .unwrap();
    assert_eq!(first, EnqueueResult::Queued);

    let second = env
        .outbox
        .enqueue(
            "closed_ledger",
            |seq| padded_batch("closed_ledger", seq, Some("inv:99"), 5000),
            true,
        )
        .unwrap();
    assert_eq!(second, EnqueueResult::CapReached);

    assert!(env.outbox.needs_resnapshot().unwrap());
    assert_eq!(env.outbox.depth().unwrap(), 1);
    assert_eq!(
        env.outbox.watermark("closed_ledger").unwrap().as_deref(),
        Some("inv:10"),
        "a refused batch must not advance the cursor"
    );
    assert_eq!(env.outbox.last_seq("closed_ledger").unwrap(), 1);

    env.outbox.clear_needs_resnapshot().unwrap();
    assert!(!env.outbox.needs_resnapshot().unwrap());
}

#[test]
fn is_full_follows_a_refusal_just_under_the_cap_until_something_drains() {
    let probe = Env::with_cap(i64::MAX);
    probe
        .outbox
        .enqueue("s", |seq| padded_batch("s", seq, None, 0), false)
        .unwrap();
    let small = probe.outbox.bytes().unwrap();

    let env = Env::with_cap(small * 2 + 5);
    for _ in 0..2 {
        let r = env
            .outbox
            .enqueue("s", |seq| padded_batch("s", seq, None, 0), false)
            .unwrap();
        assert_eq!(r, EnqueueResult::Queued);
    }
    assert!(env.outbox.bytes().unwrap() < small * 2 + 5);
    assert!(!env.outbox.is_full().unwrap());

    let r = env
        .outbox
        .enqueue("s", |seq| padded_batch("s", seq, None, 50), false)
        .unwrap();
    assert_eq!(r, EnqueueResult::CapReached);
    assert!(env.outbox.is_full().unwrap(), "refused with room left");

    env.outbox.ack_below("s", 2).unwrap();
    assert!(!env.outbox.is_full().unwrap(), "an ack freed room");

    let r = env
        .outbox
        .enqueue("s", |seq| padded_batch("s", seq, None, 0), false)
        .unwrap();
    assert_eq!(r, EnqueueResult::Queued);
    assert!(!env.outbox.is_full().unwrap());
}

#[test]
fn cap_reached_without_resnapshot_request_leaves_the_flag_alone() {
    let env = Env::with_cap(10);
    let r = env
        .outbox
        .enqueue("s", |seq| padded_batch("s", seq, None, 0), false)
        .unwrap();
    assert_eq!(r, EnqueueResult::CapReached);
    assert!(!env.outbox.needs_resnapshot().unwrap());
}

#[test]
fn a_batch_over_the_wire_cap_is_refused_with_the_csharp_message() {
    let env = Env::new();
    let err = env
        .outbox
        .enqueue(
            "s",
            |seq| padded_batch("s", seq, None, wire_version::MAX_BODY_BYTES as usize),
            true,
        )
        .unwrap_err();
    assert!(
        err.to_string()
            .ends_with("bytes exceeds the 8388608 byte wire cap; split it."),
        "{err}"
    );
    assert_eq!(env.outbox.last_seq("s").unwrap(), 0, "rolled back");
}

#[test]
fn resequence_moves_quarantined_batches_aside_and_renumbers_the_rest() {
    let env = Env::new();
    for _ in 0..4 {
        env.outbox
            .enqueue(
                "open_checks",
                |seq| padded_batch("open_checks", seq, None, 0),
                true,
            )
            .unwrap();
    }
    let items = env.outbox.peek::<&str>(10, &[]).unwrap();
    env.outbox.quarantine(items[1].id, "test", false).unwrap(); // #2
    env.outbox.quarantine(items[2].id, "test", false).unwrap(); // #3

    let r = env.outbox.resequence("open_checks", 2).unwrap();

    assert_eq!(
        r,
        ResequenceResult {
            renumbered: 1,
            set_aside: 2
        }
    );
    let left = env.outbox.peek::<&str>(10, &[]).unwrap();
    assert_eq!(left.iter().map(|i| i.seq).collect::<Vec<_>>(), [1, 2]);
    let moved = SyncBatch::from_json(std::str::from_utf8(&left[1].payload).unwrap()).unwrap();
    assert_eq!(moved.seq, 2, "the payload carries the new seq");
    assert_eq!(moved.rows[0].key.get("id"), Some(&Some("4".to_owned())));
    assert_eq!(env.outbox.last_seq("open_checks").unwrap(), 2);
    assert_eq!(env.outbox.quarantined_count().unwrap(), 2);
    assert!(!env.outbox.has("open_checks", 3).unwrap());
    assert!(env.outbox.resequence("open_checks", 0).is_err());
}

#[test]
fn ack_below_has_and_quarantine_flags() {
    let env = Env::new();
    env.enqueue("s");
    env.enqueue("s");
    env.enqueue("s");
    assert!(env.outbox.has("s", 2).unwrap());
    assert_eq!(env.outbox.ack_below("s", 3).unwrap(), 2);
    assert!(!env.outbox.has("s", 2).unwrap());
    let id = env.outbox.peek::<&str>(1, &[]).unwrap()[0].id;
    env.outbox.mark_attempt(id).unwrap();
    env.outbox.quarantine(id, "bad", true).unwrap();
    assert!(env.outbox.is_quarantined("s", 3).unwrap());
    assert!(env.outbox.needs_resnapshot().unwrap());
    assert_eq!(env.outbox.depth().unwrap(), 0);
    assert!(
        env.outbox.bytes().unwrap() > 0,
        "quarantined bytes still count"
    );
    assert!(
        env.sink
            .messages_at(Level::Warning)
            .iter()
            .any(|m| m == &format!("Resnapshot requested: batch {id} quarantined: bad")),
        "{:?}",
        env.sink.messages_at(Level::Warning)
    );
}

#[test]
fn the_file_has_the_csharp_schema() {
    let env = Env::new();
    env.enqueue("s");
    let path = env.dir.outbox_db();
    let db = dl_sqlite::Connection::open(&path, dl_sqlite::OpenFlags::READ_ONLY).unwrap();
    let mode: String = db
        .query_row("PRAGMA journal_mode", &[], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let tables: Vec<String> = db
        .query(
            "SELECT name FROM sqlite_master WHERE type IN ('table','index') AND name NOT LIKE 'sqlite_%' ORDER BY name",
            &[],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        tables,
        [
            "key_repair",
            "meta",
            "outbox",
            "stream_resnapshot",
            "stream_state",
            "ux_key_repair",
            "ux_outbox_stream_seq"
        ]
    );
    let created: String = db
        .query_row("SELECT created_utc FROM outbox", &[], |r| r.get(0))
        .unwrap();
    assert_eq!(
        created.len(),
        "2026-11-04T03:00:00.0000000+00:00".len(),
        "{created}"
    );
    assert!(created.ends_with("+00:00"), "{created}");
}
