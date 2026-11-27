//! RT-1309: the live open-check stream is never queued behind a backlog, a parked stream
//! does not leave the uploader idle while others can send, and a deep outbox is reported.

mod outbox_support;
mod support;

use std::sync::Arc;

use dl_agent_pda_sync::config::codes;
use dl_agent_pda_sync::config::time::SharedClock;
use dl_agent_pda_sync::contracts::StrMap;
use dl_agent_pda_sync::contracts::wire::{
    SyncBatch, SyncMode, SyncRow, sync_streams, wire_version,
};
use dl_agent_pda_sync::outbox::{
    BACKLOG_REPORT_DEPTH, BatchUploader, EnqueueResult, SqliteOutbox, UploadCycleResult,
};
use dl_agent_pda_sync::reporting::Value;
use dl_agent_pda_sync::util::ctx::Ctx;
use outbox_support::{FakeCloud, agent};
use support::TestEnv;

struct Fx {
    env: TestEnv,
    cloud: FakeCloud,
    outbox: Arc<SqliteOutbox>,
    uploader: BatchUploader,
}

impl Fx {
    fn new() -> Fx {
        let env = TestEnv::new();
        let cloud = FakeCloud::start();
        let agent = agent(&env, &cloud);
        let outbox =
            Arc::new(SqliteOutbox::open(env.paths.outbox_db(), i64::MAX, &env.log).unwrap());
        let uploader = BatchUploader::new(
            outbox.clone(),
            agent.http.clone(),
            agent.tokens.clone(),
            &env.log,
            Some(env.time.clone() as SharedClock),
            Some(15),
        );
        Fx {
            env,
            cloud,
            outbox,
            uploader,
        }
    }

    fn enqueue(&self, stream: &str) {
        let agent_id = self.cloud.agent_id.to_string();
        let r = self
            .outbox
            .enqueue(
                stream,
                |seq| SyncBatch {
                    agent_id: agent_id.clone(),
                    stream: stream.into(),
                    seq,
                    mapper_ver: wire_version::MAPPER,
                    table: stream.into(),
                    mode: SyncMode::Delta,
                    rows: vec![SyncRow {
                        key: [("id".to_owned(), Some(seq.to_string()))]
                            .into_iter()
                            .collect::<StrMap>(),
                        columns: StrMap::new(),
                        hash: "h".into(),
                        src_ver: seq,
                    }],
                    tombstones: vec![],
                    watermark: None,
                    hops: Default::default(),
                },
                true,
            )
            .unwrap();
        assert_eq!(r, EnqueueResult::Queued);
    }

    fn parallel(mut self, streams: usize) -> Fx {
        self.uploader = self.uploader.with_parallel_streams(streams);
        self
    }

    fn cycle(&mut self, max: i32) -> UploadCycleResult {
        self.uploader.run_cycle(max, &Ctx::background()).unwrap()
    }

    fn applied(&self) -> Vec<String> {
        self.cloud
            .with(|c| c.applied.iter().map(|b| b.stream.clone()).collect())
    }
}

#[test]
fn open_check_batches_go_before_an_older_backlog_of_other_streams() {
    let mut fx = Fx::new();
    for _ in 0..12 {
        fx.enqueue("ledger");
    }
    fx.enqueue(sync_streams::OPEN);
    fx.enqueue(sync_streams::OPEN);

    assert_eq!(fx.cycle(3), UploadCycleResult::Progress);

    assert_eq!(
        fx.applied(),
        [sync_streams::OPEN, sync_streams::OPEN, "ledger"],
        "the live stream is sent first, in its own order, then the backlog"
    );
    let open_seqs: Vec<i64> = fx.cloud.with(|c| {
        c.applied
            .iter()
            .filter(|b| b.stream == sync_streams::OPEN)
            .map(|b| b.seq)
            .collect()
    });
    assert_eq!(open_seqs, [1, 2]);
    let timings = fx
        .env
        .sink
        .events()
        .into_iter()
        .filter(|e| e.property("QueuedMs").is_some())
        .count();
    assert_eq!(timings, 2, "each live batch logs its queue and POST time");
}

