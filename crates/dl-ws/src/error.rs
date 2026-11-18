use std::fmt;
use std::io;

/// Errors returned by this crate.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The stream failed. A read timeout shows up here with
    /// [`io::ErrorKind::WouldBlock`] or [`io::ErrorKind::TimedOut`]; see
    /// [`Error::is_timeout`].
    Io(io::Error),
    /// Connecting or the HTTP part of the handshake failed.
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    Http(dl_http::Error),
    /// The server answered the upgrade request with a status other than `101`.
    Rejected {
        /// The HTTP status code (e.g. 401).
        status: u16,
        /// The response body, as text (lossy), for diagnostics.
        body: String,
    },
    /// The `101` response or the upgrade request does not follow RFC 6455.
    Handshake(&'static str),
    /// The peer broke the framing rules. The connection is closed with code 1002.
    Protocol(&'static str),
    /// A text message or close reason is not valid UTF-8 (close code 1007).
    InvalidUtf8,
    /// A frame or message is larger than the configured limit (close code 1009).
    MessageTooBig {
        /// The limit in bytes.
        limit: usize,
    },
    /// The peer closed the TCP connection without a close frame.
    ConnectionReset,
    /// The close handshake has completed or a previous error ended the connection.
    Closed,
    /// The argument cannot be sent (a control payload over 125 bytes, an invalid close
    /// code, or a send after `close`).
    InvalidInput(&'static str),
}

impl Error {
    /// Whether this is a read or write timeout; the connection is still usable after a
    /// read timeout.
    pub fn is_timeout(&self) -> bool {
        matches!(self, Error::Io(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut))
    }

    /// The close code sent to the peer for this error, if any.
    pub(crate) fn close_code(&self) -> Option<u16> {
        match self {
            Error::Protocol(_) => Some(1002),
            Error::InvalidUtf8 => Some(1007),
            Error::MessageTooBig { .. } => Some(1009),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "WebSocket I/O error: {e}"),
            #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
            Error::Http(e) => write!(f, "WebSocket connect failed: {e}"),
            Error::Rejected { status, body } => {
                write!(f, "WebSocket upgrade rejected with status {status}")?;
                let body = body.trim();
                if !body.is_empty() {
                    let cut: String = body.chars().take(200).collect();
                    write!(f, ": {cut}")?;
                }
                Ok(())
            }
            Error::Handshake(why) => write!(f, "invalid WebSocket handshake: {why}"),
            Error::Protocol(why) => write!(f, "WebSocket protocol error: {why}"),
            Error::InvalidUtf8 => f.write_str("WebSocket text is not valid UTF-8"),
            Error::MessageTooBig { limit } => {
                write!(f, "WebSocket message exceeds the limit of {limit} bytes")
            }
            Error::ConnectionReset => {
                f.write_str("WebSocket connection closed without a close frame")
            }
            Error::Closed => f.write_str("WebSocket connection is closed"),
            Error::InvalidInput(why) => write!(f, "invalid WebSocket input: {why}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
            Error::Http(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
impl From<dl_http::Error> for Error {
    fn from(e: dl_http::Error) -> Error {
        Error::Http(e)
    }
}
