//! Server mode end to end on loopback: dl-tls accepts and the peer is the dl-tls client or,
//! as independent references, `curl` and `openssl s_client` when they are installed.
//! Skipped where the platform has no TLS backend. The certificates and keys come from
//! `testdata/gen.sh` and are test-only.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dl_tls::{Config, Error, KeyAlgorithm, PrivateKey, ServerConfig, TlsStream, Verify, Version};

const EC_CERT: &[u8] = include_bytes!("../testdata/ec-cert.der");
const EC_KEY: &[u8] = include_bytes!("../testdata/ec-key.p8.der");
const RSA_CERT: &[u8] = include_bytes!("../testdata/rsa-cert.der");
const RSA_KEY: &[u8] = include_bytes!("../testdata/rsa-key.p8.der");
const LEAF_CERT: &[u8] = include_bytes!("../testdata/leaf-cert.der");
const LEAF_KEY: &[u8] = include_bytes!("../testdata/leaf-key.p8.der");
const INTER_CERT: &[u8] = include_bytes!("../testdata/inter-cert.der");
const ROOT_PEM: &str = include_str!("../testdata/root-cert.pem");

type Accepted = Result<TlsStream<TcpStream>, Error>;

fn key(der: &[u8]) -> PrivateKey {
    PrivateKey::from_pkcs8_der(der).expect("test key")
}

fn ec() -> ServerConfig {
    ServerConfig::new(vec![EC_CERT.to_vec()], key(EC_KEY))
}

/// Leaf and intermediate; the root is only known to the reference clients.
fn chain() -> ServerConfig {
    ServerConfig::new(vec![LEAF_CERT.to_vec(), INTER_CERT.to_vec()], key(LEAF_KEY))
}

/// Whether this platform can accept at all (no backend on macOS and wasm, or no libssl).
fn supported() -> bool {
    match TlsStream::accept(&ec(), io::empty()) {
        Err(Error::Unsupported(why)) => {
            eprintln!("note: skipping, no TLS backend: {why}");
            false
        }
        _ => true,
    }
}

fn have(tool: &str, arg: &str) -> bool {
    let ok = Command::new(tool)
        .arg(arg)
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("note: skipping, {tool} is not installed");
    }
    ok
}

fn timeouts(s: &TcpStream) {
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(20))).unwrap();
}

/// Accepts `n` connections on a loopback port, each handed to `f` on its own thread.
fn serve_n<T: Send + 'static>(
    n: usize,
    cfg: ServerConfig,
    f: impl Fn(Accepted) -> T + Send + Sync + Clone + 'static,
) -> (u16, JoinHandle<Vec<T>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    l.set_nonblocking(true).unwrap();
    let h = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut workers = Vec::new();
        while workers.len() < n {
            match l.accept() {
                Ok((s, _)) => {
                    s.set_nonblocking(false).unwrap();
                    timeouts(&s);
                    let (cfg, f) = (cfg.clone(), f.clone());
                    workers.push(thread::spawn(move || f(TlsStream::accept(&cfg, s))));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "no client connected");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("accept: {e}"),
            }
        }
        workers.into_iter().map(|w| w.join().unwrap()).collect()
    });
    (port, h)
}

fn serve<T: Send + 'static>(
    cfg: ServerConfig,
    f: impl Fn(Accepted) -> T + Send + Sync + Clone + 'static,
) -> (u16, JoinHandle<T>) {
    let (port, h) = serve_n(1, cfg, f);
    let one = thread::spawn(move || h.join().unwrap().pop().unwrap());
    (port, one)
}

fn tcp(port: u16) -> TcpStream {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    timeouts(&s);
    s
}

