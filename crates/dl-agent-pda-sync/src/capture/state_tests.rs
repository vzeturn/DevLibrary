//! `State/StateTests.cs`: both [`CaptureState`] implementations against the same cases.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use super::sqlite_state::SqliteCaptureState;
use super::state::*;
use crate::util::ctx::Ctx;

/// A temp `capture.db` path removed (with `-wal`/`-shm`) on drop.
struct TempDb(PathBuf);

impl TempDb {
    fn new() -> TempDb {
        static N: AtomicU32 = AtomicU32::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        TempDb(std::env::temp_dir().join(format!(
            "capture-state-{}-{nanos}-{}.db",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        )))
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut p = self.0.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(p);
        }
    }
}

/// The state under test plus the file it lives in (dropped after the state).
struct Under {
    state: Box<dyn CaptureState>,
    _db: Option<TempDb>,
}

fn create(kind: &str) -> Under {
    if kind == "memory" {
        return Under {
            state: Box::new(InMemoryCaptureState::new()),
            _db: None,
        };
    }
    let db = TempDb::new();
    Under {
        state: Box::new(SqliteCaptureState::open(&db.0).unwrap()),
        _db: Some(db),
    }
}

const KINDS: [&str; 2] = ["memory", "sqlite"];

fn ctx() -> Ctx {
    Ctx::background()
}

fn commit(stream: &str, seq: i64, writes: Vec<ShadowWrite>) -> StateCommit {
    StateCommit {
        stream: Some(stream.to_owned()),
        seq: Some(seq),
        shadow: writes,
        ..StateCommit::default()
    }
}

fn write(table: &str, upserts: Vec<ShadowEntry>, deletes: &[&str]) -> ShadowWrite {
    ShadowWrite {
        table: table.to_owned(),
        upserts,
        delete_keys: deletes.iter().map(|s| (*s).to_owned()).collect(),
    }
}

fn sorted_pairs(s: &Shadow) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = s
        .iter()
        .map(|(k, h)| (k.to_owned(), h.to_owned()))
        .collect();
    v.sort();
    v
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(k, h)| ((*k).to_owned(), (*h).to_owned()))
        .collect()
}

#[test]
fn sequence_starts_at_one_and_advances_with_each_commit() {
    for kind in KINDS {
        let u = create(kind);
        let s = &*u.state;
        assert_eq!(s.peek_next_seq("open", &ctx()).unwrap(), 1);
        s.commit(&commit("open", 1, vec![]), &ctx()).unwrap();
        s.commit(&commit("open", 2, vec![]), &ctx()).unwrap();
        assert_eq!(s.peek_next_seq("open", &ctx()).unwrap(), 3, "{kind}");
        assert_eq!(
            s.peek_next_seq("Invoice", &ctx()).unwrap(),
            1,
            "streams are independent"
        );
    }
}

#[test]
fn shadow_upsert_delete_buckets_and_refs_round_trip() {
    for kind in KINDS {
        let u = create(kind);
        let s = &*u.state;
        s.commit(
            &commit(
                "t",
                1,
                vec![write(
                    "T",
                    vec![
                        ShadowEntry::new("k1", "h1", "d1", Some("ref1")),
                        ShadowEntry::new("k2", "h2", "d1", None),
                        ShadowEntry::new("k3", "h3", "d2", None),
                    ],
                    &[],
                )],
            ),
            &ctx(),
        )
        .unwrap();

        let all = s.get_shadow("T", None, &ctx()).unwrap();
        assert_eq!(
            sorted_pairs(&all),
            pairs(&[("k1", "h1"), ("k2", "h2"), ("k3", "h3")]),
            "{kind}"
        );
        let mut d1: Vec<&str> = Vec::new();
        let d1_shadow = s.get_shadow("T", Some("d1"), &ctx()).unwrap();
        d1.extend(d1_shadow.keys());
        d1.sort_unstable();
        assert_eq!(d1, ["k1", "k2"]);
        assert_eq!(s.get_buckets("T", &ctx()).unwrap(), ["d1", "d2"]);
        let entries = s.get_shadow_entries("T", "d1", &ctx()).unwrap();
        assert!(
            entries
                .iter()
                .any(|e| e.key == "k1" && e.ref_.as_deref() == Some("ref1"))
        );
        let by_keys = s
            .get_shadow_by_keys("T", &["k1".to_owned(), "zz".to_owned()], &ctx())
            .unwrap();
        assert_eq!(sorted_pairs(&by_keys), pairs(&[("k1", "h1")]));

        s.commit(
            &commit(
                "t",
                2,
                vec![write(
                    "T",
                    vec![ShadowEntry::new("k1", "h1b", "d2", None)],
                    &["k2"],
                )],
            ),
            &ctx(),
        )
        .unwrap();

        let all = s.get_shadow("T", None, &ctx()).unwrap();
        assert_eq!(sorted_pairs(&all), pairs(&[("k1", "h1b"), ("k3", "h3")]));
        assert!(
            s.get_shadow("T", Some("d1"), &ctx()).unwrap().is_empty(),
            "k1 moved bucket and k2 was deleted"
        );
        assert!(
            s.get_shadow("Other", None, &ctx()).unwrap().is_empty(),
            "tables are independent"
        );
    }
}

