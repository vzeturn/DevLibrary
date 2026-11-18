//! Connection tests against the in-process hub in `fake_hub`.

mod fake_hub;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use dl_http::{Client, ClientOptions, Proxy};
use dl_json::Value;

use self::fake_hub::{FakeHub, Peer, Seen, echo_session, negotiate_ok, start};
use crate::protocol::HubMessage;
use crate::{CloseReason, ConnectOptions, Error, HubConnection};

const T: Option<Duration> = Some(Duration::from_secs(10));
const WAIT: Duration = Duration::from_secs(10);

fn http() -> Arc<Client> {
    Arc::new(Client::with_options(ClientOptions {
        proxy: Proxy::None,
        ..Default::default()
    }))
}

fn options(hub: &FakeHub, negotiate: bool) -> ConnectOptions {
    let mut o = ConnectOptions::new(format!("{}/hub", hub.base));
    o.negotiate = negotiate;
    o.http = Some(http());
    o.timeout = Duration::from_secs(10);
    o
}

fn echo_hub() -> FakeHub {
    start(negotiate_ok, echo_session)
}

fn strings(args: &[Value]) -> Vec<String> {
    args.iter()
        .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
        .collect()
}

#[test]
fn negotiate_handshake_and_invoke() {
    let hub = echo_hub();
    let conn = HubConnection::connect(&options(&hub, true)).unwrap();

    assert_eq!(
        conn.invoke("Echo", vec!["xin chào".into()], T).unwrap(),
        "xin chào"
    );
    assert_eq!(
        conn.invoke("Add", vec![2.into(), 40.into()], T)
            .unwrap()
            .as_i64(),
        Some(42)
    );
    match conn.invoke("Fail", vec![], T) {
        Err(Error::Hub { target, message }) => {
            assert_eq!((target.as_str(), message.as_str()), ("Fail", "boom"));
        }
        other => panic!("{other:?}"),
    }
    match conn.invoke("Missing", vec![], T) {
        Err(e @ Error::Hub { .. }) => assert!(e.to_string().contains("Missing"), "{e}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(conn.invoke("Notify", vec![], T).unwrap(), Value::Null);

    // Numbers keep every digit both ways.
    let exact = dl_json::parse_with(
        "[12345678901234567890.1234567890, 18446744073709551617]",
        dl_json::ParseOptions {
            keep_raw_numbers: true,
            ..Default::default()
        },
    )
    .unwrap();
    let back = conn.invoke("Echo", vec![exact.clone()], T).unwrap();
    assert_eq!(
        back.to_string(),
        "[12345678901234567890.1234567890,18446744073709551617]"
    );

    let reqs = hub.requests();
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].target, "/hub/negotiate?negotiateVersion=1");
    assert_eq!(reqs[1].method, "GET");
    assert_eq!(reqs[1].target, "/hub?id=tok%2Fen%2B1");
    assert!(reqs[1].header("authorization").is_none());

    let received = hub.received();
    assert_eq!(received[0], r#"{"protocol":"json","version":1}"#);
    assert_eq!(
        received[1],
        r#"{"arguments":["xin chào"],"invocationId":"0","target":"Echo","type":1}"#
    );
    conn.close();
    assert!(matches!(conn.wait_closed(), CloseReason::Local));
}

#[test]
fn handshake_error_fails_connect() {
    let hub = start(negotiate_ok, |mut peer: Peer| {
        peer.recv_raw();
        peer.send_raw("{\"error\":\"Requested protocol 'json' is not available.\"}\u{1e}");
        thread::sleep(Duration::from_millis(200));
    });
    match HubConnection::connect(&options(&hub, false)) {
        Err(Error::Handshake(e)) => assert!(e.contains("not available"), "{e}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn handshake_timeout() {
    let hub = start(negotiate_ok, |mut peer: Peer| {
        peer.recv_raw();
        thread::sleep(Duration::from_millis(800));
    });
    let mut o = options(&hub, false);
    o.timeout = Duration::from_millis(300);
    assert!(matches!(HubConnection::connect(&o), Err(Error::Timeout)));
}

#[test]
fn events_reach_handlers_across_frame_boundaries() {
    let hub = start(negotiate_ok, |mut peer: Peer| {
        peer.recv_raw();
        // The handshake response and a first event in one WebSocket message.
        peer.send_raw(
            "{}\u{1e}{\"type\":1,\"target\":\"Event\",\"arguments\":[\"after handshake\"]}\u{1e}",
        );
        while let Some(m) = peer.recv() {
            let HubMessage::Invocation {
                invocation_id: Some(id),
                ..
            } = m
            else {
                continue;
            };
            // Completion and two events in one message, then one event in three parts.
            peer.send_raw(&format!(
                "{{\"type\":3,\"invocationId\":\"{id}\",\"result\":7}}\u{1e}{{\"type\":1,\"target\":\"EVENT\",\"arguments\":[\"a\"]}}\u{1e}{{\"type\":6}}\u{1e}{{\"type\":1,\"target\":\"event\",\"arguments\":[\"b\"]}}\u{1e}"
            ));
            peer.send_raw("{\"type\":1,\"tar");
            peer.send_raw("get\":\"eVeNt\",\"arguments\":[\"split\"");
            peer.send_raw("]}\u{1e}");
        }
    });
    let (tx, rx) = mpsc::channel();
    let mut o = options(&hub, false);
    let tx2 = tx.clone();
    o.on("event", move |args| tx2.send(strings(args)).unwrap());
    let conn = HubConnection::connect(&o).unwrap();
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), ["after handshake"]);
    assert_eq!(conn.invoke("Go", vec![], T).unwrap().as_i64(), Some(7));
    for want in ["a", "b", "split"] {
        assert_eq!(rx.recv_timeout(WAIT).unwrap(), [want]);
    }
    drop(tx);
    conn.close();
}

#[test]
fn handlers_for_server_invocations_and_order() {
    let hub = echo_hub();
    let conn = HubConnection::connect(&options(&hub, true)).unwrap();
    let (tx, rx) = mpsc::channel();
    let (tx1, tx2) = (tx.clone(), tx);
    conn.on("notify", move |args| {
        tx1.send(format!("first {}", strings(args)[0])).unwrap()
    });
    conn.on("Notify", move |args| {
        tx2.send(format!("second {}", strings(args)[0])).unwrap()
    });
    conn.on("Unrelated", |_| panic!("must not run"));
    conn.invoke("Notify", vec!["x".into()], T).unwrap();
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), "first x");
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), "second x");
}

