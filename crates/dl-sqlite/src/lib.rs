#![doc = include_str!("../README.md")]

mod error;
mod sys;
mod value;

#[cfg(test)]
mod tests;

use core::ffi::c_int;
use core::fmt;
use core::ops::Deref;
use std::ffi::CString;
use std::path::Path;
use std::time::Duration;

pub use error::{Error, codes};
pub use value::{FromSql, ToSql, Type, Value, ValueRef};

/// The version of the loaded SQLite library (`sqlite3_libversion()`), e.g. `"3.40.1"`.
///
/// Loads the library on first use; the error says why it is unavailable.
pub fn library_version() -> Result<&'static str, Error> {
    sys::lib().map(|l| l.version.as_str())
}

/// The version of the loaded SQLite library as a number (`sqlite3_libversion_number()`),
/// e.g. `3040001` for 3.40.1.
pub fn library_version_number() -> Result<i32, Error> {
    sys::lib().map(|l| l.version_number)
}

/// Builds a parameter slice (`&[&dyn ToSql]`) from a list of values.
///
/// ```no_run
/// # let conn = dl_sqlite::Connection::open_in_memory()?;
/// let id: i64 = 7;
/// conn.execute(
///     "INSERT INTO outbox (id, body, sent) VALUES (?1, ?2, ?3)",
///     dl_sqlite::params![id, "payload", None::<i64>],
/// )?;
/// # Ok::<(), dl_sqlite::Error>(())
/// ```
#[macro_export]
macro_rules! params {
    () => {
        &[] as &[&dyn $crate::ToSql]
    };
    ($($p:expr),+ $(,)?) => {
        &[$(&$p as &dyn $crate::ToSql),+] as &[&dyn $crate::ToSql]
    };
}

const OPEN_READONLY: c_int = 0x0000_0001;
const OPEN_READWRITE: c_int = 0x0000_0002;
const OPEN_CREATE: c_int = 0x0000_0004;
const OPEN_URI: c_int = 0x0000_0040;
const OPEN_NOMUTEX: c_int = 0x0000_8000;

/// How to open a database file.
///
/// One of three modes, optionally with URI file names. `SQLITE_OPEN_NOMUTEX` is always
/// added: a [`Connection`] is `Send` but not `Sync`, so only one thread uses it at a time
/// and SQLite's per-connection mutex would only cost time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpenFlags {
    bits: c_int,
}

impl OpenFlags {
    /// Read and write; create the file if it does not exist (the default).
    pub const READ_WRITE_CREATE: OpenFlags = OpenFlags {
        bits: OPEN_READWRITE | OPEN_CREATE,
    };
    /// Read and write an existing file.
    pub const READ_WRITE: OpenFlags = OpenFlags {
        bits: OPEN_READWRITE,
    };
    /// Read an existing file; writes fail with `codes::READONLY`.
    pub const READ_ONLY: OpenFlags = OpenFlags {
        bits: OPEN_READONLY,
    };

    /// Interpret `file:` URIs (`SQLITE_OPEN_URI`). Off by default, so a path is always a
    /// path (unless the library was built with URIs on globally).
    pub const fn with_uri(self, uri: bool) -> OpenFlags {
        OpenFlags {
            bits: if uri {
                self.bits | OPEN_URI
            } else {
                self.bits & !OPEN_URI
            },
        }
    }
}

impl Default for OpenFlags {
    fn default() -> OpenFlags {
        OpenFlags::READ_WRITE_CREATE
    }
}

/// When a transaction takes its locks (`BEGIN DEFERRED / IMMEDIATE / EXCLUSIVE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum TransactionBehavior {
    /// Locks are taken by the first read or write (SQLite's default).
    #[default]
    Deferred,
    /// The write lock is taken at `BEGIN`, so `SQLITE_BUSY` shows up there rather than in
    /// the middle of the transaction. Use it for transactions that write.
    Immediate,
    /// Like `Immediate`; in rollback-journal mode readers are excluded as well.
    Exclusive,
}

/// A database connection, closed with `sqlite3_close_v2` when dropped.
///
/// `Connection` is `Send` (it can move to another thread) but not `Sync`:
///
/// ```compile_fail
/// fn shared<T: Sync>() {}
/// shared::<dl_sqlite::Connection>();
/// ```
pub struct Connection {
    raw: sys::RawDb,
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection").finish_non_exhaustive()
    }
}

