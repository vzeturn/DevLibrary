#![doc = include_str!("../README.md")]

use std::fmt;
use std::io::{self, Read, Write};

#[cfg_attr(
    not(any(windows, all(unix, not(target_vendor = "apple")))),
    allow(dead_code)
)]
mod alpn;
#[cfg_attr(not(all(unix, not(target_vendor = "apple"))), allow(dead_code))]
mod record;

#[cfg(windows)]
mod schannel_sys;

#[cfg(windows)]
#[path = "schannel.rs"]
mod imp;

#[cfg(all(unix, not(target_vendor = "apple")))]
mod openssl_sys;

#[cfg(all(unix, not(target_vendor = "apple")))]
#[path = "openssl.rs"]
mod imp;

#[cfg(not(any(windows, all(unix, not(target_vendor = "apple")))))]
#[path = "unsupported.rs"]
mod imp;
/// How the server certificate is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verify {
    /// Validate the chain against the OS trust store and match the host name (default).
    Full,
    /// Accept any certificate. Equivalent to SQL Server's `TrustServerCertificate=true`;
    /// the connection is encrypted but not authenticated.
    None,
}

/// Client configuration for one connection.
#[derive(Debug, Clone)]
pub struct Config {
    server_name: String,
    verify: Verify,
    alpn: Vec<Vec<u8>>,
}

impl Config {
    /// A configuration that verifies the certificate for `server_name` (SNI and host check).
    pub fn new(server_name: impl Into<String>) -> Config {
        Config {
            server_name: server_name.into(),
            verify: Verify::Full,
            alpn: Vec::new(),
        }
    }

    /// Sets the verification mode.
    pub fn verify(mut self, verify: Verify) -> Config {
        self.verify = verify;
        self
    }

    /// Sets the ALPN protocols to offer, most preferred first (e.g. `b"http/1.1"`).
    pub fn alpn(mut self, protocols: &[&[u8]]) -> Config {
        self.alpn = protocols.iter().map(|p| p.to_vec()).collect();
        self
    }

    /// The server name sent as SNI and checked against the certificate.
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// The verification mode.
    pub fn verify_mode(&self) -> Verify {
        self.verify
    }

    /// The ALPN protocols offered.
    pub fn alpn_protocols(&self) -> &[Vec<u8>] {
        &self.alpn
    }
}

/// TLS protocol version negotiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Version {
    /// TLS 1.2.
    Tls12,
    /// TLS 1.3.
    Tls13,
}

