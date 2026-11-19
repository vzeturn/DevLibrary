//! The crate's error type. Messages never carry connection strings, keys, SQL text or
//! guest data: they end up in logs and problem reports.

use std::fmt;
use std::io;

/// Everything that can go wrong in the module.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Settings, identity or catalog are missing or malformed.
    Config(String),
    /// A file could not be read or written (`what` names the file's role, not its content).
    Io(String, io::Error),
    /// An envelope, a wrapped key or a vault file failed to open or seal.
    Crypto(String),
    /// JSON from the cloud or from disk does not have the expected shape.
    Json(String),
    /// SQL Server failed: its error number when it answered (`-2` for a timeout, as
    /// `SqlException` reports it), and a message without SQL text.
    Sql {
        /// SQL Server error number, `-2` for a timeout, `None` below SQL Server.
        number: Option<i32>,
        /// What failed.
        message: String,
    },
    /// The cloud could not be reached or answered with an unexpected status.
    Transport(String),
    /// The cloud refused this agent (HTTP 401); `clock` when it blamed the clock.
    Refused {
        /// `true` when the cloud said the machine clock is wrong.
        clock: bool,
    },
    /// The cloud rejected the catalog version (HTTP 409).
    CatalogRejected,
    /// A reply was stamped further from this machine's clock than the replay window allows.
    ClockSkew {
        /// Reply time minus local time, in seconds.
        seconds: i64,
    },
}

impl Error {
    /// An I/O error for the file playing `role` (`"identity"`, `"journal"`...).
    pub fn io(role: impl Into<String>, e: io::Error) -> Error {
        Error::Io(role.into(), e)
    }

    /// The SQL Server error number, when SQL Server answered.
    pub fn sql_number(&self) -> Option<i32> {
        match self {
            Error::Sql { number, .. } => *number,
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(m) => write!(f, "configuration: {m}"),
            Error::Io(role, e) => write!(f, "{role}: {e}"),
            Error::Crypto(m) => write!(f, "crypto: {m}"),
            Error::Json(m) => write!(f, "json: {m}"),
            Error::Sql {
                number: Some(n),
                message,
            } => write!(f, "sql {n}: {message}"),
            Error::Sql {
                number: None,
                message,
            } => write!(f, "sql: {message}"),
            Error::Transport(m) => write!(f, "cloud: {m}"),
            Error::Refused { clock: true } => f.write_str("cloud refused this agent: clock skew"),
            Error::Refused { clock: false } => f.write_str("cloud refused this agent's key"),
            Error::CatalogRejected => f.write_str("cloud rejected the operation catalog"),
            Error::ClockSkew { seconds } => write!(
                f,
                "the cloud's reply is stamped {seconds} s away from this machine's clock"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(_, e) => Some(e),
            _ => None,
        }
    }
}

impl From<Error> for dl_agent::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Config(m) => dl_agent::Error::Config(m),
            other => dl_agent::Error::Module(other.to_string()),
        }
    }
}
