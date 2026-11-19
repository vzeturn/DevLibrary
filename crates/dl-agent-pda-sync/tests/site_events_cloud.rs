//! `Reporting/CloudSiteEventTransportTests.cs`: `POST /sync/v1/events` against a fake
//! cloud: authorised, and each failure answer handled.

mod support;

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::time::DateTimeOffset;
use dl_agent_pda_sync::contracts::wire::{
    SiteEvent, SiteEventBatch, site_event_levels as lv, site_event_sources,
};
use dl_agent_pda_sync::reporting::{
    CloudSiteEventTransport, MemorySiteEventStore, SiteEventFlushResult, SiteEventFlusher,
    SiteEventSendStatus, SiteEventStore,
};
use dl_agent_pda_sync::security::{AgentKey, names};
use dl_agent_pda_sync::transport::TokenProvider;
use dl_agent_pda_sync::util::ctx::Ctx;
use support::TestEnv;

#[derive(Default)]
struct Cloud {
    token_counter: u32,
    forced: VecDeque<u16>,
    poison: Vec<String>,
    requests: u32,
    batch_sizes: Vec<usize>,
    log: Vec<SiteEvent>,
    stored: HashMap<String, SiteEvent>,
}

struct FakeCloud {
    base: String,
    state: Arc<Mutex<Cloud>>,
    stop: Arc<AtomicBool>,
    port: u16,
    thread: Option<std::thread::JoinHandle<()>>,
    agent_token: String,
    agent_id: dl_uuid::Uuid,
}

/// Method, target, headers, body.
type Request = (String, String, Vec<(String, String)>, Vec<u8>);

fn read_request(stream: &TcpStream) -> Option<Request> {
    let mut r = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut parts = line.trim_end().splitn(3, ' ');
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        r.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':')?;
        headers.push((k.trim().to_owned(), v.trim().to_owned()));
    }
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).ok()?;
    Some((method, target, headers, body))
}

fn handle(
    st: &mut Cloud,
    id: dl_uuid::Uuid,
    token: &str,
    method: &str,
    target: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> (u16, String) {
    let text = String::from_utf8_lossy(body).into_owned();
    match (method, target) {
        ("POST", "/sync/v1/token") => {
            if !text.contains(token) {
                return (401, String::new());
            }
            st.token_counter += 1;
            (
                200,
                format!(
                    "{{\"result\":200,\"data\":{{\"accessToken\":\"jwt-{}\",\"tokenType\":\"Bearer\",\"expiresIn\":900,\"agentId\":\"{id}\"}}}}",
                    st.token_counter
                ),
            )
        }
        ("POST", "/sync/v1/events") => {
            st.requests += 1;
            let auth = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("Authorization"))
                .map(|(_, v)| v.as_str());
            if auth != Some(format!("Bearer jwt-{}", st.token_counter).as_str()) {
                return (401, String::new());
            }
            if let Some(s) = st.forced.pop_front() {
                return (s, format!("{{\"result\":{s}}}"));
            }
            let Ok(batch) = SiteEventBatch::from_json(&text) else {
                return (400, "{\"result\":400}".into());
            };
            st.batch_sizes.push(batch.events.len());
            if batch.agent_id != id.to_string()
                || batch.events.iter().any(|e| st.poison.contains(&e.code))
            {
                return (400, "{\"result\":400}".into());
            }
            for e in batch.events {
                st.log.push(e.clone());
                st.stored.insert(e.event_id.to_string(), e);
            }
            (200, "{\"result\":200,\"data\":{}}".into())
        }
        _ => (404, String::new()),
    }
}

