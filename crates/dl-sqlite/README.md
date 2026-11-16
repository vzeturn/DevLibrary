# dl-sqlite

SQLite for on-premises agents (outbox queues, capture state, cursors) without bundling
SQLite or any dependency: the crate loads the operating system's SQLite library at run
time and wraps its C API in a small, rusqlite-like interface.

| Platform | Library loaded |
|----------|----------------|
| Windows (x64, x86, arm64) | `winsqlite3.dll` from System32 (`LoadLibraryExW` with `LOAD_LIBRARY_SEARCH_SYSTEM32`), part of Windows 10 and later |
| Linux and other Unix | `libsqlite3.so.0`, then `libsqlite3.so` (`dlopen`; Debian/Ubuntu package `libsqlite3-0`) |
| macOS | `/usr/lib/libsqlite3.dylib` |
| WebAssembly, others | none: every call returns `Error::Unsupported` |

`DL_SQLITE_LIBRARY=<full path>` loads that library instead, for sites that ship their
own SQLite build. The library is loaded once, all symbols are resolved up front, and a
missing library or symbol is reported as `Error::Unsupported("symbol sqlite3_... not found
in ...")`. `dl_sqlite::library_version()` returns `sqlite3_libversion()`.

## Replaces

| Ecosystem | Packages |
|-----------|----------|
| .NET      | `Microsoft.Data.Sqlite`, `SQLitePCLRaw.bundle_e_sqlite3` / `SQLitePCLRaw.provider.winsqlite3` |
| Go        | `github.com/mattn/go-sqlite3` (cgo), `modernc.org/sqlite` |
| Rust      | `rusqlite`, `libsqlite3-sys` |

Unlike those, SQLite itself is not bundled: the version is whatever the OS provides
(check `library_version()`), and features compiled out of that build (e.g. some
extensions) are not available.

## Usage

```rust,no_run
use std::time::Duration;
use dl_sqlite::{Connection, OpenFlags, TransactionBehavior, params};

let mut db = Connection::open(r"C:\ProgramData\My Agent\outbox.db", OpenFlags::default())?;
db.busy_timeout(Duration::from_secs(5))?;
db.execute_batch(
    "PRAGMA journal_mode=WAL;
     PRAGMA synchronous=FULL;
     CREATE TABLE IF NOT EXISTS outbox (
         id      INTEGER PRIMARY KEY AUTOINCREMENT,
         key     TEXT NOT NULL UNIQUE,
         body    BLOB NOT NULL,
         sent_at INTEGER
     );",
)?;

// Enqueue: the UNIQUE key makes a retry of the same capture a no-op.
match db.execute(
    "INSERT INTO outbox (key, body) VALUES (?1, ?2)",
    params!["inv-2024-0001", b"{...}".as_slice()],
) {
    Ok(_) => println!("queued as {}", db.last_insert_rowid()),
    Err(e) if e.is_constraint() => println!("already queued"),
    Err(e) => return Err(e),
}

// Drain in one write transaction; it rolls back if dropped before commit.
let tx = db.transaction_with(TransactionBehavior::Immediate)?;
let batch = tx.query(
    "SELECT id, body FROM outbox WHERE sent_at IS NULL ORDER BY id LIMIT 100",
    params![],
    |row| Ok((row.get::<i64>(0)?, row.get::<Vec<u8>>(1)?)),
)?;
for (id, _body) in &batch {
    tx.execute(
        "UPDATE outbox SET sent_at = CAST(strftime('%s', 'now') AS INTEGER) WHERE id = ?",
        params![id],
    )?;
}
tx.commit()?;
# Ok::<(), dl_sqlite::Error>(())
```

Prepared statements for hot loops:

```rust,no_run
use dl_sqlite::{Connection, params};

let db = Connection::open_in_memory()?;
db.execute_batch("CREATE TABLE kv (k TEXT PRIMARY KEY, v TEXT) WITHOUT ROWID")?;
let mut put = db.prepare("INSERT OR REPLACE INTO kv VALUES (?, ?)")?;
for (k, v) in [("cursor", "42"), ("since", "2024-06-01")] {
    put.execute(params![k, v])?;
}
let mut get = db.prepare("SELECT k, v FROM kv ORDER BY k")?;
get.bind(params![])?;
while get.step()? {
    let k: String = get.get(0)?;
    let v: Option<String> = get.get(1)?;
    println!("{k} = {v:?}");
}
# Ok::<(), dl_sqlite::Error>(())
```

