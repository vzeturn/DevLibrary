//! A scripted TDS server on loopback for tests of the connection path: PRELOGIN encryption
//! answers, a TLS handshake that never completes (as an engine that cannot speak the offered
//! TLS versions, see [`TlsPeer`]), plain LOGIN7 with SSPI round trips, a LOGINACK with the
//! TDS and product version of the emulated server (SQL Server 2008 answers TDS 7.3), and
//! scripted token streams (then `DONE`) for every later request.
//!
//! Enabled by the `fake-server` feature; not for production use.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

const PRELOGIN: u8 = 0x12;
const LOGIN7: u8 = 0x10;
const SSPI: u8 = 0x11;
const REPLY: u8 = 0x04;
const ENCRYPT_ON: u8 = 1;
const ENCRYPT_NOT_SUP: u8 = 2;

/// TDS 7.4 (SQL Server 2012 and later), the default LOGINACK version.
pub const TDS_74: u32 = 0x7400_0004;
/// TDS 7.3B, the LOGINACK version of SQL Server 2008 R2.
pub const TDS_73B: u32 = 0x730B_0003;
/// TDS 7.3A, the LOGINACK version of SQL Server 2008.
pub const TDS_73A: u32 = 0x730A_0003;

/// How the server behaves when the client asks for encryption (and
/// [`Script::never_encrypt`] is off). None of them completes a handshake: the fake has no
/// TLS server, so a login over TLS is checked against a real server
/// (`DL_TDS_TEST_TLS10_URL`, `tests/legacy_tls.rs`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TlsPeer {
    /// Read the ClientHello, then close the connection.
    #[default]
    DropAfterHello,
    /// Look at the ClientHello without reading it and close with it unread, so the client
    /// sees a TCP reset in the middle of the handshake, as SQL Server 2008 (R2) without the
    /// TLS 1.2 update answers a hello it cannot handle.
    ResetOnHello,
    /// A TLS 1.0-only engine: a ClientHello that does not offer TLS 1.0 is answered with a
    /// TLS 1.0 ServerHello (the client must refuse it and alert); one that offers TLS 1.0
    /// gets the reset of [`TlsPeer::ResetOnHello`] (SQL Server 2008 R2 RTM resets the
    /// connection around the TLS 1.0 login).
    Tls10Only,
}

/// How the server answers.
#[derive(Clone, Debug, Default)]
pub struct Script {
    /// SSPI tokens sent one by one after LOGIN7; each answer the client sends is recorded.
    pub sspi_challenges: Vec<Vec<u8>>,
    /// Answer `NOT_SUP` even when the client asks for encryption (an engine without TLS).
    pub never_encrypt: bool,
    /// What happens when the client asks for encryption.
    pub tls: TlsPeer,
    /// TDS version in LOGINACK; `0` means [`TDS_74`].
    pub tds_version: u32,
    /// Product version `(major, minor, build)` in the PRELOGIN answer and LOGINACK;
    /// `None` means SQL Server 2022 RTM-ish `16.0.4000`.
    pub server_version: Option<(u8, u8, u16)>,
    /// Token streams answered to the requests after login, in order; `DONE` afterwards.
    pub responses: Vec<Vec<u8>>,
}

/// What one client connection did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attempt {
    /// The `ENCRYPTION` byte of the client's PRELOGIN.
    pub client_encryption: u8,
    /// The `INSTOPT` instance name of the client's PRELOGIN (empty for the default
    /// instance).
    pub instance: String,
    /// The alert description the client sent after a [`TlsPeer::Tls10Only`] ServerHello
    /// (`70` = protocol_version).
    pub tls_alert: Option<u8>,
    /// The TDS version the client's LOGIN7 asked for.
    pub login_tds_version: u32,
    /// TLS versions the client's ClientHello offered (`0x0303` = TLS 1.2), lowest last;
    /// empty when no TLS handshake was attempted.
    pub tls_versions: Vec<u16>,
    /// The LOGIN7 user name (empty for integrated login).
    pub user: String,
    /// The SSPI token in LOGIN7 followed by each SSPI message the client sent.
    pub sspi: Vec<Vec<u8>>,
    /// LOGINACK was sent.
    pub logged_in: bool,
    /// Requests after login (SQL batches, RPCs) answered with `DONE`.
    pub requests: usize,
}

