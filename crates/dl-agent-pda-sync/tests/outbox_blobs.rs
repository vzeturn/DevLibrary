//! `blobs.db`: port of BlobOutboxTests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use dl_agent_pda_sync::contracts::StrMap;
use dl_agent_pda_sync::contracts::SyncCatalog;
use dl_agent_pda_sync::contracts::wire::{SyncBlobRef, blob_contract};
use dl_agent_pda_sync::outbox::{BlobEnqueueResult, SqliteBlobOutbox};
use dl_agent_pda_sync::reporting::Logger;
use dl_hash::{Digest, Sha256};

static NEXT: AtomicU32 = AtomicU32::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("pda-blobs-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        TempDir(p)
    }

    fn db(&self) -> PathBuf {
        self.0.join("blobs.db")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open(dir: &TempDir, cap: i64) -> SqliteBlobOutbox {
    SqliteBlobOutbox::open(dir.db(), cap, SyncCatalog::load().unwrap(), &Logger::none()).unwrap()
}

fn sha(content: &[u8]) -> String {
    dl_encoding::hex::encode(Sha256::digest(content))
}

/// The `DefRVCList.Picture` cell of one revenue centre (a catalog `hash_exclude` column).
fn picture(rvc: &str, content: &[u8], column: &str) -> SyncBlobRef {
    let key: StrMap = [("RVCNo".to_owned(), Some(rvc.to_owned()))]
        .into_iter()
        .collect();
    SyncBlobRef {
        table: "DefRVCList".into(),
        key,
        column: column.into(),
        sha256: sha(content),
    }
}

#[test]
fn a_newer_value_of_the_same_cell_replaces_the_pending_one() {
    let dir = TempDir::new();
    let q = open(&dir, i64::MAX);
    assert_eq!(
        q.enqueue(&picture("1", &[1], "Picture"), &[1]).unwrap(),
        BlobEnqueueResult::Queued
    );
    assert_eq!(
        q.enqueue(&picture("2", &[2], "Picture"), &[2]).unwrap(),
        BlobEnqueueResult::Queued
    );
    assert_eq!(
        q.enqueue(&picture("1", &[3], "PICTURE"), &[3]).unwrap(),
        BlobEnqueueResult::Queued
    );

    assert_eq!(
        q.depth().unwrap(),
        2,
        "one entry per (table, row, column), column casing ignored"
    );
    let items = q.peek(10).unwrap();
    let rvcs: Vec<_> = items
        .iter()
        .map(|i| i.ref_.key.get("RVCNo").cloned().flatten().unwrap())
        .collect();
    assert_eq!(rvcs, ["1", "2"]);
    assert_eq!(items[0].content, [3]);
    assert_eq!(items[0].ref_.sha256, sha(&[3]));
}

#[test]
fn a_ref_the_cloud_would_refuse_is_not_queued() {
    for (table, column) in [
        ("app.revenue_centers", "Picture"),
        ("DefRVCList", "RVCName"),
    ] {
        let dir = TempDir::new();
        let q = open(&dir, i64::MAX);
        let mut bad = picture("1", &[1], "Picture");
        bad.table = table.into();
        bad.column = column.into();

        assert_eq!(
            q.enqueue(&bad, &[1]).unwrap(),
            BlobEnqueueResult::Refused,
            "{table}.{column}"
        );
        assert_eq!(q.depth().unwrap(), 0);
    }
}

#[test]
fn empty_or_oversized_content_is_refused() {
    let dir = TempDir::new();
    let q = open(&dir, i64::MAX);
    assert_eq!(
        q.enqueue(&picture("1", &[], "Picture"), &[]).unwrap(),
        BlobEnqueueResult::Refused
    );
    let big = vec![0u8; blob_contract::MAX_BYTES as usize + 1];
    assert_eq!(
        q.enqueue(&picture("1", &big, "Picture"), &big).unwrap(),
        BlobEnqueueResult::Refused
    );
    assert_eq!(q.depth().unwrap(), 0);
}

#[test]
fn at_the_cap_nothing_is_dropped_and_replacing_a_cell_does_not_count_its_old_value() {
    let dir = TempDir::new();
    let q = open(&dir, 10);
    let six = [1u8; 6];
    let five = [2u8; 5];
    let eight = [1, 1, 1, 1, 1, 1, 9, 9];

    assert_eq!(
        q.enqueue(&picture("1", &six, "Picture"), &six).unwrap(),
        BlobEnqueueResult::Queued
    );
    assert_eq!(
        q.enqueue(&picture("2", &five, "Picture"), &five).unwrap(),
        BlobEnqueueResult::CapReached
    );
    assert_eq!(
        q.enqueue(&picture("1", &eight, "Picture"), &eight).unwrap(),
        BlobEnqueueResult::Queued,
        "the replaced value frees its bytes"
    );

    assert_eq!(q.depth().unwrap(), 1);
    assert_eq!(q.bytes().unwrap(), 8);
}

#[test]
fn queue_survives_a_restart() {
    let dir = TempDir::new();
    let q = open(&dir, i64::MAX);
    q.enqueue(&picture("7", &[7, 7], "Picture"), &[7, 7])
        .unwrap();
    drop(q);

    let q = open(&dir, i64::MAX);

    let items = q.peek(10).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].ref_, picture("7", &[7, 7], "Picture"));
    assert_eq!(items[0].content, [7, 7]);
}

