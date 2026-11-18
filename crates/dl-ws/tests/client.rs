//! Client tests against an in-process WebSocket server on 127.0.0.1. The server parses
//! the upgrade request itself and uses dl-ws only for the server-side framing.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dl_http::{Client, ClientOptions, Proxy};
use dl_ws::{CloseFrame, Config, ConnectOptions, Error, Message, Role, State, WebSocket};

fn client() -> Client {
    Client::with_options(ClientOptions {
        proxy: Proxy::None,
        ..Default::default()
    })
}

/// The upgrade request as the server saw it.
#[derive(Debug, Clone, Default)]
struct Seen {
    line: String,
    headers: Vec<(String, String)>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Reads an HTTP request head byte by byte (so no frame bytes are consumed).
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
    let line = lines.next()?.to_owned();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.to_owned(), v.trim().to_owned()))
        .collect();
    Some(Seen { line, headers })
}

/// Starts a server; `session` runs for every accepted, upgraded connection.
fn serve<F>(extra_response: &'static str, session: F) -> (String, Arc<Mutex<Vec<Seen>>>)
where
    F: Fn(WebSocket<TcpStream>) + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let session = Arc::new(session);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let log = log.clone();
            let session = session.clone();
            thread::spawn(move || {
                let Some(req) = read_head(&mut s) else { return };
                log.lock().unwrap().push(req.clone());
                if req.header("authorization") == Some("Bearer wrong") {
                    let _ = s.write_all(
                        b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 9\r\n\r\nbad token",
                    );
                    return;
                }
                let method = req.line.split(' ').next().unwrap_or("");
                let fields: Vec<(&str, &str)> = req
                    .headers
                    .iter()
                    .map(|(n, v)| (n.as_str(), v.as_str()))
                    .collect();
                let accept = match dl_ws::check_request(method, &fields) {
                    Ok(a) => a,
                    Err(e) => {
                        let body = e.to_string();
                        let _ = write!(
                            s,
                            "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        );
                        return;
                    }
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n{extra_response}\r\n"
                );
                session(WebSocket::from_raw(s, Role::Server, Config::default()));
            });
        }
    });
    (format!("ws://{addr}"), seen)
}