/// A running fake server; connections are served one at a time on a background thread.
pub struct FakeServer {
    addr: SocketAddr,
    attempts: Arc<Mutex<Vec<Attempt>>>,
}

impl FakeServer {
    /// Starts listening on `127.0.0.1` on a free port.
    ///
    /// # Errors
    /// The listener cannot be bound.
    pub fn start(script: Script) -> io::Result<FakeServer> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&attempts);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let rec = match log.lock() {
                    Ok(mut l) => {
                        l.push(Attempt::default());
                        Recorder {
                            log: Arc::clone(&log),
                            at: l.len() - 1,
                        }
                    }
                    Err(_) => return,
                };
                // A client that drops mid-way is part of what the tests exercise.
                let _ = serve(stream, &script, &rec);
            }
        });
        Ok(FakeServer { addr, attempts })
    }

    /// The listening address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The connections served so far, in order.
    pub fn attempts(&self) -> Vec<Attempt> {
        self.attempts.lock().map(|a| a.clone()).unwrap_or_default()
    }
}

/// Publishes each step of a connection as it happens, so a test sees a session that is
/// still open.
struct Recorder {
    log: Arc<Mutex<Vec<Attempt>>>,
    at: usize,
}

impl Recorder {
    fn update(&self, f: impl FnOnce(&mut Attempt)) {
        if let Ok(mut l) = self.log.lock()
            && let Some(a) = l.get_mut(self.at)
        {
            f(a);
        }
    }
}

fn read_message(s: &mut TcpStream) -> io::Result<(u8, Vec<u8>)> {
    let mut payload = Vec::new();
    loop {
        let mut h = [0u8; 8];
        s.read_exact(&mut h)?;
        let len = usize::from(u16::from_be_bytes([h[2], h[3]]));
        if len < 8 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "short packet"));
        }
        let start = payload.len();
        payload.resize(start + len - 8, 0);
        s.read_exact(&mut payload[start..])?;
        if h[1] & 0x01 != 0 {
            return Ok((h[0], payload));
        }
    }
}

fn send(s: &mut TcpStream, ptype: u8, payload: &[u8]) -> io::Result<()> {
    let len = u16::try_from(payload.len() + 8)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "fake reply too large"))?;
    let mut p = vec![ptype, 0x01];
    p.extend_from_slice(&len.to_be_bytes());
    p.extend_from_slice(&[0, 0, 1, 0]);
    p.extend_from_slice(payload);
    s.write_all(&p)?;
    s.flush()
}

fn prelogin_option(msg: &[u8], wanted: u8) -> Option<&[u8]> {
    let mut pos = 0;
    while let Some(&token) = msg.get(pos) {
        if token == 0xFF {
            return None;
        }
        let h = msg.get(pos..pos + 5)?;
        let off = usize::from(u16::from_be_bytes([h[1], h[2]]));
        let len = usize::from(u16::from_be_bytes([h[3], h[4]]));
        if token == wanted {
            return msg.get(off..off + len);
        }
        pos += 5;
    }
    None
}

fn prelogin_reply(encryption: u8, (major, minor, build): (u8, u8, u16)) -> Vec<u8> {
    // VERSION, ENCRYPTION, terminator; data at offset 11.
    let [b1, b2] = build.to_be_bytes();
    vec![
        0x00, 0, 11, 0, 6, 0x01, 0, 17, 0, 1, 0xFF, major, minor, b1, b2, 0, 0, encryption,
    ]
}

