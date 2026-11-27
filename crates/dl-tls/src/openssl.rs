//! TLS client and server sessions over the system OpenSSL (Linux and other non-Apple Unix).
//!
//! OpenSSL runs on two memory BIOs; this module moves bytes between them and the stream.
//! Network bytes are fed one whole TLS record at a time, so bytes after the last record
//! OpenSSL needed stay in `recv` and `into_parts` returns them exactly.

use std::io::{self, Read, Write};
use std::sync::Arc;

use crate::openssl_sys::{
    self as sys, CtxError, SSL_ERROR_WANT_READ, SSL_ERROR_WANT_WRITE, SSL_ERROR_ZERO_RETURN,
};
use crate::record::{Frame, frame};
use crate::{Config, Error, ServerConfig, Verify, Version, alpn};

const READ_CHUNK: usize = 18 * 1024;
const NO_LIBSSL: &str = "libssl.so.3 / libssl.so.1.1 not found";

/// A server's OpenSSL context, built once per [`ServerConfig`].
pub(crate) struct ServerCred(sys::ServerCtx);

impl ServerCred {
    pub(crate) fn new(cfg: &ServerConfig) -> Result<ServerCred, Error> {
        if !sys::available() {
            return Err(Error::Unsupported(NO_LIBSSL));
        }
        let ctx = sys::ServerCtx::new(
            cfg.cert_chain_der(),
            cfg.private_key().pkcs8_der(),
            cfg.alpn_protocols().to_vec(),
            min_version(cfg.minimum_version()),
        )
        .map_err(|e| {
            let what = match e {
                CtxError::Setup => return openssl_error(),
                CtxError::Certificate => "OpenSSL cannot use a certificate of the chain",
                CtxError::Key => "OpenSSL cannot read the private key",
                CtxError::Mismatch => "the private key does not match the leaf certificate",
            };
            let (_, detail) = sys::error_string();
            Error::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{what}: {detail}"),
            ))
        })?;
        Ok(ServerCred(ctx))
    }
}

pub(crate) struct Session {
    ssl: sys::Ssl,
    /// Network bytes read but not yet given to OpenSSL.
    recv: Vec<u8>,
    closed: bool,
    version: Option<Version>,
    alpn: Option<Vec<u8>>,
    /// Keeps a server's context (and its ALPN callback data) alive for the session.
    _cred: Option<Arc<ServerCred>>,
}

fn openssl_error() -> Error {
    let (code, message) = sys::error_string();
    Error::Handshake {
        code: code as i64,
        message,
    }
}

fn min_version(v: Version) -> core::ffi::c_long {
    match v {
        Version::Tls10 => sys::TLS1_VERSION,
        Version::Tls11 => sys::TLS1_1_VERSION,
        Version::Tls12 => sys::TLS1_2_VERSION,
        Version::Tls13 => sys::TLS1_3_VERSION,
    }
}

/// Runs `SSL_do_handshake` until it completes, moving flights between the BIOs and `s`.
fn handshake<S: Read + Write>(
    ssl: &mut sys::Ssl,
    recv: &mut Vec<u8>,
    s: &mut S,
) -> Result<(), Error> {
    loop {
        match ssl.handshake() {
            Ok(()) => return Ok(flush_out(ssl, s)?),
            Err(SSL_ERROR_WANT_READ) => {
                flush_out(ssl, s)?;
                if !feed_record(ssl, recv, s)? {
                    return Err(Error::Io(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "connection closed during the TLS handshake",
                    )));
                }
            }
            Err(SSL_ERROR_WANT_WRITE) => flush_out(ssl, s)?,
            Err(_) => {
                // Send any alert, then report the most specific cause.
                let _ = flush_out(ssl, s);
                // c_long is i32 on 32-bit targets, so the conversion is not always a no-op.
                #[allow(clippy::useless_conversion)]
                let verify = i64::from(ssl.verify_result());
                if let Some(e) = verify_error(verify) {
                    return Err(e);
                }
                return Err(openssl_error());
            }
        }
    }
}

fn verify_error(code: i64) -> Option<Error> {
    match code {
        0 => None,
        2 | 18 | 19 | 20 | 21 => Some(Error::UntrustedCertificate),
        62 | 64 => Some(Error::NameMismatch),
        9 | 10 => Some(Error::ExpiredCertificate),
        c => Some(Error::Handshake {
            code: c,
            message: format!("certificate verify error {c}"),
        }),
    }
}

fn io_err(e: Error) -> io::Error {
    match e {
        Error::Io(e) => e,
        other => io::Error::new(io::ErrorKind::InvalidData, other),
    }
}

fn flush_out<S: Write>(ssl: &mut sys::Ssl, s: &mut S) -> io::Result<()> {
    let out = ssl.drain();
    if out.is_empty() {
        return Ok(());
    }
    s.write_all(&out)?;
    s.flush()
}

