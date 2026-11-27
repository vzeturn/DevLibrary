//! RT-1318 per-hop stamps on uploaded batches: the cloud gets `t_detected` and `t_read`
//! as the outbox stored them, plus `t_sent` added when the batch is posted.

mod outbox_support;
mod support;

use std::sync::Arc;

use dl_agent_pda_sync::config::time::SharedClock;
use dl_agent_pda_sync::contracts::StrMap;
use dl_agent_pda_sync::contracts::wire::{BatchHops, SyncBatch, SyncMode, SyncRow, wire_version};
use dl_agent_pda_sync::outbox::{BatchUploader, EnqueueResult, SqliteOutbox, UploadCycleResult};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::hop_clock;
use outbox_support::{FakeCloud, agent};
use support::TestEnv;

#[test]
fn an_uploaded_batch_carries_its_stamps_and_when_it_was_sent() {
    let env = TestEnv::new();
    let cloud = FakeCloud::start();
    let agent = agent(&env, &cloud);
    let outbox = Arc::new(SqliteOutbox::open(env.paths.outbox_db(), i64::MAX, &env.log).unwrap());
    let mut uploader = BatchUploader::new(
        outbox.clone(),
        agent.http.clone(),
        agent.tokens.clone(),
        &env.log,
        Some(env.time.clone() as SharedClock),
        Some(15),
    );
    let agent_id = cloud.agent_id.to_string();
    let detected = hop_clock::now_micros();
    let read = hop_clock::now_micros();
    for hops in [
        BatchHops {
            t_detected: Some(detected),
            t_read: Some(read),
            t_sent: None,
        },
        // A batch an older agent left in the outbox.
        BatchHops::default(),
    ] {
        let queued = outbox
            .enqueue(
                "open_checks",
                |seq| SyncBatch {
                    agent_id: agent_id.clone(),
                    stream: "open_checks".into(),
                    seq,
                    mapper_ver: wire_version::MAPPER,
                    table: "Invoice".into(),
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
                    hops,
                },
                true,
            )
            .unwrap();
        assert_eq!(queued, EnqueueResult::Queued);
    }

    assert_eq!(
        uploader.run_cycle(10, &Ctx::background()).unwrap(),
        UploadCycleResult::Progress
    );

    let applied = cloud.with(|c| c.applied.clone());
    assert_eq!(applied.len(), 2);
    let first = applied[0].hops;
    assert_eq!(
        (first.t_detected, first.t_read),
        (Some(detected), Some(read))
    );
    assert!(first.t_sent.is_some_and(|sent| sent >= read), "{first:?}");
    let second = applied[1].hops;
    assert_eq!((second.t_detected, second.t_read), (None, None));
    assert!(second.t_sent.is_some(), "{second:?}");
}
