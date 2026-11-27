//! TLS client and server sessions over SChannel (Windows).

use std::io::{self, Read, Write};
use std::sync::Arc;

use crate::schannel_server_sys::{self as certs, Cert, StoreError, TempKey};
use crate::schannel_sys::{self as sys, Input};
use crate::{Config, Error, ServerConfig, Verify, Version, alpn};

/// Bytes requested from the stream per read.
const READ_CHUNK: usize = 18 * 1024;
/// A handshake flight or a record larger than this is refused.
const MAX_BUFFER: usize = 1 << 20;
/// Records encrypted into one `write_all`.
const RECORDS_PER_WRITE: usize = 4;

/// A server's SChannel credentials, built once per [`ServerConfig`] and shared by its
/// sessions.
pub(crate) struct ServerCred {
    // Fields drop in declaration order: the credentials before the certificate and the key.
    cred: sys::Cred,
    _cert: Cert,
    /// The temporary persisted key, when SChannel refused an ephemeral one.
    key: Option<TempKey>,
}

impl ServerCred {
    pub(crate) fn new(cfg: &ServerConfig) -> Result<ServerCred, Error> {
        let chain = cfg.cert_chain_der();
        let (leaf, intermediates) = chain
            .split_first()
            .ok_or_else(|| Error::Io(alpn::invalid("the certificate chain is empty")))?;
        certs::add_intermediates(intermediates).map_err(|e| match e {
            StoreError::Certificate => Error::Io(alpn::invalid(
                "Windows cannot parse an intermediate certificate",
            )),
            StoreError::Store(code) => Error::Io(io::Error::other(format!(
                "cannot add the intermediate certificates to the CA store (error {code:#x})"
            ))),
        })?;
        let enabled = server_protocols(cfg.minimum_version());
        let pkcs8 = cfg.private_key().pkcs8_der();
        let new_leaf = || {
            Cert::from_der(leaf)
                .ok_or_else(|| Error::Io(alpn::invalid("Windows cannot parse the certificate")))
        };
        // An ephemeral key never touches the disk. SChannel does its key operations in LSASS
        // and may refuse a key handle of this process; a temporary persisted key, deleted with
        // the credentials, is the fallback.
        let cert = new_leaf()?;
        cert.attach_ephemeral_key(pkcs8).map_err(key_error)?;
        if let Ok(cred) = acquire_server(&cert, enabled) {
            return Ok(ServerCred {
                cred,
                _cert: cert,
                key: None,
            });
        }
        let cert = new_leaf()?;
        let key = cert.attach_persisted_key(pkcs8).map_err(key_error)?;
        let cred = acquire_server(&cert, enabled).map_err(map_status)?;
        Ok(ServerCred {
            cred,
            _cert: cert,
            key: Some(key),
        })
    }

    /// Whether the private key is an ephemeral CNG key (otherwise a temporary persisted one).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn ephemeral_key(&self) -> bool {
        self.key.is_none()
    }
}

fn key_error(code: i32) -> Error {
    Error::Io(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "Windows cannot import the private key (error {:#x})",
            code as u32
        ),
    ))
}

/// Modern credentials (TLS 1.3) first, then the legacy structure.
fn acquire_server(cert: &Cert, enabled: u32) -> Result<sys::Cred, i32> {
    sys::acquire_server(cert, true, enabled).or_else(|_| sys::acquire_server(cert, false, enabled))
}

/// The local end of a session.
enum Peer {
    Client {
        cred: sys::Cred,
        target: Vec<u16>,
    },
    Server {
        cred: Arc<ServerCred>,
        /// `SEC_APPLICATION_PROTOCOLS` buffer for the first step; empty for no ALPN.
        alpn: Vec<u8>,
    },
}

impl Peer {
    /// One handshake step with bytes received from the other side.
    fn step(&mut self, ctx: &mut Option<sys::Ctx>, token: &[u8]) -> sys::Step {
        match self {
            Peer::Client { cred, target } => {
                sys::initialize(cred, ctx, target, Input::Token(token))
            }
            Peer::Server { cred, alpn } => {
                let alpn = (!alpn.is_empty()).then_some(&alpn[..]);
                sys::accept(&cred.cred, ctx, Some(token), alpn)
            }
        }
    }

