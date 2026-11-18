//! Tests against the real OS library. They skip (with a note) only when the library cannot
//! be loaded.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::*;

fn available() -> bool {
    match library_version() {
        Ok(v) => {
            println!("SQLite library version {v}");
            true
        }
        Err(e) => {
            println!("note: skipping dl-sqlite test: {e}");
            false
        }
    }
}

fn mem() -> Option<Connection> {
    if !available() {
        return None;
    }
    Some(Connection::open_in_memory().unwrap())
}

/// A database file in the temp directory, deleted (with its -wal/-shm) on drop.
struct TempDb(PathBuf);

impl TempDb {
    fn new(tag: &str) -> TempDb {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let name = format!("dl-sqlite-{tag}-{}-{n}-ă.db", std::process::id());
        let t = TempDb(std::env::temp_dir().join(name));
        t.cleanup();
        t
    }

    fn cleanup(&self) {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut p = self.0.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(p);
        }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[test]
fn version() {
    if !available() {
        return;
    }
    assert!(library_version().unwrap().starts_with("3."));
    assert!(library_version_number().unwrap() >= 3_007_015);
}

#[test]
fn connection_is_send() {
    fn send<T: Send>() {}
    send::<Connection>();
}

#[test]
fn create_insert_select() {
    let Some(c) = mem() else { return };
    c.execute_batch(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL, data BLOB);
         -- a comment between statements
         CREATE INDEX t_name ON t(name);",
    )
    .unwrap();
    let n = c
        .execute(
            "INSERT INTO t (name, score, data) VALUES (?, ?, ?)",
            params!["a", 1.5, b"\x00\x01".as_slice()],
        )
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(c.last_insert_rowid(), 1);
    c.execute(
        "INSERT INTO t (name, score, data) VALUES (?1, ?2, ?3)",
        params!["b", 2_i32, None::<Vec<u8>>],
    )
    .unwrap();
    assert_eq!(c.changes(), 1);

