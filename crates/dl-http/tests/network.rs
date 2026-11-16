//! Tests against real servers. Each one prints a note and passes when the network (or,
//! for HTTPS, dl-tls) is not available.

mod support;

use dl_http::{Client, ClientOptions, Error, Proxy, Response, Url};
use std::time::{Duration, Instant};
use support::ConnectProxy;

fn client() -> Client {
    Client::with_options(ClientOptions {
        timeout: Some(Duration::from_secs(20)),
        ..Default::default()
    })
}

/// Returns the response, or `None` (after printing why) when the environment cannot run
/// the test.
fn reachable(what: &str, result: Result<Response, Error>) -> Option<Response> {
    match result {
        Ok(r) if r.status.is_server_error() => {
            println!("note: skipping {what}: server answered {}", r.status);
            None
        }
        Ok(r) => Some(r),
        Err(Error::Tls(dl_tls::Error::Unsupported(why))) => {
            println!("note: skipping {what}: dl-tls is not available ({why})");
            None
        }
        Err(e @ (Error::Io(_) | Error::Timeout | Error::ProxyConnect { .. })) => {
            println!("note: skipping {what}: network unavailable ({e})");
            None
        }
        Err(e) => panic!("{what}: {e}"),
    }
}

#[test]
fn http_example_com() {
    let Some(resp) = reachable("http://example.com/", client().get("http://example.com/")) else {
        return;
    };
    assert_eq!(resp.status.as_u16(), 200);
    assert!(resp.text().unwrap().contains("Example Domain"));
}

#[test]
fn https_example_com() {
    let Some(resp) = reachable("https://example.com/", client().get("https://example.com/")) else {
        return;
    };
    assert_eq!(resp.status.as_u16(), 200);
    assert!(resp.text().unwrap().contains("Example Domain"));
}

#[test]
fn https_httpbin_post_json() {
    let body = br#"{"agent":"dl-http","n":42}"#;
    let Some(resp) = reachable(
        "https://httpbin.org/post",
        client().post_json("https://httpbin.org/post", body),
    ) else {
        return;
    };
    assert_eq!(resp.status.as_u16(), 200);
    let text = resp.text().unwrap();
    assert!(text.contains(r#""agent": "dl-http""#), "{text}");
    assert!(
        text.contains(r#""Content-Type": "application/json""#),
        "{text}"
    );
}

#[test]
fn https_through_local_connect_proxy() {
    let proxy = ConnectProxy::start(None);
    let c = Client::with_options(ClientOptions {
        proxy: Proxy::Http(Url::parse(&proxy.url("")).unwrap()),
        timeout: Some(Duration::from_secs(20)),
        ..Default::default()
    });
    let result = c.get("https://example.com/");
    let Some(resp) = reachable("https://example.com/ via CONNECT", result) else {
        return;
    };
    assert_eq!(resp.status.as_u16(), 200);
    assert_eq!(proxy.requests()[0].line, "CONNECT example.com:443 HTTP/1.1");
}

#[test]
fn connect_timeout_to_blackhole() {
    // 10.255.255.1 is normally unroutable: connects hang until the connect timeout (or
    // fail at once with "network unreachable").
    let c = Client::with_options(ClientOptions {
        connect_timeout: Some(Duration::from_millis(500)),
        proxy: Proxy::None,
        ..Default::default()
    });
    let start = Instant::now();
    let result = c.get("http://10.255.255.1:81/");
    let elapsed = start.elapsed();
    match result {
        Err(Error::Timeout | Error::Io(_)) => {
            assert!(elapsed < Duration::from_secs(3), "took {elapsed:?}");
        }
        other => println!("note: 10.255.255.1 is routable here: {other:?}"),
    }
}
