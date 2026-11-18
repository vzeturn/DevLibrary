//! An in-process hub for the tests: a tiny HTTP responder for negotiate and the
//! WebSocket upgrade on 127.0.0.1, with dl-ws doing only the server-side framing.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dl_json::Value;
use dl_ws::{Config, Message, Role, WebSocket};

use crate::protocol::{self, HubMessage, RecordReader};

/// A request the hub received.
#[derive(Debug, Clone, Default)]
pub struct Seen {
    pub method: String,
    /// Path and query.
    pub target: String,
    pub headers: Vec<(String, String)>,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Answers negotiate requests: `(request, hub base URL) -> (status, body)`.
pub type Negotiator = dyn Fn(&Seen, &str) -> (u16, String) + Send + Sync;
/// Runs one WebSocket session.
pub type Session = dyn Fn(Peer) + Send + Sync;

pub struct FakeHub {
    /// `http://127.0.0.1:port`.
    pub base: String,
    pub requests: Arc<Mutex<Vec<Seen>>>,
    /// Every message the sessions received from clients, as text, in order.
    pub received: Arc<Mutex<Vec<String>>>,
}

impl FakeHub {
    pub fn requests(&self) -> Vec<Seen> {
        self.requests.lock().unwrap().clone()
    }

    pub fn received(&self) -> Vec<String> {
        self.received.lock().unwrap().clone()
    }
}

/// The standard negotiate answer (version 1, WebSockets offered).
pub fn negotiate_ok(_: &Seen, _: &str) -> (u16, String) {
    (
        200,
        r#"{"negotiateVersion":1,"connectionId":"cid-1","connectionToken":"tok/en+1","availableTransports":[{"transport":"WebSockets","transferFormats":["Text","Binary"]},{"transport":"LongPolling","transferFormats":["Text","Binary"]}]}"#.to_owned(),
    )
}

fn read_head(s: &mut TcpStream) -> Option<Seen> {
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match s.read(&mut b) {
            Ok(1) => head.push(b[0]),
            _ => return None,
        }
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let target = first.next()?.to_owned();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.to_owned(), v.trim().to_owned()))
        .collect();
    Some(Seen {
        method,
        target,
        headers,
    })
}

fn respond(s: &mut TcpStream, status: u16, body: &str) {
    let _ = write!(
        s,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

pub fn start(
    negotiator: impl Fn(&Seen, &str) -> (u16, String) + Send + Sync + 'static,
    session: impl Fn(Peer) + Send + Sync + 'static,
) -> FakeHub {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let received = Arc::new(Mutex::new(Vec::new()));
    let hub = FakeHub {
        base: base.clone(),
        requests: requests.clone(),
        received: received.clone(),
    };
    let negotiator: Arc<Negotiator> = Arc::new(negotiator);
    let session: Arc<Session> = Arc::new(session);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let (requests, received) = (requests.clone(), received.clone());
            let (negotiator, session, base) = (negotiator.clone(), session.clone(), base.clone());
            thread::spawn(move || {
                // Ends sessions that a test forgot about.
                let _ = s.set_read_timeout(Some(Duration::from_secs(20)));
                let Some(req) = read_head(&mut s) else { return };
                requests.lock().unwrap().push(req.clone());
                if req.method == "POST" {
                    let (status, body) = negotiator(&req, &base);
                    respond(&mut s, status, &body);
                    return;
                }
                if req.header("authorization") == Some("Bearer reject") {
                    return respond(&mut s, 401, "unauthorized");
                }
                let fields: Vec<(&str, &str)> = req
                    .headers
                    .iter()
                    .map(|(n, v)| (n.as_str(), v.as_str()))
                    .collect();
                let accept = match dl_ws::check_request(&req.method, &fields) {
                    Ok(a) => a,
                    Err(e) => return respond(&mut s, 400, &e.to_string()),
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                );
                session(Peer {
                    ws: WebSocket::from_raw(s, Role::Server, Config::default()),
                    records: RecordReader::new(1 << 20),
                    received,
                });
            });
        }
    });
    hub
}

