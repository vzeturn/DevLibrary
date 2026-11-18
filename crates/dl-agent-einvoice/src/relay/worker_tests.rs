//! Relay worker tests: an in-memory scripted hub and transport (hand-written fakes), plus
//! the real platform client against a fake hub on a local socket.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use dl_uuid::Uuid;

use super::transport::ProviderAnswer;
use super::*;

const VNPT: &str = "https://abc-tt78.vnpt-invoice.com.vn/PublishService.asmx";

/// A scripted hub: polls pop `script`; when it is empty the hub raises `stop` and answers
/// `Empty`. Every posted result is kept.
#[derive(Clone)]
struct ScriptHub {
    script: Arc<Mutex<VecDeque<Result<Poll, ApiError>>>>,
    results: Arc<Mutex<Vec<(String, RelayResult)>>>,
    stop: StopSignal,
}

impl ScriptHub {
    fn new(stop: &StopSignal, script: Vec<Result<Poll, ApiError>>) -> ScriptHub {
        ScriptHub {
            script: Arc::new(Mutex::new(script.into())),
            results: Arc::default(),
            stop: stop.clone(),
        }
    }

    fn results(&self) -> Vec<(String, RelayResult)> {
        self.results.lock().unwrap().clone()
    }
}

impl Hub for ScriptHub {
    fn next(&self) -> Result<Poll, ApiError> {
        match self.script.lock().unwrap().pop_front() {
            Some(r) => r,
            None => {
                self.stop.stop();
                Ok(Poll::Empty)
            }
        }
    }

    fn result(&self, id: &str, res: &RelayResult) -> Result<(), ApiError> {
        self.results
            .lock()
            .unwrap()
            .push((id.to_owned(), res.clone()));
        Ok(())
    }
}

/// What the fake transport saw of one call.
#[derive(Clone, Debug, PartialEq)]
struct SeenCall {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    timeout: Duration,
}

#[derive(Clone, Default)]
struct FakeTransport {
    seen: Arc<Mutex<Vec<SeenCall>>>,
    answer: Option<Result<ProviderAnswer, CallError>>,
}

impl Transport for FakeTransport {
    fn call(&self, call: &ProviderCall) -> Result<ProviderAnswer, CallError> {
        self.seen.lock().unwrap().push(SeenCall {
            url: call.url.to_string(),
            headers: call.headers.clone(),
            body: call.body.clone(),
            timeout: call.timeout,
        });
        self.answer.clone().unwrap_or(Ok(ProviderAnswer {
            status: 200,
            content_type: Some("text/xml".into()),
            body: b"<ok/>".to_vec(),
        }))
    }
}

fn call(id: &str, url: &str) -> RelayRequest {
    RelayRequest {
        id: id.into(),
        method: "POST".into(),
        url: url.into(),
        header: vec![("Content-Type".into(), "text/xml".into())],
        body: b"<login>secret</login>".to_vec(),
        timeout_ms: 0,
    }
}

type Slept = Arc<Mutex<Vec<Duration>>>;

fn worker<H: Hub>(
    hub: H,
    transport: FakeTransport,
    stop: &StopSignal,
) -> (Worker<H, FakeTransport>, Arc<Stats>, Slept) {
    let stats = Arc::new(Stats::default());
    let slept: Slept = Arc::default();
    let rec = slept.clone();
    let ctx = ModuleContext::standalone("einvoice", dl_json::Value::Null, std::env::temp_dir());
    let w = Worker::new(hub, transport, stop.clone(), stats.clone(), ctx)
        .with_sleep(Box::new(move |_, d| rec.lock().unwrap().push(d)));
    (w, stats, slept)
}

#[test]
fn refuses_lan_http_other_ports_userinfo_and_methods_without_calling() {
    let stop = StopSignal::new();
    let mut put = call("r5", VNPT);
    put.method = "PUT".into();
    let script = vec![
        Ok(Poll::Call(call("r1", "https://192.168.1.10/SMILE_FO"))),
        Ok(Poll::Call(call("r2", "http://abc.vnpt-invoice.com.vn/x"))),
        Ok(Poll::Call(call(
            "r3",
            "https://abc.vnpt-invoice.com.vn:8443/x",
        ))),
        Ok(Poll::Call(call(
            "r4",
            "https://u:p@abc.vnpt-invoice.com.vn/x",
        ))),
        Ok(Poll::Call(put)),
    ];
    let hub = ScriptHub::new(&stop, script);
    let fake = FakeTransport::default();
    let (mut w, stats, _) = worker(hub.clone(), fake.clone(), &stop);
    assert_eq!(w.run(), Exit::Stopped);
    assert!(fake.seen.lock().unwrap().is_empty(), "nothing may leave");
    let got: Vec<(String, String)> = hub
        .results()
        .into_iter()
        .map(|(id, r)| (id, r.error))
        .collect();
    let want = |id: &str, why: &str| (id.to_owned(), format!("agent từ chối: {why}"));
    assert_eq!(
        got,
        vec![
            want("r1", allow::HOST_NOT_ALLOWED),
            want("r2", allow::NOT_HTTPS),
            want("r3", allow::NOT_443),
            want("r4", allow::HAS_USERINFO),
            want("r5", allow::METHOD_NOT_ALLOWED),
        ]
    );
    assert_eq!(stats.relay_refused.load(Ordering::Relaxed), 5);
    assert_eq!(stats.relay_failures.load(Ordering::Relaxed), 5);
    assert_eq!(stats.relay_calls.load(Ordering::Relaxed), 5);
}