#[test]
fn ack_of_a_stale_upload_keeps_the_newer_value_queued_meanwhile() {
    let dir = TempDir::new();
    let q = open(&dir, i64::MAX);
    q.enqueue(&picture("1", &[1], "Picture"), &[1]).unwrap();
    let uploading = q.peek(1).unwrap().remove(0);
    q.enqueue(&picture("1", &[2], "Picture"), &[2]).unwrap();

    assert!(!q.ack(uploading.id, &uploading.ref_.sha256).unwrap());

    assert_eq!(q.peek(10).unwrap()[0].ref_.sha256, sha(&[2]));
    assert!(q.ack(uploading.id, &sha(&[2])).unwrap());
    assert_eq!(q.depth().unwrap(), 0);
}

#[test]
fn quarantined_entry_leaves_the_depth_until_a_newer_value_replaces_it() {
    let dir = TempDir::new();
    let q = open(&dir, i64::MAX);
    q.enqueue(&picture("1", &[1], "Picture"), &[1]).unwrap();
    let id = q.peek(1).unwrap()[0].id;
    q.mark_attempt(id).unwrap();
    q.quarantine(id, "content refused with HTTP 400").unwrap();

    assert_eq!(q.depth().unwrap(), 0);
    assert_eq!(q.quarantined_count().unwrap(), 1);
    assert!(q.peek(10).unwrap().is_empty());

    q.enqueue(&picture("1", &[2], "Picture"), &[2]).unwrap();
    assert_eq!(q.depth().unwrap(), 1);
    assert_eq!(q.quarantined_count().unwrap(), 0);
}

#[test]
fn the_ref_key_matches_the_csharp_identity() {
    let dir = TempDir::new();
    let q = open(&dir, i64::MAX);
    assert_eq!(
        q.ref_key(&picture("1", &[1], "PICTURE")).as_deref(),
        Some("DefRVCList\u{1f}RVCNo=1\u{1f}Picture")
    );
    q.enqueue(&picture("1", &[1], "Picture"), &[1]).unwrap();
    let db = dl_sqlite::Connection::open(dir.db(), dl_sqlite::OpenFlags::READ_ONLY).unwrap();
    let (key, json): (String, String) = db
        .query_row("SELECT ref_key, ref_json FROM blob_queue", &[], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(key, "DefRVCList\u{1f}RVCNo=1\u{1f}Picture");
    assert_eq!(
        json,
        format!(
            r#"{{"table":"DefRVCList","key":{{"RVCNo":"1"}},"column":"Picture","sha256":"{}"}}"#,
            sha(&[1])
        )
    );
}
