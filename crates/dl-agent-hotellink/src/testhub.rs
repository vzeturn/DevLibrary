//! A fake HL hub on a local socket for the tests: records every request and answers through
//! a handler. One request per connection (`Connection: close`).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// One recorded request.
#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    /// Path with query.
    pub path: String,
    /// Header names in lower case.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Req {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }
}

type Handler = dyn Fn(&Req) -> (u16, String) + Send + Sync;

/// The fake hub.
pub struct FakeHub {
    pub url: String,
    pub reqs: Arc<Mutex<Vec<Req>>>,
}

impl FakeHub {
    pub fn start(handler: impl Fn(&Req) -> (u16, String) + Send + Sync + 'static) -> FakeHub {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", l.local_addr().expect("addr"));
        let reqs = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let log = reqs.clone();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(s) = s else { continue };
                let (h, log) = (handler.clone(), log.clone());
                std::thread::spawn(move || serve(s, &*h, &log));
            }
        });
        FakeHub { url, reqs }
    }

    /// The recorded requests whose path starts with `prefix`.
    pub fn hits(&self, prefix: &str) -> Vec<Req> {
        self.reqs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|r| r.path.starts_with(prefix))
            .cloned()
            .collect()
    }
}

/// A URL nobody listens on (connection refused).
pub fn dead_url() -> String {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", l.local_addr().expect("addr"));
    drop(l);
    url
}

fn serve(mut s: std::net::TcpStream, h: &Handler, log: &Mutex<Vec<Req>>) {
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    let head_end = loop {
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        match s.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(k) => raw.extend_from_slice(&buf[..k]),
        }
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_owned();
    let path = first.next().unwrap_or("").to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let len = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = raw[head_end..].to_vec();
    while body.len() < len {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(k) => body.extend_from_slice(&buf[..k]),
        }
    }
    let req = Req {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    log.lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(req.clone());
    let (code, out) = h(&req);
    let _ = write!(
        s,
        "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
        out.len()
    );
}
