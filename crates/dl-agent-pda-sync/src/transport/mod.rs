//! Cloud HTTP client, request signing, token and enrollment (port of `Transport/`).
//!
//! Calls block the calling thread; a [`Ctx`](crate::util::ctx::Ctx) plays the C#
//! `CancellationToken`.

pub mod enrollment;
pub mod http;
pub mod retry;
pub mod signer;
pub mod token;

pub use enrollment::{EnrollmentClient, RedeemResult};
pub use http::{SyncHttpClient, SyncResponse};
pub use retry::RetryPolicy;
pub use signer::{RequestSigner, SignedHeaders};
pub use token::TokenProvider;

use crate::config::AgentError;
use crate::reporting::log::Exception;
use dl_json::Value;
use std::fmt;

/// A failed cloud call.
#[derive(Debug, Clone)]
pub enum Error {
    /// A coded failure (`AgentException`).
    Agent(AgentError),
    /// The context was cancelled or its deadline passed (`OperationCanceledException`).
    Cancelled,
    /// What the C# code lets escape as an unexpected exception (malformed JSON, a missing
    /// member, an unreadable upload file), with the .NET exception type.
    Unexpected {
        /// The .NET exception type, e.g. `System.Text.Json.JsonException`.
        type_name: &'static str,
        /// The message.
        message: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Agent(e) => write!(f, "{e}"),
            Error::Cancelled => f.write_str("The operation was canceled."),
            Error::Unexpected { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Agent(e) => Some(e),
            _ => None,
        }
    }
}

impl From<AgentError> for Error {
    fn from(e: AgentError) -> Error {
        Error::Agent(e)
    }
}

impl Error {
    /// The coded failure, when this is one.
    pub fn agent(&self) -> Option<&AgentError> {
        match self {
            Error::Agent(e) => Some(e),
            _ => None,
        }
    }

    /// The stable code of a coded failure.
    pub fn code(&self) -> Option<&str> {
        self.agent().map(AgentError::code)
    }

    /// This failure as a logged exception.
    pub fn to_exception(&self) -> Exception {
        match self {
            Error::Agent(e) => Exception::from_agent_error(e),
            Error::Cancelled => Exception::new(
                "System.Threading.Tasks.TaskCanceledException",
                "A task was canceled.",
            ),
            Error::Unexpected { type_name, message } => Exception::new(*type_name, message.clone()),
        }
    }
}

/// `JsonElement` accessors with the exceptions the C# code would throw.
pub(crate) mod element {
    use super::Error;
    use dl_json::Value;

    fn kind(v: &Value) -> &'static str {
        match v {
            Value::Null => "Null",
            Value::Bool(true) => "True",
            Value::Bool(false) => "False",
            Value::Number(_) => "Number",
            Value::String(_) => "String",
            Value::Array(_) => "Array",
            Value::Object(_) => "Object",
        }
    }

    fn invalid(message: String) -> Error {
        Error::Unexpected {
            type_name: "System.InvalidOperationException",
            message,
        }
    }

    /// `TryGetProperty(name)` (ordinal; the last of repeated members).
    pub fn try_get<'a>(v: &'a Value, name: &str) -> Result<Option<&'a Value>, Error> {
        match v {
            Value::Object(m) => Ok(m
                .iter()
                .rev()
                .find(|(k, _)| k.as_str() == name)
                .map(|(_, x)| x)),
            other => Err(invalid(format!(
                "The requested operation requires an element of type 'Object', but the target element has type '{}'.",
                kind(other)
            ))),
        }
    }

    /// `GetProperty(name)`.
    pub fn get<'a>(v: &'a Value, name: &str) -> Result<&'a Value, Error> {
        try_get(v, name)?.ok_or_else(|| Error::Unexpected {
            type_name: "System.Collections.Generic.KeyNotFoundException",
            message: "The given key was not present in the dictionary.".to_owned(),
        })
    }

    /// `GetString()`: `None` for null.
    pub fn string(v: &Value) -> Result<Option<String>, Error> {
        match v {
            Value::Null => Ok(None),
            Value::String(s) => Ok(Some(s.clone())),
            other => Err(invalid(format!(
                "The requested operation requires an element of type 'String', but the target element has type '{}'.",
                kind(other)
            ))),
        }
    }

    /// `GetInt32()`.
    pub fn int32(v: &Value) -> Result<i32, Error> {
        match v {
            Value::Number(n) => {
                let text = n
                    .as_raw_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| n.to_string());
                let digits = text.strip_prefix('-').unwrap_or(&text);
                if !digits.is_empty()
                    && digits.bytes().all(|b| b.is_ascii_digit())
                    && let Ok(i) = text.parse::<i32>()
                {
                    return Ok(i);
                }
                Err(Error::Unexpected {
                    type_name: "System.FormatException",
                    message: "One of the identified items was in an invalid format.".to_owned(),
                })
            }
            other => Err(invalid(format!(
                "The requested operation requires an element of type 'Number', but the target element has type '{}'.",
                kind(other)
            ))),
        }
    }

    /// `JsonElement.ToString()`.
    pub fn to_string(v: &Value) -> String {
        crate::config::stj_read::element_to_string(v)
    }
}

/// Parses `data` as the C# code reads a `JsonElement` (helper for callers of
/// [`SyncResponse::data`]).
pub fn data_member<'a>(data: &'a Value, name: &str) -> Result<Option<&'a Value>, Error> {
    element::try_get(data, name)
}