/// Echoes text and binary messages until the client closes.
fn echo(mut ws: WebSocket<TcpStream>) {
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => {
                if ws.send_text(&t).is_err() {
                    return;
                }
            }
            Ok(Message::Binary(b)) => {
                if ws.send_binary(&b).is_err() {
                    return;
                }
            }
            Ok(Message::Close(_)) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

#[test]
fn echo_round_trip_and_clean_close() {
    let (base, seen) = serve("", echo);
    let options = ConnectOptions {
        headers: vec![("Authorization".into(), "Bearer good".into())],
        timeout: Some(Duration::from_secs(5)),
        ..Default::default()
    };
    let (mut ws, resp) =
        dl_ws::connect_with(&client(), &format!("{base}/hubs/order?id=7"), &options).unwrap();
    assert_eq!(resp.status.as_u16(), 101);
    assert_eq!(ws.protocol(), None);

    ws.send_text("Bàn 5 gọi phục vụ").unwrap();
    assert_eq!(
        ws.read().unwrap(),
        Message::Text("Bàn 5 gọi phục vụ".into())
    );
    let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    ws.send_binary(&big).unwrap();
    assert_eq!(ws.read().unwrap(), Message::Binary(big));
    ws.ping(b"hb").unwrap();
    assert_eq!(ws.read().unwrap(), Message::Pong(b"hb".to_vec()));

    ws.close(Some(CloseFrame::new(1000, "done"))).unwrap();
    match ws.read().unwrap() {
        Message::Close(Some(c)) => assert_eq!(c.code, 1000),
        m => panic!("expected close, got {m:?}"),
    }
    assert_eq!(ws.state(), State::Closed);

    let req = seen.lock().unwrap()[0].clone();
    assert_eq!(req.line, "GET /hubs/order?id=7 HTTP/1.1");
    assert_eq!(req.header("authorization"), Some("Bearer good"));
    assert_eq!(req.header("sec-websocket-version"), Some("13"));
    assert_eq!(req.header("upgrade"), Some("websocket"));
    assert!(req.header("sec-websocket-extensions").is_none());
}

#[test]
fn rejected_upgrade_reports_status_and_body() {
    let (base, _) = serve("", echo);
    let options = ConnectOptions {
        headers: vec![("Authorization".into(), "Bearer wrong".into())],
        ..Default::default()
    };
    match dl_ws::connect_with(&client(), &base, &options) {
        Err(Error::Rejected { status, body }) => {
            assert_eq!(status, 401);
            assert_eq!(body, "bad token");
        }
        other => panic!("expected Rejected, got {:?}", other.map(|_| ())),
    }
}

#[test]
fn reserved_headers_are_refused() {
    let options = ConnectOptions {
        headers: vec![("Sec-WebSocket-Key".into(), "x".into())],
        ..Default::default()
    };
    assert!(matches!(
        dl_ws::connect_with(&client(), "ws://127.0.0.1:9/", &options),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn unoffered_subprotocol_or_extension_fails_the_handshake() {
    let (base, _) = serve("Sec-WebSocket-Protocol: chat\r\n", echo);
    assert!(matches!(
        dl_ws::connect_with(&client(), &base, &ConnectOptions::default()),
        Err(Error::Handshake(_))
    ));
    let options = ConnectOptions {
        protocols: vec!["superchat".into(), "chat".into()],
        ..Default::default()
    };
    let (ws, _) = dl_ws::connect_with(&client(), &base, &options).unwrap();
    assert_eq!(ws.protocol(), Some("chat"));

    let (base, _) = serve("Sec-WebSocket-Extensions: permessage-deflate\r\n", echo);
    assert!(matches!(
        dl_ws::connect_with(&client(), &base, &ConnectOptions::default()),
        Err(Error::Handshake(_))
    ));
}

#[test]
fn server_initiated_messages_arrive_with_the_101_and_close_is_echoed() {
    // The first frame is written immediately after the 101, often in the same segment.
    let (base, _) = serve("", |mut ws| {
        ws.send_text("{\"type\":6}\u{1e}").unwrap();
        ws.ping(b"srv").unwrap();
        ws.close(Some(CloseFrame::new(1001, "restarting"))).unwrap();
        // Wait for the client's answer to finish the handshake.
        loop {
            match ws.read() {
                Ok(Message::Pong(p)) => assert_eq!(p, b"srv"),
                Ok(Message::Close(c)) => {
                    assert_eq!(c.unwrap().code, 1001);
                    return;
                }
                Ok(m) => panic!("unexpected {m:?}"),
                Err(e) => panic!("{e}"),
            }
        }
    });
    let (mut ws, _) = dl_ws::connect_with(&client(), &base, &ConnectOptions::default()).unwrap();
    assert_eq!(
        ws.read().unwrap(),
        Message::Text("{\"type\":6}\u{1e}".into())
    );
    assert_eq!(ws.read().unwrap(), Message::Ping(b"srv".to_vec()));
    assert_eq!(
        ws.read().unwrap(),
        Message::Close(Some(CloseFrame::new(1001, "restarting")))
    );
    assert!(matches!(ws.read(), Err(Error::Closed)));
}

#[test]
fn read_timeouts_let_the_caller_interleave_sends() {
    let (base, _) = serve("", |mut ws| {
        thread::sleep(Duration::from_millis(150));
        ws.send_text("late").unwrap();
        echo(ws);
    });
    let (mut ws, _) = dl_ws::connect_with(&client(), &base, &ConnectOptions::default()).unwrap();
    ws.set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let mut timeouts = 0;
    let first = loop {
        match ws.read() {
            Ok(m) => break m,
            Err(e) if e.is_timeout() => {
                timeouts += 1;
                assert!(timeouts < 500);
            }
            Err(e) => panic!("{e}"),
        }
    };
    assert!(timeouts > 0);
    assert_eq!(first, Message::Text("late".into()));
    ws.send_text("again").unwrap();
    ws.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    assert_eq!(ws.read().unwrap(), Message::Text("again".into()));
}

#[test]
fn shutdown_handle_unblocks_a_reader() {
    let (base, _) = serve("", |mut ws| {
        // Never sends; wait for the client to go away.
        let _ = ws.read();
    });
    let (mut ws, _) = dl_ws::connect_with(&client(), &base, &ConnectOptions::default()).unwrap();
    let handle = ws.shutdown_handle().unwrap();
    let t = thread::spawn(move || {
        thread::sleep(Duration::from_millis(80));
        handle.shutdown().unwrap();
    });
    let start = Instant::now();
    let r = ws.read();
    assert!(
        matches!(r, Err(Error::ConnectionReset | Error::Io(_))),
        "{r:?}"
    );
    assert!(start.elapsed() < Duration::from_secs(5));
    t.join().unwrap();
}

#[test]
fn abrupt_disconnect_is_connection_reset() {
    let (base, _) = serve("", |ws| drop(ws.into_inner()));
    let (mut ws, _) = dl_ws::connect_with(&client(), &base, &ConnectOptions::default()).unwrap();
    assert!(matches!(
        ws.read(),
        Err(Error::ConnectionReset | Error::Io(_))
    ));
    assert!(matches!(ws.read(), Err(Error::Closed)));
}