    /// The step that builds close_notify after `apply_shutdown`.
    fn close_step(&mut self, ctx: &mut Option<sys::Ctx>) -> sys::Step {
        match self {
            Peer::Client { cred, target } => sys::initialize(cred, ctx, target, Input::None),
            Peer::Server { cred, .. } => sys::accept(&cred.cred, ctx, None, None),
        }
    }
}

pub(crate) struct Session {
    // Declared before `peer`: the context must be deleted before its credentials.
    ctx: Option<sys::Ctx>,
    peer: Peer,
    sizes: sys::StreamSizes,
    /// Encrypted bytes read from the stream and not yet decrypted.
    recv: Vec<u8>,
    /// Decrypted bytes not yet returned by `read`.
    plain: Vec<u8>,
    plain_pos: usize,
    closed: bool,
    version: Option<Version>,
    alpn: Option<Vec<u8>>,
    peer_cert: Option<Vec<u8>>,
}

pub(crate) fn map_status(st: i32) -> Error {
    let code = st as u32;
    match code {
        0x8009_0325 | 0x800B_0109 | 0x800B_010A | 0x800B_0112 => Error::UntrustedCertificate,
        0x8009_0322 | 0x800B_010F => Error::NameMismatch,
        0x8009_0328 | 0x800B_0101 => Error::ExpiredCertificate,
        _ => Error::Handshake {
            code: i64::from(code),
            message: describe(code).to_owned(),
        },
    }
}

fn describe(code: u32) -> &'static str {
    match code {
        0x8009_0326 => "the peer rejected the handshake (illegal message)",
        0x8009_0327 => "unknown or unsupported certificate",
        0x8009_0330 => "the decryption failed",
        0x8009_0331 => "no common TLS version or cipher suite (SEC_E_ALGORITHM_MISMATCH)",
        0x8009_0308 => "invalid token from the peer (SEC_E_INVALID_TOKEN)",
        0x8009_030D => "the credentials were not recognized (SEC_E_UNKNOWN_CREDENTIALS)",
        0x8009_030E => "no credentials (SEC_E_NO_CREDENTIALS)",
        0x8009_0304 => "internal error in the security package",
        0x8009_0302 => "the function is not supported (SEC_E_UNSUPPORTED_FUNCTION)",
        0x8009_0349 => "the certificate chain could not be built (SEC_E_CERT_UNKNOWN)",
        _ => "SChannel error",
    }
}

fn io_err(e: Error) -> io::Error {
    match e {
        Error::Io(e) => e,
        other => io::Error::new(io::ErrorKind::InvalidData, other),
    }
}

fn write_flight<S: Write>(s: &mut S, bytes: &[u8]) -> io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    s.write_all(bytes)?;
    s.flush()
}

/// Reads at least one more byte into `buf`. EOF is an error during the handshake.
fn read_more<S: Read>(s: &mut S, buf: &mut Vec<u8>) -> io::Result<usize> {
    if buf.len() >= MAX_BUFFER {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "TLS message too large",
        ));
    }
    let start = buf.len();
    buf.resize(start + READ_CHUNK, 0);
    loop {
        match s.read(&mut buf[start..]) {
            Ok(n) => {
                buf.truncate(start + n);
                return Ok(n);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                buf.truncate(start);
                return Err(e);
            }
        }
    }
}

fn keep_tail(buf: &mut Vec<u8>, extra: usize) {
    let extra = extra.min(buf.len());
    buf.drain(..buf.len() - extra);
}

