//! The crate's error type and the SQLite result codes it reports.

use core::fmt;

use crate::Type;

/// SQLite result codes (primary and the extended codes agents usually match on).
///
/// Primary codes are the low 8 bits of an extended code:
/// `codes::CONSTRAINT_UNIQUE & 0xff == codes::CONSTRAINT`.
pub mod codes {
    /// Successful result.
    pub const OK: i32 = 0;
    /// Generic error (also SQL syntax errors and missing tables).
    pub const ERROR: i32 = 1;
    /// An internal logic error in SQLite.
    pub const INTERNAL: i32 = 2;
    /// Access permission denied.
    pub const PERM: i32 = 3;
    /// The operation was aborted.
    pub const ABORT: i32 = 4;
    /// The database file is locked by another connection.
    pub const BUSY: i32 = 5;
    /// A table in the database is locked (same connection or shared cache).
    pub const LOCKED: i32 = 6;
    /// A memory allocation failed.
    pub const NOMEM: i32 = 7;
    /// Attempt to write a read-only database.
    pub const READONLY: i32 = 8;
    /// The operation was interrupted.
    pub const INTERRUPT: i32 = 9;
    /// A disk I/O error occurred.
    pub const IOERR: i32 = 10;
    /// The database disk image is malformed.
    pub const CORRUPT: i32 = 11;
    /// Insertion failed because the database is full (or the disk is).
    pub const FULL: i32 = 13;
    /// Unable to open the database file.
    pub const CANTOPEN: i32 = 14;
    /// The database schema changed.
    pub const SCHEMA: i32 = 17;
    /// A string or blob exceeds the size limit.
    pub const TOOBIG: i32 = 18;
    /// A constraint was violated.
    pub const CONSTRAINT: i32 = 19;
    /// Data type mismatch.
    pub const MISMATCH: i32 = 20;
    /// The library was used incorrectly.
    pub const MISUSE: i32 = 21;
    /// A bind parameter index is out of range.
    pub const RANGE: i32 = 25;
    /// The file is not a database.
    pub const NOTADB: i32 = 26;
    /// `sqlite3_step` has another row ready.
    pub const ROW: i32 = 100;
    /// `sqlite3_step` has finished executing.
    pub const DONE: i32 = 101;

    /// BUSY while recovering a WAL file after a crash.
    pub const BUSY_RECOVERY: i32 = 261;
    /// BUSY because a read transaction cannot be upgraded to a write transaction (WAL).
    pub const BUSY_SNAPSHOT: i32 = 517;
    /// CHECK constraint failed.
    pub const CONSTRAINT_CHECK: i32 = 275;
    /// FOREIGN KEY constraint failed.
    pub const CONSTRAINT_FOREIGNKEY: i32 = 787;
    /// NOT NULL constraint failed.
    pub const CONSTRAINT_NOTNULL: i32 = 1299;
    /// PRIMARY KEY constraint failed.
    pub const CONSTRAINT_PRIMARYKEY: i32 = 1555;
    /// UNIQUE constraint failed.
    pub const CONSTRAINT_UNIQUE: i32 = 2067;
}