impl Connection {
    /// Opens (or, with [`OpenFlags::READ_WRITE_CREATE`], creates) a database file.
    /// Extended result codes are enabled.
    pub fn open<P: AsRef<Path>>(path: P, flags: OpenFlags) -> Result<Connection, Error> {
        let path = path.as_ref();
        let Some(s) = path.to_str() else {
            return Err(Error::InvalidPath(path.display().to_string()));
        };
        Connection::open_str(s, flags)
    }

    /// Opens a private, temporary in-memory database.
    pub fn open_in_memory() -> Result<Connection, Error> {
        Connection::open_str(":memory:", OpenFlags::READ_WRITE_CREATE)
    }

    fn open_str(path: &str, flags: OpenFlags) -> Result<Connection, Error> {
        let c = CString::new(path).map_err(|_| Error::InvalidPath(path.to_owned()))?;
        let raw = sys::RawDb::open(&c, flags.bits | OPEN_NOMUTEX)?;
        Ok(Connection { raw })
    }

    /// Runs every statement of `sql` in order (rows they return are discarded), e.g.
    /// schema scripts and `PRAGMA` settings. Stops at the first error.
    pub fn execute_batch(&self, sql: &str) -> Result<(), Error> {
        let mut rest = sql.as_bytes();
        while !rest.is_empty() {
            let (stmt, used) = self.raw.prepare(rest)?;
            if let Some(mut stmt) = stmt {
                loop {
                    match stmt.step() {
                        codes::ROW => {}
                        codes::DONE => break,
                        rc => return Err(self.raw.error(rc)),
                    }
                }
            }
            if used == 0 {
                break;
            }
            rest = rest.get(used..).unwrap_or_default();
        }
        Ok(())
    }

    /// Runs one statement with parameters and returns the number of rows it changed
    /// (`sqlite3_changes`, meaningful for INSERT, UPDATE and DELETE).
    pub fn execute(&self, sql: &str, params: &[&dyn ToSql]) -> Result<usize, Error> {
        self.prepare(sql)?.execute(params)
    }

    /// Runs a query and maps every row with `f`.
    pub fn query<T, F>(&self, sql: &str, params: &[&dyn ToSql], f: F) -> Result<Vec<T>, Error>
    where
        F: FnMut(&Row<'_>) -> Result<T, Error>,
    {
        self.prepare(sql)?.query(params, f)
    }

    /// Runs a query and maps its first row with `f`; [`Error::QueryReturnedNoRows`] when
    /// there is none.
    pub fn query_row<T, F>(&self, sql: &str, params: &[&dyn ToSql], f: F) -> Result<T, Error>
    where
        F: FnOnce(&Row<'_>) -> Result<T, Error>,
    {
        self.prepare(sql)?.query_row(params, f)
    }

    /// Compiles one statement. Trailing whitespace, `;` and comments are allowed; a second
    /// statement is [`Error::MultipleStatements`] (use [`execute_batch`](Self::execute_batch)).
    pub fn prepare(&self, sql: &str) -> Result<Statement<'_>, Error> {
        let bytes = sql.as_bytes();
        let (stmt, used) = self.raw.prepare(bytes)?;
        let Some(stmt) = stmt else {
            return Err(Error::EmptyStatement);
        };
        let mut offset = used;
        while offset < bytes.len() {
            let rest = bytes.get(offset..).unwrap_or_default();
            let (extra, n) = self.raw.prepare(rest)?;
            if extra.is_some() {
                return Err(Error::MultipleStatements { offset: used });
            }
            if n == 0 {
                break;
            }
            offset += n;
        }
        Ok(Statement {
            conn: self,
            raw: stmt,
        })
    }

    /// The rowid of the last successful INSERT on this connection (0 if none).
    pub fn last_insert_rowid(&self) -> i64 {
        self.raw.last_insert_rowid()
    }

    /// Rows changed by the last INSERT, UPDATE or DELETE (`sqlite3_changes`).
    pub fn changes(&self) -> u64 {
        u64::try_from(self.raw.changes()).unwrap_or(0)
    }

    /// Whether no transaction is open (`sqlite3_get_autocommit`).
    pub fn is_autocommit(&self) -> bool {
        self.raw.is_autocommit()
    }

    /// Retries for up to `timeout` when another connection holds a lock, instead of
    /// failing at once with `SQLITE_BUSY`. `Duration::ZERO` turns retrying off (the default).
    /// Precision is milliseconds; longer than `i32::MAX` ms is clamped.
    pub fn busy_timeout(&self, timeout: Duration) -> Result<(), Error> {
        let ms = c_int::try_from(timeout.as_millis()).unwrap_or(c_int::MAX);
        match self.raw.busy_timeout(ms) {
            codes::OK => Ok(()),
            rc => Err(self.raw.error(rc)),
        }
    }

    /// Starts a `BEGIN DEFERRED` transaction.
    pub fn transaction(&mut self) -> Result<Transaction<'_>, Error> {
        self.transaction_with(TransactionBehavior::Deferred)
    }