/// A TLS 1.0 ServerHello record (TLS_RSA_WITH_AES_128_CBC_SHA, no extensions).
fn tls10_server_hello() -> Vec<u8> {
    let mut body = vec![0x03, 0x01];
    body.extend_from_slice(&[0x5A; 32]); // random
    body.extend_from_slice(&[0, 0x00, 0x2F, 0]); // no session id, suite, no compression
    let mut hs = vec![0x02, 0, 0, body.len() as u8];
    hs.extend_from_slice(&body);
    let mut rec = vec![0x16, 0x03, 0x01, 0, hs.len() as u8];
    rec.extend_from_slice(&hs);
    rec
}

/// The description byte of a TLS alert record, if `rec` is one.
fn alert_description(rec: &[u8]) -> Option<u8> {
    (rec.first() == Some(&0x15)).then(|| rec.get(6).copied())?
}

/// TLS versions in a ClientHello: the `supported_versions` extension, else the legacy
/// version field.
pub fn client_hello_versions(record: &[u8]) -> Vec<u16> {
    let hello = |r: &[u8]| -> Option<Vec<u16>> {
        // Record header (5) + handshake header (4).
        if r.first() != Some(&0x16) || r.get(5) != Some(&0x01) {
            return None;
        }
        let legacy = u16::from_be_bytes([*r.get(9)?, *r.get(10)?]);
        let mut p = 11 + 32;
        p += 1 + usize::from(*r.get(p)?);
        p += 2 + usize::from(u16::from_be_bytes([*r.get(p)?, *r.get(p + 1)?]));
        p += 1 + usize::from(*r.get(p)?);
        let end = p + 2 + usize::from(u16::from_be_bytes([*r.get(p)?, *r.get(p + 1)?]));
        p += 2;
        while p + 4 <= end.min(r.len()) {
            let kind = u16::from_be_bytes([r[p], r[p + 1]]);
            let len = usize::from(u16::from_be_bytes([r[p + 2], r[p + 3]]));
            let data = r.get(p + 4..p + 4 + len)?;
            if kind == 0x002B {
                let n = usize::from(*data.first()?);
                return Some(
                    data.get(1..1 + n)?
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_be_bytes(*c))
                        .filter(|v| v & 0x0F0F != 0x0A0A) // GREASE
                        .collect(),
                );
            }
            p += 4 + len;
        }
        Some(vec![legacy])
    };
    hello(record).unwrap_or_default()
}

fn utf16(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    String::from_utf16_lossy(&units)
}

fn field(login: &[u8], at: usize) -> Option<(usize, usize)> {
    let off = usize::from(u16::from_le_bytes([*login.get(at)?, *login.get(at + 1)?]));
    let len = usize::from(u16::from_le_bytes([
        *login.get(at + 2)?,
        *login.get(at + 3)?,
    ]));
    Some((off, len))
}

