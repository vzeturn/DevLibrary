use std::fmt;
use std::io;

/// Errors returned by this crate.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The URL is malformed or uses a scheme other than `http` / `https`.
    InvalidUrl {
        /// What is wrong with the URL.
        reason: &'static str,
    },
    /// A header name or value contains characters that HTTP does not allow.
    InvalidHeader {
        /// What is wrong with the header.
        reason: &'static str,
    },
    /// Network I/O failed (DNS, connect, read or write).
    Io(io::Error),
    /// The overall deadline or the connect timeout expired.
    Timeout,
    /// The TLS handshake failed.
    Tls(dl_tls::Error),
    /// The peer sent a malformed, oversized or unsupported message.
    InvalidResponse {
        /// What is wrong with the response.
        reason: &'static str,
    },
    /// The response body is larger than the configured limit.
    BodyTooLarge {
        /// The limit in bytes.
        limit: usize,
    },
    /// The server redirected more often than allowed.
    TooManyRedirects {
        /// The configured maximum.
        max: u32,
    },
    /// The proxy answered the `CONNECT` request with a non-2xx status.
    ProxyConnect {
        /// The proxy's status code (407 means proxy authentication is required).
        status: u16,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidUrl { reason } => write!(f, "invalid URL: {reason}"),
            Error::InvalidHeader { reason } => write!(f, "invalid header: {reason}"),
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Timeout => f.write_str("request timed out"),
            Error::Tls(e) => write!(f, "{e}"),
            Error::InvalidResponse { reason } => write!(f, "invalid HTTP response: {reason}"),
            Error::BodyTooLarge { limit } => {
                write!(f, "response body exceeds the limit of {limit} bytes")
            }
            Error::TooManyRedirects { max } => write!(f, "more than {max} redirects"),
            Error::ProxyConnect { status } => {
                write!(f, "proxy refused the CONNECT tunnel with status {status}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Tls(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

impl From<dl_tls::Error> for Error {
    fn from(e: dl_tls::Error) -> Error {
        Error::Tls(e)
    }
}
