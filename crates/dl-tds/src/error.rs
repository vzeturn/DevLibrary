//! The crate's error type.

use std::fmt;
use std::io;

/// Everything that can go wrong while configuring, connecting or running a request.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The socket failed (connect, read or write).
    Io(io::Error),
    /// The TLS layer failed (handshake, certificate check, or TLS not available).
    Tls(dl_tls::Error),
    /// The connection string or a [`Config`](crate::Config) value is invalid.
    Config {
        /// The connection-string key involved (lower case), or `""` for the whole string.
        key: String,
        /// What is wrong.
        message: String,
    },
    /// SQL Server returned an ERROR token for the request.
    ///
    /// When one statement produces several errors, these fields describe the **last** one
    /// (go-mssqldb does the same); every error, and every INFO message, is also available
    /// from [`Client::take_messages`](crate::Client::take_messages).
    Server {
        /// Error number (`ERROR_NUMBER()`), e.g. 102 for a syntax error.
        number: i32,
        /// Error state (`ERROR_STATE()`).
        state: u8,
        /// Severity class (`ERROR_SEVERITY()`), 11–25 for errors.
        class: u8,
        /// Message text.
        message: String,
        /// Name of the server that raised the error.
        server: String,
        /// Stored procedure or trigger name, empty for a batch.
        procedure: String,
        /// Line number within the batch or procedure (1-based; 0 if unknown).
        line: i32,
    },
    /// The server's byte stream is malformed. `token` is the TDS token type being parsed
    /// (`0` outside a token, e.g. in a packet header or PRELOGIN), `offset` the byte offset
    /// within the response message (or packet) where parsing failed.
    Malformed {
        /// TDS token type (e.g. `0x81` COLMETADATA, `0xD1` ROW).
        token: u8,
        /// Byte offset within the message.
        offset: usize,
        /// What was expected.
        what: &'static str,
    },
    /// `Encrypt=true`/`strict` was requested but the server does not support encryption.
    EncryptionNotSupported,
    /// The server asked to reconnect to another server (Azure SQL routing), which is not
    /// supported.
    Routing {
        /// Server to reconnect to.
        server: String,
        /// Port to reconnect to.
        port: u16,
    },
    /// A CHAR/VARCHAR/TEXT column uses a code page this crate does not decode. Only the
    /// double-byte code pages 932, 936, 949 and 950 (Japanese, Chinese, Korean collations)
    /// are affected; read such columns as NVARCHAR (`CAST(col AS nvarchar(...))`).
    UnsupportedCodePage {
        /// Windows code page number.
        code_page: u16,
    },
    /// A column value cannot be converted to the requested Rust type.
    Conversion {
        /// Column index (0-based).
        column: usize,
        /// The stored value's kind, e.g. `"String"`.
        from: &'static str,
        /// The requested Rust type, e.g. `"i32"`.
        to: &'static str,
        /// Why the conversion failed.
        reason: String,
    },
    /// A NULL value was read into a non-`Option` type.
    Null {
        /// Column index (0-based).
        column: usize,
    },
    /// The column index or name does not exist in the row.
    ColumnNotFound(String),
    /// A parameter cannot be sent (bad name, value out of range for its SQL type).
    Param {
        /// Parameter name.
        name: String,
        /// What is wrong.
        message: String,
    },
    /// The server did not answer within the configured timeout. The request was cancelled
    /// with an ATTENTION packet; the connection is still usable unless the cancellation
    /// itself failed (then [`Client::is_broken`](crate::Client::is_broken) is `true`).
    Timeout,
    /// The request was stopped through a [`CancelToken`](crate::CancelToken). Like
    /// [`Error::Timeout`] it was cancelled with an ATTENTION packet and the connection is
    /// still usable unless [`Client::is_broken`](crate::Client::is_broken) says otherwise.
    Cancelled,
    /// The connection is unusable after an earlier I/O, TLS or protocol failure.
    Broken,
    /// The SQL Server Browser (UDP 1434) did not return a TCP port for the instance.
    Browser(String),
    /// A feature is not available: integrated authentication outside Windows, COMPUTE BY
    /// rows, a server asking for integrated authentication on a SQL-authentication login.
    Unsupported(&'static str),
    /// Windows integrated authentication failed in the SSPI layer (no credentials for the
    /// target, unreachable domain controller, ...). The message never contains tokens.
    Sspi {
        /// The SSPI function that failed, e.g. `"InitializeSecurityContextW"`.
        function: &'static str,
        /// Its `SECURITY_STATUS` (an HRESULT such as `0x8009030C`).
        status: i32,
    },
    /// NTLM authentication with explicit Windows credentials (`User ID=DOMAIN\user`) could
    /// not build its answer: the server's CHALLENGE is malformed or asks for something this
    /// NTLMv2-only client does not do. A wrong password is [`Error::Server`] (18456), not this.
    /// The message never contains credentials or token bytes.
    Ntlm(&'static str),
}

/// Names of the SSPI status codes integrated authentication runs into most often.
fn sspi_status_name(status: i32) -> Option<&'static str> {
    Some(match status as u32 {
        0x8009_0300 => "SEC_E_INSUFFICIENT_MEMORY",
        0x8009_0301 => "SEC_E_INVALID_HANDLE",
        0x8009_0304 => "SEC_E_INTERNAL_ERROR",
        0x8009_0305 => "SEC_E_SECPKG_NOT_FOUND",
        0x8009_0308 => "SEC_E_INVALID_TOKEN",
        0x8009_030C => "SEC_E_LOGON_DENIED",
        0x8009_030D => "SEC_E_UNKNOWN_CREDENTIALS",
        0x8009_030E => "SEC_E_NO_CREDENTIALS",
        0x8009_030F => "SEC_E_MESSAGE_ALTERED",
        0x8009_0311 => "SEC_E_NO_AUTHENTICATING_AUTHORITY",
        0x8009_0322 => "SEC_E_WRONG_PRINCIPAL",
        0x8009_0324 => "SEC_E_TIME_SKEW",
        0x8009_0350 => "SEC_E_DOWNGRADE_DETECTED",
        0x8009_0363 => "SEC_E_MUTUAL_AUTH_FAILED",
        _ => return None,
    })
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Tls(e) => write!(f, "{e}"),
            Error::Config { key, message } if key.is_empty() => {
                write!(f, "invalid connection string: {message}")
            }
            Error::Config { key, message } => {
                write!(f, "invalid connection string value for '{key}': {message}")
            }
            Error::Server {
                number,
                state,
                class,
                message,
                procedure,
                line,
                ..
            } => {
                write!(
                    f,
                    "mssql: {message} (number {number}, state {state}, class {class}"
                )?;
                if !procedure.is_empty() {
                    write!(f, ", procedure {procedure}")?;
                }
                write!(f, ", line {line})")
            }
            Error::Malformed {
                token,
                offset,
                what,
            } => {
                write!(
                    f,
                    "malformed TDS stream (token {token:#04x}, offset {offset}): {what}"
                )
            }
            Error::EncryptionNotSupported => f.write_str("server does not support encryption"),
            Error::Routing { server, port } => {
                write!(
                    f,
                    "server requested routing to {server}:{port}, which is not supported"
                )
            }
            Error::UnsupportedCodePage { code_page } => {
                write!(
                    f,
                    "code page {code_page} is not supported for CHAR/VARCHAR data"
                )
            }
            Error::Conversion {
                column,
                from,
                to,
                reason,
            } => write!(
                f,
                "column {column}: cannot convert {from} to {to}: {reason}"
            ),
            Error::Null { column } => {
                write!(f, "column {column} is NULL; read it as Option<T>")
            }
            Error::ColumnNotFound(c) => write!(f, "column {c} not found"),
            Error::Param { name, message } => write!(f, "parameter @{name}: {message}"),
            Error::Timeout => f.write_str("request timed out and was cancelled"),
            Error::Cancelled => f.write_str("request was cancelled by the caller"),
            Error::Broken => f.write_str("connection is broken"),
            Error::Browser(m) => write!(f, "SQL Server Browser: {m}"),
            Error::Unsupported(m) => write!(f, "not supported: {m}"),
            Error::Sspi { function, status } => {
                write!(
                    f,
                    "integrated authentication failed: {function} returned {:#010X}",
                    *status as u32
                )?;
                if let Some(name) = sspi_status_name(*status) {
                    write!(f, " ({name})")?;
                }
                Ok(())
            }
            Error::Ntlm(m) => write!(f, "NTLM authentication failed: {m}"),
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
        match e {
            dl_tls::Error::Io(io) => Error::Io(io),
            other => Error::Tls(other),
        }
    }
}

impl Error {
    /// `true` for [`Error::Tls`]: the TLS handshake failed, including a connection the
    /// server dropped or reset during the handshake (the usual answer of a server that
    /// cannot speak the offered TLS versions). Callers use it to decide whether retrying
    /// with an older TLS version or without encryption can help.
    pub fn is_tls(&self) -> bool {
        matches!(self, Error::Tls(_))
    }

    /// `true` for [`Error::Server`].
    pub fn is_server(&self) -> bool {
        matches!(self, Error::Server { .. })
    }

    /// The SQL Server error number for [`Error::Server`], `None` otherwise.
    pub fn number(&self) -> Option<i32> {
        match self {
            Error::Server { number, .. } => Some(*number),
            _ => None,
        }
    }

    pub(crate) fn config(key: &str, message: impl Into<String>) -> Error {
        Error::Config {
            key: key.to_string(),
            message: message.into(),
        }
    }
}
