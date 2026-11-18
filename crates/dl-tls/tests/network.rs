//! Handshakes with real servers. Skipped (with a note) when the network is unreachable,
//! unless `DL_TLS_REQUIRE_NETWORK=1`.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use dl_tls::{Config, Error, TlsStream, Verify, Version};

fn tcp(host: &str) -> Option<TcpStream> {
    let addrs = match (host, 443).to_socket_addrs() {
        Ok(a) => a.collect::<Vec<_>>(),
        Err(e) => return skip(host, &e),
    };
    for a in addrs {
        if let Ok(s) = TcpStream::connect_timeout(&a, Duration::from_secs(10)) {
            s.set_read_timeout(Some(Duration::from_secs(20))).ok()?;
            s.set_write_timeout(Some(Duration::from_secs(20))).ok()?;
            return Some(s);
        }
    }
    skip(host, &io::Error::from(io::ErrorKind::ConnectionRefused))
}

fn skip<T>(host: &str, e: &dyn std::fmt::Display) -> Option<T> {
    if std::env::var("DL_TLS_REQUIRE_NETWORK").as_deref() == Ok("1") {
        panic!("{host} unreachable: {e}");
    }
    eprintln!("note: skipping, {host} unreachable: {e}");
    None
}

fn connect(host: &str, cfg: Config) -> Option<Result<TlsStream<TcpStream>, Error>> {
    match TlsStream::connect(&cfg, tcp(host)?) {
        // macOS, static musl builds and machines without libssl have no backend.
        Err(Error::Unsupported(why)) => {
            eprintln!("note: skipping, no TLS backend: {why}");
            None
        }
        r => Some(r),
    }
}

fn get(tls: &mut TlsStream<TcpStream>, host: &str) -> String {
    let req = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    tls.write_all(req.as_bytes()).unwrap();
    let mut out = Vec::new();
    match tls.read_to_end(&mut out) {
        Ok(_) => {}
        // Some servers close without close_notify after the body.
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof && !out.is_empty() => {}
        Err(e) => panic!("read from {host}: {e}"),
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn example_com_get() {
    let Some(r) = connect("example.com", Config::new("example.com")) else {
        return;
    };
    let mut tls = r.unwrap();
    assert!(tls.version().unwrap() >= Version::Tls12);
    assert!(
        tls.peer_certificate_der()
            .is_some_and(|d| d.len() > 200 && d[0] == 0x30)
    );
    let resp = get(&mut tls, "example.com");
    assert!(
        resp.starts_with("HTTP/1.1 200") || resp.starts_with("HTTP/1.1 30"),
        "{resp:.80}"
    );
}

#[test]
fn google_alpn_http11() {
    let cfg = Config::new("www.google.com").alpn(&[b"http/1.1"]);
    let Some(r) = connect("www.google.com", cfg) else {
        return;
    };
    let mut tls = r.unwrap();
    assert_eq!(tls.alpn_protocol(), Some(&b"http/1.1"[..]));
    let resp = get(&mut tls, "www.google.com");
    assert!(resp.starts_with("HTTP/1.1 "), "{resp:.80}");
}

#[test]
fn cloudflare_version() {
    let Some(r) = connect("www.cloudflare.com", Config::new("www.cloudflare.com")) else {
        return;
    };
    let tls = r.unwrap();
    let v = tls.version().unwrap();
    eprintln!("www.cloudflare.com negotiated {v:?}");
    assert!(v >= Version::Tls12);
}

type Check = fn(&Error) -> bool;

#[test]
fn bad_certificates_are_refused() {
    let cases: [(&str, Check); 3] = [
        ("expired.badssl.com", |e| {
            matches!(e, Error::ExpiredCertificate)
        }),
        ("wrong.host.badssl.com", |e| {
            matches!(e, Error::NameMismatch)
        }),
        ("self-signed.badssl.com", |e| {
            matches!(e, Error::UntrustedCertificate)
        }),
    ];
    for (host, want) in cases {
        let Some(r) = connect(host, Config::new(host)) else {
            return;
        };
        match r {
            Ok(_) => panic!("{host}: handshake succeeded"),
            // SChannel stalls on this expired chain while it fetches revocation data from a
            // dead URL (curl with SChannel behaves the same); the handshake still fails.
            Err(Error::Io(e)) if cfg!(windows) && host.starts_with("expired") => {
                assert!(matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ));
                eprintln!("note: {host} timed out in SChannel validation");
            }
            // badssl.com sometimes resets connections from CI networks before the
            // certificate arrives (seen on GitHub's Ubuntu runners for expired.badssl.com).
            // The handshake still did not succeed, but the certificate check was not
            // reached, so this case says nothing about it: note it and move on.
            Err(Error::Io(e))
                if matches!(
                    e.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::UnexpectedEof
                ) && std::env::var("DL_TLS_REQUIRE_NETWORK").as_deref() != Ok("1") =>
            {
                eprintln!("note: skipping {host}, the server dropped the connection: {e}");
            }
            Err(e) => assert!(want(&e), "{host}: unexpected error {e:?}"),
        }
    }
}

#[test]
fn verify_none_accepts_self_signed() {
    let host = "self-signed.badssl.com";
    let Some(r) = connect(host, Config::new(host).verify(Verify::None)) else {
        return;
    };
    let mut tls = r.unwrap();
    let resp = get(&mut tls, host);
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp:.80}");
}

#[test]
fn into_parts_after_handshake_has_no_leftover() {
    let Some(r) = connect("example.com", Config::new("example.com")) else {
        return;
    };
    let (stream, leftover) = r.unwrap().into_parts();
    assert!(leftover.is_empty(), "{} leftover bytes", leftover.len());
    drop(stream);
}

#[test]
fn shutdown_then_server_closes() {
    let Some(r) = connect("example.com", Config::new("example.com")) else {
        return;
    };
    let mut tls = r.unwrap();
    tls.shutdown().unwrap();
}

#[test]
fn read_timeout_surfaces_as_timeout() {
    let Some(r) = connect("example.com", Config::new("example.com")) else {
        return;
    };
    let mut tls = r.unwrap();
    tls.get_ref()
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let mut buf = [0u8; 64];
    match tls.read(&mut buf) {
        Err(e) => assert!(
            matches!(
                e.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ),
            "{e:?}"
        ),
        // A TLS 1.3 server may send session tickets; those yield no application data,
        // so a read that returns must not return data either.
        Ok(n) => panic!("read returned {n} bytes without a request"),
    }
}

#[test]
fn large_body_round_trip() {
    // A 1 MiB POST body forces many records per write.
    let host = "httpbin.org";
    let Some(r) = connect(host, Config::new(host).alpn(&[b"http/1.1"])) else {
        return;
    };
    let mut tls = r.unwrap();
    let body = vec![b'a'; 1 << 20];
    let head = format!(
        "POST /anything HTTP/1.1\r\nHost: {host}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    tls.write_all(head.as_bytes()).unwrap();
    tls.write_all(&body).unwrap();
    let mut out = Vec::new();
    let _ = tls.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out);
    if text.starts_with("HTTP/1.1 503") || text.starts_with("HTTP/1.1 502") {
        eprintln!("note: httpbin.org is overloaded ({:.40})", text);
        return;
    }
    assert!(text.starts_with("HTTP/1.1 200"), "{text:.120}");
    assert!(out.len() > 1 << 20, "echoed {} bytes", out.len());
}
