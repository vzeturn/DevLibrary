//! The crate error type.

use std::io;

use core::fmt;

/// Errors from configuration parsing, global initialization and sinks.
///
/// Logging itself never returns errors to the call site: failures inside sinks are
/// passed to the logger's error hook (see
/// [`LoggerBuilder::on_error`](crate::LoggerBuilder::on_error)) as [`Error::Sink`].
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A level name that is not one of the accepted names.
    ParseLevel {
        /// The rejected text.
        input: String,
    },
    /// A malformed filter directive string (see [`Filter::parse`](crate::Filter::parse)).
    ParseFilter {
        /// Byte offset of the offending directive in the input.
        position: usize,
        /// What is wrong.
        reason: &'static str,
    },
    /// [`try_init`](crate::try_init) was called after a global logger was installed.
    AlreadyInitialized,
    /// Setting up a sink failed: creating a log directory, opening a file or starting a
    /// background thread.
    Io {
        /// What was being done, for example `open log file logs/app.log`.
        context: String,
        /// The underlying error.
        source: io::Error,
    },
    /// A sink failed to write or flush a record.
    Sink {
        /// The sink's [`name`](crate::Sink::name), for example `file:logs/app.log`.
        sink: String,
        /// The underlying error.
        source: io::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::ParseLevel { input } => write!(
                f,
                "unknown log level {input:?} (expected trace, debug, info, warn, error or fatal)"
            ),
            Error::ParseFilter { position, reason } => {
                write!(f, "invalid log filter at byte {position}: {reason}")
            }
            Error::AlreadyInitialized => f.write_str("a global logger is already initialized"),
            Error::Io { context, source } => write!(f, "{context}: {source}"),
            Error::Sink { sink, source } => write!(f, "log sink {sink:?} failed: {source}"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } | Error::Sink { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_source() {
        let e = Error::ParseFilter {
            position: 9,
            reason: "unknown level",
        };
        assert_eq!(e.to_string(), "invalid log filter at byte 9: unknown level");
        assert!(core::error::Error::source(&e).is_none());
        let e = Error::Sink {
            sink: "file:app.log".into(),
            source: io::Error::other("disk full"),
        };
        assert_eq!(e.to_string(), "log sink \"file:app.log\" failed: disk full");
        assert!(core::error::Error::source(&e).is_some());
        let e = Error::Io {
            context: "open log file app.log".into(),
            source: io::Error::other("denied"),
        };
        assert_eq!(e.to_string(), "open log file app.log: denied");
        assert_eq!(
            Error::AlreadyInitialized.to_string(),
            "a global logger is already initialized"
        );
    }
}