fn client(name: &str) -> Config {
    Config::new(name).verify(Verify::None)
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// What a server saw of one connection.
#[derive(Debug, PartialEq)]
struct Seen {
    version: Option<Version>,
    alpn: Option<Vec<u8>>,
    server_name: Option<String>,
}

fn seen(t: &TlsStream<TcpStream>) -> Seen {
    Seen {
        version: t.version(),
        alpn: t.alpn_protocol().map(<[u8]>::to_vec),
        server_name: t.server_name().map(str::to_owned),
    }
}

/// Reads "hello", answers "world", then closes with close_notify.
fn hello_world(t: Accepted) -> Seen {
    let mut t = t.expect("accept");
    let mut got = [0u8; 5];
    t.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"hello");
    t.write_all(b"world").unwrap();
    t.flush().unwrap();
    t.shutdown().unwrap();
    seen(&t)
}

/// The client half of [`hello_world`].
fn say_hello(cfg: &Config, port: u16) -> TlsStream<TcpStream> {
    let mut c = TlsStream::connect(cfg, tcp(port)).expect("connect");
    c.write_all(b"hello").unwrap();
    c.flush().unwrap();
    let mut got = Vec::new();
    c.read_to_end(&mut got).unwrap();
    assert_eq!(got, b"world");
    c
}

#[test]
fn dl_tls_client_and_server_move_data_both_ways() {
    if !supported() {
        return;
    }
    const LEN: usize = 1 << 20;
    let (port, server) = serve(ec().alpn(&[b"http/1.1"]), |t| {
        let mut t = t.expect("accept");
        assert_eq!(t.peer_certificate_der(), None);
        let mut got = vec![0; LEN];
        t.read_exact(&mut got).unwrap();
        assert!(got == pattern(LEN), "the server received other bytes");
        t.write_all(&pattern(LEN)).unwrap();
        t.flush().unwrap();
        t.shutdown().unwrap();
        seen(&t)
    });
    let cfg = client("localhost").alpn(&[b"h2", b"http/1.1"]);
    let mut c = TlsStream::connect(&cfg, tcp(port)).expect("connect");
    assert_eq!(c.alpn_protocol(), Some(&b"http/1.1"[..]));
    assert_eq!(c.server_name(), None);
    assert_eq!(c.peer_certificate_der().as_deref(), Some(EC_CERT));
    c.write_all(&pattern(LEN)).unwrap();
    c.flush().unwrap();
    let mut back = vec![0; LEN];
    c.read_exact(&mut back).unwrap();
    assert!(back == pattern(LEN), "the client received other bytes");
    // close_notify ends the stream cleanly.
    let mut rest = Vec::new();
    assert_eq!(c.read_to_end(&mut rest).unwrap(), 0);
    let s = server.join().unwrap();
    assert!(s.version >= Some(Version::Tls12), "{s:?}");
    assert_eq!(s.version, c.version());
    assert_eq!(s.alpn.as_deref(), Some(&b"http/1.1"[..]));
    assert_eq!(s.server_name.as_deref(), Some("localhost"));
}

#[test]
fn rsa_key_serves() {
    if !supported() {
        return;
    }
    let k = key(RSA_KEY);
    assert_eq!(k.algorithm(), KeyAlgorithm::Rsa);
    let (port, server) = serve(ServerConfig::new(vec![RSA_CERT.to_vec()], k), hello_world);
    let c = say_hello(&client("localhost"), port);
    assert_eq!(c.peer_certificate_der().as_deref(), Some(RSA_CERT));
    let s = server.join().unwrap();
    assert!(s.version >= Some(Version::Tls12), "{s:?}");
}

#[test]
fn tls13_minimum_negotiates_tls13() {
    if !supported() {
        return;
    }
    let (port, server) = serve(ec().min_version(Version::Tls13), hello_world);
    let c = say_hello(&client("localhost"), port);
    assert_eq!(c.version(), Some(Version::Tls13));
    assert_eq!(server.join().unwrap().version, Some(Version::Tls13));
}