    /// Starts a transaction with the given behaviour. It rolls back when dropped unless
    /// [`Transaction::commit`] succeeded.
    pub fn transaction_with(
        &mut self,
        behavior: TransactionBehavior,
    ) -> Result<Transaction<'_>, Error> {
        self.execute_batch(match behavior {
            TransactionBehavior::Deferred => "BEGIN DEFERRED",
            TransactionBehavior::Immediate => "BEGIN IMMEDIATE",
            TransactionBehavior::Exclusive => "BEGIN EXCLUSIVE",
        })?;
        Ok(Transaction {
            conn: self,
            finished: false,
        })
    }
}

/// A prepared statement, finalized when dropped.
pub struct Statement<'c> {
    conn: &'c Connection,
    raw: sys::RawStmt,
}

impl fmt::Debug for Statement<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Statement").finish_non_exhaustive()
    }
}

impl Statement<'_> {
    /// Number of parameters (`?`, `?N`, `:name`, ...) in the statement.
    pub fn parameter_count(&self) -> usize {
        self.raw.parameter_count()
    }

    /// Resets the statement and binds all parameters in order; the count must match
    /// [`parameter_count`](Self::parameter_count).
    pub fn bind(&mut self, params: &[&dyn ToSql]) -> Result<(), Error> {
        let expected = self.parameter_count();
        if params.len() != expected {
            return Err(Error::ParameterCount {
                expected,
                given: params.len(),
            });
        }
        self.raw.reset();
        for (i, p) in params.iter().enumerate() {
            self.bind_value(i + 1, p.to_sql())?;
        }
        Ok(())
    }

    /// Resets the statement and binds one parameter at a 1-based `index` (`?1` is 1).
    pub fn bind_at(&mut self, index: usize, value: &dyn ToSql) -> Result<(), Error> {
        self.raw.reset();
        self.bind_value(index, value.to_sql())
    }

    fn bind_value(&mut self, index: usize, value: ValueRef<'_>) -> Result<(), Error> {
        let count = self.parameter_count();
        let i = match c_int::try_from(index) {
            Ok(i) if index >= 1 && index <= count => i,
            _ => return Err(Error::ParameterIndex { index, count }),
        };
        match self.raw.bind(i, value) {
            codes::OK => Ok(()),
            rc => Err(self.conn.raw.error(rc)),
        }
    }

    /// Sets every parameter to NULL.
    pub fn clear_bindings(&mut self) {
        self.raw.clear_bindings();
    }

    /// Runs the statement to the next row: `true` when a row is ready (read it with the
    /// getters), `false` when done.
    pub fn step(&mut self) -> Result<bool, Error> {
        match self.raw.step() {
            codes::ROW => Ok(true),
            codes::DONE => Ok(false),
            rc => Err(self.conn.raw.error(rc)),
        }
    }

    /// Rewinds the statement so it can run again; bindings are kept. Releases the locks a
    /// partly read query holds. (Errors of the last step were already returned by `step`.)
    pub fn reset(&mut self) {
        self.raw.reset();
    }

    /// Number of result columns (0 for statements that return no rows).
    pub fn column_count(&self) -> usize {
        self.raw.column_count()
    }

    /// Name of a result column (0-based), as given by `AS` or derived by SQLite.
    pub fn column_name(&self, index: usize) -> Result<String, Error> {
        self.raw.column_name(index)
    }

    /// Names of all result columns.
    pub fn column_names(&self) -> Result<Vec<String>, Error> {
        (0..self.column_count())
            .map(|i| self.raw.column_name(i))
            .collect()
    }

    /// Storage class of a column of the current row.
    pub fn column_type(&self, index: usize) -> Result<Type, Error> {
        self.raw.column_type(index)
    }

    /// A column of the current row, borrowed until the next `step`/`reset`.
    pub fn get_ref(&self, index: usize) -> Result<ValueRef<'_>, Error> {
        self.raw.value(index)
    }

    /// A column of the current row converted to `T`.
    pub fn get<T: FromSql>(&self, index: usize) -> Result<T, Error> {
        T::from_sql(self.raw.value(index)?).map_err(|e| e.at_column(index))
    }

    /// Binds `params`, runs the statement to completion and returns
    /// [`Connection::changes`].
    pub fn execute(&mut self, params: &[&dyn ToSql]) -> Result<usize, Error> {
        self.bind(params)?;
        let r = loop {
            match self.step() {
                Ok(true) => {}
                Ok(false) => break Ok(usize::try_from(self.conn.raw.changes()).unwrap_or(0)),
                Err(e) => break Err(e),
            }
        };
        self.raw.reset();
        r
    }

    /// Binds `params` and maps every row with `f`.
    pub fn query<T, F>(&mut self, params: &[&dyn ToSql], mut f: F) -> Result<Vec<T>, Error>
    where
        F: FnMut(&Row<'_>) -> Result<T, Error>,
    {
        self.bind(params)?;
        let mut out = Vec::new();
        let r = loop {
            match self.step() {
                Ok(true) => match f(&Row { raw: &self.raw }) {
                    Ok(v) => out.push(v),
                    Err(e) => break Err(e),
                },
                Ok(false) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        self.raw.reset();
        r.map(|()| out)
    }

    /// Binds `params` and maps the first row with `f`; [`Error::QueryReturnedNoRows`]
    /// when there is none.
    pub fn query_row<T, F>(&mut self, params: &[&dyn ToSql], f: F) -> Result<T, Error>
    where
        F: FnOnce(&Row<'_>) -> Result<T, Error>,
    {
        self.bind(params)?;
        let r = match self.step() {
            Ok(true) => f(&Row { raw: &self.raw }),
            Ok(false) => Err(Error::QueryReturnedNoRows),
            Err(e) => Err(e),
        };
        self.raw.reset();
        r
    }
}

/// The current row of a query, passed to the closures of `query` and `query_row`.
pub struct Row<'s> {
    raw: &'s sys::RawStmt,
}

impl fmt::Debug for Row<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Row").finish_non_exhaustive()
    }
}

