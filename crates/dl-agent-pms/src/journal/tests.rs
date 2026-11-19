use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use dl_json::Map;

use super::exec::{CODE_JOURNAL_FULL, CODE_JOURNAL_UNAVAILABLE, Journaled};
use super::*;
use crate::ctx::Ctx;
use crate::exec::Executor;
use crate::log::Logger;
use crate::telemetry::{self, MemoryReporter, Reporter, Telemetry};
use crate::wire::{Op, Row};

const OP_ID: &str = "6f1c2b7e-8d4a-4c3b-9e2f-1a2b3c4d5e6f";
const OP_ID2: &str = "7a1c2b7e-8d4a-4c3b-9e2f-1a2b3c4d5e6f";

/// A fresh directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let n = N.fetch_add(1, Ordering::SeqCst);
        TempDir(std::env::temp_dir().join(format!(
            "pms-journal-{tag}-{}-{nanos}-{n}",
            std::process::id()
        )))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn applied(v: &str) -> OpResult {
    let mut row = Row::new();
    row.insert("v".into(), Value::String(v.into()));
    OpResult {
        id: OP_ID.into(),
        status: wire::STATUS_APPLIED.into(),
        sets: vec![vec![row]],
        rowcounts: vec![1],
        outputs: BTreeMap::new(),
        error: None,
        replayed: None,
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn entries_round_trip_under_a_hashed_name() {
    let t = TempDir::new("rt");
    let j = Journal::open_dir(&t.0, Limits::default()).unwrap();
    let key = "tenant-1:fo.x:42";
    j.put(key, "fo.x.set", &applied("a")).unwrap();
    let e = j.get(key).unwrap().unwrap();
    assert_eq!(
        (e.op.as_str(), e.result.clone()),
        ("fo.x.set", applied("a"))
    );
    assert!(e.at_ms > 0);
    assert_eq!(j.get("other").unwrap(), None);
    assert_eq!(names(&t.0), vec![entry_name(key)]);
    let raw = fs::read_to_string(t.0.join(entry_name(key))).unwrap();
    assert!(!raw.contains(key), "the key is never stored");
    assert!(raw.starts_with(r#"{"v":1,"op":"fo.x.set","at":"#), "{raw}");
    // Writing the same key again replaces the entry; the counts stay right.
    j.put(key, "fo.x.set", &applied("b")).unwrap();
    assert_eq!(j.get(key).unwrap().unwrap().result, applied("b"));
    let h = j.health();
    assert_eq!(h.entries, 1);
    assert_eq!(
        h.bytes,
        fs::metadata(t.0.join(entry_name(key))).unwrap().len()
    );
    assert!(!h.full());
}

#[test]
fn open_removes_stale_temporaries_and_expired_entries() {
    let t = TempDir::new("open");
    fs::create_dir_all(&t.0).unwrap();
    let j = Journal::open_dir(&t.0, Limits::default()).unwrap();
    j.put("old", "a.b.c", &applied("o")).unwrap();
    j.put("new", "a.b.c", &applied("n")).unwrap();
    drop(j);
    let old = fs::File::options()
        .write(true)
        .open(t.0.join(entry_name("old")))
        .unwrap();
    old.set_modified(SystemTime::now() - RETENTION - Duration::from_secs(60))
        .unwrap();
    drop(old);
    fs::write(
        t.0.join(format!("{}{TMP_MARK}1-2", entry_name("x"))),
        b"half",
    )
    .unwrap();
    fs::write(t.0.join("notes.txt"), b"left alone").unwrap();
    let j = Journal::open_dir(&t.0, Limits::default()).unwrap();
    assert_eq!(names(&t.0), {
        let mut v = vec![entry_name("new"), "notes.txt".to_owned()];
        v.sort();
        v
    });
    assert_eq!(j.health().entries, 1);
    assert_eq!(j.get("old").unwrap(), None);
    assert!(RETENTION >= Duration::from_secs(7 * 24 * 3600));
}

#[test]
fn the_cap_refuses_new_entries_until_some_expire() {
    let t = TempDir::new("cap");
    let limits = Limits {
        retention: Duration::from_secs(3600),
        max_entries: 2,
        max_bytes: MAX_BYTES,
    };
    let j = Journal::open_dir(&t.0, limits).unwrap();
    j.put("a", "a.b.c", &applied("1")).unwrap();
    j.put("b", "a.b.c", &applied("2")).unwrap();
    assert!(j.health().full());
    let err = j.put("c", "a.b.c", &applied("3")).unwrap_err();
    assert!(
        matches!(err, JournalError::Full { entries: 2, .. }),
        "{err}"
    );
    assert_eq!(j.get("c").unwrap(), None);
    let small = Limits {
        max_entries: MAX_ENTRIES,
        max_bytes: 10,
        ..limits
    };
    let t2 = TempDir::new("cap-bytes");
    let j2 = Journal::open_dir(&t2.0, small).unwrap();
    assert!(matches!(
        j2.put("a", "a.b.c", &applied("1")),
        Err(JournalError::Full { .. })
    ));
    assert!(names(&t2.0).is_empty());
}

#[test]
fn a_damaged_entry_is_corrupt_not_a_miss() {
    let t = TempDir::new("bad");
    let j = Journal::open_dir(&t.0, Limits::default()).unwrap();
    for raw in [
        &b"garbage"[..],
        b"{\"v\":2,\"op\":\"a\",\"at\":1}\n{}\n",
        b"{\"v\":1,\"op\":\"a\",\"at\":1}\n{\"type\":\"ping\",\"ts\":1}\n",
    ] {
        fs::write(t.0.join(entry_name("k")), raw).unwrap();
        assert!(matches!(j.get("k"), Err(JournalError::Corrupt(_))));
    }
}

#[test]
fn inspect_creates_nothing() {
    let t = TempDir::new("inspect");
    let h = inspect(&t.0).unwrap();
    assert_eq!((h.entries, h.bytes, h.full()), (0, 0, false));
    assert!(!t.0.exists());
    let j = Journal::open(&t.0).unwrap();
    j.put("k", "a.b.c", &applied("1")).unwrap();
    let h = inspect(&t.0).unwrap();
    assert_eq!(h.entries, 1);
    assert_eq!(h.dir, t.0.join(DIR));
    let js = dl_json::to_string(&h.to_json());
    assert!(js.contains(r#""retention_days":8"#), "{js}");
}

/// Counts calls; answers `status` (or blocks on `gate` first when set).
struct Fake {
    calls: AtomicUsize,
    status: &'static str,
    gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}

impl Fake {
    fn new(status: &'static str) -> Arc<Fake> {
        Arc::new(Fake {
            calls: AtomicUsize::new(0),
            status,
            gate: Mutex::new(None),
        })
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Executor for Fake {
    fn execute(&self, _: &Ctx, op: &Op) -> OpResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some((started, release)) = self.gate.lock().unwrap().take() {
            started.send(()).unwrap();
            release.recv().unwrap();
        }
        let mut r = applied("ran");
        r.id = op.id.clone();
        if self.status != wire::STATUS_APPLIED {
            r = crate::exec::failed(&op.id, "SQL_ERROR", "boom");
        }
        r
    }
    fn contract_max(&self) -> i64 {
        3
    }
}

fn op(id: &str, name: &str, key: Option<&str>, replay: Option<bool>) -> Op {
    Op {
        id: id.into(),
        op: name.into(),
        contract_ver: 1,
        target_db: "SMILE_FO".into(),
        args: Value::Object(Map::new()),
        request_key: key.map(str::to_owned),
        expected_stamp: None,
        deadline_ms: 1000,
        replay,
    }
}

fn code(r: &OpResult) -> (&str, &str, &str) {
    let e = r.error.as_ref().expect("an error");
    (r.status.as_str(), e.code.as_str(), e.message.as_str())
}

#[test]
fn replays_are_answered_from_the_journal_only() {
    let t = TempDir::new("replay");
    let j = Arc::new(Journal::open_dir(&t.0, Limits::default()).unwrap());
    let fake = Fake::new(wire::STATUS_APPLIED);
    let x = Journaled::new(fake.clone(), Some(j.clone()), Logger::discard(), None);
    let ctx = Ctx::background();
    assert_eq!(x.contract_max(), 3);

    // A read carries no key and passes straight through.
    let r = x.execute(&ctx, &op(OP_ID, "fo.hotelDate.get", None, None));
    assert_eq!((r.status.as_str(), r.replayed), ("applied", None));
    assert_eq!(fake.calls(), 1);
    assert_eq!(j.health().entries, 0);

    // A replay of an unknown key runs nothing.
    let r = x.execute(&ctx, &op(OP_ID, "fo.x.set", Some("k1"), Some(true)));
    assert_eq!(
        code(&r),
        (
            "rejected",
            wire::ERR_REQUEST_KEY_UNKNOWN,
            "request key not in the op journal"
        )
    );
    let r = x.execute(&ctx, &op(OP_ID, "fo.x.set", None, Some(true)));
    assert_eq!(code(&r).1, wire::ERR_REQUEST_KEY_UNKNOWN);
    assert_eq!(fake.calls(), 1);

    // A keyed write runs once and is recorded after it applied.
    let r = x.execute(&ctx, &op(OP_ID, "fo.x.set", Some("k1"), None));
    assert_eq!((r.status.as_str(), r.replayed), ("applied", None));
    assert_eq!(fake.calls(), 2);
    assert_eq!(j.get("k1").unwrap().unwrap().op, "fo.x.set");

    // Its replay (and a resend without the flag) is the stored answer, replayed: true.
    for replay in [Some(true), None] {
        let r = x.execute(&ctx, &op(OP_ID2, "fo.x.set", Some("k1"), replay));
        assert_eq!((r.id.as_str(), r.status.as_str()), (OP_ID2, "applied"));
        assert_eq!(r.replayed, Some(true));
        assert_eq!(r.sets, applied("ran").sets);
        wire::encode(&Frame::Result(r)).unwrap();
    }
    assert_eq!(fake.calls(), 2);

    // The same key for another op is a conflict, not a replay.
    let r = x.execute(&ctx, &op(OP_ID2, "fo.y.set", Some("k1"), Some(true)));
    assert_eq!(
        code(&r),
        ("rejected", wire::ERR_OP_REJECTED, "request_key_conflict")
    );

    // A damaged entry is refused, never re-run.
    fs::write(t.0.join(entry_name("k1")), b"garbage").unwrap();
    let r = x.execute(&ctx, &op(OP_ID2, "fo.x.set", Some("k1"), None));
    assert_eq!(
        code(&r),
        ("rejected", wire::ERR_OP_REJECTED, "journal_corrupt")
    );
    assert_eq!(fake.calls(), 2);
}

#[test]
fn only_applied_writes_are_recorded() {
    let t = TempDir::new("failed");
    let j = Arc::new(Journal::open_dir(&t.0, Limits::default()).unwrap());
    let fake = Fake::new(wire::STATUS_FAILED);
    let x = Journaled::new(fake.clone(), Some(j.clone()), Logger::discard(), None);
    let ctx = Ctx::background();
    let r = x.execute(&ctx, &op(OP_ID, "fo.x.set", Some("k"), None));
    assert_eq!(r.status, wire::STATUS_FAILED);
    assert_eq!(j.health().entries, 0);
    // Rolled back: the cloud re-arms the key normally, and the write runs again.
    x.execute(&ctx, &op(OP_ID2, "fo.x.set", Some("k"), None));
    assert_eq!(fake.calls(), 2);
    let r = x.execute(&ctx, &op(OP_ID2, "fo.x.set", Some("k"), Some(true)));
    assert_eq!(code(&r).1, wire::ERR_REQUEST_KEY_UNKNOWN);
}

#[test]
fn a_full_or_missing_journal_refuses_keyed_writes_before_they_run() {
    let t = TempDir::new("full");
    let limits = Limits {
        max_entries: 1,
        ..Limits::default()
    };
    let j = Arc::new(Journal::open_dir(&t.0, limits).unwrap());
    let rep = Arc::new(MemoryReporter::default());
    let tel = Arc::new(Telemetry::new(Some(rep.clone() as Arc<dyn Reporter>)));
    let fake = Fake::new(wire::STATUS_APPLIED);
    let x = Journaled::new(fake.clone(), Some(j), Logger::discard(), Some(tel));
    let ctx = Ctx::background();
    assert_eq!(
        x.execute(&ctx, &op(OP_ID, "fo.x.set", Some("a"), None))
            .status,
        wire::STATUS_APPLIED
    );
    let r = x.execute(&ctx, &op(OP_ID2, "fo.x.set", Some("b"), None));
    assert_eq!(code(&r), ("failed", CODE_JOURNAL_FULL, "op journal full"));
    assert_eq!(fake.calls(), 1);
    assert_eq!(rep.codes(), vec![telemetry::CODE_JOURNAL_FAILED]);
    // Unkeyed ops still run.
    x.execute(&ctx, &op(OP_ID2, "fo.hotelDate.get", None, None));
    assert_eq!(fake.calls(), 2);

    let none = Journaled::new(fake.clone(), None, Logger::discard(), None);
    let r = none.execute(&ctx, &op(OP_ID, "fo.x.set", Some("a"), None));
    assert_eq!(
        code(&r),
        ("failed", CODE_JOURNAL_UNAVAILABLE, "journal_unavailable")
    );
    let r = none.execute(&ctx, &op(OP_ID, "fo.x.set", Some("a"), Some(true)));
    assert_eq!(code(&r).1, wire::ERR_REQUEST_KEY_UNKNOWN);
    assert_eq!(fake.calls(), 2);
}

#[test]
fn the_same_key_twice_at_once_is_busy() {
    let t = TempDir::new("busy");
    let j = Arc::new(Journal::open_dir(&t.0, Limits::default()).unwrap());
    let fake = Fake::new(wire::STATUS_APPLIED);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *fake.gate.lock().unwrap() = Some((started_tx, release_rx));
    let x = Arc::new(Journaled::new(
        fake.clone(),
        Some(j),
        Logger::discard(),
        None,
    ));
    let x2 = x.clone();
    let h = std::thread::spawn(move || {
        x2.execute(&Ctx::background(), &op(OP_ID, "fo.x.set", Some("k"), None))
    });
    started_rx.recv().unwrap();
    let r = x.execute(&Ctx::background(), &op(OP_ID2, "fo.x.set", Some("k"), None));
    assert_eq!(code(&r).1, wire::ERR_AGENT_BUSY);
    release_tx.send(()).unwrap();
    assert_eq!(h.join().unwrap().status, wire::STATUS_APPLIED);
    // Done: the key is free again and now answered from the journal.
    let r = x.execute(&Ctx::background(), &op(OP_ID2, "fo.x.set", Some("k"), None));
    assert_eq!(r.replayed, Some(true));
    assert_eq!(fake.calls(), 1);
}