#[test]
fn panicking_handler_does_not_stop_dispatch() {
    let hub = echo_hub();
    let conn = HubConnection::connect(&options(&hub, true)).unwrap();
    let (tx, rx) = mpsc::channel();
    conn.on("notify", |args| {
        if args[0] == "panic" {
            panic!("handler failure (expected in this test)");
        }
    });
    conn.on("notify", move |args| tx.send(strings(args)).unwrap());
    conn.invoke("Notify", vec!["panic".into()], T).unwrap();
    conn.invoke("Notify", vec!["ok".into()], T).unwrap();
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), ["panic"]);
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), ["ok"]);
}

#[test]
fn send_is_fire_and_forget() {
    let hub = echo_hub();
    let conn = HubConnection::connect(&options(&hub, true)).unwrap();
    let (tx, rx) = mpsc::channel();
    conn.on("told", move |args| tx.send(args.to_vec()).unwrap());
    conn.send("Tell", vec![1.into(), "two".into()]).unwrap();
    let args = rx.recv_timeout(WAIT).unwrap();
    assert_eq!(args[0], "Tell");
    assert_eq!(args[1].to_string(), r#"[1,"two"]"#);
    assert!(
        hub.received()
            .contains(&r#"{"arguments":[1,"two"],"target":"Tell","type":1}"#.to_owned())
    );
}

#[test]
fn client_result_requests_get_an_error_completion() {
    let hub = echo_hub();
    let conn = HubConnection::connect(&options(&hub, true)).unwrap();
    let (tx, rx) = mpsc::channel();
    conn.on("getvalue", move |_| tx.send(()).unwrap());
    conn.invoke("AskClient", vec![], T).unwrap();
    rx.recv_timeout(WAIT).unwrap();
    // The reply goes out before the next invocation; Echo makes sure it was read.
    conn.invoke("Echo", vec![1.into()], T).unwrap();
    assert!(
        hub.received().contains(
            &r#"{"type":3,"invocationId":"srv-1","error":"Client didn't provide a result."}"#
                .to_owned()
        )
    );
}

#[test]
fn keepalive_pings_are_sent_when_idle() {
    let hub = echo_hub();
    let mut o = options(&hub, true);
    o.keep_alive_interval = Duration::from_millis(60);
    let conn = HubConnection::connect(&o).unwrap();
    thread::sleep(Duration::from_millis(500));
    // Pings are read by the session; Echo makes sure everything before it was read.
    conn.invoke("Echo", vec![1.into()], T).unwrap();
    let pings = hub
        .received()
        .iter()
        .filter(|m| m.as_str() == r#"{"type":6}"#)
        .count();
    assert!(pings >= 3, "{pings} pings");
    assert!(conn.close_reason().is_none());
}

#[test]
fn busy_connections_do_not_ping() {
    let hub = echo_hub();
    let mut o = options(&hub, true);
    o.keep_alive_interval = Duration::from_millis(300);
    let conn = HubConnection::connect(&o).unwrap();
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(700) {
        conn.invoke("Echo", vec![1.into()], T).unwrap();
        thread::sleep(Duration::from_millis(30));
    }
    assert!(!hub.received().iter().any(|m| m == r#"{"type":6}"#));
}

#[test]
fn silent_server_times_out() {
    let hub = start(negotiate_ok, |mut peer: Peer| {
        peer.accept_handshake();
        while peer.recv_raw().is_some() {}
    });
    let mut o = options(&hub, false);
    o.server_timeout = Duration::from_millis(300);
    let conn = HubConnection::connect(&o).unwrap();
    let start = Instant::now();
    match conn.wait_closed_timeout(WAIT) {
        Some(CloseReason::Transport(Error::ServerTimeout)) => {}
        other => panic!("{other:?}"),
    }
    assert!(start.elapsed() >= Duration::from_millis(250));
    assert!(matches!(conn.invoke("Echo", vec![], T), Err(Error::Closed)));
}

#[test]
fn server_pings_keep_the_connection_alive() {
    let hub = start(negotiate_ok, |mut peer: Peer| {
        peer.accept_handshake();
        for _ in 0..10 {
            thread::sleep(Duration::from_millis(100));
            peer.send(&HubMessage::Ping);
        }
        while peer.recv_raw().is_some() {}
    });
    let mut o = options(&hub, false);
    o.server_timeout = Duration::from_millis(400);
    let conn = HubConnection::connect(&o).unwrap();
    thread::sleep(Duration::from_millis(900));
    assert!(conn.close_reason().is_none());
    match conn.wait_closed_timeout(WAIT) {
        Some(CloseReason::Transport(Error::ServerTimeout)) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn server_close_with_error() {
    let hub = start(negotiate_ok, |mut peer: Peer| {
        peer.accept_handshake();
        while let Some(m) = peer.recv() {
            if matches!(m, HubMessage::Invocation { .. }) {
                peer.send(&HubMessage::Close {
                    error: Some("Server is shutting down.".into()),
                    allow_reconnect: true,
                });
            }
        }
    });
    let conn = HubConnection::connect(&options(&hub, false)).unwrap();
    assert!(matches!(conn.invoke("Slow", vec![], T), Err(Error::Closed)));
    match conn.wait_closed() {
        CloseReason::Server {
            error,
            allow_reconnect,
        } => {
            assert_eq!(error.as_deref(), Some("Server is shutting down."));
            assert!(allow_reconnect);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(conn.send("x", vec![]), Err(Error::Closed)));
}

#[test]
fn websocket_closed_without_close_message() {
    let hub = start(negotiate_ok, |mut peer: Peer| {
        peer.accept_handshake();
        let _ = peer
            .ws
            .close(Some(dl_ws::CloseFrame::new(1011, "internal error")));
        while peer.recv_raw().is_some() {}
    });
    let conn = HubConnection::connect(&options(&hub, false)).unwrap();
    match conn.wait_closed_timeout(WAIT) {
        Some(CloseReason::Transport(Error::WebSocketClosed { code, reason })) => {
            assert_eq!((code, reason.as_str()), (Some(1011), "internal error"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn malformed_message_ends_the_connection() {
    let hub = start(negotiate_ok, |mut peer: Peer| {
        peer.accept_handshake();
        peer.send_raw("{\"type\":3,\u{1e}");
        while peer.recv_raw().is_some() {}
    });
    let conn = HubConnection::connect(&options(&hub, false)).unwrap();
    match conn.wait_closed_timeout(WAIT) {
        Some(CloseReason::Transport(Error::Json(_))) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn invoke_timeout_leaves_the_connection_usable() {
    let hub = echo_hub();
    let conn = HubConnection::connect(&options(&hub, true)).unwrap();
    let start = Instant::now();
    assert!(matches!(
        conn.invoke("Slow", vec![], Some(Duration::from_millis(200))),
        Err(Error::Timeout)
    ));
    let took = start.elapsed();
    assert!(took >= Duration::from_millis(200) && took < Duration::from_secs(5));
    assert_eq!(
        conn.invoke("Echo", vec!["still".into()], T).unwrap(),
        "still"
    );
}

#[test]
fn streams_are_refused() {
    let hub = echo_hub();
    let conn = HubConnection::connect(&options(&hub, true)).unwrap();
    assert!(matches!(
        conn.invoke("Stream", vec![], T),
        Err(Error::StreamingNotSupported)
    ));
    assert_eq!(
        conn.invoke("Echo", vec![1.into()], T).unwrap().as_i64(),
        Some(1)
    );
}

#[test]
fn concurrent_invokes_from_eight_threads() {
    let hub = echo_hub();
    let conn = HubConnection::connect(&options(&hub, true)).unwrap();
    let workers: Vec<_> = (0..8)
        .map(|t| {
            let conn = conn.clone();
            thread::spawn(move || {
                for i in 0..25 {
                    let v = format!("{t}-{i}");
                    let got = conn.invoke("Echo", vec![v.as_str().into()], T).unwrap();
                    assert_eq!(got, v.as_str());
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
    let invocations = hub
        .received()
        .iter()
        .filter(|m| m.contains("\"target\":\"Echo\""))
        .count();
    assert_eq!(invocations, 200);
}

#[test]
fn invoke_and_close_from_inside_a_handler() {
    let hub = echo_hub();
    let conn = HubConnection::connect(&options(&hub, true)).unwrap();
    let (tx, rx) = mpsc::channel();
    let inner = conn.clone();
    conn.on("notify", move |args| {
        if args[0] == "close" {
            inner.close();
            tx.send(Ok(Value::from("closed"))).unwrap();
        } else {
            tx.send(inner.invoke("Echo", vec![args[0].clone()], T))
                .unwrap();
        }
    });
    conn.invoke("Notify", vec!["from handler".into()], T)
        .unwrap();
    assert_eq!(rx.recv_timeout(WAIT).unwrap().unwrap(), "from handler");
    // The handler may close the connection before Notify's own completion arrives.
    let r = conn.invoke("Notify", vec!["close".into()], T);
    assert!(matches!(r, Ok(Value::Null) | Err(Error::Closed)), "{r:?}");
    assert_eq!(rx.recv_timeout(WAIT).unwrap().unwrap(), "closed");
    assert!(matches!(conn.wait_closed(), CloseReason::Local));
    // The handler (and its clone of the connection) was released.
    conn.on("late", |_| {});
    assert!(matches!(conn.invoke("Echo", vec![], T), Err(Error::Closed)));
}

#[test]
fn negotiate_redirect_switches_url_and_token() {
    let hub = start(
        |req: &Seen, base: &str| {
            if req.target.starts_with("/hub/negotiate") {
                (
                    200,
                    format!(r#"{{"url":"{base}/other?x=1","accessToken":"second token"}}"#),
                )
            } else {
                negotiate_ok(req, base)
            }
        },
        echo_session,
    );
    let mut o = options(&hub, true);
    o.access_token = Some("first".into());
    o.headers.push(("X-Tenant".into(), "7".into()));
    let conn = HubConnection::connect(&o).unwrap();
    assert_eq!(
        conn.invoke("Echo", vec![5.into()], T).unwrap().as_i64(),
        Some(5)
    );
    let reqs = hub.requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[0].target, "/hub/negotiate?negotiateVersion=1");
    assert_eq!(reqs[0].header("authorization"), Some("Bearer first"));
    assert_eq!(reqs[1].target, "/other/negotiate?x=1&negotiateVersion=1");
    assert_eq!(reqs[1].header("authorization"), Some("Bearer second token"));
    assert_eq!(
        reqs[2].target,
        "/other?x=1&id=tok%2Fen%2B1&access_token=second%20token"
    );
    assert_eq!(reqs[2].header("authorization"), Some("Bearer second token"));
    for r in &reqs {
        assert_eq!(r.header("x-tenant"), Some("7"), "{}", r.target);
    }
}

#[test]
fn access_token_goes_into_the_query_without_negotiate() {
    let hub = echo_hub();
    let mut o = options(&hub, false);
    o.access_token = Some("a b+c/=".into());
    let conn = HubConnection::connect(&o).unwrap();
    conn.invoke("Echo", vec![1.into()], T).unwrap();
    let reqs = hub.requests();
    assert_eq!(reqs.len(), 1, "no negotiate request");
    assert_eq!(reqs[0].target, "/hub?access_token=a%20b%2Bc%2F%3D");
    assert_eq!(reqs[0].header("authorization"), Some("Bearer a b+c/="));
}

#[test]
fn negotiate_failures() {
    let hub = start(
        |req: &Seen, _: &str| {
            match req.header("authorization") {
            Some("Bearer bad") => (401, String::new()),
            Some("Bearer err") => (200, r#"{"error":"Negotiate failed."}"#.into()),
            Some("Bearer lp") => (
                200,
                r#"{"negotiateVersion":1,"connectionToken":"t","availableTransports":[{"transport":"LongPolling","transferFormats":["Text"]}]}"#.into(),
            ),
            Some("Bearer loop") => (200, format!(r#"{{"url":"http://{}/hub"}}"#, req.header("host").unwrap_or_default())),
            _ => (500, "boom".into()),
        }
        },
        echo_session,
    );
    let mut o = options(&hub, true);
    let mut attempt = |token: &str| {
        o.access_token = Some(token.into());
        HubConnection::connect(&o).map(|_| ())
    };
    assert!(matches!(
        attempt("bad"),
        Err(Error::NegotiateRejected { status: 401, .. })
    ));
    match attempt("err") {
        Err(Error::Negotiate(e)) => assert_eq!(e, "Negotiate failed."),
        other => panic!("{other:?}"),
    }
    assert!(matches!(attempt("lp"), Err(Error::Negotiate(_))));
    match attempt("loop") {
        Err(Error::Negotiate(e)) => assert!(e.contains("redirects"), "{e}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn rejected_upgrade_and_bad_options() {
    let hub = echo_hub();
    let mut o = options(&hub, false);
    o.access_token = Some("reject".into());
    match HubConnection::connect(&o) {
        Err(Error::WebSocket(e)) => {
            assert!(
                matches!(&*e, dl_ws::Error::Rejected { status: 401, body } if body == "unauthorized"),
                "{e}"
            );
        }
        other => panic!("{other:?}"),
    }
    let bad = ConnectOptions {
        url: "ftp://h/hub".into(),
        ..options(&hub, false)
    };
    assert!(matches!(
        HubConnection::connect(&bad),
        Err(Error::InvalidUrl(_))
    ));
    let zero = ConnectOptions {
        keep_alive_interval: Duration::ZERO,
        ..options(&hub, false)
    };
    assert!(matches!(
        HubConnection::connect(&zero),
        Err(Error::InvalidOptions(_))
    ));
}

#[test]
fn local_close_and_drop() {
    let ended = Arc::new(AtomicUsize::new(0));
    let e2 = ended.clone();
    let hub = start(negotiate_ok, move |peer: Peer| {
        echo_session(peer);
        e2.fetch_add(1, Ordering::SeqCst);
    });
    let conn = HubConnection::connect(&options(&hub, false)).unwrap();
    let pending = {
        let c = conn.clone();
        thread::spawn(move || c.invoke("Slow", vec![], None))
    };
    thread::sleep(Duration::from_millis(100));
    conn.close();
    assert!(matches!(pending.join().unwrap(), Err(Error::Closed)));
    assert!(matches!(conn.wait_closed(), CloseReason::Local));
    assert!(matches!(conn.invoke("Echo", vec![], T), Err(Error::Closed)));
    conn.close(); // no-op

    // Dropping every clone closes the connection.
    let conn = HubConnection::connect(&options(&hub, false)).unwrap();
    let clone = conn.clone();
    conn.invoke("Echo", vec![1.into()], T).unwrap();
    drop(conn);
    drop(clone);
    let start = Instant::now();
    while ended.load(Ordering::SeqCst) < 2 {
        assert!(start.elapsed() < WAIT, "server session did not end");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn oversized_messages_end_the_connection() {
    let hub = start(negotiate_ok, |mut peer: Peer| {
        peer.accept_handshake();
        peer.send_raw(&"x".repeat(600));
        peer.send_raw(&"x".repeat(600));
        while peer.recv_raw().is_some() {}
    });
    let mut o = options(&hub, false);
    o.max_message_size = 1000;
    let conn = HubConnection::connect(&o).unwrap();
    match conn.wait_closed_timeout(WAIT) {
        Some(CloseReason::Transport(Error::MessageTooLarge { limit: 1000 })) => {}
        other => panic!("{other:?}"),
    }
}