/// The server side of one connection.
pub struct Peer {
    pub ws: WebSocket<TcpStream>,
    records: RecordReader,
    received: Arc<Mutex<Vec<String>>>,
}

impl Peer {
    /// The next message from the client as text; `None` when the client is gone.
    pub fn recv_raw(&mut self) -> Option<String> {
        loop {
            if let Some(r) = self.records.next_record() {
                let text = String::from_utf8_lossy(r).into_owned();
                self.received.lock().unwrap().push(text.clone());
                return Some(text);
            }
            match self.ws.read() {
                Ok(Message::Text(t)) => self.records.push(t.as_bytes()).ok()?,
                Ok(Message::Binary(b)) => self.records.push(&b).ok()?,
                Ok(Message::Ping(_) | Message::Pong(_)) => {}
                Ok(Message::Close(_)) | Err(_) => return None,
            }
        }
    }

    /// The next message from the client, parsed.
    pub fn recv(&mut self) -> Option<HubMessage> {
        let raw = self.recv_raw()?;
        protocol::parse(raw.as_bytes()).ok()
    }

    pub fn send_raw(&mut self, text: &str) {
        let _ = self.ws.send_text(text);
    }

    pub fn send(&mut self, m: &HubMessage) {
        self.send_raw(&protocol::encode(m));
    }

    pub fn complete(&mut self, id: &str, result: Value) {
        self.send(&HubMessage::Completion {
            invocation_id: id.to_owned(),
            result: Some(result),
            error: None,
        });
    }

    /// Reads the handshake request and accepts it.
    pub fn accept_handshake(&mut self) -> Option<()> {
        self.recv_raw()?;
        self.send_raw("{}\u{1e}");
        Some(())
    }
}

/// A hub with a few methods:
/// `Echo(x) -> x`, `Add(a, b) -> a + b`, `Fail()` errors with `boom`, `Slow()` never
/// completes, `Notify(x)` sends `NOTIFY(x)` to the client and then completes,
/// `Stream()` answers with a stream item, `AskClient()` sends a client-result
/// invocation; non-blocking invocations `Tell(..)` are answered with `told(target,
/// args)`.
pub fn echo_session(mut peer: Peer) {
    if peer.accept_handshake().is_none() {
        return;
    }
    while let Some(m) = peer.recv() {
        let HubMessage::Invocation {
            invocation_id,
            target,
            arguments,
        } = m
        else {
            continue;
        };
        let Some(id) = invocation_id else {
            peer.send(&HubMessage::Invocation {
                invocation_id: None,
                target: "told".into(),
                arguments: vec![target.into(), Value::Array(arguments)],
            });
            continue;
        };
        match target.as_str() {
            "Echo" => peer.complete(&id, arguments.first().cloned().unwrap_or_default()),
            "Add" => {
                let sum: i64 = arguments.iter().filter_map(Value::as_i64).sum();
                peer.complete(&id, sum.into());
            }
            "Fail" => peer.send(&HubMessage::Completion {
                invocation_id: id,
                result: None,
                error: Some("boom".into()),
            }),
            "Slow" => {}
            "Notify" => {
                peer.send(&HubMessage::Invocation {
                    invocation_id: None,
                    target: "NOTIFY".into(),
                    arguments,
                });
                peer.complete(&id, Value::Null);
            }
            "Stream" => peer.send(&HubMessage::StreamItem {
                invocation_id: id,
                item: 1.into(),
            }),
            "AskClient" => {
                peer.send(&HubMessage::Invocation {
                    invocation_id: Some("srv-1".into()),
                    target: "GetValue".into(),
                    arguments: vec![],
                });
                peer.complete(&id, Value::Null);
            }
            _ => peer.send(&HubMessage::Completion {
                invocation_id: id,
                result: None,
                error: Some(format!("Unknown hub method '{target}'")),
            }),
        }
    }
}
