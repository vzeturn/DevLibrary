//! Client tests against in-process HTTP servers and proxies on 127.0.0.1.

mod support;

use dl_http::{Client, ClientOptions, Error, Method, Proxy, Request, Url};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};
use support::{ConnectProxy, Server, settle};

fn client() -> Client {
    client_with(ClientOptions::default())
}

/// A direct (no proxy) client with the given options.
fn client_with(options: ClientOptions) -> Client {
    Client::with_options(ClientOptions {
        proxy: Proxy::None,
        ..options
    })
}

fn url(s: &str) -> Url {
    Url::parse(s).unwrap()
}

#[test]
fn get_and_post_json_round_trip() {
    let server = Server::start(|peer, _| {
        while let Some(req) = peer.read_request() {
            if req.method() == "POST" {
                let body = format!(r#"{{"received":{}}}"#, String::from_utf8_lossy(&req.body));
                peer.respond(
                    "201 Created",
                    "Content-Type: application/json\r\n",
                    body.as_bytes(),
                );
            } else {
                peer.respond(
                    "200 OK",
                    "Content-Type: text/plain\r\nX-Multi: a\r\nX-Multi: b\r\n",
                    b"hello",
                );
            }
        }
    });
    let c = client();

    let resp = c.get(&server.url("/hello?x=1")).unwrap();
    assert_eq!(resp.status.as_u16(), 200);
    assert_eq!(resp.reason, "OK");
    assert_eq!(resp.text().unwrap(), "hello");
    assert_eq!(resp.headers.get("content-type"), Some("text/plain"));
    assert_eq!(
        resp.headers.get_all("x-multi").collect::<Vec<_>>(),
        ["a", "b"]
    );

    let req = Request::post(
        url(&server.url("/api/v1/invoices")),
        br#"{"id":42}"#.to_vec(),
    )
    .with_header("Authorization", "Bearer abc.def")
    .unwrap()
    .with_header("X-Device-ID", "agent-7")
    .unwrap()
    .with_header("Content-Type", "application/json")
    .unwrap();
    let resp = c.request(&req).unwrap();
    assert_eq!(resp.status.as_u16(), 201);
    assert_eq!(resp.text().unwrap(), r#"{"received":{"id":42}}"#);

    let resp = c.post_json(&server.url("/ping"), b"{}").unwrap();
    assert_eq!(resp.text().unwrap(), r#"{"received":{}}"#);

    let reqs = server.requests();
    assert_eq!(reqs.len(), 3);
    let get = &reqs[0];
    assert_eq!(get.line, "GET /hello?x=1 HTTP/1.1");
    assert_eq!(get.header("host"), Some(&*server.addr.to_string()));
    assert!(get.header("user-agent").unwrap().starts_with("dl-http/"));
    assert_eq!(get.header("content-length"), None);
    assert_eq!(get.header("accept-encoding"), None);
    let post = &reqs[1];
    assert_eq!(post.line, "POST /api/v1/invoices HTTP/1.1");
    assert_eq!(post.header("authorization"), Some("Bearer abc.def"));
    assert_eq!(post.header("x-device-id"), Some("agent-7"));
    assert_eq!(post.header("content-type"), Some("application/json"));
    assert_eq!(post.header("content-length"), Some("9"));
    assert_eq!(post.body, br#"{"id":42}"#);
    assert_eq!(reqs[2].header("content-type"), Some("application/json"));
    assert_eq!(server.accepted(), 1);
}

#[test]
fn custom_user_agent_and_host() {
    let server = Server::ok("ok");
    let c = client_with(ClientOptions {
        user_agent: Some("agent/2.0".into()),
        ..Default::default()
    });
    c.get(&server.url("/")).unwrap();
    let req = Request::get(url(&server.url("/v")))
        .with_header("User-Agent", "override")
        .unwrap();
    c.request(&req).unwrap();
    let c = client_with(ClientOptions {
        user_agent: None,
        ..Default::default()
    });
    c.get(&server.url("/none")).unwrap();
    let reqs = server.requests();
    assert_eq!(reqs[0].header("user-agent"), Some("agent/2.0"));
    assert_eq!(reqs[1].header("user-agent"), Some("override"));
    assert_eq!(reqs[2].header("user-agent"), None);

    let bad = client_with(ClientOptions {
        user_agent: Some("bad\r\nInjected: 1".into()),
        ..Default::default()
    });
    assert!(matches!(
        bad.get(&server.url("/")),
        Err(Error::InvalidHeader { .. })
    ));
}

#[test]
fn keep_alive_reuses_connections() {
    let server = Server::start(|peer, _| {
        while let Some(req) = peer.read_request() {
            if req.target() == "/close" {
                peer.respond("200 OK", "Connection: close\r\n", b"bye");
                peer.close();
                return;
            }
            peer.respond("200 OK", "", b"again");
        }
    });
    let c = client();
    for _ in 0..5 {
        assert_eq!(c.get(&server.url("/")).unwrap().text().unwrap(), "again");
    }
    assert_eq!(server.accepted(), 1);
    assert_eq!(c.get(&server.url("/close")).unwrap().text().unwrap(), "bye");
    assert_eq!(c.get(&server.url("/")).unwrap().text().unwrap(), "again");
    assert_eq!(server.accepted(), 2);
    c.close_idle();
    assert_eq!(c.get(&server.url("/")).unwrap().text().unwrap(), "again");
    assert_eq!(server.accepted(), 3);
}

#[test]
fn concurrent_requests_share_a_client() {
    let server = Server::ok("parallel");
    let c = client();
    thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                for _ in 0..5 {
                    assert_eq!(c.get(&server.url("/")).unwrap().text().unwrap(), "parallel");
                }
            });
        }
    });
    assert_eq!(server.requests().len(), 40);
    assert!(server.accepted() < 40, "no connection was reused");
}

