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

mod client_hello;
mod pkcs8;
mod server;

pub use server::{KeyAlgorithm, PrivateKey, ServerConfig};

#[cfg(windows)]
mod schannel_server_sys;
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
    min_version: Version,
}

impl Config {
    /// A configuration that verifies the certificate for `server_name` (SNI and host check)
    /// and accepts TLS 1.2 and 1.3.
    pub fn new(server_name: impl Into<String>) -> Config {
        Config {
            server_name: server_name.into(),
            verify: Verify::Full,
            alpn: Vec::new(),
            min_version: Version::Tls12,
        }
    }

    /// Sets the verification mode.
    pub fn verify(mut self, verify: Verify) -> Config {
        self.verify = verify;
        self
    }

    /// Sets the oldest protocol version offered (default [`Version::Tls12`]).
    ///
    /// [`Version::Tls10`] and [`Version::Tls11`] exist for old servers that cannot do
    /// TLS 1.2, such as an unpatched SQL Server 2008. They are weak: use them only as a
    /// fallback after a TLS 1.2 handshake failed. On OpenSSL 3 this also lowers the
    /// security level to 0 for the connection, because level 1 and above refuse
    /// TLS 1.0/1.1. The OS may still refuse them: Windows lets administrators disable
    /// old protocols for every process (SChannel registry keys), and such a handshake
    /// fails with [`Error::Handshake`].
    pub fn min_version(mut self, version: Version) -> Config {
        self.min_version = version;
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

    /// The oldest protocol version offered.
    pub fn minimum_version(&self) -> Version {
        self.min_version
    }
}

/// A TLS protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Version {
    /// TLS 1.0 (RFC 2246). Only as a fallback for old servers.
    Tls10,
    /// TLS 1.1 (RFC 4346). Only as a fallback for old servers.
    Tls11,
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

/// A TLS session over `S`: a client from [`TlsStream::connect`] or a server from
/// [`TlsStream::accept`].
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
    server_name: Option<String>,
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
        Ok(TlsStream {
            stream,
            session,
            server_name: None,
        })
    }

    /// Runs the server handshake over `stream`, presenting `config`'s certificate chain.
    ///
    /// The first call on a `config` builds the OS credential, so configuration errors
    /// (an empty chain, an invalid ALPN name, a certificate the OS cannot parse, on Linux a
    /// key that does not match the leaf) come back from it before anything is read, as
    /// [`Error::Io`] with [`io::ErrorKind::InvalidInput`] or as [`Error::Handshake`].
    /// A client offering ALPN with no protocol in common is refused with
    /// [`Error::Handshake`] code 120 (`no_application_protocol`). Clients are not asked for
    /// certificates. I/O errors of `stream` are returned unchanged inside [`Error::Io`]; set
    /// a read timeout on the stream so a silent client cannot hold the accept forever.
    pub fn accept(config: &ServerConfig, mut stream: S) -> Result<TlsStream<S>, Error> {
        let cred = config.credential()?;
        let prelude = server::read_client_hello(config, &mut stream)?;
        let session = imp::Session::accept(&cred, &mut stream, prelude.recv, &prelude.alpn)?;
        Ok(TlsStream {
            stream,
            session,
            server_name: prelude.server_name,
        })
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
            ..
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

    /// On a server session, the host name the client sent as SNI (RFC 6066 §3), exactly as
    /// sent (compare it case-insensitively). `None` when the client sent none or one that is
    /// not printable ASCII, and always on client sessions.
    pub fn server_name(&self) -> Option<&str> {
        self.server_name.as_deref()
    }

    /// The peer's leaf certificate in DER form, if the backend exposes it: on a client
    /// session the server's certificate; `None` on server sessions (clients are not asked
    /// for certificates).
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
            .field("server_name", &self.server_name)
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
        assert_eq!(c.minimum_version(), Version::Tls12);
        let c = c.min_version(Version::Tls10);
        assert_eq!(c.minimum_version(), Version::Tls10);
    }

    #[test]
    fn version_order() {
        assert!(Version::Tls13 > Version::Tls12);
        assert!(Version::Tls12 > Version::Tls11);
        assert!(Version::Tls11 > Version::Tls10);
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