/// Errors returned by this crate.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// The SQLite library could not be loaded (absent, missing symbol, or no library on this
    /// platform, e.g. WebAssembly). The text says why.
    Unsupported(String),
    /// An error reported by SQLite.
    Sqlite {
        /// Primary result code (`codes::BUSY`, `codes::CONSTRAINT`, ...).
        code: i32,
        /// Extended result code (`codes::CONSTRAINT_UNIQUE`, ...); equals `code` when
        /// SQLite has no more specific one.
        extended_code: i32,
        /// The message from `sqlite3_errmsg` (or `sqlite3_errstr` when there is none).
        message: String,
    },
    /// The path is not valid UTF-8 or contains a NUL character.
    InvalidPath(String),
    /// The number of parameters given differs from the number in the statement.
    ParameterCount {
        /// Parameters in the statement (`sqlite3_bind_parameter_count`).
        expected: usize,
        /// Parameters given.
        given: usize,
    },
    /// A 1-based parameter index outside `1..=count`.
    ParameterIndex {
        /// The index given.
        index: usize,
        /// Parameters in the statement.
        count: usize,
    },
    /// A 0-based column index outside the statement's columns.
    ColumnIndex {
        /// The index given.
        index: usize,
        /// Columns in the result.
        count: usize,
    },
    /// The column holds a type the target Rust type cannot be read from.
    InvalidType {
        /// 0-based column index.
        column: usize,
        /// The storage class the target type reads.
        expected: Type,
        /// The storage class of the value.
        found: Type,
    },
    /// An integer does not fit the target Rust type.
    OutOfRange {
        /// 0-based column index.
        column: usize,
        /// The stored value.
        value: i64,
    },
    /// Text that is not valid UTF-8 was read into a string type.
    Utf8 {
        /// 0-based column index.
        column: usize,
        /// Length of the valid UTF-8 prefix in bytes.
        valid_up_to: usize,
    },
    /// A column was read while the statement was not positioned on a row.
    NoCurrentRow,
    /// `query_row` found no row.
    QueryReturnedNoRows,
    /// `prepare` was given more than one statement.
    MultipleStatements {
        /// Byte offset of the text after the first statement.
        offset: usize,
    },
    /// The SQL contains no statement (only whitespace or comments).
    EmptyStatement,
}

impl Error {
    /// The primary SQLite result code, if this error came from SQLite.
    pub fn code(&self) -> Option<i32> {
        match self {
            Error::Sqlite { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// The extended SQLite result code (e.g. `codes::CONSTRAINT_UNIQUE` = 2067).
    pub fn extended_code(&self) -> Option<i32> {
        match self {
            Error::Sqlite { extended_code, .. } => Some(*extended_code),
            _ => None,
        }
    }

    /// Whether the database file was locked by another connection (`SQLITE_BUSY`, any
    /// extended form). Retry later, or set [`Connection::busy_timeout`](crate::Connection::busy_timeout).
    pub fn is_busy(&self) -> bool {
        self.code() == Some(codes::BUSY)
    }

    /// Whether a constraint (UNIQUE, PRIMARY KEY, NOT NULL, CHECK, FOREIGN KEY) failed.
    pub fn is_constraint(&self) -> bool {
        self.code() == Some(codes::CONSTRAINT)
    }

    /// Sets the column index of a conversion error (`FromSql` implementations report 0).
    pub(crate) fn at_column(self, column: usize) -> Error {
        match self {
            Error::InvalidType {
                expected, found, ..
            } => Error::InvalidType {
                column,
                expected,
                found,
            },
            Error::OutOfRange { value, .. } => Error::OutOfRange { column, value },
            Error::Utf8 { valid_up_to, .. } => Error::Utf8 {
                column,
                valid_up_to,
            },
            other => other,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported(why) => write!(f, "SQLite is not available: {why}"),
            Error::Sqlite {
                extended_code,
                message,
                ..
            } => write!(f, "{message} (SQLite error {extended_code})"),
            Error::InvalidPath(p) => {
                write!(f, "path is not valid UTF-8 or contains NUL: {p}")
            }
            Error::ParameterCount { expected, given } => {
                write!(f, "statement has {expected} parameters, {given} given")
            }
            Error::ParameterIndex { index, count } => {
                write!(f, "parameter index {index} out of range 1..={count}")
            }
            Error::ColumnIndex { index, count } => {
                write!(f, "column index {index} out of range ({count} columns)")
            }
            Error::InvalidType {
                column,
                expected,
                found,
            } => write!(f, "column {column}: expected {expected}, found {found}"),
            Error::OutOfRange { column, value } => {
                write!(f, "column {column}: integer {value} out of range")
            }
            Error::Utf8 {
                column,
                valid_up_to,
            } => write!(
                f,
                "column {column}: text is not valid UTF-8 (valid up to byte {valid_up_to})"
            ),
            Error::NoCurrentRow => f.write_str("the statement is not positioned on a row"),
            Error::QueryReturnedNoRows => f.write_str("query returned no rows"),
            Error::MultipleStatements { offset } => write!(
                f,
                "SQL contains more than one statement (the rest starts at byte {offset})"
            ),
            Error::EmptyStatement => f.write_str("SQL contains no statement"),
        }
    }
}

impl core::error::Error for Error {}
