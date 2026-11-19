//! The failures of a capture pass: the C# exception types the capture library and the
//! agent's capture runner throw and catch, as one error type with the facts the runner
//! reads (`SourceSchemaFault` marks, the SQL Server errors, backpressure codes).

use crate::config::AgentError;
use crate::reporting::log::Exception;
use crate::reporting::sql_error::SqlFailure;

/// What kind of failure (the C# exception type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// `OperationCanceledException`: the caller's context was cancelled.
    Cancelled,
    /// `SourceBusyException`: the source is overloaded or locked; back off.
    Busy,
    /// `SqlException`: a SQL Server failure ([`CaptureError::sql`] has the errors).
    Sql,
    /// `SqliteException`: a local SQLite store failed.
    LocalStore,
    /// `CaptureBackpressureException`: a local queue is full; the payload is the status
    /// code (`OUTBOX_FULL`, `BLOB_QUEUE_FULL`).
    Backpressure(String),
    /// `AgentException` with its stable code.
    Agent(String),
    /// `TimeZoneNotFoundException` / `InvalidTimeZoneException`.
    TimeZone,
    /// `ArgumentException` (unsafe identifier, misaligned keyset).
    Argument,
    /// `InvalidOperationException`.
    InvalidOperation,
    /// `FormatException` (corrupt cursor text).
    Format,
    /// `NotSupportedException` (a key type the cursor codec cannot store).
    NotSupported,
    /// `IOException`.
    Io,
    /// `NullReferenceException` (kept where the C# code has that bug).
    NullReference,
    /// Any other exception.
    Other,
}

impl ErrorKind {
    /// The full .NET type name of the matching exception.
    pub fn dotnet_type(&self) -> &'static str {
        match self {
            ErrorKind::Cancelled => "System.OperationCanceledException",
            ErrorKind::Busy => "MM.PdaOrder.Sync.Capture.Source.SourceBusyException",
            ErrorKind::Sql => crate::reporting::sql_error::SQL_EXCEPTION_TYPE,
            ErrorKind::LocalStore => "Microsoft.Data.Sqlite.SqliteException",
            ErrorKind::Backpressure(_) => {
                "MM.PdaOrder.SyncAgent.Capture.CaptureBackpressureException"
            }
            ErrorKind::Agent(_) => crate::config::AGENT_EXCEPTION_TYPE,
            ErrorKind::TimeZone => "System.TimeZoneNotFoundException",
            ErrorKind::Argument => "System.ArgumentException",
            ErrorKind::InvalidOperation => "System.InvalidOperationException",
            ErrorKind::Format => "System.FormatException",
            ErrorKind::NotSupported => "System.NotSupportedException",
            ErrorKind::Io => "System.IO.IOException",
            ErrorKind::NullReference => "System.NullReferenceException",
            ErrorKind::Other => "System.Exception",
        }
    }
}

/// A capture failure: kind, message, the SQL Server errors of a `SqlException`, an inner
/// failure, and the `SourceSchemaFault` marks (`Exception.Data` in C#).
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureError {
    /// The kind.
    pub kind: ErrorKind,
    /// The message (can quote row values: log it only where C# logs the exception).
    pub message: String,
    /// The SQL Server errors, for [`ErrorKind::Sql`] (boxed: keeps `Result` small).
    pub sql: Option<Box<SqlFailure>>,
    /// The inner failure (`InnerException`).
    pub inner: Option<Box<CaptureError>>,
    /// `SourceSchemaFault.DataKey`: a failure that belongs to one table's shape.
    pub schema_fault: bool,
    /// `SourceSchemaFault.TableKey`: the vendor table the failed read was for.
    pub table: Option<String>,
}

