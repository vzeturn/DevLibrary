//! Logging interface (the C# `ILogger<T>`): components log formatted messages with the .NET
//! category name and, for failures, the exception as [`DotnetError`]. The host supplies the
//! sink (console / log file); tests use a list.

use std::sync::Arc;

use crate::config::DotnetError;

/// `Microsoft.Extensions.Logging.LogLevel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// `Trace` (console `trce`).
    Trace,
    /// `Debug` (console `dbug`).
    Debug,
    /// `Information` (console `info`).
    Information,
    /// `Warning` (console `warn`).
    Warning,
    /// `Error` (console `fail`).
    Error,
    /// `Critical` (console `crit`).
    Critical,
}

/// One log entry.
#[derive(Debug, Clone, Copy)]
pub struct Record<'a> {
    /// Severity.
    pub level: Level,
    /// The .NET category (`ILogger<T>`: the full type name of `T`).
    pub category: &'a str,
    /// `EventId.Id`; 0 for the agent's own messages.
    pub event_id: i32,
    /// The formatted message (template arguments substituted, `null` as `(null)`).
    pub message: &'a str,
    /// The exception passed to the log call.
    pub exception: Option<&'a DotnetError>,
}

/// A log sink. Must not panic and must not block for long.
pub trait Logger: Send + Sync {
    /// Writes one entry.
    fn log(&self, record: &Record<'_>);
}

/// Discards everything (`NullLogger`).
#[derive(Debug, Clone, Copy, Default)]
pub struct NullLogger;

impl Logger for NullLogger {
    fn log(&self, _: &Record<'_>) {}
}

/// A sink bound to one category (`ILogger<T>`).
#[derive(Clone)]
pub struct Log {
    sink: Arc<dyn Logger>,
    category: &'static str,
}

impl std::fmt::Debug for Log {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Log")
            .field("category", &self.category)
            .finish_non_exhaustive()
    }
}

impl Log {
    /// Logs to `sink` under `category`.
    pub fn new(sink: Arc<dyn Logger>, category: &'static str) -> Log {
        Log { sink, category }
    }

    /// The category name.
    pub fn category(&self) -> &'static str {
        self.category
    }

    /// Logs `message` at `level` with an optional exception.
    pub fn log(&self, level: Level, message: &str, exception: Option<&DotnetError>) {
        self.sink.log(&Record {
            level,
            category: self.category,
            event_id: 0,
            message,
            exception,
        });
    }

    /// `LogInformation`.
    pub fn info(&self, message: &str) {
        self.log(Level::Information, message, None);
    }

    /// `LogWarning`.
    pub fn warn(&self, message: &str, exception: Option<&DotnetError>) {
        self.log(Level::Warning, message, exception);
    }

    /// `LogError`.
    pub fn error(&self, message: &str, exception: Option<&DotnetError>) {
        self.log(Level::Error, message, exception);
    }

    /// `LogCritical`.
    pub fn critical(&self, message: &str, exception: Option<&DotnetError>) {
        self.log(Level::Critical, message, exception);
    }
}

/// How `Microsoft.Extensions.Logging` formats a `null` template argument.
pub fn or_null(value: Option<&str>) -> &str {
    value.unwrap_or("(null)")
}