#[test]
fn a_cycle_whose_whole_peek_parks_peeks_again_for_the_other_streams() {
    let mut fx = Fx::new();
    fx.cloud
        .with(|c| c.parent_pending.insert("Trn".into(), "Invoice".into()));
    fx.enqueue("Trn");
    fx.enqueue("Trn");
    fx.enqueue("Invoice");

    assert_eq!(
        fx.cycle(2),
        UploadCycleResult::Progress,
        "the parked stream filled the peek, yet the cycle goes on to the rest"
    );

    assert_eq!(fx.applied(), ["Invoice"]);
    assert_eq!(fx.outbox.depth().unwrap(), 2);
    assert_eq!(fx.cycle(2), UploadCycleResult::Idle);
}

#[test]
fn a_deep_outbox_is_reported_with_its_streams() {
    let mut fx = Fx::new();
    let depth = usize::try_from(BACKLOG_REPORT_DEPTH).unwrap();
    for _ in 0..8 {
        fx.enqueue("keys");
    }
    for i in 0..depth {
        fx.enqueue(if i % 4 == 0 { "masters" } else { "ledger" });
    }

    // The first cycle sends the 8 key batches; the 100 left behind are reported.
    assert_eq!(fx.cycle(8), UploadCycleResult::Progress);
    assert_eq!(fx.cycle(8), UploadCycleResult::Progress);

    let reports: Vec<_> = fx
        .env
        .sink
        .events()
        .into_iter()
        .filter(|e| e.property("Code") == Some(&Value::from(codes::SYNC_OUTBOX_BACKLOG)))
        .collect();
    assert_eq!(reports.len(), 1, "one report per interval");
    assert_eq!(
        reports[0].property("Streams"),
        Some(&Value::from("ledger=75, masters=25"))
    );
}

/// RT-1318: several streams are posted side by side, each in seq order, and a stream parked
/// by a 424 never has its next batch sent past the one that waits.
#[test]
fn parallel_streams_keep_each_stream_in_order_and_a_parked_stream_waits() {
    let mut fx = Fx::new().parallel(4);
    fx.cloud
        .with(|c| c.parent_pending.insert("Trn".into(), "Invoice".into()));
    for _ in 0..4 {
        for stream in [sync_streams::OPEN, "ledger", "masters", "Trn"] {
            fx.enqueue(stream);
        }
    }

    assert_eq!(fx.cycle(16), UploadCycleResult::Progress);

    let (seqs, rejections, parent_answers) = fx.cloud.with(|c| {
        let mut seqs: std::collections::HashMap<String, Vec<i64>> = Default::default();
        for b in &c.applied {
            seqs.entry(b.stream.clone()).or_default().push(b.seq);
        }
        (seqs, c.rejections.clone(), c.parent_pending_answers)
    });
    for stream in [sync_streams::OPEN, "ledger", "masters"] {
        assert_eq!(seqs[stream], [1, 2, 3, 4], "stream {stream} in seq order");
    }
    assert!(!seqs.contains_key("Trn"));
    assert_eq!(
        parent_answers, 1,
        "only Trn#1 was sent; Trn#2.. wait behind it"
    );
    assert!(
        rejections.is_empty(),
        "no seq gap was ever sent: {rejections:?}"
    );
    assert_eq!(fx.outbox.depth().unwrap(), 4);
}

#[test]
fn a_child_answered_before_its_parent_landed_in_the_same_round_goes_again_unparked() {
    let mut fx = Fx::new().parallel(4);
    fx.cloud
        .with(|c| c.parent_race.insert("Trn".into(), "Invoice".into()));
    fx.enqueue("Trn");
    fx.enqueue("Invoice");
    fx.enqueue("Trn");

    assert_eq!(fx.cycle(8), UploadCycleResult::Progress);

    let (trn, parent_answers) = fx.cloud.with(|c| {
        let trn: Vec<i64> = c
            .applied
            .iter()
            .filter(|b| b.stream == "Trn")
            .map(|b| b.seq)
            .collect();
        (trn, c.parent_pending_answers)
    });
    assert_eq!(parent_answers, 1, "Trn#1 met the race once");
    assert_eq!(trn, [1, 2], "Trn went again in the same cycle, in order");
    assert_eq!(fx.outbox.depth().unwrap(), 0, "nothing parked");
}