#[test]
fn forwards_only_three_headers_and_caps_the_timeout() {
    let stop = StopSignal::new();
    let mut a = call("a", VNPT);
    a.header = vec![
        ("Authorization".into(), "Bearer hub-key".into()),
        ("Cookie".into(), "x=1".into()),
        ("SOAPAction".into(), "urn:ImportInv".into()),
        ("content-type".into(), "text/xml".into()),
        ("Accept".into(), "*/*".into()),
        ("Host".into(), "10.0.0.1".into()),
    ];
    a.timeout_ms = 600_000;
    let mut b = call("b", VNPT);
    b.timeout_ms = 1500;
    let hub = ScriptHub::new(&stop, vec![Ok(Poll::Call(a)), Ok(Poll::Call(b))]);
    let fake = FakeTransport::default();
    let (mut w, stats, _) = worker(hub.clone(), fake.clone(), &stop);
    w.run();
    let seen = fake.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    let names: Vec<&str> = seen[0].headers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["SOAPAction", "content-type", "Accept"]);
    assert_eq!(seen[0].timeout, Duration::from_secs(60));
    assert_eq!(seen[1].timeout, Duration::from_millis(1500));
    assert_eq!(seen[0].body, b"<login>secret</login>");
    let (_, res) = &hub.results()[0];
    assert_eq!(res.status, 200);
    assert_eq!(res.content_type.as_deref(), Some("text/xml"));
    assert_eq!(res.body, b"<ok/>");
    assert_eq!(stats.relay_failures.load(Ordering::Relaxed), 0);
}

#[test]
fn a_redirect_goes_back_to_the_hub_unfollowed() {
    let stop = StopSignal::new();
    let hub = ScriptHub::new(&stop, vec![Ok(Poll::Call(call("r", VNPT)))]);
    let fake = FakeTransport {
        answer: Some(Ok(ProviderAnswer {
            status: 302,
            content_type: None,
            body: Vec::new(),
        })),
        ..Default::default()
    };
    let (mut w, _, _) = worker(hub.clone(), fake.clone(), &stop);
    w.run();
    assert_eq!(fake.seen.lock().unwrap().len(), 1, "one call, no follow-up");
    let (_, res) = &hub.results()[0];
    assert_eq!((res.status, res.error.as_str()), (302, ""));
}

