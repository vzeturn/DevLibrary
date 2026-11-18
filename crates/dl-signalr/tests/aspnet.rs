//! Cross-check against a real ASP.NET Core SignalR hub (`testdata/hub`).
//!
//! ```text
//! dotnet run --project crates/dl-signalr/testdata/hub --urls http://127.0.0.1:5199
//! DL_SIGNALR_TEST_URL=http://127.0.0.1:5199/hub cargo test -p dl-signalr --test aspnet -- --ignored
//! ```
//!
//! `DL_SIGNALR_TEST_TOKEN` overrides the access token (the hub reads the same variable).

#![cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use dl_http::{Client, ClientOptions, Proxy};
use dl_json::Value;
use dl_signalr::{CloseReason, ConnectOptions, Error, HubConnection};

const T: Option<Duration> = Some(Duration::from_secs(10));
const WAIT: Duration = Duration::from_secs(10);

fn options(negotiate: bool) -> ConnectOptions {
    let url = std::env::var("DL_SIGNALR_TEST_URL").expect("set DL_SIGNALR_TEST_URL");
    let token =
        std::env::var("DL_SIGNALR_TEST_TOKEN").unwrap_or_else(|_| "dl-test token+/=".to_owned());
    let mut o = ConnectOptions::new(url);
    o.negotiate = negotiate;
    o.access_token = Some(token);
    o.http = Some(Arc::new(Client::with_options(ClientOptions {
        proxy: Proxy::None,
        ..Default::default()
    })));
    o
}

fn exercise(negotiate: bool) {
    let (tx, rx) = mpsc::channel();
    let mut o = options(negotiate);
    let events = tx.clone();
    // Registered in lowercase: the hub sends "Notify".
    o.on("notify", move |args| events.send(args.to_vec()).unwrap());
    let hub = HubConnection::connect(&o).unwrap();

    assert_eq!(
        hub.invoke("Echo", vec!["xin chào 🙂".into()], T).unwrap(),
        "xin chào 🙂"
    );
    assert_eq!(
        hub.invoke("Add", vec![2.into(), 40.into()], T)
            .unwrap()
            .as_i64(),
        Some(42)
    );
    assert_eq!(hub.invoke("Nothing", vec![], T).unwrap(), Value::Null);

    let exact = dl_json::parse_with(
        r#"{"total":12345678901234567890.1234567890,"id":18446744073709551617,"cents":-0.10}"#,
        dl_json::ParseOptions {
            keep_raw_numbers: true,
            ..Default::default()
        },
    )
    .unwrap();
    let back = hub.invoke("EchoJson", vec![exact], T).unwrap();
    assert_eq!(
        back.to_string(),
        r#"{"total":12345678901234567890.1234567890,"id":18446744073709551617,"cents":-0.10}"#
    );

    match hub.invoke("Fail", vec![], T) {
        Err(Error::Hub { target, message }) => {
            assert_eq!(target, "Fail");
            assert!(message.contains("boom"), "{message}");
            println!("Fail -> {message}");
        }
        other => panic!("{other:?}"),
    }
    match hub.invoke("Crash", vec![], T) {
        Err(Error::Hub { message, .. }) => {
            assert!(!message.contains("secret"), "{message}");
            println!("Crash -> {message}");
        }
        other => panic!("{other:?}"),
    }
    match hub.invoke("NoSuchMethod", vec![], T) {
        Err(Error::Hub { message, .. }) => println!("NoSuchMethod -> {message}"),
        other => panic!("{other:?}"),
    }
    match hub.invoke("Counter", vec![3.into()], T) {
        Err(e @ (Error::Hub { .. } | Error::StreamingNotSupported)) => println!("Counter -> {e}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        hub.invoke("Delay", vec![2000.into()], Some(Duration::from_millis(200))),
        Err(Error::Timeout)
    ));

    // Server-to-client invocation, case-insensitive target.
    hub.invoke("Notify", vec!["hello".into()], T).unwrap();
    let args = rx.recv_timeout(WAIT).unwrap();
    assert_eq!(args[0], "hello");
    assert_eq!(args[1].as_i64(), Some(42));

    // Fire-and-forget.
    hub.send("Notify", vec!["sent".into()]).unwrap();
    assert_eq!(rx.recv_timeout(WAIT).unwrap()[0], "sent");

    // Invoke from inside a handler.
    let inner = hub.clone();
    let from_handler = tx.clone();
    hub.on("Notify", move |args| {
        if args[0] == "call back" {
            let r = inner.invoke("Echo", vec!["inside".into()], T).unwrap();
            from_handler.send(vec![r]).unwrap();
        }
    });
    hub.invoke("Notify", vec!["call back".into()], T).unwrap();
    let mut got = [
        rx.recv_timeout(WAIT).unwrap(),
        rx.recv_timeout(WAIT).unwrap(),
    ];
    got.sort_by_key(|a| a.len());
    assert_eq!(got[0], [Value::from("inside")]);

    // 8 threads at once.
    let workers: Vec<_> = (0..8)
        .map(|t| {
            let hub = hub.clone();
            thread::spawn(move || {
                for i in 0..25 {
                    let v = format!("{t}/{i}");
                    assert_eq!(
                        hub.invoke("Echo", vec![v.as_str().into()], T).unwrap(),
                        v.as_str()
                    );
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }

    // The hub's ClientTimeoutInterval is 3 s: idle for 5 s with 1 s client pings.
    let mut o = options(negotiate);
    o.keep_alive_interval = Duration::from_secs(1);
    o.server_timeout = Duration::from_secs(3); // the hub pings every second
    let idle = HubConnection::connect(&o).unwrap();
    thread::sleep(Duration::from_secs(5));
    assert!(idle.close_reason().is_none(), "{:?}", idle.close_reason());
    assert_eq!(
        idle.invoke("Echo", vec!["awake".into()], T).unwrap(),
        "awake"
    );
    idle.close();
    // Note: ASP.NET Core arms ClientTimeoutInterval only after the client's first ping
    // (DefaultHubDispatcher: `case PingMessage: connection.StartClientTimeout()`), so a
    // client that never pings is not dropped. The idle connection above was armed by its
    // first ping and survived 5 s > 3 s, so the later pings arrived too.

    // The server aborting the connection.
    let kicked = HubConnection::connect(&options(negotiate)).unwrap();
    let _ = kicked.invoke("Kick", vec![], T);
    let reason = kicked.wait_closed_timeout(WAIT).expect("still open");
    println!("Kick -> {reason}");
    assert!(!matches!(reason, CloseReason::Local));

    hub.close();
    assert!(matches!(hub.wait_closed(), CloseReason::Local));
    assert!(matches!(
        hub.invoke("Echo", vec!["x".into()], T),
        Err(Error::Closed)
    ));

    // A wrong token is refused: by negotiate, or by the upgrade without it.
    let mut bad = options(negotiate);
    bad.access_token = Some("wrong".into());
    match HubConnection::connect(&bad) {
        Err(e @ (Error::NegotiateRejected { status: 401, .. } | Error::WebSocket(_))) => {
            println!("wrong token -> {e}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
#[ignore = "needs a running ASP.NET Core hub; set DL_SIGNALR_TEST_URL"]
fn aspnet_hub_with_negotiate() {
    exercise(true);
}

#[test]
#[ignore = "needs a running ASP.NET Core hub; set DL_SIGNALR_TEST_URL"]
fn aspnet_hub_skip_negotiation() {
    exercise(false);
}