/// Gives OpenSSL the next whole record, reading from the stream as needed. Returns false
/// at a clean EOF before any byte of a new record.
fn feed_record<S: Read>(ssl: &mut sys::Ssl, recv: &mut Vec<u8>, s: &mut S) -> io::Result<bool> {
    loop {
        match frame(recv) {
            Frame::Complete(n) => {
                if !ssl.feed(&recv[..n]) {
                    return Err(io::Error::other("OpenSSL refused input"));
                }
                recv.drain(..n);
                return Ok(true);
            }
            Frame::Malformed => {
                // Let OpenSSL see it and report its own protocol error.
                let all = core::mem::take(recv);
                if !ssl.feed(&all) {
                    return Err(io::Error::other("OpenSSL refused input"));
                }
                return Ok(true);
            }
            Frame::Incomplete => {
                let start = recv.len();
                recv.resize(start + READ_CHUNK, 0);
                let n = loop {
                    match s.read(&mut recv[start..]) {
                        Ok(n) => break n,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            recv.truncate(start);
                            return Err(e);
                        }
                    }
                };
                recv.truncate(start + n);
                if n == 0 {
                    if start == 0 {
                        return Ok(false);
                    }
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "connection closed in the middle of a TLS record",
                    ));
                }
            }
        }
    }
}

impl Session {
    pub(crate) fn connect<S: Read + Write>(cfg: &Config, s: &mut S) -> Result<Session, Error> {
        let name = cfg.server_name();
        if name.contains('\0') || (name.is_empty() && cfg.verify_mode() == Verify::Full) {
            return Err(Error::Io(alpn::invalid("invalid TLS server name")));
        }
        let alpn_wire = alpn::encode_wire(cfg.alpn_protocols())?;
        if !sys::available() {
            return Err(Error::Unsupported(NO_LIBSSL));
        }
        let is_ip = name
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok();
        let host = name.trim_matches(['[', ']']);
        let verify = cfg.verify_mode() == Verify::Full;
        let min = min_version(cfg.minimum_version());
        let mut ssl =
            sys::Ssl::new(host, is_ip, verify, &alpn_wire, min).ok_or_else(openssl_error)?;
        let mut recv = Vec::new();
        handshake(&mut ssl, &mut recv, s)?;
        Ok(Session::established(ssl, recv, None))
    }

    /// Runs the server handshake. `recv` holds what the client already sent; OpenSSL's ALPN
    /// callback makes the same choice as `server::read_client_hello`, so `_alpn` is unused.
    pub(crate) fn accept<S: Read + Write>(
        cred: &Arc<ServerCred>,
        s: &mut S,
        mut recv: Vec<u8>,
        _alpn: &[Vec<u8>],
    ) -> Result<Session, Error> {
        let mut ssl = sys::Ssl::server(&cred.0).ok_or_else(openssl_error)?;
        handshake(&mut ssl, &mut recv, s)?;
        Ok(Session::established(ssl, recv, Some(Arc::clone(cred))))
    }

    fn established(ssl: sys::Ssl, recv: Vec<u8>, cred: Option<Arc<ServerCred>>) -> Session {
        let version = match ssl.version() {
            0x304 => Some(Version::Tls13),
            0x303 => Some(Version::Tls12),
            0x302 => Some(Version::Tls11),
            0x301 => Some(Version::Tls10),
            _ => None,
        };
        let alpn = ssl.alpn_selected();
        Session {
            ssl,
            recv,
            closed: false,
            version,
            alpn,
            _cred: cred,
        }
    }

    pub(crate) fn read<S: Read + Write>(&mut self, s: &mut S, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.closed {
            return Ok(0);
        }
        loop {
            match self.ssl.read(buf) {
                Ok(n) => return Ok(n),
                Err(SSL_ERROR_ZERO_RETURN) => {
                    self.closed = true;
                    return Ok(0);
                }
                Err(SSL_ERROR_WANT_READ) => {
                    // Post-handshake messages (TLS 1.3 tickets, key updates) may need a reply.
                    flush_out(&mut self.ssl, s)?;
                    if !feed_record(&mut self.ssl, &mut self.recv, s)? {
                        return Ok(0);
                    }
                }
                Err(SSL_ERROR_WANT_WRITE) => flush_out(&mut self.ssl, s)?,
                Err(_) => return Err(io_err(openssl_error())),
            }
        }
    }

    pub(crate) fn write<S: Read + Write>(&mut self, s: &mut S, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match self.ssl.write(buf) {
            Ok(n) => {
                let out = self.ssl.drain();
                s.write_all(&out)?;
                Ok(n)
            }
            Err(_) => Err(io_err(openssl_error())),
        }
    }

    pub(crate) fn shutdown<S: Read + Write>(&mut self, s: &mut S) -> io::Result<()> {
        self.ssl.shutdown();
        flush_out(&mut self.ssl, s)
    }

    pub(crate) fn take_leftover(&mut self) -> Vec<u8> {
        debug_assert!(!self.ssl.input_pending());
        core::mem::take(&mut self.recv)
    }

    pub(crate) fn version(&self) -> Option<Version> {
        self.version
    }

    pub(crate) fn alpn_protocol(&self) -> Option<&[u8]> {
        self.alpn.as_deref()
    }

    pub(crate) fn peer_certificate_der(&self) -> Option<Vec<u8>> {
        self.ssl.peer_certificate_der()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_verify_codes() {
        assert!(verify_error(0).is_none());
        assert!(matches!(
            verify_error(19),
            Some(Error::UntrustedCertificate)
        ));
        assert!(matches!(verify_error(62), Some(Error::NameMismatch)));
        assert!(matches!(verify_error(10), Some(Error::ExpiredCertificate)));
        assert!(matches!(
            verify_error(7),
            Some(Error::Handshake { code: 7, .. })
        ));
    }
}