/// Runs handshake steps with `recv` until SChannel reports `SEC_E_OK`.
fn negotiate<S: Read + Write>(
    peer: &mut Peer,
    ctx: &mut Option<sys::Ctx>,
    s: &mut S,
    recv: &mut Vec<u8>,
    mut need_read: bool,
) -> Result<(), Error> {
    let mut incomplete_creds = 0;
    loop {
        if need_read && read_more(s, recv)? == 0 {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed during the TLS handshake",
            )));
        }
        let step = peer.step(ctx, recv);
        match step.status {
            sys::SEC_E_OK => {
                write_flight(s, &step.token)?;
                keep_tail(recv, step.extra);
                return Ok(());
            }
            sys::SEC_I_CONTINUE_NEEDED => {
                write_flight(s, &step.token)?;
                keep_tail(recv, step.extra);
                need_read = step.extra == 0;
            }
            sys::SEC_E_INCOMPLETE_MESSAGE => need_read = true,
            sys::SEC_I_INCOMPLETE_CREDENTIALS => {
                // The server asked for a client certificate; continue without one.
                incomplete_creds += 1;
                if incomplete_creds > 4 {
                    return Err(map_status(step.status));
                }
                need_read = false;
            }
            st => {
                // With EXTENDED_ERROR the token may carry an alert for the peer.
                let _ = write_flight(s, &step.token);
                return Err(map_status(st));
            }
        }
    }
}

/// `SP_PROT_*` bits (client or server, from `bits`) for `min` up to TLS 1.3.
fn protocols(min: Version, bits: [u32; 4]) -> u32 {
    [
        Version::Tls10,
        Version::Tls11,
        Version::Tls12,
        Version::Tls13,
    ]
    .into_iter()
    .zip(bits)
    .filter(|&(v, _)| v >= min)
    .fold(0, |acc, (_, bit)| acc | bit)
}

fn enabled_protocols(min: Version) -> u32 {
    protocols(
        min,
        [
            sys::SP_PROT_TLS1_0_CLIENT,
            sys::SP_PROT_TLS1_1_CLIENT,
            sys::SP_PROT_TLS1_2_CLIENT,
            sys::SP_PROT_TLS1_3_CLIENT,
        ],
    )
}

fn server_protocols(min: Version) -> u32 {
    protocols(
        min,
        [
            sys::SP_PROT_TLS1_0_SERVER,
            sys::SP_PROT_TLS1_1_SERVER,
            sys::SP_PROT_TLS1_2_SERVER,
            sys::SP_PROT_TLS1_3_SERVER,
        ],
    )
}

fn negotiated_version(p: Option<u32>) -> Option<Version> {
    let has = |client: u32, server: u32| p.is_some_and(|p| p & (client | server) != 0);
    if has(sys::SP_PROT_TLS1_3_CLIENT, sys::SP_PROT_TLS1_3_SERVER) {
        Some(Version::Tls13)
    } else if has(sys::SP_PROT_TLS1_2_CLIENT, sys::SP_PROT_TLS1_2_SERVER) {
        Some(Version::Tls12)
    } else if has(sys::SP_PROT_TLS1_1_CLIENT, sys::SP_PROT_TLS1_1_SERVER) {
        Some(Version::Tls11)
    } else if has(sys::SP_PROT_TLS1_0_CLIENT, sys::SP_PROT_TLS1_0_SERVER) {
        Some(Version::Tls10)
    } else {
        None
    }
}

impl Session {
    pub(crate) fn connect<S: Read + Write>(cfg: &Config, s: &mut S) -> Result<Session, Error> {
        if cfg.server_name().contains('\0')
            || (cfg.server_name().is_empty() && cfg.verify_mode() == Verify::Full)
        {
            return Err(Error::Io(alpn::invalid("invalid TLS server name")));
        }
        let target: Vec<u16> = cfg
            .server_name()
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        let alpn_buf = if cfg.alpn_protocols().is_empty() {
            None
        } else {
            Some(alpn::schannel_buffer(cfg.alpn_protocols())?)
        };
        let verify = cfg.verify_mode() == Verify::Full;
        let enabled = enabled_protocols(cfg.minimum_version());
        // Modern credentials (TLS 1.3) first; fall back to the legacy structure on Windows
        // versions that reject it. Nothing has been sent when the fallback happens.
        let (cred, mut ctx, first) = match Self::start(verify, true, enabled, &target, &alpn_buf) {
            Ok(v) => v,
            Err(_) => Self::start(verify, false, enabled, &target, &alpn_buf)?,
        };
        write_flight(s, &first)?;
        let mut recv = Vec::new();
        let mut peer = Peer::Client { cred, target };
        negotiate(&mut peer, &mut ctx, s, &mut recv, true)?;
        Session::established(ctx, peer, recv, alpn_buf.is_some())
    }

