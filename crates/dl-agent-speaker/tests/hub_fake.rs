//! The speaker against an in-process fake PDA order hub: negotiate, WebSocket upgrade,
//! SignalR handshake, `JoinRvc` per RVC, then guest events that must reach the right
//! play/speak calls of a fake speaker.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dl_agent_speaker::hub::{HubLink, HubSettings, TokenSource};
use dl_agent_speaker::mode::{Cache, Mode, parse_mode};
use dl_agent_speaker::output::{Announcer, Output};
use dl_json::Value;
use dl_signalr::protocol::{self, HubMessage, RecordReader};
use dl_winsvc::StopSignal;
use dl_ws::{Config, Message, Role, WebSocket};

/// One HTTP request head the hub saw.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    target: String,
    authorization: Option<String>,
}

/// What the fake hub does.
#[derive(Clone)]
struct Script {
    /// Status of the negotiate answer.
    negotiate_status: u16,
    /// RVCs whose `JoinRvc` succeeds; others get `FORBIDDEN_RVC`.
    allowed: Vec<i64>,
    /// Invocations sent once every join was answered.
    events: Vec<(String, Vec<Value>)>,
}

/// Hub messages from the client, as `(target, arguments)`.
type Invocations = Mutex<Vec<(String, Vec<Value>)>>;

struct FakeHub {
    base: String,
    requests: Arc<Mutex<Vec<Seen>>>,
    /// Every hub message received from the client, as `(target, arguments)`.
    invocations: Arc<Invocations>,
}

fn read_head(s: &mut TcpStream) -> Option<(Seen, Vec<(String, String)>)> {
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
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.to_owned(), v.trim().to_owned()))
        .collect();
    let authorization = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("authorization"))
        .map(|(_, v)| v.clone());
    Some((
        Seen {
            method,
            target,
            authorization,
        },
        headers,
    ))
}

fn respond(s: &mut TcpStream, status: u16, body: &str) {
    let _ = write!(
        s,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

const NEGOTIATE_OK: &str = r#"{"negotiateVersion":1,"connectionId":"cid-1","connectionToken":"ctok-1","availableTransports":[{"transport":"WebSockets","transferFormats":["Text","Binary"]}]}"#;

fn start(script: Script) -> FakeHub {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let invocations = Arc::new(Mutex::new(Vec::new()));
    let hub = FakeHub {
        base,
        requests: requests.clone(),
        invocations: invocations.clone(),
    };
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let (requests, invocations, script) =
                (requests.clone(), invocations.clone(), script.clone());
            thread::spawn(move || {
                let _ = s.set_read_timeout(Some(Duration::from_secs(20)));
                let Some((req, headers)) = read_head(&mut s) else {
                    return;
                };
                requests.lock().unwrap().push(req.clone());
                if req.method == "POST" {
                    let body = if script.negotiate_status == 200 {
                        NEGOTIATE_OK
                    } else {
                        "unauthorized"
                    };
                    return respond(&mut s, script.negotiate_status, body);
                }
                let fields: Vec<(&str, &str)> = headers
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
                session(
                    WebSocket::from_raw(s, Role::Server, Config::default()),
                    &script,
                    &invocations,
                );
            });
        }
    });
    hub
}

