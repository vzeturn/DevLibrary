//! Port of Go `jobqueue/file_test.go` plus the order and the legacy import.

use super::*;

pub(crate) fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("dl-hl-q-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn job(id: &str, booking: &str) -> Job {
    Job {
        job_id: id.into(),
        booking_id: booking.into(),
        ..Job::default()
    }
}

#[test]
fn lifecycle() {
    let q = FileQueue::open(&temp_dir("life").join("queue.jsonl")).expect("open");
    q.enqueue(&job("j1", "b1")).expect("enqueue");
    let pend = q.pending();
    assert_eq!(pend.len(), 1);
    assert_eq!(pend[0].state, PENDING);
    assert_eq!(q.depth(), 1);

    q.mark_applied("j1", true, "", None).expect("applied");
    assert!(q.pending().is_empty());
    let un = q.unacked();
    assert_eq!((un.len(), un[0].state.as_str()), (1, APPLIED));

    q.mark_acked("j1").expect("acked");
    assert!(q.unacked().is_empty());
    assert_eq!(q.depth(), 0);
}

#[test]
fn compact_drops_acked() {
    let path = temp_dir("compact").join("queue.jsonl");
    let q = FileQueue::open(&path).expect("open");
    q.enqueue(&job("j1", "b1")).expect("enqueue");
    q.mark_applied("j1", true, "", None).expect("applied");
    q.mark_acked("j1").expect("acked");
    q.enqueue(&job("j2", "b2")).expect("enqueue");
    q.compact().expect("compact");
    let q2 = FileQueue::open(&path).expect("reopen");
    assert_eq!(q2.depth(), 1);
    let pend = q2.pending();
    assert_eq!((pend.len(), pend[0].job_id.as_str()), (1, "j2"));
    let raw = std::fs::read_to_string(&path).expect("read");
    assert_eq!(raw.lines().count(), 1, "{raw}");
}

#[test]
fn corrupt_trailing_line_tolerated() {
    let path = temp_dir("corrupt").join("queue.jsonl");
    let q = FileQueue::open(&path).expect("open");
    q.enqueue(&job("j1", "b1")).expect("enqueue");
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append");
    f.write_all(br#"{"jobId":"j2","stat"#).expect("write");
    drop(f);
    let q2 = FileQueue::open(&path).expect("open must tolerate a corrupt line");
    assert_eq!(q2.depth(), 1);
}

#[test]
fn restart_replays_unacked() {
    let path = temp_dir("replay").join("queue.jsonl");
    let q = FileQueue::open(&path).expect("open");
    q.enqueue(&job("j1", "b1")).expect("enqueue");
    let res = PmsResult {
        action: "save".into(),
        completed: true,
        synced_at: "2026-06-10T08:00:00Z".into(),
        ..PmsResult::default()
    };
    q.mark_applied("j1", false, "boom", Some(&res))
        .expect("applied");
    let q2 = FileQueue::open(&path).expect("restart");
    let un = q2.unacked();
    assert_eq!(un.len(), 1);
    assert_eq!(un[0].job_id, "j1");
    assert!(!un[0].ok);
    assert_eq!(un[0].last_error, "boom");
    assert_eq!(un[0].pms_result.as_ref(), Some(&res));
    assert_eq!(
        un[0].job.booking_id, "b1",
        "the job survives the round trip"
    );
}

#[test]
fn enqueue_is_idempotent_and_ordered() {
    let q = FileQueue::open(&temp_dir("order").join("queue.jsonl")).expect("open");
    for id in ["j3", "j1", "j2"] {
        q.enqueue(&job(id, "b")).expect("enqueue");
    }
    q.mark_applied("j1", true, "", None).expect("applied");
    q.enqueue(&job("j1", "b")).expect("again");
    let ids: Vec<String> = q.pending().into_iter().map(|e| e.job_id).collect();
    assert_eq!(ids, ["j3", "j2"]);
    assert_eq!(q.unacked().len(), 1, "a re-enqueue keeps the applied state");
    q.mark_acked("unknown").expect("ignored");
    assert_eq!(q.depth(), 3);
}

#[test]
fn line_format_matches_go() {
    let e = Entry {
        job_id: "j1".into(),
        booking_id: "b1".into(),
        state: APPLIED.into(),
        ok: true,
        updated_at: "2026-06-10T08:00:00Z".into(),
        ..Entry::default()
    };
    let line = e.to_line();
    assert!(
        line.starts_with(r#"{"jobId":"j1","bookingId":"b1","job":{"#),
        "{line}"
    );
    assert!(
        line.ends_with(
            "\"state\":\"applied\",\"ok\":true,\"updatedAt\":\"2026-06-10T08:00:00Z\"}\n"
        ),
        "{line}"
    );
    assert_eq!(Entry::from_line(&line).as_ref(), Some(&e));
}

#[test]
fn import_legacy_once() {
    let dir = temp_dir("legacy");
    let legacy_path = dir.join("go").join("queue.jsonl");
    let old = FileQueue::open(&legacy_path).expect("go queue");
    for id in ["p1", "a1", "k1", "dup"] {
        old.enqueue(&job(id, "b")).expect("enqueue");
    }
    old.mark_applied("a1", true, "", None).expect("applied");
    old.mark_applied("k1", true, "", None).expect("applied");
    old.mark_acked("k1").expect("acked");
    let before = std::fs::read(&legacy_path).expect("read");

    let q = FileQueue::open(&dir.join("new").join("queue.jsonl")).expect("open");
    q.enqueue(&job("dup", "b")).expect("enqueue");
    q.mark_applied("dup", false, "kept", None).expect("applied");
    let marker = dir.join("new").join("legacy-queue-imported");
    assert_eq!(q.import_legacy(&legacy_path, &marker).expect("import"), 2);
    let pend: Vec<String> = q.pending().into_iter().map(|e| e.job_id).collect();
    let un: Vec<String> = q.unacked().into_iter().map(|e| e.job_id).collect();
    assert_eq!(pend, ["p1"]);
    assert_eq!(
        un,
        ["dup", "a1"],
        "the local entry wins over the legacy one"
    );
    assert!(marker.exists());
    assert_eq!(
        std::fs::read(&legacy_path).expect("read"),
        before,
        "Go file untouched"
    );

    old.enqueue(&job("late", "b")).expect("enqueue");
    assert_eq!(
        q.import_legacy(&legacy_path, &marker).expect("again"),
        0,
        "runs once"
    );
    let none = dir.join("missing.jsonl");
    assert_eq!(q.import_legacy(&none, &dir.join("m2")).expect("missing"), 0);
}