/// The first connection answers one request, then reads the next one and closes without
/// answering: what a server whose keep-alive timer fired at the wrong moment does.
fn stale_server() -> Server {
    Server::start(|peer, index| {
        if index == 0 {
            if peer.read_request().is_some() {
                peer.respond("200 OK", "", b"first");
            }
            let _ = peer.read_request();
            peer.close();
            return;
        }
        while peer.read_request().is_some() {
            peer.respond("200 OK", "", b"fresh");
        }
    })
}

#[test]
fn stale_keep_alive_is_retried_for_get() {
    let server = stale_server();
    let c = client();
    assert_eq!(c.get(&server.url("/")).unwrap().text().unwrap(), "first");
    assert_eq!(c.get(&server.url("/")).unwrap().text().unwrap(), "fresh");
    assert_eq!(server.accepted(), 2);
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn stale_keep_alive_is_not_retried_for_sent_post() {
    let server = stale_server();
    let c = client();
    assert_eq!(c.get(&server.url("/")).unwrap().text().unwrap(), "first");
    match c.post_json(&server.url("/"), b"{}") {
        Err(Error::Io(e)) => assert!(
            matches!(
                e.kind(),
                std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
            ),
            "{e}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(server.accepted(), 1);
}

#[test]
fn idle_connection_closed_by_server_is_not_reused() {
    let server = Server::start(|peer, _| {
        if peer.read_request().is_some() {
            peer.respond("200 OK", "", b"once");
        }
        peer.close();
    });
    let c = client();
    for _ in 0..3 {
        assert_eq!(
            c.post_json(&server.url("/"), b"{}")
                .unwrap()
                .text()
                .unwrap(),
            "once"
        );
        settle();
    }
    assert_eq!(server.accepted(), 3);
}

#[test]
fn fresh_connection_closed_early_is_an_error() {
    let server = Server::start(|peer, _| {
        let _ = peer.read_request();
        peer.close();
    });
    let c = client();
    assert!(matches!(c.get(&server.url("/")), Err(Error::Io(_))));
    assert_eq!(server.accepted(), 1);
}

#[test]
fn chunked_response_in_pieces() {
    let server = Server::start(|peer, _| {
        while peer.read_request().is_some() {
            for piece in [
                &b"HTTP/1.1 200 OK\r\nTransfer-"[..],
                b"Encoding: chunked\r\n\r\n5;ext=1\r\nhel",
                b"lo\r\n6\r\n world\r\n",
                b"0\r\nX-Trailer: yes\r\n",
                b"\r\n",
            ] {
                peer.write(piece);
                thread::sleep(Duration::from_millis(15));
            }
        }
    });
    let c = client();
    for _ in 0..2 {
        let resp = c.get(&server.url("/")).unwrap();
        assert_eq!(resp.text().unwrap(), "hello world");
        assert_eq!(resp.trailers.get("x-trailer"), Some("yes"));
    }
    assert_eq!(server.accepted(), 1);
}

#[test]
fn read_to_close_bodies() {
    let server = Server::start(|peer, _| {
        if let Some(req) = peer.read_request() {
            if req.target() == "/http10" {
                peer.write(b"HTTP/1.0 200 OK\r\n\r\nold ");
                peer.write(b"school");
            } else {
                peer.write(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nuntil ");
                thread::sleep(Duration::from_millis(20));
                peer.write(b"the end");
            }
            peer.close();
        }
    });
    let c = client();
    assert_eq!(
        c.get(&server.url("/")).unwrap().text().unwrap(),
        "until the end"
    );
    assert_eq!(
        c.get(&server.url("/http10")).unwrap().text().unwrap(),
        "old school"
    );
    assert_eq!(server.accepted(), 2);
}

#[test]
fn head_and_interim_responses() {
    let server = Server::start(|peer, _| {
        while let Some(req) = peer.read_request() {
            match req.method() {
                "HEAD" => peer.write(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n"),
                _ => peer.write(
                    b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\nLink: </a>\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
                ),
            }
        }
    });
    let c = client();
    let resp = c
        .request(&Request::new(Method::Head, url(&server.url("/"))))
        .unwrap();
    assert_eq!(resp.headers.get("content-length"), Some("1000"));
    assert!(resp.body.is_empty());
    let resp = c.get(&server.url("/")).unwrap();
    assert_eq!(resp.status.as_u16(), 200);
    assert_eq!(resp.text().unwrap(), "ok");
    assert_eq!(server.accepted(), 1);
}

#[test]
fn overall_timeout() {
    let server = Server::start(|peer, _| {
        while let Some(req) = peer.read_request() {
            if req.target() == "/drip" {
                // Each byte arrives well within a per-read timeout, but the whole body
                // takes far longer than the overall deadline.
                peer.write(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n");
                for _ in 0..100 {
                    peer.write(b"x");
                    thread::sleep(Duration::from_millis(50));
                }
            } else {
                thread::sleep(Duration::from_secs(3));
                peer.respond("200 OK", "", b"late");
            }
        }
    });
    let c = client_with(ClientOptions {
        timeout: Some(Duration::from_millis(300)),
        ..Default::default()
    });
    let start = Instant::now();
    assert!(matches!(c.get(&server.url("/slow")), Err(Error::Timeout)));
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );

    let start = Instant::now();
    assert!(matches!(c.get(&server.url("/drip")), Err(Error::Timeout)));
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );

    // A per-request timeout overrides the client's.
    let c = client_with(ClientOptions {
        timeout: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    let req = Request::get(url(&server.url("/slow"))).with_timeout(Duration::from_millis(200));
    let start = Instant::now();
    assert!(matches!(c.request(&req), Err(Error::Timeout)));
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn body_limit() {
    let server = Server::start(|peer, _| {
        while let Some(req) = peer.read_request() {
            match req.target() {
                "/length" => peer.respond("200 OK", "", &[b'a'; 100]),
                "/chunked" => peer.write(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\naaaaaaaa\r\n8\r\naaaaaaaa\r\n0\r\n\r\n",
                ),
                "/fits" => peer.respond("200 OK", "", &[b'a'; 10]),
                _ => {
                    peer.write(b"HTTP/1.1 200 OK\r\n\r\n");
                    peer.write(&[b'a'; 100]);
                    peer.close();
                    return;
                }
            }
        }
    });
    let c = client_with(ClientOptions {
        max_body: 10,
        ..Default::default()
    });
    for path in ["/length", "/chunked", "/close"] {
        match c.get(&server.url(path)) {
            Err(Error::BodyTooLarge { limit: 10 }) => {}
            other => panic!("{path}: {other:?}"),
        }
    }
    assert_eq!(c.get(&server.url("/fits")).unwrap().body.len(), 10);
}

#[test]
fn malformed_responses() {
    let server = Server::start(|peer, _| {
        if let Some(req) = peer.read_request() {
            match req.target() {
                "/smuggle" => peer.write(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 3\r\n\r\n0\r\n\r\n",
                ),
                "/fold" => peer.write(b"HTTP/1.1 200 OK\r\nX: a\r\n b\r\nContent-Length: 0\r\n\r\n"),
                "/badchunk" => peer.write(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n"),
                "/truncated" => {
                    peer.write(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc");
                    peer.close();
                    return;
                }
                _ => peer.write(b"SSH-2.0-OpenSSH_9.6\r\n"),
            }
            // Keep the connection open: errors must not depend on the peer closing.
            thread::sleep(Duration::from_secs(2));
        }
    });
    let c = client_with(ClientOptions {
        timeout: Some(Duration::from_secs(1)),
        ..Default::default()
    });
    for path in ["/smuggle", "/fold", "/badchunk", "/truncated", "/garbage"] {
        match c.get(&server.url(path)) {
            Err(Error::InvalidResponse { .. }) => {}
            other => panic!("{path}: {other:?}"),
        }
    }
}

#[test]
fn redirects() {
    let other = Server::start(|peer, _| {
        while peer.read_request().is_some() {
            peer.respond("200 OK", "", b"done");
        }
    });
    let other_end = other.url("/end");
    let server = Server::start(move |peer, _| {
        while let Some(req) = peer.read_request() {
            match req.target() {
                "/start" => peer.respond("302 Found", "Location: /middle?step=2\r\n", b"moved"),
                "/middle?step=2" => peer.respond(
                    "301 Moved Permanently",
                    &format!("Location: {other_end}\r\n"),
                    b"",
                ),
                "/same" => peer.respond("307 Temporary Redirect", "Location: final\r\n", b""),
                "/final" => peer.respond("200 OK", "", b"same origin"),
                "/loop" => peer.respond("302 Found", "Location: /loop\r\n", b""),
                "/nolocation" => peer.respond("302 Found", "", b"stay"),
                _ => peer.respond("404 Not Found", "", b""),
            }
        }
    });
    let follow = client_with(ClientOptions {
        max_redirects: 3,
        ..Default::default()
    });
    let req = Request::get(url(&server.url("/start")))
        .with_header("Authorization", "Bearer secret")
        .unwrap()
        .with_header("X-Device-ID", "d1")
        .unwrap();
    let resp = follow.request(&req).unwrap();
    assert_eq!(resp.text().unwrap(), "done");
    let a = server.requests();
    assert_eq!(a[0].target(), "/start");
    assert_eq!(a[1].target(), "/middle?step=2");
    assert_eq!(a[1].header("authorization"), Some("Bearer secret"));
    let b = other.requests();
    assert_eq!(b[0].target(), "/end");
    assert_eq!(
        b[0].header("authorization"),
        None,
        "credentials leaked across origins"
    );
    assert_eq!(b[0].header("x-device-id"), Some("d1"));

    let req = Request::get(url(&server.url("/same")))
        .with_header("Authorization", "Bearer s")
        .unwrap();
    assert_eq!(follow.request(&req).unwrap().text().unwrap(), "same origin");
    assert_eq!(
        server.requests().last().unwrap().header("authorization"),
        Some("Bearer s")
    );

    assert!(matches!(
        follow.get(&server.url("/loop")),
        Err(Error::TooManyRedirects { max: 3 })
    ));
    assert_eq!(
        follow
            .get(&server.url("/nolocation"))
            .unwrap()
            .text()
            .unwrap(),
        "stay"
    );

    // Not followed: default options, and methods other than GET/HEAD.
    let resp = client().get(&server.url("/start")).unwrap();
    assert_eq!(resp.status.as_u16(), 302);
    assert_eq!(resp.text().unwrap(), "moved");
    let resp = follow.post_json(&server.url("/start"), b"{}").unwrap();
    assert_eq!(resp.status.as_u16(), 302);
}

#[test]
fn http_proxy_absolute_form_with_auth() {
    let proxy = Server::start(|peer, _| {
        while peer.read_request().is_some() {
            peer.respond("200 OK", "", b"via proxy");
        }
    });
    let c = Client::with_options(ClientOptions {
        proxy: Proxy::Http(url(&format!("http://user:pa%3Ass@{}", proxy.addr))),
        ..Default::default()
    });
    for _ in 0..2 {
        let resp = c.get("http://upstream.invalid:8080/path?q=1").unwrap();
        assert_eq!(resp.text().unwrap(), "via proxy");
    }
    let reqs = proxy.requests();
    assert_eq!(
        reqs[0].line,
        "GET http://upstream.invalid:8080/path?q=1 HTTP/1.1"
    );
    assert_eq!(reqs[0].header("host"), Some("upstream.invalid:8080"));
    // base64("user:pa:ss")
    assert_eq!(
        reqs[0].header("proxy-authorization"),
        Some("Basic dXNlcjpwYTpzcw==")
    );
    assert_eq!(proxy.accepted(), 1);

    let c = Client::with_options(ClientOptions {
        proxy: Proxy::Http(url(&format!("http://{}", proxy.addr))),
        ..Default::default()
    });
    c.get("http://upstream.invalid/").unwrap();
    let last = proxy.requests().pop().unwrap();
    assert_eq!(last.line, "GET http://upstream.invalid/ HTTP/1.1");
    assert_eq!(last.header("proxy-authorization"), None);
}

#[test]
fn connect_tunnel_through_proxy() {
    // The tunnel target records the first bytes it receives (a TLS ClientHello once
    // dl-tls is implemented) and closes.
    let target = Server::start(|peer, _| {
        use std::io::Read;
        let mut first = [0u8; 1];
        let n = peer.stream.read(&mut first).unwrap_or(0);
        peer.log.lock().unwrap().push(support::Received {
            line: format!("first={:?}", &first[..n]),
            headers: Vec::new(),
            body: first[..n].to_vec(),
        });
        peer.close();
    });
    // base64("agent:s3cret")
    let proxy = ConnectProxy::start(Some("Basic YWdlbnQ6czNjcmV0"));
    let c = Client::with_options(ClientOptions {
        proxy: Proxy::Http(url(&proxy.url("agent:s3cret@"))),
        timeout: Some(Duration::from_secs(5)),
        ..Default::default()
    });
    let result = c.get(&format!("https://127.0.0.1:{}/", target.addr.port()));
    let reqs = proxy.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].line,
        format!("CONNECT 127.0.0.1:{} HTTP/1.1", target.addr.port())
    );
    assert_eq!(
        reqs[0].header("host"),
        Some(&*format!("127.0.0.1:{}", target.addr.port()))
    );
    assert_eq!(
        reqs[0].header("proxy-authorization"),
        Some("Basic YWdlbnQ6czNjcmV0")
    );
    match result {
        Err(Error::Tls(dl_tls::Error::Unsupported(why))) => {
            println!("note: dl-tls is not available ({why}); tunnel opened, TLS part skipped");
        }
        Err(_) => {
            // The target is not a TLS server, so the handshake fails, but the ClientHello
            // must have travelled through the tunnel.
            settle();
            let got = target.requests();
            assert_eq!(got.len(), 1);
            assert_eq!(got[0].body, [0x16], "expected a TLS handshake record");
        }
        Ok(r) => panic!("unexpected success: {}", r.status),
    }

    // Wrong or missing credentials.
    let c = Client::with_options(ClientOptions {
        proxy: Proxy::Http(url(&proxy.url(""))),
        ..Default::default()
    });
    assert!(matches!(
        c.get(&format!("https://127.0.0.1:{}/", target.addr.port())),
        Err(Error::ProxyConnect { status: 407 })
    ));
}

#[test]
fn connection_refused() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let c = client_with(ClientOptions {
        connect_timeout: Some(Duration::from_secs(5)),
        ..Default::default()
    });
    match c.get(&format!("http://127.0.0.1:{port}/")) {
        Err(Error::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::ConnectionRefused),
        Err(Error::Timeout) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn invalid_urls_fail_before_connecting() {
    let c = client();
    for u in ["ftp://x/", "http://", "not a url", "http://a b/"] {
        assert!(matches!(c.get(u), Err(Error::InvalidUrl { .. })), "{u}");
    }
}

#[test]
fn upgrade_hands_over_the_connection_with_early_bytes() {
    use std::io::{Read, Write};
    let server = Server::start(|peer, _| {
        let Some(req) = peer.read_request() else {
            return;
        };
        if req.header("upgrade") != Some("echo") {
            peer.respond("400 Bad Request", "", b"need upgrade");
            return;
        }
        // The first payload bytes arrive in the same segment as the head.
        peer.write(
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: echo\r\nConnection: Upgrade\r\n\r\nhi:",
        );
        let mut buf = [0u8; 64];
        loop {
            match peer.stream.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => peer.write(&buf[..n]),
            }
        }
    });
    let c = client();
    let req = Request::get(url(&server.url("/ws")))
        .with_header("Connection", "Upgrade")
        .unwrap()
        .with_header("Upgrade", "echo")
        .unwrap();
    let (resp, conn) = c.upgrade(&req).unwrap();
    assert_eq!(resp.status.as_u16(), 101);
    assert_eq!(resp.headers.get("upgrade"), Some("echo"));
    let mut conn = conn.expect("switched");
    assert!(!conn.is_tls());
    let mut got = [0u8; 3];
    conn.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"hi:");
    conn.write_all(b"ping").unwrap();
    let mut echo = [0u8; 4];
    conn.read_exact(&mut echo).unwrap();
    assert_eq!(&echo, b"ping");

    // A read timeout loses nothing and the connection keeps working.
    conn.set_read_timeout(Some(Duration::from_millis(30)))
        .unwrap();
    let err = conn.read(&mut echo).unwrap_err();
    assert!(matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    conn.write_all(b"more").unwrap();
    conn.read_exact(&mut echo).unwrap();
    assert_eq!(&echo, b"more");

    // Another thread can unblock a pending read.
    conn.set_read_timeout(None).unwrap();
    let handle = conn.shutdown_handle().unwrap();
    let t = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        handle.shutdown().unwrap();
    });
    let start = Instant::now();
    let r = conn.read(&mut echo);
    assert!(matches!(r, Ok(0) | Err(_)));
    assert!(start.elapsed() < Duration::from_secs(5));
    t.join().unwrap();
}

#[test]
fn upgrade_refused_returns_the_response() {
    let server = Server::start(|peer, _| {
        while peer.read_request().is_some() {
            peer.respond("401 Unauthorized", "", b"no token");
        }
    });
    let req = Request::get(url(&server.url("/ws")))
        .with_header("Connection", "Upgrade")
        .unwrap()
        .with_header("Upgrade", "websocket")
        .unwrap();
    let (resp, conn) = client().upgrade(&req).unwrap();
    assert_eq!(resp.status.as_u16(), 401);
    assert_eq!(resp.text().unwrap(), "no token");
    assert!(conn.is_none());
}