    /// Acquires credentials and makes the first handshake step (the ClientHello).
    fn start(
        verify: bool,
        modern: bool,
        enabled: u32,
        target: &[u16],
        alpn_buf: &Option<Vec<u8>>,
    ) -> Result<(sys::Cred, Option<sys::Ctx>, Vec<u8>), Error> {
        let mut cred = sys::acquire(verify, modern, enabled).map_err(map_status)?;
        let mut ctx = None;
        let input = match alpn_buf {
            Some(b) => Input::Alpn(b),
            None => Input::None,
        };
        let step = sys::initialize(&mut cred, &mut ctx, target, input);
        if step.status != sys::SEC_I_CONTINUE_NEEDED || step.token.is_empty() {
            return Err(map_status(step.status));
        }
        Ok((cred, ctx, step.token))
    }

    /// Runs the server handshake. `recv` holds what the client already sent (its
    /// ClientHello); `alpn` lists the protocols SChannel may select.
    pub(crate) fn accept<S: Read + Write>(
        cred: &Arc<ServerCred>,
        s: &mut S,
        mut recv: Vec<u8>,
        alpn: &[Vec<u8>],
    ) -> Result<Session, Error> {
        let alpn_buf = if alpn.is_empty() {
            Vec::new()
        } else {
            alpn::schannel_buffer(alpn)?
        };
        let offered = !alpn_buf.is_empty();
        let mut peer = Peer::Server {
            cred: Arc::clone(cred),
            alpn: alpn_buf,
        };
        let mut ctx = None;
        let need_read = recv.is_empty();
        negotiate(&mut peer, &mut ctx, s, &mut recv, need_read)?;
        Session::established(ctx, peer, recv, offered)
    }

    fn established(
        ctx: Option<sys::Ctx>,
        peer: Peer,
        recv: Vec<u8>,
        alpn_offered: bool,
    ) -> Result<Session, Error> {
        let mut ctx = ctx.ok_or(Error::Handshake {
            code: 0,
            message: "SChannel returned no security context".to_owned(),
        })?;
        let sizes = sys::stream_sizes(&mut ctx).map_err(map_status)?;
        let version = negotiated_version(sys::protocol(&mut ctx));
        let alpn = if alpn_offered {
            sys::application_protocol(&mut ctx)
        } else {
            None
        };
        let peer_cert = match peer {
            Peer::Client { .. } => sys::remote_cert_der(&mut ctx),
            // No client certificates are requested.
            Peer::Server { .. } => None,
        };
        Ok(Session {
            ctx: Some(ctx),
            peer,
            sizes,
            recv,
            plain: Vec::new(),
            plain_pos: 0,
            closed: false,
            version,
            alpn,
            peer_cert,
        })
    }