/// One SignalR session: handshake, answer the joins, then push the scripted events.
fn session(mut ws: WebSocket<TcpStream>, script: &Script, invocations: &Invocations) {
    let mut records = RecordReader::new(1 << 20);
    let mut recv = move |ws: &mut WebSocket<TcpStream>| -> Option<String> {
        loop {
            if let Some(r) = records.next_record() {
                return Some(String::from_utf8_lossy(r).into_owned());
            }
            match ws.read() {
                Ok(Message::Text(t)) => records.push(t.as_bytes()).ok()?,
                Ok(Message::Binary(b)) => records.push(&b).ok()?,
                Ok(Message::Ping(_) | Message::Pong(_)) => {}
                Ok(Message::Close(_)) | Err(_) => return None,
            }
        }
    };
    if recv(&mut ws).is_none() {
        return; // no handshake request
    }
    let _ = ws.send_text("{}\u{1e}");
    let mut sent_events = false;
    while let Some(raw) = recv(&mut ws) {
        let Ok(HubMessage::Invocation {
            invocation_id,
            target,
            arguments,
        }) = protocol::parse(raw.as_bytes())
        else {
            continue;
        };
        invocations
            .lock()
            .unwrap()
            .push((target.clone(), arguments.clone()));
        let Some(id) = invocation_id else { continue };
        let rvc = arguments.first().and_then(Value::as_i64).unwrap_or(0);
        let error = (!script.allowed.contains(&rvc)).then(|| "FORBIDDEN_RVC".to_owned());
        let _ = ws.send_text(&protocol::encode(&HubMessage::Completion {
            invocation_id: id,
            result: error.is_none().then_some(Value::Null),
            error,
        }));
        let joins = invocations
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _)| t == "JoinRvc")
            .count();
        if !sent_events && joins >= 3 && !script.events.is_empty() {
            sent_events = true;
            for (target, arguments) in &script.events {
                let _ = ws.send_text(&protocol::encode(&HubMessage::Invocation {
                    invocation_id: None,
                    target: target.clone(),
                    arguments: arguments.clone(),
                }));
            }
        }
    }
}

/// A fake speaker that records what it was asked to do.
#[derive(Default)]
struct FakePlayer(Mutex<Vec<String>>);