impl CaptureError {
    /// A failure of `kind`.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> CaptureError {
        CaptureError {
            kind,
            message: message.into(),
            sql: None,
            inner: None,
            schema_fault: false,
            table: None,
        }
    }

    /// The context was cancelled.
    pub fn cancelled() -> CaptureError {
        CaptureError::new(ErrorKind::Cancelled, "The operation was canceled.")
    }

    /// `SourceBusyException(message, inner)`.
    pub fn busy(message: impl Into<String>, inner: Option<CaptureError>) -> CaptureError {
        CaptureError {
            inner: inner.map(Box::new),
            ..CaptureError::new(ErrorKind::Busy, message)
        }
    }

    /// A `SqlException`.
    pub fn sql(failure: SqlFailure) -> CaptureError {
        CaptureError {
            message: failure.message(),
            sql: Some(Box::new(failure)),
            ..CaptureError::new(ErrorKind::Sql, "")
        }
    }

    /// A `SqliteException`.
    pub fn local_store(message: impl Into<String>) -> CaptureError {
        CaptureError::new(ErrorKind::LocalStore, message)
    }

    /// `CaptureBackpressureException(code, message)`.
    pub fn backpressure(code: &str, message: impl Into<String>) -> CaptureError {
        CaptureError::new(ErrorKind::Backpressure(code.to_owned()), message)
    }

    /// An `ArgumentException`.
    pub fn argument(message: impl Into<String>) -> CaptureError {
        CaptureError::new(ErrorKind::Argument, message)
    }

    /// An `InvalidOperationException`.
    pub fn invalid_operation(message: impl Into<String>) -> CaptureError {
        CaptureError::new(ErrorKind::InvalidOperation, message)
    }

    /// A `FormatException`.
    pub fn format(message: impl Into<String>) -> CaptureError {
        CaptureError::new(ErrorKind::Format, message)
    }

    /// Whether this is [`ErrorKind::Cancelled`].
    pub fn is_cancelled(&self) -> bool {
        self.kind == ErrorKind::Cancelled
    }

    /// Whether this is [`ErrorKind::Busy`] (`SourceBusyException`).
    pub fn is_busy(&self) -> bool {
        self.kind == ErrorKind::Busy
    }

    /// `SourceSchemaFault.Mark(ex, table)`: flags a table schema failure and records the
    /// table unless one is already named.
    pub fn mark_schema_fault(&mut self, table: &str) {
        self.schema_fault = true;
        if self.table.is_none() {
            self.table = Some(table.to_owned());
        }
    }

    /// This failure and its inner ones, outermost first.
    pub fn chain(&self) -> impl Iterator<Item = &CaptureError> {
        std::iter::successors(Some(self), |e| e.inner.as_deref())
    }

    /// `SourceSchemaFault.Is`: this or an inner failure is a marked schema failure.
    pub fn is_schema_fault(&self) -> bool {
        self.chain().any(|e| e.schema_fault)
    }

    /// `SourceSchemaFault.TableOf`: the vendor table named on the chain.
    pub fn table_of(&self) -> Option<&str> {
        self.chain().find_map(|e| e.table.as_deref())
    }

    /// The short .NET type name (`GetType().Name`), e.g. `SqlException`.
    pub fn short_type_name(&self) -> &'static str {
        let full = self.kind.dotnet_type();
        full.rsplit('.').next().unwrap_or(full)
    }

    /// This failure as a logged exception, with the SQL errors and inner exceptions.
    pub fn to_exception(&self) -> Exception {
        let mut ex = Exception::new(self.kind.dotnet_type(), self.message.clone());
        if let Some(sql) = &self.sql {
            let mut sql = (**sql).clone();
            if sql.capture_table.is_none() {
                sql.capture_table.clone_from(&self.table);
            }
            ex.sql = Some(sql);
        }
        if let Some(inner) = &self.inner {
            ex.inner = Some(Box::new(inner.to_exception()));
        }
        ex
    }
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

impl From<dl_sqlite::Error> for CaptureError {
    fn from(e: dl_sqlite::Error) -> CaptureError {
        CaptureError::local_store(e.to_string())
    }
}

impl From<AgentError> for CaptureError {
    fn from(e: AgentError) -> CaptureError {
        CaptureError::new(ErrorKind::Agent(e.code().to_owned()), e.message())
    }
}

/// `TimeZoneNotFoundException` for an unknown zone id, as `FindSystemTimeZoneById` throws.
pub fn time_zone_not_found(id: &str) -> CaptureError {
    CaptureError::new(
        ErrorKind::TimeZone,
        format!("The time zone ID '{id}' was not found on the local computer."),
    )
}

/// Shorthand for results of the capture module.
pub type Result<T> = std::result::Result<T, CaptureError>;
