//! A tiny HTTP/1.1 server on a local socket for the uploader and hub tests.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// One request the server received.
#[derive(Debug, Clone, Default)]
pub struct Recorded {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }

    pub fn query(&self) -> &str {
        self.target.split_once('?').map(|(_, q)| q).unwrap_or("")
    }

    pub fn json(&self) -> dl_json::Value {
        dl_json::from_str::<dl_json::Value>(&String::from_utf8_lossy(&self.body)).unwrap()
    }
}

/// The answer to one request.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Reply {
        Reply {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn json(status: u16, v: &dl_json::Value) -> Reply {
        Reply::new(status, dl_json::to_string(v)).with("content-type", "application/json")
    }

    pub fn with(mut self, k: &str, v: &str) -> Reply {
        self.headers.push((k.to_owned(), v.to_owned()));
        self
    }
}

type Handler = Arc<dyn Fn(&Recorded) -> Reply + Send + Sync>;

/// The server; stops when dropped.
pub struct Server {
    pub addr: SocketAddr,
    log: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<AtomicBool>,
}

impl Server {
    pub fn start(handler: impl Fn(&Recorded) -> Reply + Send + Sync + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Handler = Arc::new(handler);
        let (log2, stop2) = (log.clone(), stop.clone());
        thread::spawn(move || {
            for conn in listener.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                let (h, l) = (handler.clone(), log2.clone());
                thread::spawn(move || serve(conn, &h, &l));
            }
        });
        Server { addr, log, stop }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.log.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
    }
}

fn serve(conn: TcpStream, handler: &Handler, log: &Mutex<Vec<Recorded>>) {
    conn.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut out = conn.try_clone().unwrap();
    let mut r = BufReader::new(conn);
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let mut req = Recorded {
            method: parts.next().unwrap_or("").to_owned(),
            target: parts.next().unwrap_or("").to_owned(),
            ..Recorded::default()
        };
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                req.headers.push((k.trim().to_owned(), v.trim().to_owned()));
            }
        }
        let len: usize = req
            .header("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        req.body = vec![0; len];
        if r.read_exact(&mut req.body).is_err() {
            return;
        }
        let reply = handler(&req);
        log.lock().unwrap().push(req);
        let mut head = format!(
            "HTTP/1.1 {} X\r\ncontent-length: {}\r\n",
            reply.status,
            reply.body.len()
        );
        for (k, v) in &reply.headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        if out
            .write_all(head.as_bytes())
            .and_then(|()| out.write_all(&reply.body))
            .is_err()
        {
            return;
        }
    }
}

/// A fresh empty directory under the system temp dir.
pub fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dl-agent-backup-{tag}-{}", dl_uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// An address nothing listens on.
pub fn dead_url() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let a = l.local_addr().unwrap();
    drop(l);
    format!("http://{a}")
}