impl FakePlayer {
    fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl Output for FakePlayer {
    fn play(&self, name: &str, wav: &'static [u8]) -> Result<(), String> {
        assert!(wav.starts_with(b"RIFF"));
        self.0.lock().unwrap().push(format!("play {name}"));
        Ok(())
    }
    fn speak(&self, text: &str, lang: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(format!("speak {lang} {text}"));
        Ok(())
    }
}

/// A fixed token that counts invalidations.
struct Tokens {
    invalidated: AtomicU32,
    issued: AtomicU32,
}

impl Tokens {
    fn new() -> Arc<Tokens> {
        Arc::new(Tokens {
            invalidated: AtomicU32::new(0),
            issued: AtomicU32::new(0),
        })
    }
}

impl TokenSource for Tokens {
    fn token(&self) -> Result<String, String> {
        self.issued.fetch_add(1, Ordering::SeqCst);
        Ok("tok-1".into())
    }
    fn invalidate(&self) {
        self.invalidated.fetch_add(1, Ordering::SeqCst);
    }
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "timed out: {what}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn run_link(
    hub: &FakeHub,
    rvcs: Vec<i64>,
    cache: Arc<Cache>,
    tokens: Arc<Tokens>,
    refused_retry: Duration,
) -> (
    Arc<HubLink>,
    Arc<FakePlayer>,
    Arc<Announcer>,
    StopSignal,
    thread::JoinHandle<()>,
) {
    let player = Arc::new(FakePlayer::default());
    let announcer = Announcer::start(cache, player.clone()).unwrap();
    let mut settings = HubSettings::new(format!("{}/hubs/order", hub.base), rvcs);
    settings.call_timeout = Duration::from_secs(5);
    settings.refused_retry = refused_retry;
    let link = Arc::new(HubLink::new(settings, tokens, announcer.clone()));
    let stop = StopSignal::new();
    let worker = {
        let (link, stop) = (link.clone(), stop.clone());
        thread::spawn(move || link.run(&stop))
    };
    (link, player, announcer, stop, worker)
}

#[test]
fn joins_each_rvc_and_announces_guest_events_with_the_rvc_mode() {
    let hub = start(Script {
        negotiate_status: 200,
        allowed: vec![1, 2],
        events: vec![
            (
                "GuestOrderRequested".into(),
                vec![1.into(), "T05".into(), 42.into()],
            ),
            (
                "GuestServiceRequested".into(),
                vec![2.into(), "7".into(), 43.into(), "REQUEST_BILL".into()],
            ),
            // Not this module's business: the print relay is pda-sync's.
            ("PrintJob".into(), vec![Value::Null]),
        ],
    });
    let cache = Arc::new(Cache::new());
    cache.set(1, Mode::default());
    cache.set(2, parse_mode(r#"{"tone":null,"voice":true,"lang":"en"}"#));
    let tokens = Tokens::new();
    let (link, player, announcer, stop, worker) = run_link(
        &hub,
        vec![1, 2, 3],
        cache,
        tokens.clone(),
        Duration::from_secs(60),
    );

    wait_until("three output calls", || player.calls().len() >= 3);
    let state = link.state();
    stop.stop();
    worker.join().unwrap();
    announcer.stop();

    // RVC 1 (default mode): guest tone, then the Vietnamese phrase; RVC 2: tone off,
    // English phrase for the bill request. Nothing for PrintJob.
    assert_eq!(
        player.calls(),
        [
            "play guest",
            "speak vi Bàn T05 đặt món",
            "speak en Table 7 requests the bill"
        ]
    );
    assert!(state.connected);
    assert_eq!(state.joined, [1, 2]);
    assert_eq!(state.refused, [(3, "FORBIDDEN_RVC".to_owned())]);
    assert_eq!(state.connects, 1);

    let joins: Vec<Vec<Value>> = hub
        .invocations
        .lock()
        .unwrap()
        .iter()
        .filter(|(t, _)| t == "JoinRvc")
        .map(|(_, a)| a.clone())
        .collect();
    assert_eq!(
        joins,
        [vec![Value::from(1)], vec![2.into()], vec![3.into()]]
    );

    let reqs = hub.requests.lock().unwrap().clone();
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].target, "/hubs/order/negotiate?negotiateVersion=1");
    assert_eq!(reqs[1].method, "GET");
    assert!(
        reqs[1].target.starts_with("/hubs/order?id=ctok-1"),
        "{}",
        reqs[1].target
    );
    // The token goes only in the Authorization header, never in the URL.
    assert!(!reqs[1].target.contains("tok-1&") && !reqs[1].target.contains("access_token"));
    for r in &reqs[..2] {
        assert_eq!(r.authorization.as_deref(), Some("Bearer tok-1"));
    }
    assert_eq!(tokens.invalidated.load(Ordering::SeqCst), 0);
    let m: std::collections::HashMap<_, _> = announcer.stats().metrics().into_iter().collect();
    assert_eq!(
        (m["events"], m["tones_played"], m["phrases_spoken"]),
        (2, 1, 2)
    );
}

#[test]
fn a_cloud_that_refuses_every_rvc_is_closed_and_retried_later() {
    let hub = start(Script {
        negotiate_status: 200,
        allowed: vec![],
        events: vec![],
    });
    let tokens = Tokens::new();
    let (link, player, announcer, stop, worker) = run_link(
        &hub,
        vec![1, 2, 3],
        Arc::new(Cache::new()),
        tokens.clone(),
        Duration::from_millis(100),
    );
    // Two full attempts: the connection is closed after the refusals and opened again.
    wait_until("a second attempt", || {
        hub.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == "POST")
            .count()
            >= 2
    });
    wait_until("the refusal is recorded", || {
        link.state().refused.len() == 3
    });
    let state = link.state();
    stop.stop();
    worker.join().unwrap();
    announcer.stop();
    assert!(!state.connected);
    assert!(state.joined.is_empty());
    assert!(state.refused.iter().all(|(_, m)| m == "FORBIDDEN_RVC"));
    assert_eq!(state.connects, 0);
    assert!(player.calls().is_empty());
    assert!(tokens.issued.load(Ordering::SeqCst) >= 2);
}

#[test]
fn a_refused_token_is_forgotten_and_the_link_retries() {
    let hub = start(Script {
        negotiate_status: 401,
        allowed: vec![1],
        events: vec![],
    });
    let tokens = Tokens::new();
    let (link, _player, announcer, stop, worker) = run_link(
        &hub,
        vec![1],
        Arc::new(Cache::new()),
        tokens.clone(),
        Duration::from_secs(60),
    );
    wait_until("the token is invalidated", || {
        tokens.invalidated.load(Ordering::SeqCst) >= 1
    });
    wait_until("the error is recorded", || {
        link.state().last_error.is_some()
    });
    let state = link.state();
    stop.stop();
    worker.join().unwrap();
    announcer.stop();
    assert!(!state.connected);
    let err = state.last_error.unwrap();
    assert!(err.contains("401"), "{err}");
    assert!(!err.contains("tok-1"), "{err}");
}