    pub(crate) fn read<S: Read + Write>(&mut self, s: &mut S, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.plain_pos < self.plain.len() {
                let n = buf.len().min(self.plain.len() - self.plain_pos);
                buf[..n].copy_from_slice(&self.plain[self.plain_pos..self.plain_pos + n]);
                self.plain_pos += n;
                if self.plain_pos == self.plain.len() {
                    self.plain.clear();
                    self.plain_pos = 0;
                }
                return Ok(n);
            }
            if self.closed || buf.is_empty() {
                return Ok(0);
            }
            if self.recv.is_empty() {
                if read_more(s, &mut self.recv)? == 0 {
                    return Ok(0);
                }
                continue;
            }
            let d = sys::decrypt(ctx_mut(&mut self.ctx)?, &mut self.recv);
            match d.status {
                sys::SEC_E_OK => {
                    self.plain.extend_from_slice(&self.recv[d.plain]);
                    keep_tail(&mut self.recv, d.extra);
                }
                sys::SEC_E_INCOMPLETE_MESSAGE => {
                    if read_more(s, &mut self.recv)? == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "connection closed in the middle of a TLS record",
                        ));
                    }
                }
                sys::SEC_I_RENEGOTIATE => {
                    // TLS 1.3 post-handshake messages (NewSessionTicket, KeyUpdate): hand the
                    // remaining bytes back to the handshake machinery.
                    self.plain.extend_from_slice(&self.recv[d.plain]);
                    keep_tail(&mut self.recv, d.extra);
                    let need_read = self.recv.is_empty();
                    negotiate(&mut self.peer, &mut self.ctx, s, &mut self.recv, need_read)
                        .map_err(io_err)?;
                }
                sys::SEC_I_CONTEXT_EXPIRED => {
                    // close_notify from the peer.
                    self.closed = true;
                    keep_tail(&mut self.recv, d.extra);
                }
                st => return Err(io_err(map_status(st))),
            }
        }
    }

    pub(crate) fn write<S: Read + Write>(&mut self, s: &mut S, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let max = (self.sizes.maximum_message as usize).max(1);
        let take = buf.len().min(max * RECORDS_PER_WRITE);
        let mut wire = Vec::with_capacity(take + RECORDS_PER_WRITE * 64);
        for chunk in buf[..take].chunks(max) {
            let rec = sys::encrypt(ctx_mut(&mut self.ctx)?, &self.sizes, chunk)
                .map_err(|st| io_err(map_status(st)))?;
            wire.extend_from_slice(&rec);
        }
        s.write_all(&wire)?;
        Ok(take)
    }

    pub(crate) fn shutdown<S: Read + Write>(&mut self, s: &mut S) -> io::Result<()> {
        let st = sys::apply_shutdown(ctx_mut(&mut self.ctx)?);
        if st != sys::SEC_E_OK {
            return Err(io_err(map_status(st)));
        }
        let step = self.peer.close_step(&mut self.ctx);
        if step.status == sys::SEC_E_OK || step.status == sys::SEC_I_CONTEXT_EXPIRED {
            write_flight(s, &step.token)
        } else {
            Err(io_err(map_status(step.status)))
        }
    }

    pub(crate) fn take_leftover(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.recv)
    }

    pub(crate) fn version(&self) -> Option<Version> {
        self.version
    }

    pub(crate) fn alpn_protocol(&self) -> Option<&[u8]> {
        self.alpn.as_deref()
    }

    pub(crate) fn peer_certificate_der(&self) -> Option<Vec<u8>> {
        self.peer_cert.clone()
    }
}

fn ctx_mut(ctx: &mut Option<sys::Ctx>) -> io::Result<&mut sys::Ctx> {
    ctx.as_mut()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "TLS session is closed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PrivateKey;

    #[test]
    fn protocol_bits() {
        assert_eq!(server_protocols(Version::Tls12), 0x400 | 0x1000);
        assert_eq!(server_protocols(Version::Tls13), 0x1000);
        assert_eq!(enabled_protocols(Version::Tls12), 0x800 | 0x2000);
        assert_eq!(negotiated_version(Some(0x1000)), Some(Version::Tls13));
        assert_eq!(negotiated_version(Some(0x800)), Some(Version::Tls12));
        assert_eq!(negotiated_version(None), None);
    }

    #[test]
    fn server_credential_key_form() {
        let key = PrivateKey::from_pkcs8_der(include_bytes!("../testdata/ec-key.p8.der")).unwrap();
        let cfg = ServerConfig::new(
            vec![include_bytes!("../testdata/ec-cert.der").to_vec()],
            key,
        );
        let cred = ServerCred::new(&cfg).expect("server credentials");
        // Evidence for the report (run with --nocapture): which key form SChannel took.
        eprintln!(
            "note: SChannel server credentials use {} key",
            if cred.ephemeral_key() {
                "an ephemeral CNG"
            } else {
                "a temporary persisted CNG"
            }
        );
    }
}