/// Errors from the TLS layer.
#[derive(Debug)]
pub enum Error {
    /// The underlying stream failed.
    Io(io::Error),
    /// The certificate chain is not trusted by the OS store.
    UntrustedCertificate,
    /// The certificate does not match the server name.
    NameMismatch,
    /// The certificate has expired or is not yet valid.
    ExpiredCertificate,
    /// The handshake failed for another reason; `code` is the OS status (SECURITY_STATUS or
    /// OpenSSL error), `message` a description.
    Handshake {
        /// OS status code.
        code: i64,
        /// Human-readable description.
        message: String,
    },
    /// No TLS implementation is available (e.g. OpenSSL not installed, or wasm).
    Unsupported(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "TLS I/O error: {e}"),
            Error::UntrustedCertificate => f.write_str("TLS certificate is not trusted"),
            Error::NameMismatch => f.write_str("TLS certificate does not match the server name"),
            Error::ExpiredCertificate => f.write_str("TLS certificate is expired or not yet valid"),
            Error::Handshake { code, message } => {
                write!(f, "TLS handshake failed ({code:#x}): {message}")
            }
            Error::Unsupported(why) => write!(f, "TLS is not available: {why}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

/// A TLS client session over `S`.
///
/// Wire behaviour every implementation keeps (TDS relies on it):
/// - each handshake flight is written with one `write_all` followed by one `flush`;
/// - each `write` call emits whole TLS records, written to `S` with `write_all`;
/// - no bytes are read from `S` after the handshake unless `read` is called.
///
/// `TlsStream<std::net::TcpStream>` is `Send`.
pub struct TlsStream<S> {
    stream: S,
    session: imp::Session,
}

impl<S: Read + Write> TlsStream<S> {
    /// Runs the client handshake over `stream`.
    ///
    /// An invalid configuration (a NUL byte in the server name, an empty server name with
    /// [`Verify::Full`], or an ALPN name that is empty or longer than 255 bytes) is reported
    /// as [`Error::Io`] with [`io::ErrorKind::InvalidInput`]. I/O errors of `stream` are
    /// returned unchanged inside [`Error::Io`].
    pub fn connect(config: &Config, mut stream: S) -> Result<TlsStream<S>, Error> {
        let session = imp::Session::connect(config, &mut stream)?;
        Ok(TlsStream { stream, session })
    }

    /// The underlying stream.
    pub fn get_ref(&self) -> &S {
        &self.stream
    }

    /// The underlying stream, mutably. Writing to it directly corrupts the session.
    pub fn get_mut(&mut self) -> &mut S {
        &mut self.stream
    }

    /// Ends TLS without close_notify and returns the stream plus any raw bytes already read
    /// from it that the session did not consume.
    pub fn into_parts(self) -> (S, Vec<u8>) {
        let TlsStream {
            stream,
            mut session,
        } = self;
        let leftover = session.take_leftover();
        (stream, leftover)
    }

    /// The negotiated protocol version.
    pub fn version(&self) -> Option<Version> {
        self.session.version()
    }

    /// The ALPN protocol the server selected.
    pub fn alpn_protocol(&self) -> Option<&[u8]> {
        self.session.alpn_protocol()
    }

    /// The server's leaf certificate in DER form, if the backend exposes it.
    pub fn peer_certificate_der(&self) -> Option<Vec<u8>> {
        self.session.peer_certificate_der()
    }

    /// Sends close_notify. The stream stays open.
    pub fn shutdown(&mut self) -> io::Result<()> {
        self.session.shutdown(&mut self.stream)
    }
}

impl<S: Read + Write> Read for TlsStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.session.read(&mut self.stream, buf)
    }
}

impl<S: Read + Write> Write for TlsStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.session.write(&mut self.stream, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

impl<S: fmt::Debug> fmt::Debug for TlsStream<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsStream")
            .field("stream", &self.stream)
            .field("version", &self.session.version())
            .field(
                "alpn",
                &self.session.alpn_protocol().map(String::from_utf8_lossy),
            )
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn assert_send<T: Send>() {}
    const _: () = assert_send::<TlsStream<std::net::TcpStream>>();

    #[test]
    fn tls_stream_over_tcp_is_send() {
        assert_send::<TlsStream<std::net::TcpStream>>();
    }

    #[test]
    fn config_accessors() {
        let c = Config::new("example.com")
            .verify(Verify::None)
            .alpn(&[b"h2", b"http/1.1"]);
        assert_eq!(c.server_name(), "example.com");
        assert_eq!(c.verify_mode(), Verify::None);
        assert_eq!(c.alpn_protocols(), &[b"h2".to_vec(), b"http/1.1".to_vec()]);
    }

    #[test]
    fn version_order() {
        assert!(Version::Tls13 > Version::Tls12);
    }

    #[test]
    fn error_display() {
        let e = Error::Handshake {
            code: 0x8009_0326,
            message: "illegal message".into(),
        };
        assert_eq!(
            e.to_string(),
            "TLS handshake failed (0x80090326): illegal message"
        );
        let e = Error::from(io::Error::from(io::ErrorKind::UnexpectedEof));
        assert!(matches!(e, Error::Io(ref io) if io.kind() == io::ErrorKind::UnexpectedEof));
        assert!(std::error::Error::source(&e).is_some());
    }
}
