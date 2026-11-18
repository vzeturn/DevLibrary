use std::fmt;
use std::sync::Arc;

/// Errors returned by this crate.
///
/// `Clone`, so a [`CloseReason::Transport`](crate::CloseReason::Transport) can be handed
/// to every caller of [`HubConnection::wait_closed`](crate::HubConnection::wait_closed);
/// the wrapped I/O, HTTP and WebSocket errors are shared through an `Arc`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Error {
    /// The hub URL is not a valid `http`, `https`, `ws` or `wss` URL.
    InvalidUrl(String),
    /// A [`ConnectOptions`](crate::ConnectOptions) value is out of range (a zero
    /// duration or size).
    InvalidOptions(&'static str),
    /// The negotiate request was answered with a status other than 200.
    NegotiateRejected {
        /// The HTTP status code, e.g. 401.
        status: u16,
        /// The response body as text (lossy), for diagnostics.
        body: String,
    },
    /// The negotiate response carried an `error`, does not offer the WebSockets
    /// transport, redirected too often or lacks a connection token.
    Negotiate(String),
    /// The negotiate request failed (DNS, connect, TLS, timeout, ...).
    Http(Arc<dl_http::Error>),
    /// Connecting the WebSocket or reading or writing it failed.
    WebSocket(Arc<dl_ws::Error>),
    /// An operating system error, e.g. when starting the connection threads.
    Io(Arc<std::io::Error>),
    /// The server rejected the handshake (its response carried an `error`).
    Handshake(String),
    /// A message is not valid JSON; the error carries the position inside the message.
    Json(dl_json::Error),
    /// A message is valid JSON but not a valid hub message (e.g. `type` is missing or a
    /// field has the wrong JSON type).
    InvalidMessage(&'static str),
    /// A message is larger than [`ConnectOptions::max_message_size`](crate::ConnectOptions::max_message_size).
    MessageTooLarge {
        /// The limit in bytes.
        limit: usize,
    },
    /// The hub method failed on the server (the completion carried an `error`).
    Hub {
        /// The hub method that was invoked.
        target: String,
        /// The error text from the server. ASP.NET Core sends the `HubException`
        /// message, or a generic text for other exceptions unless
        /// `EnableDetailedErrors` is on.
        message: String,
    },
    /// No answer in time: the completion of an invocation or the handshake response.
    Timeout,
    /// No message (not even a ping) arrived from the server within
    /// [`ConnectOptions::server_timeout`](crate::ConnectOptions::server_timeout).
    ServerTimeout,
    /// The server answered an invocation with a stream; streaming is not supported.
    StreamingNotSupported,
    /// The WebSocket was closed without a hub close message.
    WebSocketClosed {
        /// The close status code, if the peer sent one.
        code: Option<u16>,
        /// The close reason text.
        reason: String,
    },
    /// The connection has ended (before or during the call). See
    /// [`HubConnection::wait_closed`](crate::HubConnection::wait_closed) for why.
    Closed,
    /// Connecting is not supported on this target (`wasm32-unknown-unknown`).
    Unsupported,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidUrl(why) => write!(f, "invalid SignalR hub URL: {why}"),
            Error::InvalidOptions(why) => write!(f, "invalid SignalR connect options: {why}"),
            Error::NegotiateRejected { status, body } => {
                write!(f, "SignalR negotiate failed with status {status}")?;
                let body = body.trim();
                if !body.is_empty() {
                    let cut: String = body.chars().take(200).collect();
                    write!(f, ": {cut}")?;
                }
                Ok(())
            }
            Error::Negotiate(why) => write!(f, "SignalR negotiate failed: {why}"),
            Error::Http(e) => write!(f, "SignalR negotiate request failed: {e}"),
            Error::WebSocket(e) => write!(f, "SignalR transport error: {e}"),
            Error::Io(e) => write!(f, "SignalR I/O error: {e}"),
            Error::Handshake(why) => write!(f, "SignalR handshake rejected: {why}"),
            Error::Json(e) => write!(f, "invalid SignalR message: {e}"),
            Error::InvalidMessage(why) => write!(f, "invalid SignalR message: {why}"),
            Error::MessageTooLarge { limit } => {
                write!(f, "SignalR message exceeds the limit of {limit} bytes")
            }
            Error::Hub { target, message } => {
                write!(f, "hub method {target:?} failed: {message}")
            }
            Error::Timeout => f.write_str("SignalR call timed out"),
            Error::ServerTimeout => {
                f.write_str("SignalR server timeout: no message from the server in time")
            }
            Error::StreamingNotSupported => {
                f.write_str("the hub method returned a stream; streaming is not supported")
            }
            Error::WebSocketClosed { code, reason } => {
                f.write_str("the WebSocket was closed by the server")?;
                if let Some(c) = code {
                    write!(f, " with status {c}")?;
                }
                if !reason.is_empty() {
                    write!(f, ": {reason}")?;
                }
                Ok(())
            }
            Error::Closed => f.write_str("the SignalR connection is closed"),
            Error::Unsupported => {
                f.write_str("SignalR connections are not supported on this target")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Http(e) => Some(&**e),
            Error::WebSocket(e) => Some(&**e),
            Error::Io(e) => Some(&**e),
            Error::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<dl_ws::Error> for Error {
    fn from(e: dl_ws::Error) -> Error {
        Error::WebSocket(Arc::new(e))
    }
}

impl From<dl_http::Error> for Error {
    fn from(e: dl_http::Error) -> Error {
        Error::Http(Arc::new(e))
    }
}

impl From<dl_json::Error> for Error {
    fn from(e: dl_json::Error) -> Error {
        Error::Json(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(Arc::new(e))
    }
}