impl Row<'_> {
    /// A column (0-based) converted to `T`.
    pub fn get<T: FromSql>(&self, index: usize) -> Result<T, Error> {
        T::from_sql(self.raw.value(index)?).map_err(|e| e.at_column(index))
    }

    /// A column, borrowed.
    pub fn get_ref(&self, index: usize) -> Result<ValueRef<'_>, Error> {
        self.raw.value(index)
    }

    /// Storage class of a column.
    pub fn column_type(&self, index: usize) -> Result<Type, Error> {
        self.raw.column_type(index)
    }

    /// Number of columns.
    pub fn column_count(&self) -> usize {
        self.raw.column_count()
    }

    /// Name of a column.
    pub fn column_name(&self, index: usize) -> Result<String, Error> {
        self.raw.column_name(index)
    }
}

/// An open transaction; dereferences to the [`Connection`] for running statements.
///
/// Dropping it without a successful [`commit`](Self::commit) rolls back (unless SQLite
/// already ended the transaction itself, e.g. after `SQLITE_FULL`).
pub struct Transaction<'c> {
    conn: &'c Connection,
    finished: bool,
}

impl fmt::Debug for Transaction<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transaction")
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl Transaction<'_> {
    /// `COMMIT`. On error (e.g. `SQLITE_BUSY`) the transaction is rolled back as the
    /// guard is dropped.
    pub fn commit(mut self) -> Result<(), Error> {
        self.conn.execute_batch("COMMIT")?;
        self.finished = true;
        Ok(())
    }

    /// `ROLLBACK` now, reporting its error.
    pub fn rollback(mut self) -> Result<(), Error> {
        self.finished = true;
        if self.conn.is_autocommit() {
            return Ok(());
        }
        self.conn.execute_batch("ROLLBACK")
    }
}

impl Deref for Transaction<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.finished && !self.conn.is_autocommit() {
            // Nothing to report from a destructor; the connection is left in autocommit.
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}
