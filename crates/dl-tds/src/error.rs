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
    /// The connection is unusable after an earlier I/O, TLS or protocol failure.
    Broken,
    /// The SQL Server Browser (UDP 1434) did not return a TCP port for the instance.
    Browser(String),
    /// A feature the server requested is not implemented (e.g. integrated authentication
    /// continuation, COMPUTE BY rows).
    Unsupported(&'static str),
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
            Error::Broken => f.write_str("connection is broken"),
            Error::Browser(m) => write!(f, "SQL Server Browser: {m}"),
            Error::Unsupported(m) => write!(f, "not supported: {m}"),
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
