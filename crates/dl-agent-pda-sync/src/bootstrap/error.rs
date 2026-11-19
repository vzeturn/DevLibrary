//! The failure of a bootstrap call: a coded `AgentException`, or what the C# code lets
//! escape unwrapped (a `SqlException` outside the connection step, a secret store I/O
//! failure, a cancellation).

use crate::config::AgentError;
use crate::reporting::log::Exception;
use crate::reporting::sql_error::SqlFailure;
use crate::security::SecretError;
use std::fmt;

/// A failed bootstrap, probe or enrollment step.
#[derive(Debug, Clone)]
pub enum Error {
    /// A coded failure (`AgentException`).
    Agent(AgentError),
    /// An exception the C# code does not catch (its .NET type and message; `sql` is set for
    /// a `SqlException`).
    Unexpected(Box<Exception>),
}

impl Error {
    /// The coded failure, when this is one.
    pub fn agent(&self) -> Option<&AgentError> {
        match self {
            Error::Agent(e) => Some(e),
            Error::Unexpected(_) => None,
        }
    }

    /// The stable code of a coded failure.
    pub fn code(&self) -> Option<&str> {
        self.agent().map(AgentError::code)
    }

    /// The SQL Server failure of an escaped `SqlException`.
    pub fn sql(&self) -> Option<&SqlFailure> {
        match self {
            Error::Unexpected(ex) => ex.sql.as_ref(),
            Error::Agent(_) => None,
        }
    }

    /// This failure as a logged exception.
    pub fn to_exception(&self) -> Exception {
        match self {
            Error::Agent(e) => Exception::from_agent_error(e),
            Error::Unexpected(ex) => (**ex).clone(),
        }
    }

    /// An escaped exception of `type_name`.
    pub fn unexpected(type_name: &str, message: impl Into<String>) -> Error {
        Error::Unexpected(Box::new(Exception::new(type_name, message)))
    }

    /// `OperationCanceledException`: the context was cancelled.
    pub fn cancelled() -> Error {
        Error::unexpected(
            "System.OperationCanceledException",
            "The operation was canceled.",
        )
    }

    /// An escaped `SqlException`.
    pub fn sql_exception(failure: &SqlFailure) -> Error {
        Error::Unexpected(Box::new(failure.to_exception()))
    }

    /// A secret store failure: `KEY_STORE_FAILED` stays coded, the rest escapes as the
    /// exception the C# store throws.
    pub fn from_secret(e: &SecretError) -> Error {
        match e {
            SecretError::KeyStore(a) => Error::Agent(a.clone()),
            SecretError::InvalidName(_) => {
                Error::unexpected("System.ArgumentException", e.to_string())
            }
            SecretError::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied => {
                Error::unexpected("System.UnauthorizedAccessException", e.to_string())
            }
            SecretError::Io(_) => Error::unexpected("System.IO.IOException", e.to_string()),
            SecretError::Protect(_) => Error::unexpected(
                "System.Security.Cryptography.CryptographicException",
                e.to_string(),
            ),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Agent(e) => write!(f, "{e}"),
            Error::Unexpected(ex) => f.write_str(&ex.message),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Agent(e) => Some(e),
            Error::Unexpected(_) => None,
        }
    }
}

impl From<AgentError> for Error {
    fn from(e: AgentError) -> Error {
        Error::Agent(e)
    }
}

/// Bootstrap result.
pub type Result<T> = std::result::Result<T, Error>;

/// How a `dl_tds` failure surfaces in C#: `Ok` with the `SqlException` facts, or `Err`
/// with the client-side exception SqlClient throws instead (conversion, parameter,
/// configuration errors).
pub fn tds_failure(
    e: &dl_tds::Error,
    messages: &[dl_tds::Message],
) -> std::result::Result<SqlFailure, Box<Exception>> {
    match SqlFailure::from_tds(e, messages) {
        Some(f) => Ok(f),
        None => {
            let ty = match e {
                dl_tds::Error::Conversion { .. } | dl_tds::Error::Null { .. } => {
                    "System.InvalidCastException"
                }
                dl_tds::Error::Param { .. } | dl_tds::Error::Config { .. } => {
                    "System.ArgumentException"
                }
                _ => "System.InvalidOperationException",
            };
            Err(Box::new(Exception::new(ty, e.to_string())))
        }
    }
}