fn serve(mut s: TcpStream, script: &Script, rec: &Recorder) -> io::Result<()> {
    let (t, msg) = read_message(&mut s)?;
    if t != PRELOGIN {
        return Ok(());
    }
    let enc = prelogin_option(&msg, 0x01)
        .and_then(|v| v.first().copied())
        .unwrap_or(0);
    let instance = prelogin_option(&msg, 0x02)
        .map(|v| String::from_utf8_lossy(v.split(|&b| b == 0).next().unwrap_or(&[])).into_owned())
        .unwrap_or_default();
    rec.update(|a| {
        a.client_encryption = enc;
        a.instance = instance;
    });
    let version = script.server_version.unwrap_or((16, 0, 4000));
    if enc != ENCRYPT_NOT_SUP && !script.never_encrypt {
        // Offer encryption, then fail the handshake the way `script.tls` says.
        send(&mut s, REPLY, &prelogin_reply(ENCRYPT_ON, version))?;
        return refuse_tls(s, script.tls, rec);
    }
    send(&mut s, REPLY, &prelogin_reply(ENCRYPT_NOT_SUP, version))?;
    let (t, login) = read_message(&mut s)?;
    if t != LOGIN7 {
        return Ok(());
    }
    if let Some(v) = login.get(4..8) {
        let v = u32::from_le_bytes([v[0], v[1], v[2], v[3]]);
        rec.update(|a| a.login_tds_version = v);
    }
    if let Some((off, chars)) = field(&login, 40) {
        let user = login
            .get(off..off + chars * 2)
            .map(utf16)
            .unwrap_or_default();
        rec.update(|a| a.user = user);
    }
    if let Some((off, len)) = field(&login, 78)
        && len > 0
    {
        let tok = login.get(off..off + len).unwrap_or_default().to_vec();
        rec.update(|a| a.sspi.push(tok));
    }
    for challenge in &script.sspi_challenges {
        let mut tok = vec![0xED];
        tok.extend_from_slice(&(challenge.len() as u16).to_le_bytes());
        tok.extend_from_slice(challenge);
        send(&mut s, REPLY, &tok)?;
        let (t, answer) = read_message(&mut s)?;
        if t != SSPI {
            return Ok(());
        }
        rec.update(|a| a.sspi.push(answer));
    }
    let mut ack = vec![0xAD];
    let name: Vec<u8> = "Microsoft SQL Server"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let body_len = 1 + 4 + 1 + name.len() + 4;
    ack.extend_from_slice(&(body_len as u16).to_le_bytes());
    let tds = if script.tds_version == 0 {
        TDS_74
    } else {
        script.tds_version
    };
    ack.push(1);
    ack.extend_from_slice(&tds.to_be_bytes());
    ack.push(20);
    ack.extend_from_slice(&name);
    let (major, minor, build) = version;
    ack.extend_from_slice(&[major, minor]);
    ack.extend_from_slice(&build.to_be_bytes());
    ack.extend_from_slice(&done());
    // Recorded first: a client that returns at LOGINACK must already see it.
    rec.update(|a| a.logged_in = true);
    send(&mut s, REPLY, &ack)?;
    let mut answers = script.responses.iter();
    while read_message(&mut s).is_ok() {
        rec.update(|a| a.requests += 1);
        match answers.next() {
            Some(r) => send(&mut s, REPLY, r)?,
            None => send(&mut s, REPLY, &done())?,
        }
    }
    Ok(())
}

/// Ends a TLS handshake the client started, as `peer` says.
fn refuse_tls(mut s: TcpStream, peer: TlsPeer, rec: &Recorder) -> io::Result<()> {
    if peer == TlsPeer::DropAfterHello {
        let (_, hello) = read_message(&mut s)?;
        rec.update(|a| a.tls_versions = client_hello_versions(&hello));
        return Ok(());
    }
    let hello = peek_packet(&s)?;
    let offered = client_hello_versions(&hello);
    let tls10 = offered.contains(&0x0301);
    rec.update(|a| a.tls_versions = offered);
    if peer == TlsPeer::Tls10Only && !tls10 {
        read_message(&mut s)?;
        send(&mut s, PRELOGIN, &tls10_server_hello())?;
        if let Ok((_, alert)) = read_message(&mut s) {
            let d = alert_description(&alert);
            rec.update(|a| a.tls_alert = d);
        }
        return Ok(());
    }
    // Returning closes the socket with the hello unread: the kernel answers with RST.
    Ok(())
}

/// The payload of the first packet waiting on `s`, without consuming it.
fn peek_packet(s: &TcpStream) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; 64 * 1024];
    for _ in 0..500 {
        let n = s.peek(&mut buf)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if n >= 8 {
            let len = usize::from(u16::from_be_bytes([buf[2], buf[3]]));
            if len >= 8 && n >= len {
                return Ok(buf[8..len].to_vec());
            }
        }
        thread::sleep(std::time::Duration::from_millis(10));
    }
    Err(io::ErrorKind::TimedOut.into())
}

fn done() -> Vec<u8> {
    let mut d = vec![0xFD, 0, 0, 0xC1, 0];
    d.extend_from_slice(&0u64.to_le_bytes());
    d
}
