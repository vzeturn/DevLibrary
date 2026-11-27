//! Cross-check with the C# agent: reads the `outbox.db` / `blobs.db` its `SqliteOutbox` /
//! `SqliteBlobOutbox` wrote and continues them. Gated on `PDA_SYNC_OUTBOX_XCHECK=<dir>`
//! (the directory `xcheck.cs write <dir>` filled); skipped otherwise.

use dl_agent_pda_sync::contracts::time::DateTimeOffset;
use dl_agent_pda_sync::contracts::wire::{SyncBatch, SyncMode, SyncRow, wire_version};
use dl_agent_pda_sync::contracts::{StrMap, SyncCatalog, WireJson};
use dl_agent_pda_sync::outbox::{EnqueueResult, SqliteBlobOutbox, SqliteOutbox};
use dl_agent_pda_sync::reporting::Logger;

fn batch(stream: &str, seq: i64, wm: Option<&str>) -> SyncBatch {
    let key: StrMap = [("id".to_owned(), Some(seq.to_string()))]
        .into_iter()
        .collect();
    let columns: StrMap = [
        ("name".to_owned(), Some("Phở <b>+1</b>".to_owned())),
        ("n".to_owned(), None),
    ]
    .into_iter()
    .collect();
    SyncBatch {
        agent_id: "agent-1".into(),
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
        watermark: wm.map(str::to_owned),
        hops: Default::default(),
    }
}

#[test]
fn files_written_by_the_csharp_agent_are_read_and_continued() {
    let Ok(dir) = std::env::var("PDA_SYNC_OUTBOX_XCHECK") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let at = DateTimeOffset::utc(2026, 11, 4, 3, 0, 0).unwrap();
    let o = SqliteOutbox::open(dir.join("outbox.db"), i64::MAX, &Logger::none()).unwrap();
    assert_eq!(o.depth().unwrap(), 3);
    assert_eq!(o.quarantined_count().unwrap(), 1);
    assert_eq!(o.last_seq("closed_ledger").unwrap(), 3);
    assert_eq!(
        o.watermark("closed_ledger").unwrap().as_deref(),
        Some("inv:2")
    );
    let items = o.peek::<&str>(10, &[]).unwrap();
    let got: Vec<(String, i64)> = items.iter().map(|i| (i.stream.clone(), i.seq)).collect();
    assert_eq!(
        got,
        [
            ("closed_ledger".to_owned(), 1),
            ("closed_ledger".to_owned(), 3),
            ("open".to_owned(), 1)
        ]
    );
    // The C# payload bytes are the Rust serialisation of the same batch.
    assert_eq!(
        String::from_utf8(items[0].payload.clone()).unwrap(),
        batch("closed_ledger", 1, Some("inv:1")).to_json()
    );
    assert_eq!(o.pending_stream_resnapshot().unwrap(), ["Invoice"]);
    let last = o.last_recovery_resnapshot().unwrap().unwrap();
    assert_eq!(last, at);
    assert_eq!(last.offset_minutes(), 420);
    assert!(o.recovery_resnapshot_deferred().unwrap());
    assert!(o.needs_resnapshot().unwrap());

    // Continue: the next seq follows the C# numbering, the cursor survives.
    let r = o
        .enqueue(
            "closed_ledger",
            |seq| batch("closed_ledger", seq, Some("inv:4")),
            true,
        )
        .unwrap();
    assert_eq!(r, EnqueueResult::Queued);
    assert_eq!(o.last_seq("closed_ledger").unwrap(), 4);
    o.stream_resnapshot_captured().unwrap();

    let b = SqliteBlobOutbox::open(
        dir.join("blobs.db"),
        i64::MAX,
        SyncCatalog::load().unwrap(),
        &Logger::none(),
    )
    .unwrap();
    let blobs = b.peek(10).unwrap();
    assert_eq!(blobs.len(), 1);
    assert_eq!(blobs[0].content, [1, 2, 3]);
    assert_eq!(blobs[0].ref_.column, "PICTURE");
    // Same cell in the catalog spelling replaces the C# entry (same ref_key).
    let mut newer = blobs[0].ref_.clone();
    newer.column = "Picture".into();
    newer.sha256 = "71e1ec59dd990e14f06592c26146a79cbce0f1a8b0e1e1d8cb3c1b9c3b6a0a4f".into();
    let _ = b.enqueue(&newer, &[4]).unwrap();
    assert_eq!(b.depth().unwrap(), 1);
}
