//! Tiny HTTP/1.1 test server and CONNECT proxy built on `std::net`.
//!
//! The parsing here is deliberately independent of dl-http so the tests do not check
//! the crate against itself.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// A request as received by the test server.
#[derive(Debug, Clone)]
pub struct Received {
    pub line: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Received {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn target(&self) -> &str {
        self.line.split(' ').nth(1).unwrap_or("")
    }

    pub fn method(&self) -> &str {
        self.line.split(' ').next().unwrap_or("")
    }
}

/// One accepted connection.
pub struct Peer {
    pub stream: TcpStream,
    buf: Vec<u8>,
    pub log: Arc<Mutex<Vec<Received>>>,
}

impl Peer {
    /// Reads one request (head plus `Content-Length` body). `None` at end of stream.
    pub fn read_request(&mut self) -> Option<Received> {
        let head_end = loop {
            if let Some(i) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) | Err(_) => return None,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
            }
        };
        let head = String::from_utf8_lossy(&self.buf[..head_end - 4]).into_owned();
        self.buf.drain(..head_end);
        let mut lines = head.split("\r\n");
        let line = lines.next().unwrap_or("").to_owned();
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(n, v)| (n.to_owned(), v.trim().to_owned()))
            .collect();
        let len: usize = headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0);
        while self.buf.len() < len {
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) | Err(_) => return None,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
            }
        }
        let body: Vec<u8> = self.buf.drain(..len).collect();
        let r = Received {
            line,
            headers,
            body,
        };
        self.log.lock().unwrap().push(r.clone());
        Some(r)
    }

    pub fn write(&mut self, bytes: &[u8]) {
        let _ = self.stream.write_all(bytes);
        let _ = self.stream.flush();
    }

    /// Sends a response with `Content-Length`.
    pub fn respond(&mut self, status: &str, extra_headers: &str, body: &[u8]) {
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra_headers}\r\n",
            body.len()
        );
        let mut all = head.into_bytes();
        all.extend_from_slice(body);
        self.write(&all);
    }

    pub fn close(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// A server that runs `handler` on its own thread for every accepted connection.
pub struct Server {
    pub addr: SocketAddr,
    pub accepted: Arc<AtomicUsize>,
    pub log: Arc<Mutex<Vec<Received>>>,
}

impl Server {
    /// `handler(peer, connection_index)`.
    pub fn start<F>(handler: F) -> Server
    where
        F: Fn(&mut Peer, usize) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let log = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(handler);
        let (acc, lg) = (accepted.clone(), log.clone());
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let index = acc.fetch_add(1, Ordering::SeqCst);
                let handler = handler.clone();
                let log = lg.clone();
                thread::spawn(move || {
                    let mut peer = Peer {
                        stream,
                        buf: Vec::new(),
                        log,
                    };
                    handler(&mut peer, index);
                });
            }
        });
        Server {
            addr,
            accepted,
            log,
        }
    }

    /// A server answering every request with `200 OK` and `body`, keeping connections open.
    pub fn ok(body: &'static str) -> Server {
        Server::start(move |peer, _| {
            while peer.read_request().is_some() {
                peer.respond("200 OK", "", body.as_bytes());
            }
        })
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    pub fn requests(&self) -> Vec<Received> {
        self.log.lock().unwrap().clone()
    }
}

/// A forwarding proxy that handles `CONNECT host:port` and records each CONNECT request.
/// When `credentials` is set (`"Basic ..."`), requests without that
/// `Proxy-Authorization` value get `407`.
pub struct ConnectProxy {
    pub addr: SocketAddr,
    pub log: Arc<Mutex<Vec<Received>>>,
}

impl ConnectProxy {
    pub fn start(credentials: Option<&'static str>) -> ConnectProxy {
        let server = Server::start(move |peer, _| {
            let Some(req) = peer.read_request() else {
                return;
            };
            if req.method() != "CONNECT" {
                peer.respond("405 Method Not Allowed", "", b"");
                return;
            }
            if let Some(expected) = credentials
                && req.header("proxy-authorization") != Some(expected)
            {
                peer.respond(
                    "407 Proxy Authentication Required",
                    "Proxy-Authenticate: Basic realm=\"test\"\r\n",
                    b"",
                );
                return;
            }
            let Ok(upstream) = TcpStream::connect(req.target()) else {
                peer.respond("502 Bad Gateway", "", b"");
                return;
            };
            peer.write(b"HTTP/1.1 200 Connection established\r\n\r\n");
            pipe(peer.stream.try_clone().unwrap(), upstream);
        });
        ConnectProxy {
            addr: server.addr,
            log: server.log,
        }
    }

    pub fn url(&self, userinfo: &str) -> String {
        format!("http://{userinfo}{}", self.addr)
    }

    pub fn requests(&self) -> Vec<Received> {
        self.log.lock().unwrap().clone()
    }
}

/// Copies bytes both ways until either side closes.
fn pipe(client: TcpStream, upstream: TcpStream) {
    let (mut c_read, mut u_write) = (client.try_clone().unwrap(), upstream.try_clone().unwrap());
    let up = thread::spawn(move || {
        let _ = std::io::copy(&mut c_read, &mut u_write);
        let _ = u_write.shutdown(Shutdown::Write);
    });
    let (mut u_read, mut c_write) = (upstream, client);
    let _ = std::io::copy(&mut u_read, &mut c_write);
    let _ = c_write.shutdown(Shutdown::Both);
    let _ = up.join();
}

/// Sleeps briefly so a peer can observe a close.
pub fn settle() {
    thread::sleep(Duration::from_millis(50));
}
