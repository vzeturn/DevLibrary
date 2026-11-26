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
                },
                true,
            )
            .unwrap();
        assert_eq!(r, EnqueueResult::Queued);
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
