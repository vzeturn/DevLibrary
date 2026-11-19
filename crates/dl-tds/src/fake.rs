//! A scripted TDS server on loopback for tests of the connection path: PRELOGIN encryption
//! answers, a TLS handshake that never completes (as an engine that cannot speak the offered
//! TLS versions), plain LOGIN7 with SSPI round trips, and `DONE` for every later request.
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

/// How the server answers.
#[derive(Clone, Debug, Default)]
pub struct Script {
    /// SSPI tokens sent one by one after LOGIN7; each answer the client sends is recorded.
    pub sspi_challenges: Vec<Vec<u8>>,
    /// Answer `NOT_SUP` even when the client asks for encryption (an engine without TLS).
    pub never_encrypt: bool,
}

/// What one client connection did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attempt {
    /// The `ENCRYPTION` byte of the client's PRELOGIN.
    pub client_encryption: u8,
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

fn prelogin_reply(encryption: u8) -> Vec<u8> {
    // VERSION (16.0.4000), ENCRYPTION, terminator; data at offset 11.
    vec![
        0x00, 0, 11, 0, 6, 0x01, 0, 17, 0, 1, 0xFF, 16, 0, 0x0F, 0xA0, 0, 0, encryption,
    ]
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
    rec.update(|a| a.client_encryption = enc);
    if enc != ENCRYPT_NOT_SUP && !script.never_encrypt {
        // Offer encryption, read the ClientHello, then drop: the handshake cannot finish.
        send(&mut s, REPLY, &prelogin_reply(ENCRYPT_ON))?;
        let (_, hello) = read_message(&mut s)?;
        rec.update(|a| a.tls_versions = client_hello_versions(&hello));
        return Ok(());
    }
    send(&mut s, REPLY, &prelogin_reply(ENCRYPT_NOT_SUP))?;
    let (t, login) = read_message(&mut s)?;
    if t != LOGIN7 {
        return Ok(());
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
    ack.extend_from_slice(&[1, 0x74, 0, 0, 4, 20]);
    ack.extend_from_slice(&name);
    ack.extend_from_slice(&[16, 0, 0x0F, 0xA0]);
    ack.extend_from_slice(&done());
    send(&mut s, REPLY, &ack)?;
    rec.update(|a| a.logged_in = true);
    while read_message(&mut s).is_ok() {
        rec.update(|a| a.requests += 1);
        send(&mut s, REPLY, &done())?;
    }
    Ok(())
}

fn done() -> Vec<u8> {
    let mut d = vec![0xFD, 0, 0, 0xC1, 0];
    d.extend_from_slice(&0u64.to_le_bytes());
    d
}
