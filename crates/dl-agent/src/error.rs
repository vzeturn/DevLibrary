use core::fmt;

/// Everything that can go wrong in the agent runtime.
#[derive(Debug)]
pub enum Error {
    /// File system or OS error, with what was being done.
    Io(&'static str, std::io::Error),
    /// The hub could not be reached (DNS, TCP, TLS, timeout).
    Transport(String),
    /// The hub answered with a non-success status.
    Status {
        /// HTTP status code.
        code: u16,
        /// Response body, truncated.
        body: String,
    },
    /// A JSON document did not have the expected shape.
    Json(String),
    /// A version string is not `MAJOR.MINOR.PATCH[-pre]`.
    Version(String),
    /// A release manifest was rejected (signature, freshness, downgrade, missing target).
    Manifest(String),
    /// A downloaded artifact does not match its manifest entry.
    Integrity(String),
    /// Local configuration or identity is missing or invalid.
    Config(String),
    /// The agent has not been enrolled yet.
    NotEnrolled,
    /// A module returned an error.
    Module(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(what, e) => write!(f, "{what}: {e}"),
            Error::Transport(m) => write!(f, "hub unreachable: {m}"),
            Error::Status { code, body } => write!(f, "hub answered {code}: {body}"),
            Error::Json(m) => write!(f, "invalid JSON: {m}"),
            Error::Version(m) => write!(f, "invalid version: {m}"),
            Error::Manifest(m) => write!(f, "release manifest rejected: {m}"),
            Error::Integrity(m) => write!(f, "artifact integrity check failed: {m}"),
            Error::Config(m) => write!(f, "configuration: {m}"),
            Error::NotEnrolled => f.write_str("agent is not enrolled"),
            Error::Module(m) => write!(f, "module: {m}"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Error::Io(_, e) => Some(e),
            _ => None,
        }
    }
}

impl From<dl_json::Error> for Error {
    fn from(e: dl_json::Error) -> Error {
        Error::Json(e.to_string())
    }
}

/// Wraps an I/O result with a description of the operation.
pub(crate) fn io<T>(what: &'static str, r: std::io::Result<T>) -> Result<T, Error> {
    r.map_err(|e| Error::Io(what, e))
}
