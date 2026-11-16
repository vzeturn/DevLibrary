//! TLS client session over SChannel (Windows).

use std::io::{self, Read, Write};

use crate::schannel_sys::{self as sys, Input};
use crate::{Config, Error, Verify, Version, alpn};

/// Bytes requested from the stream per read.
const READ_CHUNK: usize = 18 * 1024;
/// A handshake flight or a record larger than this is refused.
const MAX_BUFFER: usize = 1 << 20;
/// Records encrypted into one `write_all`.
const RECORDS_PER_WRITE: usize = 4;

pub(crate) struct Session {
    // Declared before `cred`: the context must be deleted before its credentials.
    ctx: Option<sys::Ctx>,
    cred: sys::Cred,
    target: Vec<u16>,
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
        0x8009_0326 => "the server rejected the handshake (illegal message)",
        0x8009_0327 => "unknown or unsupported certificate",
        0x8009_0330 => "the decryption failed",
        0x8009_0331 => "no common TLS version or cipher suite (SEC_E_ALGORITHM_MISMATCH)",
        0x8009_0308 => "invalid token from the server (SEC_E_INVALID_TOKEN)",
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

/// Runs handshake steps with `input` until SChannel reports `SEC_E_OK`.
fn negotiate<S: Read + Write>(
    cred: &mut sys::Cred,
    ctx: &mut Option<sys::Ctx>,
    target: &[u16],
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
        let step = sys::initialize(cred, ctx, target, Input::Token(recv));
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
                // With EXTENDED_ERROR the token may carry an alert for the server.
                let _ = write_flight(s, &step.token);
                return Err(map_status(st));
            }
        }
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
        // Modern credentials (TLS 1.3) first; fall back to the legacy structure on Windows
        // versions that reject it. Nothing has been sent when the fallback happens.
        let (mut cred, mut ctx, first) = match Self::start(verify, true, &target, &alpn_buf) {
            Ok(v) => v,
            Err(_) => Self::start(verify, false, &target, &alpn_buf)?,
        };
        write_flight(s, &first)?;
        let mut recv = Vec::new();
        negotiate(&mut cred, &mut ctx, &target, s, &mut recv, true)?;
        let mut ctx = ctx.ok_or(Error::Handshake {
            code: 0,
            message: "SChannel returned no security context".to_owned(),
        })?;
        let sizes = sys::stream_sizes(&mut ctx).map_err(map_status)?;
        let version = match sys::protocol(&mut ctx) {
            Some(p) if p & sys::SP_PROT_TLS1_3_CLIENT != 0 => Some(Version::Tls13),
            Some(p) if p & sys::SP_PROT_TLS1_2_CLIENT != 0 => Some(Version::Tls12),
            _ => None,
        };
        let alpn = if alpn_buf.is_some() {
            sys::application_protocol(&mut ctx)
        } else {
            None
        };
        let peer_cert = sys::remote_cert_der(&mut ctx);
        Ok(Session {
            ctx: Some(ctx),
            cred,
            target,
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

    /// Acquires credentials and makes the first handshake step (the ClientHello).
    fn start(
        verify: bool,
        modern: bool,
        target: &[u16],
        alpn_buf: &Option<Vec<u8>>,
    ) -> Result<(sys::Cred, Option<sys::Ctx>, Vec<u8>), Error> {
        let mut cred = sys::acquire(verify, modern).map_err(map_status)?;
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
                    negotiate(
                        &mut self.cred,
                        &mut self.ctx,
                        &self.target,
                        s,
                        &mut self.recv,
                        need_read,
                    )
                    .map_err(io_err)?;
                }
                sys::SEC_I_CONTEXT_EXPIRED => {
                    // close_notify from the server.
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
        let step = sys::initialize(&mut self.cred, &mut self.ctx, &self.target, Input::None);

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