#[test]
fn transport_failure_and_oversized_answer_become_errors() {
    for (answer, want) in [
        (
            CallError::Failed("connection refused".into()),
            "connection refused",
        ),
        (
            CallError::TooLarge,
            "trả lời của cổng nhà cung cấp quá lớn để chuyển tiếp",
        ),
    ] {
        let stop = StopSignal::new();
        let hub = ScriptHub::new(&stop, vec![Ok(Poll::Call(call("r", VNPT)))]);
        let fake = FakeTransport {
            answer: Some(Err(answer)),
            ..Default::default()
        };
        let (mut w, stats, _) = worker(hub.clone(), fake, &stop);
        w.run();
        assert_eq!(hub.results()[0].1, RelayResult::failed(want));
        assert_eq!(stats.relay_failures.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn backoff_doubles_to_a_minute_and_resets_after_a_good_poll() {
    let stop = StopSignal::new();
    let fail = || Err(ApiError::Other("agent: relay poll: status 502".into()));
    let mut script: Vec<Result<Poll, ApiError>> = (0..6).map(|_| fail()).collect();
    script.push(Ok(Poll::Empty));
    script.push(fail());
    let hub = ScriptHub::new(&stop, script);
    let (mut w, stats, slept) = worker(hub, FakeTransport::default(), &stop);
    w.run();
    let secs: Vec<u64> = slept
        .lock()
        .unwrap()
        .iter()
        .map(Duration::as_secs)
        .collect();
    assert_eq!(secs, [5, 10, 20, 40, 60, 60, 5]);
    assert_eq!(stats.relay_poll_failures.load(Ordering::Relaxed), 7);
}

#[test]
fn unsupported_hub_waits_five_minutes() {
    let stop = StopSignal::new();
    let hub = ScriptHub::new(&stop, vec![Ok(Poll::Unsupported), Ok(Poll::Unsupported)]);
    let (mut w, stats, slept) = worker(hub, FakeTransport::default(), &stop);
    w.run();
    assert_eq!(*slept.lock().unwrap(), [UNSUPPORTED_WAIT, UNSUPPORTED_WAIT]);
    assert_eq!(UNSUPPORTED_WAIT, Duration::from_secs(300));
    assert_eq!(stats.relay_poll_failures.load(Ordering::Relaxed), 0);
}

#[test]
fn device_replaced_ends_the_worker() {
    let stop = StopSignal::new();
    let hub = ScriptHub::new(&stop, vec![Err(ApiError::DeviceReplaced)]);
    let (mut w, _, _) = worker(hub, FakeTransport::default(), &stop);
    assert_eq!(w.run(), Exit::Replaced);
    assert!(!stop.is_stopped(), "returned before the script ran out");
}

#[test]
fn a_stopped_worker_does_not_poll() {
    let stop = StopSignal::new();
    stop.stop();
    let hub = ScriptHub::new(&stop, vec![Ok(Poll::Call(call("r", VNPT)))]);
    let (mut w, _, _) = worker(hub.clone(), FakeTransport::default(), &stop);
    assert_eq!(w.run(), Exit::Stopped);
    assert!(hub.results().is_empty());
}

/// One request the socket hub saw.
#[derive(Debug, Clone)]
struct Got {
    line: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// A fake hub on a local socket: answers `script` in order (`(status, body)`), one
/// request per connection, and records what it saw.
fn socket_hub(script: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<Got>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let seen: Arc<Mutex<Vec<Got>>> = Arc::default();
    let rec = seen.clone();
    std::thread::spawn(move || {
        for (status, body) in script {
            let Ok((s, _)) = l.accept() else { return };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            let mut headers = Vec::new();
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                let (k, v) = h.split_once(':').unwrap();
                headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
            }
            let len = headers
                .iter()
                .find(|(k, _)| k == "content-length")
                .map_or(0, |(_, v)| v.parse().unwrap());
            let mut req_body = vec![0u8; len];
            r.read_exact(&mut req_body).unwrap();
            rec.lock().unwrap().push(Got {
                line: line.trim_end().to_owned(),
                headers,
                body: req_body,
            });
            let mut s = s;
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(reply.as_bytes()).unwrap();
        }
    });
    (base, seen)
}

#[test]
fn platform_relay_over_a_local_socket() {
    let refused = r#"{"id":"r 1","method":"POST","url":"https://10.0.0.5/SMILE_FO","body":"c2VjcmV0","timeout_ms":5000}"#;
    let (base, seen) = socket_hub(vec![
        (200, refused.into()),
        (204, String::new()),
        (204, String::new()),
        (404, String::new()),
    ]);
    let device = Uuid::parse("6f9619ff-8b86-d011-b42d-00c04fc964ff").unwrap();
    let platform = Platform::new(&base, "agent-key", device);
    let stop = StopSignal::new();
    let fake = FakeTransport::default();
    let (w, stats, _) = worker(platform, fake.clone(), &stop);
    let halt = stop.clone();
    let mut w = w.with_sleep(Box::new(move |_, d| {
        assert_eq!(d, UNSUPPORTED_WAIT);
        halt.stop();
    }));
    assert_eq!(w.run(), Exit::Stopped);
    assert!(fake.seen.lock().unwrap().is_empty());
    assert_eq!(stats.relay_refused.load(Ordering::Relaxed), 1);
    let seen = seen.lock().unwrap().clone();
    let lines: Vec<&str> = seen.iter().map(|g| g.line.as_str()).collect();
    assert_eq!(
        lines,
        [
            "POST /api/v1/agent/relay/next HTTP/1.1",
            "POST /api/v1/agent/relay/r%201/result HTTP/1.1",
            "POST /api/v1/agent/relay/next HTTP/1.1",
            "POST /api/v1/agent/relay/next HTTP/1.1",
        ]
    );
    let h = |g: &Got, k: &str| {
        g.headers
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(
        h(&seen[0], "authorization").as_deref(),
        Some("Bearer agent-key")
    );
    assert_eq!(
        h(&seen[0], "x-device-id").as_deref(),
        Some("6f9619ff-8b86-d011-b42d-00c04fc964ff")
    );
    let posted = String::from_utf8(seen[1].body.clone()).unwrap();
    assert_eq!(
        posted,
        format!(
            r#"{{"status":0,"error":"agent từ chối: {}"}}"#,
            allow::HOST_NOT_ALLOWED
        )
    );
    assert_eq!(
        h(&seen[1], "content-type").as_deref(),
        Some("application/json")
    );
}

#[test]
fn platform_relay_device_replaced_over_a_local_socket() {
    let (base, _) = socket_hub(vec![(
        401,
        r#"{"error":{"code":"device_replaced"}}"#.into(),
    )]);
    let platform = Platform::new(&base, "k", Uuid::NIL);
    let stop = StopSignal::new();
    let (mut w, _, _) = worker(platform, FakeTransport::default(), &stop);
    assert_eq!(w.run(), Exit::Replaced);
}

#[test]
fn platform_relay_poll_errors_keep_gos_messages() {
    let (base, _) = socket_hub(vec![(500, String::new()), (200, "not json".into())]);
    let platform = Platform::new(&base, "k", Uuid::NIL);
    assert_eq!(
        platform.relay_next().unwrap_err(),
        ApiError::Other("agent: relay poll: status 500".into())
    );
    let e = platform.relay_next().unwrap_err().to_string();
    assert!(e.starts_with("agent: decode relay request: "), "{e}");
}
