//! Handshakes with a server that only speaks TLS 1.0, such as SQL Server 2008 without the
//! TLS 1.2 update. Runs when `DL_TLS_TEST_TLS10_ADDR` is `host:port` of such a server,
//! for example `openssl s_server -accept 4433 -tls1 -cipher DEFAULT:@SECLEVEL=0 -www`;
//! skipped otherwise.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use dl_tls::{Config, Error, TlsStream, Verify, Version};

fn addr() -> Option<String> {
    let a = std::env::var("DL_TLS_TEST_TLS10_ADDR")
        .ok()
        .filter(|a| !a.is_empty());
    if a.is_none() {
        eprintln!("note: skipping, DL_TLS_TEST_TLS10_ADDR is not set");
    }
    a
}

fn tcp(addr: &str) -> TcpStream {
    let s = TcpStream::connect(addr).unwrap_or_else(|e| panic!("connect {addr}: {e}"));
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
    s
}

fn cfg() -> Config {
    Config::new("localhost").verify(Verify::None)
}

#[test]
fn default_minimum_refuses_tls10_server() {
    let Some(a) = addr() else { return };
    match TlsStream::connect(&cfg(), tcp(&a)) {
        Ok(t) => panic!("TLS 1.2 minimum negotiated {:?}", t.version()),
        Err(Error::Unsupported(why)) => eprintln!("note: skipping, no TLS backend: {why}"),
        // The server's protocol_version alert (or a plain close) ends the handshake.
        Err(e) => eprintln!("refused as expected: {e}"),
    }
}

#[test]
fn tls10_minimum_connects_and_moves_data() {
    let Some(a) = addr() else { return };
    let mut t = match TlsStream::connect(&cfg().min_version(Version::Tls10), tcp(&a)) {
        Ok(t) => t,
        Err(Error::Unsupported(why)) => {
            eprintln!("note: skipping, no TLS backend: {why}");
            return;
        }
        Err(e) => panic!("TLS 1.0 handshake: {e}"),
    };
    assert_eq!(t.version(), Some(Version::Tls10));
    // `s_server -www` answers any request with a status page.
    t.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
    let mut out = Vec::new();
    let _ = t.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("HTTP/1.0 200"), "{text}");
}

#[test]
fn tls11_minimum_still_refuses_tls10_only_server() {
    let Some(a) = addr() else { return };
    match TlsStream::connect(&cfg().min_version(Version::Tls11), tcp(&a)) {
        Ok(t) => panic!("TLS 1.1 minimum negotiated {:?}", t.version()),
        Err(e) => eprintln!("refused as expected: {e}"),
    }
}