## API

- `Connection::open(path, OpenFlags)` with `READ_WRITE_CREATE` (default), `READ_WRITE`,
  `READ_ONLY`, `.with_uri(bool)` (URI file names off by default);
  `Connection::open_in_memory()`. Extended result codes are on.
- `execute_batch`, `execute -> changes`, `query(sql, params, |row| ...) -> Vec<T>`,
  `query_row`, `prepare -> Statement`, `last_insert_rowid`, `changes`, `is_autocommit`,
  `busy_timeout(Duration)`, `transaction()` / `transaction_with(Immediate | Exclusive)`.
- `Statement`: `bind(params)`, `bind_at(index, value)` (1-based), `step -> bool`, `reset`,
  `clear_bindings`, `parameter_count`, `column_count`, `column_name(s)`, `column_type`,
  `get::<T>(index)` / `get_ref` (0-based), and `execute` / `query` / `query_row`.
- `Value` (`Null`, `Integer(i64)`, `Real(f64)`, `Text(String)`, `Blob(Vec<u8>)`) and the
  borrowed `ValueRef`. `ToSql` is implemented for the integer types up to `i64`/`u32`,
  `bool`, `f32`, `f64`, `str`, `String`, `[u8]`, `[u8; N]`, `Vec<u8>`, `Value`,
  `Option<T>` and references; `FromSql` for `i64` and the smaller integers (range
  checked), `u64`, `bool`, `f64`, `String`, `Vec<u8>`, `Value`, `Option<T>`.
  Parameters are `&[&dyn ToSql]` or `params![...]`.
- `Error::Sqlite { code, extended_code, message }` carries the primary and extended
  result codes and `sqlite3_errmsg`; `is_busy()`, `is_constraint()`, and `codes::*`
  (e.g. `codes::CONSTRAINT_UNIQUE` = 2067). Conversion errors name the column.

## Behaviour

- Text and blobs are bound with `SQLITE_TRANSIENT` and an explicit byte length: no NUL
  terminator is needed and embedded NULs survive. Text is read back as UTF-8; text that is
  not valid UTF-8 gives `Error::Utf8` when read as `String` (its bytes can still be read as
  `Vec<u8>`), never a panic.
- Every connection is opened with `SQLITE_OPEN_NOMUTEX`. `Connection` is `Send` but not
  `Sync`; statements and transactions borrow it, so a connection and its statements are
  used by one thread at a time. Open one connection per thread (WAL mode lets readers and
  one writer work concurrently). A library built with `SQLITE_THREADSAFE=0` is refused.
- Statements are finalized on drop; the connection is closed with `sqlite3_close_v2` on
  drop. `query`, `query_row` and `execute` reset the statement when they return, so no
  read lock is held afterwards.
- `prepare` refuses a second statement (`Error::MultipleStatements`); `execute_batch` runs
  them all.
- A `Transaction` rolls back on drop unless `commit` succeeded. If `COMMIT` fails (e.g.
  `SQLITE_BUSY`) the transaction is rolled back when the guard is dropped.
- Without `busy_timeout` a locked database fails at once with `SQLITE_BUSY`.

## Limits

- Needs SQLite 3.7.15 or later (`sqlite3_errstr`); `WITHOUT ROWID` needs 3.8.2 and
  `INSERT ... ON CONFLICT DO UPDATE` 3.24. Windows 10 1607 and later ship `winsqlite3.dll` with SQLite 3.x;
  the exact version follows Windows updates (tested with 3.53.4 on Windows 11 x64 and
  x86, and 3.40.1 on Debian 12).
- On 32-bit Windows the functions are called as `__stdcall`, which is how
  `winsqlite3.dll` is built. A `DL_SQLITE_LIBRARY` override there must be built the same
  way (`SQLITE_APICALL=__stdcall`); the `sqlite3.dll` from sqlite.org uses `__cdecl` and
  works only on x64 and arm64.
- Statically linked musl binaries cannot `dlopen`; there the library reports
  `Error::Unsupported`.
- Not exposed: named-parameter lookup (positions `?N` work for `:name` too), user
  functions, blob I/O, backup, hooks, `sqlite3_interrupt`, UTF-16 APIs.
- All `unsafe` code is in `src/sys.rs`.