    let rows = c
        .query(
            "SELECT id, name, score, data FROM t ORDER BY id",
            params![],
            |r| {
                Ok((
                    r.get::<i64>(0)?,
                    r.get::<String>(1)?,
                    r.get::<f64>(2)?,
                    r.get::<Option<Vec<u8>>>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (1, "a".to_owned(), 1.5, Some(vec![0, 1])),
            (2, "b".to_owned(), 2.0, None),
        ]
    );

    let mut st = c
        .prepare("SELECT id AS ident, name FROM t WHERE id > ?")
        .unwrap();
    assert_eq!(st.parameter_count(), 1);
    assert_eq!(st.column_count(), 2);
    assert_eq!(st.column_names().unwrap(), ["ident", "name"]);
    assert_eq!(st.get::<i64>(0), Err(Error::NoCurrentRow));
    st.bind(params![0]).unwrap();
    assert!(st.step().unwrap());
    assert_eq!(st.column_type(0).unwrap(), Type::Integer);
    assert_eq!(st.column_type(1).unwrap(), Type::Text);
    assert_eq!(st.get_ref(1).unwrap(), ValueRef::Text(b"a"));
    assert_eq!(
        st.get::<i64>(2),
        Err(Error::ColumnIndex { index: 2, count: 2 })
    );
    assert!(st.step().unwrap());
    assert!(!st.step().unwrap());
    assert_eq!(st.get::<i64>(0), Err(Error::NoCurrentRow));
    st.reset();
    st.bind_at(1, &1).unwrap();
    assert!(st.step().unwrap());
    assert_eq!(st.get::<String>(1).unwrap(), "b");
    assert_eq!(
        st.bind_at(2, &1),
        Err(Error::ParameterIndex { index: 2, count: 1 })
    );
    drop(st);

    let count: i64 = c
        .query_row("SELECT count(*) FROM t", params![], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
    assert_eq!(
        c.query_row("SELECT id FROM t WHERE id = 99", params![], |r| r
            .get::<i64>(0)),
        Err(Error::QueryReturnedNoRows)
    );
}

#[test]
fn wal_and_synchronous() {
    if !available() {
        return;
    }
    let db = TempDb::new("wal");
    let c = Connection::open(&db.0, OpenFlags::default()).unwrap();
    let mode: String = c
        .query_row("PRAGMA journal_mode=WAL", params![], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    c.execute_batch("PRAGMA synchronous=FULL; CREATE TABLE q (id INTEGER PRIMARY KEY, v TEXT);")
        .unwrap();
    let sync: i64 = c
        .query_row("PRAGMA synchronous", params![], |r| r.get(0))
        .unwrap();
    assert_eq!(sync, 2);
    c.execute("INSERT INTO q (v) VALUES ('x')", params![])
        .unwrap();
    drop(c);

    let ro = Connection::open(&db.0, OpenFlags::READ_ONLY).unwrap();
    let v: String = ro
        .query_row("SELECT v FROM q", params![], |r| r.get(0))
        .unwrap();
    assert_eq!(v, "x");
    let e = ro
        .execute("INSERT INTO q (v) VALUES ('y')", params![])
        .unwrap_err();
    assert_eq!(e.code(), Some(codes::READONLY), "{e}");
    drop(ro);

    let missing = TempDb::new("missing");
    let e = Connection::open(&missing.0, OpenFlags::READ_WRITE).unwrap_err();
    assert_eq!(e.code(), Some(codes::CANTOPEN), "{e}");
}

#[test]
fn unique_constraint() {
    let Some(c) = mem() else { return };
    c.execute_batch("CREATE TABLE u (k TEXT UNIQUE)").unwrap();
    c.execute("INSERT INTO u VALUES (?)", params!["k1"])
        .unwrap();
    let e = c
        .execute("INSERT INTO u VALUES (?)", params!["k1"])
        .unwrap_err();
    assert!(e.is_constraint(), "{e}");
    assert!(!e.is_busy());
    assert_eq!(e.code(), Some(codes::CONSTRAINT));
    assert_eq!(e.extended_code(), Some(codes::CONSTRAINT_UNIQUE));
    assert_eq!(e.extended_code(), Some(2067));
    let Error::Sqlite { message, .. } = &e else {
        panic!("{e:?}")
    };
    assert!(message.contains("UNIQUE"), "{message}");
}

#[test]
fn autoincrement() {
    let Some(c) = mem() else { return };
    c.execute_batch("CREATE TABLE a (id INTEGER PRIMARY KEY AUTOINCREMENT, v INTEGER)")
        .unwrap();
    let mut ids = Vec::new();
    for v in 0..3 {
        c.execute("INSERT INTO a (v) VALUES (?)", params![v])
            .unwrap();
        ids.push(c.last_insert_rowid());
    }
    assert_eq!(ids, [1, 2, 3]);
    assert_eq!(
        c.execute("DELETE FROM a WHERE id = 3", params![]).unwrap(),
        1
    );
    c.execute("INSERT INTO a (v) VALUES (9)", params![])
        .unwrap();
    // AUTOINCREMENT never reuses an id, even the largest deleted one.
    assert_eq!(c.last_insert_rowid(), 4);
}

#[test]
fn blob_1mib() {
    let Some(c) = mem() else { return };
    c.execute_batch("CREATE TABLE b (data BLOB)").unwrap();
    let mut x: u32 = 0x1234_5678;
    let data: Vec<u8> = (0..1 << 20)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect();
    c.execute("INSERT INTO b VALUES (?)", params![data])
        .unwrap();
    c.execute("INSERT INTO b VALUES (?)", params![Vec::<u8>::new()])
        .unwrap();
    let back = c
        .query(
            "SELECT data, length(data) FROM b ORDER BY rowid",
            params![],
            |r| Ok((r.get::<Vec<u8>>(0)?, r.get::<i64>(1)?, r.column_type(0)?)),
        )
        .unwrap();
    assert_eq!(back[0].0, data);
    assert_eq!(back[0].1, 1 << 20);
    assert_eq!(back[1], (Vec::new(), 0, Type::Blob));
}

#[test]
fn unicode_text() {
    let Some(c) = mem() else { return };
    c.execute_batch("CREATE TABLE s (v TEXT)").unwrap();
    let text = "Hóa đơn điện tử: Nguyễn Thị Ánh, 12 đường Lê Lợi, Quận 1, TP. Hồ Chí Minh ₫ 🧾";
    let with_nul = "a\0b";
    c.execute("INSERT INTO s VALUES (?)", params![text])
        .unwrap();
    c.execute("INSERT INTO s VALUES (?)", params![with_nul.to_owned()])
        .unwrap();
    c.execute("INSERT INTO s VALUES (?)", params![""]).unwrap();
    let back = c
        .query(
            "SELECT v, length(v) FROM s ORDER BY rowid",
            params![],
            |r| Ok((r.get::<String>(0)?, r.get::<i64>(1)?)),
        )
        .unwrap();
    assert_eq!(back[0].0, text);
    assert_eq!(back[0].1, text.chars().count() as i64);
    assert_eq!(back[1].0, with_nul);
    assert_eq!(back[2], (String::new(), 0));
    let found: i64 = c
        .query_row("SELECT count(*) FROM s WHERE v = ?", params![text], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(found, 1);
}

#[test]
fn invalid_utf8_is_an_error() {
    let Some(c) = mem() else { return };
    let r = c.query_row("SELECT 1, CAST(x'41ff42' AS TEXT)", params![], |r| {
        r.get::<String>(1)
    });
    assert_eq!(
        r,
        Err(Error::Utf8 {
            column: 1,
            valid_up_to: 1
        })
    );
    let raw = c
        .query_row("SELECT CAST(x'41ff42' AS TEXT)", params![], |r| {
            r.get::<Vec<u8>>(0)
        })
        .unwrap();
    assert_eq!(raw, [0x41, 0xff, 0x42]);
}

#[test]
fn nulls_and_types() {
    let Some(c) = mem() else { return };
    c.execute_batch("CREATE TABLE n (a INTEGER, b TEXT, c REAL, d BLOB)")
        .unwrap();
    c.execute(
        "INSERT INTO n VALUES (?, ?, ?, ?)",
        params![None::<i64>, None::<&str>, Value::Null, None::<Vec<u8>>],
    )
    .unwrap();
    c.query_row("SELECT a, b, c, d FROM n", params![], |r| {
        for i in 0..4 {
            assert_eq!(r.column_type(i)?, Type::Null);
            assert_eq!(r.get::<Value>(i)?, Value::Null);
            assert_eq!(r.get::<Option<String>>(i)?, None);
        }
        assert_eq!(
            r.get::<i64>(0),
            Err(Error::InvalidType {
                column: 0,
                expected: Type::Integer,
                found: Type::Null
            })
        );
        Ok(())
    })
    .unwrap();
    let values = [
        Value::Integer(-3),
        Value::Real(0.25),
        Value::Text("t".into()),
        Value::Blob(vec![9]),
    ];
    let back: Vec<Value> = c
        .query_row(
            "SELECT ?, ?, ?, ?",
            params![values[0], values[1], values[2], values[3]],
            |r| (0..4).map(|i| r.get(i)).collect(),
        )
        .unwrap();
    assert_eq!(back, values);
    let flags = c
        .query_row("SELECT ?, ?, ?", params![true, false, 300_u32], |r| {
            Ok((r.get::<bool>(0)?, r.get::<bool>(1)?, r.get::<u8>(2)))
        })
        .unwrap();
    assert_eq!(
        flags,
        (
            true,
            false,
            Err(Error::OutOfRange {
                column: 2,
                value: 300
            })
        )
    );
}

#[test]
fn i64_extremes() {
    let Some(c) = mem() else { return };
    c.execute_batch("CREATE TABLE i (v INTEGER)").unwrap();
    for v in [i64::MIN, -1, 0, 1, i64::MAX] {
        c.execute("INSERT INTO i VALUES (?)", params![v]).unwrap();
    }
    c.execute("INSERT INTO i VALUES (?)", params![u32::MAX])
        .unwrap();
    let back = c
        .query("SELECT v FROM i ORDER BY rowid", params![], |r| {
            r.get::<i64>(0)
        })
        .unwrap();
    assert_eq!(back, [i64::MIN, -1, 0, 1, i64::MAX, 4_294_967_295]);
    let f: f64 = c
        .query_row("SELECT ?", params![f64::MIN_POSITIVE], |r| r.get(0))
        .unwrap();
    assert_eq!(f, f64::MIN_POSITIVE);
}

#[test]
fn transactions() {
    let Some(mut c) = mem() else { return };
    c.execute_batch("CREATE TABLE t (v INTEGER)").unwrap();
    let count = |c: &Connection| -> i64 {
        c.query_row("SELECT count(*) FROM t", params![], |r| r.get(0))
            .unwrap()
    };
    {
        let tx = c.transaction().unwrap();
        assert!(!tx.is_autocommit());
        tx.execute("INSERT INTO t VALUES (1)", params![]).unwrap();
        assert_eq!(count(&tx), 1);
        // dropped without commit
    }
    assert!(c.is_autocommit());
    assert_eq!(count(&c), 0);

    let tx = c.transaction_with(TransactionBehavior::Immediate).unwrap();
    tx.execute("INSERT INTO t VALUES (2)", params![]).unwrap();
    tx.commit().unwrap();
    assert!(c.is_autocommit());
    assert_eq!(count(&c), 1);

    let tx = c.transaction_with(TransactionBehavior::Exclusive).unwrap();
    tx.execute("INSERT INTO t VALUES (3)", params![]).unwrap();
    tx.rollback().unwrap();
    assert_eq!(count(&c), 1);

    // Nested BEGIN is an SQLite error, and the outer transaction still rolls back.
    let tx = c.transaction().unwrap();
    tx.execute("INSERT INTO t VALUES (4)", params![]).unwrap();
    let e = tx.execute_batch("BEGIN").unwrap_err();
    assert_eq!(e.code(), Some(codes::ERROR), "{e}");
    drop(tx);
    assert_eq!(count(&c), 1);
}

#[test]
fn busy_between_two_connections() {
    if !available() {
        return;
    }
    let db = TempDb::new("busy");
    let setup = Connection::open(&db.0, OpenFlags::default()).unwrap();
    setup
        .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE o (v INTEGER);")
        .unwrap();
    drop(setup);

    let (held_tx, held_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let path = db.0.clone();
    let writer = std::thread::spawn(move || {
        let mut c = Connection::open(&path, OpenFlags::READ_WRITE).unwrap();
        let tx = c.transaction_with(TransactionBehavior::Immediate).unwrap();
        tx.execute("INSERT INTO o VALUES (1)", params![]).unwrap();
        held_tx.send(()).unwrap();
        go_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        tx.commit().unwrap();
        // The connection moved into this thread (Connection: Send) and is dropped here.
    });

    held_rx.recv().unwrap();
    let mut other = Connection::open(&db.0, OpenFlags::READ_WRITE).unwrap();
    // No busy handler: fails at once.
    let start = Instant::now();
    let e = other
        .execute("INSERT INTO o VALUES (2)", params![])
        .unwrap_err();
    assert!(e.is_busy(), "{e}");
    assert_eq!(e.code(), Some(codes::BUSY));
    let e = other
        .transaction_with(TransactionBehavior::Immediate)
        .unwrap_err();
    assert!(e.is_busy(), "{e}");
    assert!(start.elapsed() < Duration::from_millis(250));
    // Readers are not blocked by a WAL writer and see the last committed state.
    let n: i64 = other
        .query_row("SELECT count(*) FROM o", params![], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);

    other.busy_timeout(Duration::from_secs(10)).unwrap();
    go_tx.send(()).unwrap();
    let start = Instant::now();
    other
        .execute("INSERT INTO o VALUES (2)", params![])
        .unwrap();
    assert!(start.elapsed() >= Duration::from_millis(100));
    writer.join().unwrap();
    let n: i64 = other
        .query_row("SELECT count(*) FROM o", params![], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2);
    other.busy_timeout(Duration::ZERO).unwrap();
}

#[test]
fn without_rowid() {
    let Some(c) = mem() else { return };
    c.execute_batch("CREATE TABLE kv (k TEXT PRIMARY KEY, v BLOB NOT NULL) WITHOUT ROWID;")
        .unwrap();
    c.execute(
        "INSERT INTO kv VALUES (?, ?)",
        params!["cursor", [1u8, 2, 3]],
    )
    .unwrap();
    c.execute(
        "INSERT INTO kv VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        params!["cursor", [4u8]],
    )
    .unwrap();
    let v: Vec<u8> = c
        .query_row("SELECT v FROM kv WHERE k = ?", params!["cursor"], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(v, [4]);
    let e = c
        .execute("INSERT INTO kv VALUES ('cursor', x'00')", params![])
        .unwrap_err();
    assert_eq!(e.extended_code(), Some(codes::CONSTRAINT_PRIMARYKEY), "{e}");
    let e = c
        .execute("INSERT INTO kv VALUES ('n', NULL)", params![])
        .unwrap_err();
    assert_eq!(e.extended_code(), Some(codes::CONSTRAINT_NOTNULL), "{e}");
}

#[test]
fn parameter_count_mismatch() {
    let Some(c) = mem() else { return };
    c.execute_batch("CREATE TABLE p (a, b)").unwrap();
    assert_eq!(
        c.execute("INSERT INTO p VALUES (?, ?)", params![1]),
        Err(Error::ParameterCount {
            expected: 2,
            given: 1
        })
    );
    assert_eq!(
        c.execute("INSERT INTO p VALUES (1, 2)", params!["extra"]),
        Err(Error::ParameterCount {
            expected: 0,
            given: 1
        })
    );
    assert_eq!(
        c.execute("INSERT INTO p VALUES (?, ?)", &[&1, &"two"]),
        Ok(1)
    );
}

#[test]
fn invalid_sql() {
    let Some(c) = mem() else { return };
    let e = c.execute_batch("SELEC 1").unwrap_err();
    assert_eq!(e.code(), Some(codes::ERROR));
    let Error::Sqlite { message, .. } = &e else {
        panic!("{e:?}")
    };
    assert!(message.contains("syntax error"), "{message}");
    assert!(e.to_string().contains("syntax error"));

    let e = c.prepare("SELECT * FROM missing_table").unwrap_err();
    assert!(e.to_string().contains("no such table"), "{e}");

    assert_eq!(
        c.prepare("  -- nothing\n").unwrap_err(),
        Error::EmptyStatement
    );
    assert_eq!(c.prepare("").unwrap_err(), Error::EmptyStatement);
    assert_eq!(
        c.prepare("SELECT 1; SELECT 2").unwrap_err(),
        Error::MultipleStatements { offset: 9 }
    );
    assert!(c.prepare("SELECT 1; -- trailing comment\n ;").is_ok());

    // A failed statement in a batch stops it; earlier statements took effect.
    let e = c
        .execute_batch("CREATE TABLE ok1 (a); INSERT INTO nope VALUES (1); CREATE TABLE ok2 (a);")
        .unwrap_err();
    assert!(e.to_string().contains("no such table: nope"), "{e}");
    let tables: Vec<String> = c
        .query(
            "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
            params![],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, ["ok1"]);

    assert!(matches!(
        Connection::open("bad\0path", OpenFlags::default()),
        Err(Error::InvalidPath(_))
    ));
}

#[test]
fn statement_reuse_releases_locks() {
    if !available() {
        return;
    }
    let db = TempDb::new("reuse");
    let c = Connection::open(&db.0, OpenFlags::default()).unwrap();
    c.execute_batch("CREATE TABLE r (v INTEGER); INSERT INTO r VALUES (1), (2), (3);")
        .unwrap();
    let mut st = c.prepare("SELECT v FROM r ORDER BY v").unwrap();
    // query_row stops after one row and resets, so the read lock is released.
    assert_eq!(st.query_row(params![], |r| r.get::<i64>(0)).unwrap(), 1);
    let other = Connection::open(&db.0, OpenFlags::default()).unwrap();
    other.execute("DELETE FROM r", params![]).unwrap();
    assert_eq!(
        st.query(params![], |r| r.get::<i64>(0)).unwrap(),
        Vec::<i64>::new()
    );
}
