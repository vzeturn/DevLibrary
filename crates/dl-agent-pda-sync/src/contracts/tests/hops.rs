//! RT-1318 per-hop stamps on the wire, byte for byte as `EnvelopeTests.cs` pins them
//! (`Batch_and_result_carry_optional_hop_stamps`), and `t_sent` added to a stored batch.

use dl_uuid::Uuid;

use crate::contracts::json::WireJson;
use crate::contracts::time::DateTimeOffset;
use crate::contracts::wire::*;

fn batch(hops: BatchHops) -> SyncBatch {
    SyncBatch {
        agent_id: "agent-1".into(),
        stream: "open_checks".into(),
        seq: 7,
        mapper_ver: wire_version::MAPPER,
        table: "Invoice".into(),
        mode: SyncMode::Delta,
        rows: Vec::new(),
        tombstones: Vec::new(),
        watermark: None,
        hops,
    }
}

fn result(hops: ResultHops) -> SyncCommandResult {
    SyncCommandResult {
        command_id: "6f1f8a3e-2c55-4c55-9a43-1f2e3d4c5b6a"
            .parse::<Uuid>()
            .unwrap(),
        status: CommandStatus::Applied,
        reason: None,
        result: None,
        at_utc: DateTimeOffset::utc(2026, 11, 26, 8, 0, 0).unwrap(),
        exec_ms: Some(17),
        hops,
    }
}

const STAMPED: BatchHops = BatchHops {
    t_detected: Some(1_764_144_000_000_001),
    t_read: Some(1_764_144_000_003_000),
    t_sent: Some(1_764_144_000_004_500),
};

#[test]
fn a_batch_writes_its_stamps_after_the_other_members_and_reads_them_back() {
    let b = batch(STAMPED);
    let json = b.to_json();
    assert!(
        json.ends_with(r#""tombstones":[],"t_detected":1764144000000001,"t_read":1764144000003000,"t_sent":1764144000004500}"#),
        "{json}"
    );
    assert_eq!(SyncBatch::from_json(&json).unwrap(), b);
}

#[test]
fn a_batch_without_stamps_writes_none_so_an_older_cloud_never_sees_them() {
    let json = batch(BatchHops::default()).to_json();
    for name in ["t_detected", "t_read", "t_sent"] {
        assert!(!json.contains(name), "{json}");
    }
    assert_eq!(
        SyncBatch::from_json(&json).unwrap().hops,
        BatchHops::default()
    );
}

#[test]
fn a_result_writes_its_stamps_after_exec_ms_and_reads_them_back() {
    let r = result(ResultHops {
        t_received: Some(1_764_144_000_012_000),
        t_started: Some(1_764_144_000_013_000),
        t_finished: Some(1_764_144_000_030_000),
    });
    let json = r.to_json();
    assert!(
        json.ends_with(r#""exec_ms":17,"t_received":1764144000012000,"t_started":1764144000013000,"t_finished":1764144000030000}"#),
        "{json}"
    );
    assert_eq!(SyncCommandResult::from_json(&json).unwrap(), r);
    let plain = result(ResultHops::default()).to_json();
    for name in ["t_received", "t_started", "t_finished"] {
        assert!(!plain.contains(name), "{plain}");
    }
    assert_eq!(
        SyncCommandResult::from_json(&plain).unwrap().hops,
        ResultHops::default(),
        "a result from an older agent has no stamps"
    );
}

#[test]
fn t_sent_is_added_as_the_last_member_of_a_stored_batch() {
    let stored = batch(BatchHops {
        t_sent: None,
        ..STAMPED
    })
    .to_json();
    let sent = with_t_sent(stored.as_bytes(), Some(1_764_144_000_004_500));
    assert_eq!(
        std::str::from_utf8(&sent).unwrap(),
        batch(STAMPED).to_json(),
        "the same bytes the batch would have with the stamp"
    );
    let old = batch(BatchHops::default()).to_json();
    let sent = with_t_sent(old.as_bytes(), Some(5));
    assert_eq!(
        SyncBatch::from_json(std::str::from_utf8(&sent).unwrap())
            .unwrap()
            .hops,
        BatchHops {
            t_sent: Some(5),
            ..BatchHops::default()
        },
        "a batch an older agent stored still gets the stamp"
    );
}

#[test]
fn t_sent_leaves_anything_else_unchanged() {
    let stamped = batch(STAMPED).to_json();
    for payload in [
        stamped.as_str(),
        "",
        "   ",
        "{}",
        "{ }",
        "[1,2]",
        "not json}",
    ] {
        let out = with_t_sent(payload.as_bytes(), Some(9));
        assert_eq!(&*out, payload.as_bytes(), "{payload:?}");
    }
    let plain = batch(BatchHops::default()).to_json();
    assert_eq!(&*with_t_sent(plain.as_bytes(), None), plain.as_bytes());
    let spaced = format!("{plain}\n");
    let out = with_t_sent(spaced.as_bytes(), Some(9));
    assert!(
        std::str::from_utf8(&out)
            .unwrap()
            .ends_with(",\"t_sent\":9}\n")
    );
}