impl FakeCloud {
    fn start() -> FakeCloud {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let agent_id = dl_uuid::Uuid::new_v4();
        let agent_token = format!("agent-token-{}", dl_uuid::Uuid::new_v4().simple());
        let state = Arc::new(Mutex::new(Cloud::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (st, sp, tok) = (state.clone(), stop.clone(), agent_token.clone());
        let thread = std::thread::spawn(move || {
            for conn in listener.incoming() {
                if sp.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = conn else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let Some((m, t, h, b)) = read_request(&stream) else {
                    continue;
                };
                let (status, body) =
                    handle(&mut st.lock().unwrap(), agent_id, &tok, &m, &t, &h, &b);
                let mut head = format!(
                    "HTTP/1.1 {status} Status\r\nConnection: close\r\nContent-Length: {}\r\n",
                    body.len()
                );
                if !body.is_empty() {
                    head.push_str("Content-Type: application/json; charset=utf-8\r\n");
                }
                head.push_str("\r\n");
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
                let _ = stream.flush();
            }
        });
        FakeCloud {
            base: format!("http://127.0.0.1:{port}/"),
            state,
            stop,
            port,
            thread: Some(thread),
            agent_token,
            agent_id,
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Cloud) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }
}

impl Drop for FakeCloud {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Kit {
    env: TestEnv,
    cloud: FakeCloud,
    store: Arc<MemorySiteEventStore>,
    transport: CloudSiteEventTransport,
    flusher: SiteEventFlusher,
}

impl Kit {
    fn new() -> Kit {
        let env = TestEnv::new();
        let cloud = FakeCloud::start();
        let (key, _) = AgentKey::load_or_create(env.secrets.as_ref()).unwrap();
        env.secrets
            .save(names::AGENT_TOKEN, cloud.agent_token.as_bytes())
            .unwrap();
        let http = Arc::new(env.http_for(&cloud.base, Arc::new(key)));
        let tokens = Arc::new(TokenProvider::new(
            http.clone(),
            env.secrets.clone(),
            env.paths.clone(),
            env.time.clone(),
            &env.log,
        ));
        let transport =
            CloudSiteEventTransport::new(http, tokens, &cloud.agent_id.to_string(), "1.0.0-test");
        let store = Arc::new(MemorySiteEventStore::default());
        let s = store.clone();
        let flusher = SiteEventFlusher::new(
            Arc::new(move || s.clone() as Arc<dyn SiteEventStore>),
            env.time.clone(),
            &env.log,
        );
        Kit {
            env,
            cloud,
            store,
            transport,
            flusher,
        }
    }

    fn fill(&self, codes: &[&str]) {
        let at = DateTimeOffset::from_unix_nanos(
            dl_agent_pda_sync::config::Clock::now(&*self.env.time).unix_nanos(),
            0,
        )
        .unwrap();
        for c in codes {
            self.store
                .enqueue(&mut |_| SiteEvent {
                    event_id: dl_uuid::Uuid::new_v4(),
                    at_utc: at,
                    source: site_event_sources::AGENT.into(),
                    level: lv::WARN.into(),
                    code: (*c).into(),
                    message: c.to_lowercase(),
                    run_id: None,
                    detail: None,
                })
                .unwrap();
        }
    }

    fn flush(&self) -> SiteEventFlushResult {
        self.flusher
            .flush(&self.transport, &Ctx::background())
            .unwrap()
    }
}

#[test]
fn sends_a_batch_and_the_cloud_keeps_one_row_per_event() {
    let k = Kit::new();
    k.fill(&["A", "B", "C"]);
    assert_eq!(
        k.flush(),
        SiteEventFlushResult {
            sent: 3,
            dropped: 0,
            stopped_by: None
        }
    );
    assert_eq!(
        k.cloud
            .with(|c| c.log.iter().map(|e| e.code.clone()).collect::<Vec<_>>()),
        ["A", "B", "C"]
    );
    assert_eq!(k.store.depth().unwrap(), 0);
}

#[test]
fn a_401_refreshes_the_token_once_then_sends() {
    let k = Kit::new();
    k.fill(&["A"]);
    k.cloud.with(|c| c.forced.push_back(401));
    assert!(k.flush().drained());
    assert_eq!(k.cloud.with(|c| c.stored.len()), 1);
    assert_eq!(
        k.cloud.with(|c| c.token_counter),
        2,
        "the first token, then one refresh after the 401"
    );
}

#[test]
fn refused_twice_is_unauthorized_and_keeps_the_queue() {
    let k = Kit::new();
    k.fill(&["A"]);
    k.cloud.with(|c| c.forced.extend([401, 403]));
    assert_eq!(
        k.flush().stopped_by,
        Some(SiteEventSendStatus::Unauthorized)
    );
    assert_eq!(k.store.depth().unwrap(), 1);
}

#[test]
fn a_400_from_the_cloud_is_split_until_the_bad_event_is_dropped() {
    let k = Kit::new();
    k.fill(&["A", "B", "POISON", "D", "E"]);
    k.cloud.with(|c| c.poison.push("POISON".into()));
    assert_eq!(
        k.flush(),
        SiteEventFlushResult {
            sent: 4,
            dropped: 1,
            stopped_by: None
        }
    );
    let mut codes: Vec<String> = k
        .cloud
        .with(|c| c.stored.values().map(|e| e.code.clone()).collect());
    codes.sort();
    assert_eq!(codes, ["A", "B", "D", "E"]);
    assert_eq!(k.cloud.with(|c| c.batch_sizes[0]), 5);
    assert_eq!(k.store.depth().unwrap(), 0);
}

#[test]
fn a_404_backs_off_without_another_request() {
    let k = Kit::new();
    k.fill(&["A", "B"]);
    k.cloud.with(|c| c.forced.push_back(404));
    let first = k.flush();
    let requests = k.cloud.with(|c| c.requests);
    let second = k.flush();
    assert_eq!(first.stopped_by, Some(SiteEventSendStatus::NotSupported));
    assert_eq!(second.stopped_by, Some(SiteEventSendStatus::NotSupported));
    assert_eq!(k.cloud.with(|c| c.requests), requests);
    assert_eq!(k.store.depth().unwrap(), 2);
}

#[test]
fn a_cloud_that_keeps_failing_leaves_the_events_queued() {
    let k = Kit::new();
    k.fill(&["A"]);
    let attempts = k.env.retry.max_attempts;
    k.cloud
        .with(|c| c.forced.extend(std::iter::repeat_n(500, attempts as usize)));
    assert_eq!(k.flush().stopped_by, Some(SiteEventSendStatus::Failed));
    assert_eq!(k.store.depth().unwrap(), 1);
    assert!(k.cloud.with(|c| c.stored.is_empty()));
}
