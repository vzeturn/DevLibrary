//! The server-side command queue as the agent sees it (`Queue/`): the [`CommandQueue`]
//! trait (ICommandQueue), [`ApiCommandQueue`] over the API's agent routes, and the typed
//! errors (StaleClaimException, DeviceApiException, transport failures).

mod api;

use std::fmt;

use dl_uuid::Uuid;

use crate::config::DotnetError;
use crate::contracts::{DeviceCommand, PendingResult};

pub use api::{ADAPTER_HEADER, AGENT_ID_HEADER, AUTH_SCHEME, ApiCommandQueue, HTTP_TIMEOUT};

/// The server-side queue. Claiming is the ack.
pub trait CommandQueue: Send + Sync {
    /// Claims the oldest queued command for this agent's workstation; `None` when none is
    /// queued.
    fn claim(&self) -> Result<Option<DeviceCommand>, QueueError>;

    /// Sends one result, once (retries belong to the `ResultReporter`). Fails with
    /// [`QueueError::StaleClaim`] when the server refuses it for good, [`QueueError::Api`]
    /// on any other status, and [`QueueError::Transport`] / [`QueueError::Timeout`] when
    /// the API cannot be reached.
    fn send_result(&self, result: &PendingResult) -> Result<(), QueueError>;
}

/// Why the server refused a result for good (`StaleClaimReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleClaimReason {
    /// 409 DEVICE_CLAIM_NOT_HELD: a newer claim exists (or the token is wrong).
    NotHeld,
    /// 409 DEVICE_OUTCOME_CONFLICT: already completed with the other outcome.
    OutcomeConflict,
    /// 404: the command no longer exists.
    Missing,
}

impl fmt::Display for StaleClaimReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            StaleClaimReason::NotHeld => "NotHeld",
            StaleClaimReason::OutcomeConflict => "OutcomeConflict",
            StaleClaimReason::Missing => "Missing",
        })
    }
}

/// `StaleClaimException`: retrying the same result can never succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleClaim {
    /// The command whose result was refused.
    pub command_id: Uuid,
    /// Why.
    pub reason: StaleClaimReason,
}

impl fmt::Display for StaleClaim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Result for device command {} refused: {}",
            self.command_id, self.reason
        )
    }
}

/// `DeviceApiException`: the API answered with an unexpected status. The message never
/// contains the agent token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceApiError {
    /// HTTP status.
    pub status: u16,
    /// `Exception.Message`.
    pub message: String,
}

impl DeviceApiError {
    /// An error for `status` with `message`.
    pub fn new(status: u16, message: impl Into<String>) -> DeviceApiError {
        DeviceApiError {
            status,
            message: message.into(),
        }
    }
}

/// Why the API could not be reached: the `HttpRequestException` message and, for a failed
/// connect, the inner `SocketException` (Winsock / errno code and the OS text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError {
    /// `HttpRequestException.Message`.
    pub message: String,
    /// The inner `SocketException`: error code and message.
    pub socket: Option<(i32, String)>,
}

impl From<&str> for TransportError {
    fn from(message: &str) -> TransportError {
        TransportError {
            message: message.to_owned(),
            socket: None,
        }
    }
}

impl From<String> for TransportError {
    fn from(message: String) -> TransportError {
        TransportError {
            message,
            socket: None,
        }
    }
}

/// A failed queue call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueError {
    /// The server refused the result for good.
    StaleClaim(StaleClaim),
    /// Unexpected HTTP status.
    Api(DeviceApiError),
    /// The API could not be reached (`HttpRequestException`).
    Transport(TransportError),
    /// No answer within [`HTTP_TIMEOUT`] (`TaskCanceledException` from `HttpClient.Timeout`).
    Timeout,
    /// Anything else, as the .NET exception it stands for (unreadable token file,
    /// malformed claim body, invalid base URL or header value). Not retried by the reporter.
    Other(DotnetError),
}

impl QueueError {
    /// The exception the C# agent would log for this failure.
    pub fn to_dotnet(&self) -> DotnetError {
        match self {
            QueueError::StaleClaim(s) => {
                DotnetError::new("SmilePms.DeviceAgent.Queue.StaleClaimException", s.to_string())
            }
            QueueError::Api(e) => {
                DotnetError::new("SmilePms.DeviceAgent.Queue.DeviceApiException", &e.message)
            }
            QueueError::Transport(t) => {
                let e = DotnetError::new("System.Net.Http.HttpRequestException", &t.message);
                match &t.socket {
                    // SocketException.ToString() puts its error code after the type name.
                    Some((code, message)) => e.with_inner(DotnetError::new(
                        &format!("System.Net.Sockets.SocketException ({code})"),
                        message,
                    )),
                    None => e,
                }
            }
            QueueError::Timeout => DotnetError::new(
                "System.Threading.Tasks.TaskCanceledException",
                format!(
                    "The request was canceled due to the configured HttpClient.Timeout of {} seconds elapsing.",
                    HTTP_TIMEOUT.as_secs()
                ),
            )
            .with_inner(DotnetError::new(
                "System.TimeoutException",
                "The operation was canceled.",
            )),
            QueueError::Other(e) => e.clone(),
        }
    }
}

impl fmt::Display for QueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let e = self.to_dotnet();
        f.write_str(&e.message)
    }
}

impl std::error::Error for QueueError {}
