//! The module's error: a message (Go's wrapped `error` text, kept verbatim because the
//! hub stores it on the job) plus a kind for the decisions Go makes with `errors.Is`.

use std::fmt;

/// What kind of failure, for retry and routing decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The PMS database could not be reached (`ErrPMSDown`): hold the job, trip the breaker.
    PmsDown,
    /// The booking needs a human (`errManualConflict`): ack as `needsManual`.
    ManualConflict,
    /// `ErrMappingDuplicate`.
    MappingDuplicate,
    /// `ErrMappingNotFound`.
    MappingNotFound,
    /// `ErrMappingAmbiguous`.
    MappingAmbiguous,
    /// The hub answered 403: a newer install took over this hotel (`ErrAgentSuperseded`).
    Superseded,
    /// The hub answered 401: the token is wrong or revoked.
    Unauthorized,
    /// The hub could not be reached or answered with another non-200 status.
    Hub,
    /// Anything else.
    Other,
}

/// A module error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: Kind,
    msg: String,
}

/// `Result` with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// A new error.
    pub fn new(kind: Kind, msg: impl Into<String>) -> Error {
        Error {
            kind,
            msg: msg.into(),
        }
    }

    /// An [`Kind::Other`] error.
    pub fn other(msg: impl Into<String>) -> Error {
        Error::new(Kind::Other, msg)
    }

    /// The kind.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The message.
    pub fn message(&self) -> &str {
        &self.msg
    }

    /// `fmt.Errorf("<prefix>%w", err)`: prefixes the message, keeps the kind.
    pub fn context(mut self, prefix: impl AsRef<str>) -> Error {
        self.msg = format!("{}{}", prefix.as_ref(), self.msg);
        self
    }

    /// Appends text to the message, keeps the kind.
    pub fn suffix(mut self, suffix: impl AsRef<str>) -> Error {
        self.msg.push_str(suffix.as_ref());
        self
    }

    /// `isPMSDown`: the kind, or one of go-mssqldb's connection-failure phrases.
    pub fn is_pms_down(&self) -> bool {
        if self.kind == Kind::PmsDown {
            return true;
        }
        let s = self.msg.to_lowercase();
        PMS_DOWN_SIGNS.iter().any(|sig| s.contains(sig))
    }
}

/// `isPMSDown`'s substrings.
const PMS_DOWN_SIGNS: &[&str] = &[
    "unable to open tcp connection",
    "dial tcp",
    "connection refused",
    "connection reset",
    "i/o timeout",
    "login error",
    "server doesn't support",
    "no such host",
];

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for Error {}

impl From<dl_tds::Error> for Error {
    /// Transport failures (I/O, TLS, timeout, broken session) mean the PMS is down.
    fn from(e: dl_tds::Error) -> Error {
        let kind = match &e {
            dl_tds::Error::Io(_)
            | dl_tds::Error::Tls(_)
            | dl_tds::Error::Timeout
            | dl_tds::Error::Broken
            | dl_tds::Error::Browser(_)
            | dl_tds::Error::Routing { .. }
            | dl_tds::Error::EncryptionNotSupported
            | dl_tds::Error::Sspi { .. } => Kind::PmsDown,
            // Login failed / cannot open database: go-mssqldb reports both as "login error".
            dl_tds::Error::Server { number, .. } if matches!(*number, 18456 | 4060) => {
                Kind::PmsDown
            }
            _ => Kind::Other,
        };
        Error::new(kind, e.to_string())
    }
}

impl From<dl_agent::Error> for Error {
    /// A pool checkout failure (connect, login, acquire timeout) means the PMS is down.
    fn from(e: dl_agent::Error) -> Error {
        let kind = match &e {
            dl_agent::Error::Sql(_) => Kind::PmsDown,
            _ => Kind::Other,
        };
        Error::new(kind, e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_pms_down() {
        // pmserr_test.go
        for (msg, want) in [
            ("unable to open tcp connection with host 'x'", true),
            ("dial tcp 10.0.0.1:1433: connect: connection refused", true),
            ("read tcp: i/o timeout", true),
            ("Login error: mssql: Login failed for user 'sa'", true),
            ("lookup sqlhost: no such host", true),
            ("mssql: Invalid column name 'Foo'", false),
            ("mssql: String or binary data would be truncated", false),
        ] {
            assert_eq!(Error::other(msg).is_pms_down(), want, "{msg}");
        }
        assert!(Error::new(Kind::PmsDown, "x").is_pms_down());
        let wrapped = Error::new(Kind::PmsDown, "x").context("booking 1: ");
        assert!(wrapped.is_pms_down());
        assert_eq!(wrapped.to_string(), "booking 1: x");
    }
}