#[test]
fn no_sni_and_no_alpn_when_the_client_sends_none() {
    if !supported() {
        return;
    }
    let (port, server) = serve(ec().alpn(&[b"http/1.1"]), hello_world);
    // An empty name with Verify::None sends no server_name extension; no ALPN offered.
    let c = say_hello(&client(""), port);
    assert_eq!(c.alpn_protocol(), None);
    let s = server.join().unwrap();
    assert_eq!(s.server_name, None);
    assert_eq!(s.alpn, None);
}

#[test]
fn alpn_without_overlap_is_refused() {
    if !supported() {
        return;
    }
    let (port, server) = serve(ec().alpn(&[b"http/1.1"]), |t| t.map(|_| ()));
    let c = TlsStream::connect(&client("localhost").alpn(&[b"h2"]), tcp(port));
    assert!(c.is_err(), "the client connected without a common protocol");
    match server.join().unwrap() {
        Err(Error::Handshake { code: 120, .. }) => {}
        other => panic!("expected no_application_protocol, got {other:?}"),
    }
}

#[test]
fn garbage_and_early_close_are_errors() {
    if !supported() {
        return;
    }
    // Plain HTTP on the TLS port.
    let (port, server) = serve(ec(), |t| t.map(|_| ()));
    let mut s = tcp(port);
    s.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut sink = Vec::new();
    let _ = s.read_to_end(&mut sink);
    let r = server.join().unwrap();
    assert!(r.is_err(), "{r:?}");

    // Closed before sending anything, and in the middle of a ClientHello.
    for sent in [
        &b""[..],
        &[0x16, 0x03, 0x01, 0x00, 0xc4, 0x01, 0x00, 0x00, 0xc0, 0x03][..],
    ] {
        let (port, server) = serve(ec(), |t| t.map(|_| ()));
        let mut s = tcp(port);
        s.write_all(sent).unwrap();
        drop(s);
        match server.join().unwrap() {
            Err(Error::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof, "{e}"),
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn unusable_certificate_fails_before_reading() {
    if !supported() {
        return;
    }
    // io::empty() would end the handshake with UnexpectedEof if anything were read.
    let cfg = ServerConfig::new(vec![b"not a certificate".to_vec()], key(EC_KEY));
    match TlsStream::accept(&cfg, io::empty()) {
        Err(Error::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn key_of_another_certificate_fails() {
    if !supported() {
        return;
    }
    // A P-256 key, but not the one EC_CERT certifies. OpenSSL refuses it when the credential
    // is built; SChannel signs with it and the client rejects the handshake.
    let cfg = ServerConfig::new(vec![EC_CERT.to_vec()], key(LEAF_KEY));
    let (port, server) = serve(cfg, |t| t.map(|_| ()));
    let c = TlsStream::connect(&client("localhost"), tcp(port));
    assert!(
        c.is_err(),
        "the client accepted a handshake signed with the wrong key"
    );
    assert!(server.join().unwrap().is_err());
}

#[test]
fn one_config_serves_concurrent_connections() {
    if !supported() {
        return;
    }
    const N: usize = 4;
    let (port, server) = serve_n(N, chain().alpn(&[b"http/1.1"]), hello_world);
    let clients: Vec<_> = (0..N)
        .map(|_| {
            thread::spawn(move || {
                say_hello(&client("localhost").alpn(&[b"http/1.1"]), port).version()
            })
        })
        .collect();
    for c in clients {
        assert!(c.join().unwrap() >= Some(Version::Tls12));
    }
    for s in server.join().unwrap() {
        assert_eq!(s.alpn.as_deref(), Some(&b"http/1.1"[..]));
        assert_eq!(s.server_name.as_deref(), Some("localhost"));
    }
}

/// The test root CA in a temporary PEM file, removed on drop.
struct RootFile(std::path::PathBuf);

impl RootFile {
    fn new(tag: &str) -> RootFile {
        let p =
            std::env::temp_dir().join(format!("dl-tls-test-root-{}-{tag}.pem", std::process::id()));
        std::fs::write(&p, ROOT_PEM).unwrap();
        RootFile(p)
    }

    fn path(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

impl Drop for RootFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Answers one HTTP/1.1 GET with a fixed body.
fn http_once(t: Accepted) -> Result<Seen, Error> {
    let mut t = t?;
    let mut req = Vec::new();
    let mut buf = [0u8; 1024];
    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = t.read(&mut buf)?;
        assert!(
            n > 0,
            "request cut short: {:?}",
            String::from_utf8_lossy(&req)
        );
        req.extend_from_slice(&buf[..n]);
    }
    assert!(
        req.starts_with(b"GET /dl-tls HTTP/1.1\r\n"),
        "{}",
        String::from_utf8_lossy(&req)
    );
    t.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Length: 17\r\nConnection: close\r\n\r\nhello from dl-tls",
    )?;
    t.flush()?;
    t.shutdown()?;
    Ok(seen(&t))
}

/// curl, validating the chain against the test root and the name `localhost`.
fn curl(port: u16, root: &RootFile, extra: &[&str]) -> Output {
    let url = format!("https://localhost:{port}/dl-tls");
    let resolve = format!("localhost:{port}:127.0.0.1");
    let mut cmd = Command::new("curl");
    cmd.args([
        "-sS",
        "--max-time",
        "20",
        "--cacert",
        root.path(),
        "--resolve",
        &resolve,
    ]);
    if cfg!(windows) {
        // Schannel curl checks revocation by default; the test CA publishes no CRL.
        cmd.arg("--ssl-no-revoke");
    }
    cmd.args(extra).arg(&url).output().expect("run curl")
}

#[test]
fn curl_fetches_through_the_chain() {
    if !supported() || !have("curl", "--version") {
        return;
    }
    let root = RootFile::new("curl");
    for (extra, want) in [
        (&[][..], None),
        (&["--tls-max", "1.2"][..], Some(Version::Tls12)),
    ] {
        let (port, server) = serve(chain().alpn(&[b"http/1.1"]), http_once);
        let out = curl(port, &root, extra);
        let seen = server.join().unwrap().expect("accept");
        assert!(
            out.status.success(),
            "curl {extra:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.stdout, b"hello from dl-tls");
        assert_eq!(seen.server_name.as_deref(), Some("localhost"));
        assert_eq!(seen.alpn.as_deref(), Some(&b"http/1.1"[..]));
        match want {
            Some(v) => assert_eq!(seen.version, Some(v)),
            None => assert!(seen.version >= Some(Version::Tls12), "{seen:?}"),
        }
    }
}

#[test]
fn curl_below_the_minimum_is_refused() {
    if !supported() || !have("curl", "--version") {
        return;
    }
    let root = RootFile::new("curl-min");
    let (port, server) = serve(chain().min_version(Version::Tls13), |t| t.map(|t| seen(&t)));
    let out = curl(port, &root, &["--tls-max", "1.2"]);
    assert!(!out.status.success(), "curl connected with TLS 1.2");
    assert!(server.join().unwrap().is_err());
}

#[test]
fn openssl_s_client_verifies_the_chain() {
    if !supported() || !have("openssl", "version") {
        return;
    }
    let root = RootFile::new("s_client");
    let (port, server) = serve(chain().alpn(&[b"http/1.1"]), hello_world);
    let mut child = Command::new("openssl")
        .args(["s_client", "-connect", &format!("127.0.0.1:{port}")])
        .args(["-servername", "localhost", "-verify_hostname", "localhost"])
        .args([
            "-CAfile",
            root.path(),
            "-verify_return_error",
            "-alpn",
            "http/1.1",
        ])
        .arg("-ign_eof")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run openssl");
    child.stdin.take().unwrap().write_all(b"hello").unwrap();
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let s = server.join().unwrap();
    assert!(text.contains("Verify return code: 0 (ok)"), "{text}");
    assert!(text.contains("ALPN protocol: http/1.1"), "{text}");
    assert!(text.contains("world"), "{text}");
    assert_eq!(s.server_name.as_deref(), Some("localhost"));
}