#[test]
fn cursor_set_overwritten_and_deleted_by_null() {
    for kind in KINDS {
        let u = create(kind);
        let s = &*u.state;
        let mut set = StateCommit::default();
        set.cursors.insert("wm:Invoice", Some("a".to_owned()));
        s.commit(&set, &ctx()).unwrap();
        assert_eq!(
            s.get_cursor("wm:Invoice", &ctx()).unwrap().as_deref(),
            Some("a")
        );

        let mut change = StateCommit::default();
        change.cursors.insert("wm:Invoice", Some("b".to_owned()));
        s.commit(&change, &ctx()).unwrap();
        assert_eq!(
            s.get_cursor("wm:Invoice", &ctx()).unwrap().as_deref(),
            Some("b")
        );

        let mut delete = StateCommit::default();
        delete.cursors.insert("wm:Invoice", None);
        s.commit(&delete, &ctx()).unwrap();
        assert_eq!(s.get_cursor("wm:Invoice", &ctx()).unwrap(), None, "{kind}");
    }
}

#[test]
fn reset_forgets_one_table_only() {
    for kind in KINDS {
        let u = create(kind);
        let s = &*u.state;
        s.commit(
            &commit(
                "t",
                1,
                vec![
                    write("A", vec![ShadowEntry::new("k", "h", "b", None)], &[]),
                    write("B", vec![ShadowEntry::new("k", "h", "b", None)], &[]),
                ],
            ),
            &ctx(),
        )
        .unwrap();
        s.reset_table("A", &ctx()).unwrap();
        assert!(
            s.get_shadow("A", None, &ctx()).unwrap().is_empty(),
            "{kind}"
        );
        assert_eq!(s.get_shadow("B", None, &ctx()).unwrap().len(), 1);
    }
}

#[test]
fn many_keys_are_looked_up_in_chunks() {
    let u = create("sqlite");
    let s = &*u.state;
    let entries: Vec<ShadowEntry> = (0..2500)
        .map(|i| ShadowEntry::new(&format!("k{i}"), &format!("h{i}"), "b", None))
        .collect();
    let keys: Vec<String> = entries.iter().map(|e| e.key.clone()).collect();
    s.commit(&commit("t", 1, vec![write("T", entries, &[])]), &ctx())
        .unwrap();
    let found = s.get_shadow_by_keys("T", &keys, &ctx()).unwrap();
    assert_eq!(found.len(), 2500);
}

#[test]
fn sqlite_state_survives_a_restart() {
    let db = TempDb::new();
    {
        let first = SqliteCaptureState::open(&db.0).unwrap();
        let mut c = commit(
            "open",
            7,
            vec![write("T", vec![ShadowEntry::new("k", "h", "b", None)], &[])],
        );
        c.cursors.insert("wm:T", Some("pos".to_owned()));
        first.commit(&c, &ctx()).unwrap();
    }
    let second = SqliteCaptureState::open(&db.0).unwrap();
    assert_eq!(second.peek_next_seq("open", &ctx()).unwrap(), 8);
    assert_eq!(
        second.get_cursor("wm:T", &ctx()).unwrap().as_deref(),
        Some("pos")
    );
    assert_eq!(second.get_shadow("T", None, &ctx()).unwrap().len(), 1);
}

#[test]
fn a_commit_is_atomic_when_one_part_fails() {
    // C# makes the second upsert fail with a NULL key; Rust keys cannot be null, so a
    // trigger rejects the second key instead (same mid-commit failure).
    let db = TempDb::new();
    let s = SqliteCaptureState::open(&db.0).unwrap();
    {
        let raw =
            dl_sqlite::Connection::open(&db.0, dl_sqlite::OpenFlags::READ_WRITE_CREATE).unwrap();
        raw.execute_batch(
            "CREATE TRIGGER reject_bad BEFORE INSERT ON shadow WHEN NEW.key = 'bad' BEGIN SELECT RAISE(ABORT, 'bad key'); END;",
        )
        .unwrap();
    }
    let bad = commit(
        "open",
        1,
        vec![
            write("T", vec![ShadowEntry::new("k", "h", "b", None)], &[]),
            write("T", vec![ShadowEntry::new("bad", "h", "b", None)], &[]),
        ],
    );
    let e = s.commit(&bad, &ctx()).unwrap_err();
    assert_eq!(e.kind, super::ErrorKind::LocalStore);
    assert!(s.get_shadow("T", None, &ctx()).unwrap().is_empty());
    assert_eq!(s.peek_next_seq("open", &ctx()).unwrap(), 1);
}

#[test]
fn a_cancelled_context_stops_before_touching_the_store() {
    let u = create("sqlite");
    let c = Ctx::background().child();
    c.cancel();
    assert!(
        u.state
            .peek_next_seq("open", &c)
            .unwrap_err()
            .is_cancelled()
    );
}
