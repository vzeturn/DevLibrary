//! Test fixtures ported from `tests/MM.PdaOrder.SyncAgent.Tests`: `TestEnv` (temp data
//! folder, file secret store, manual clock, recording retry policy) and the part of
//! `FakeSyncServer` the transport tests need, an in-process HTTP/1.1 stand-in for the
//! cloud that verifies the S2 signature independently of the agent code.

#![allow(dead_code)]

use dl_agent_pda_sync::config::AgentPaths;
use dl_agent_pda_sync::config::time::{Clock, ManualClock, Timestamp};
use dl_agent_pda_sync::reporting::log::{Level, Logger, LoggerConfig, MemorySink};
use dl_agent_pda_sync::security::AgentKey;
use dl_agent_pda_sync::security::{FileSecretStore, SharedSecretStore};
use dl_agent_pda_sync::transport::{RequestSigner, RetryPolicy, SyncHttpClient};
use dl_hash::Digest;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A temp data dir and the wired agent pieces.
pub struct TestEnv {
    pub dir: PathBuf,
    pub paths: AgentPaths,
    pub secrets: SharedSecretStore,
    pub time: Arc<ManualClock>,
    pub sleeps: Arc<Mutex<Vec<Duration>>>,
    pub retry: RetryPolicy,
    pub sink: Arc<MemorySink>,
    pub log: Logger,
}

impl TestEnv {
    pub fn new() -> TestEnv {
        let dir = std::env::temp_dir()
            .join("pda-sync-agent-tests")
            .join(dl_uuid::Uuid::new_v4().simple().to_string());
        let paths = AgentPaths::new(&dir);
        paths.ensure_created().unwrap();
        let secrets: SharedSecretStore = Arc::new(FileSecretStore::new(paths.secrets_dir()));
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        let s = sleeps.clone();
        let retry = RetryPolicy {
            max_attempts: 4,
            sleep: Arc::new(move |d, _| {
                s.lock().unwrap().push(d);
                true
            }),
            jitter: Arc::new(|| 1.0),
            ..RetryPolicy::default()
        };
        let sink = Arc::new(MemorySink::default());
        let log = Logger::new(LoggerConfig {
            minimum: Level::Verbose,
            sinks: vec![(sink.clone(), Level::Verbose)],
            ..LoggerConfig::default()
        });
        TestEnv {
            dir,
            paths,
            secrets,
            time: Arc::new(ManualClock::new(
                Timestamp::parse_iso("2026-10-24T08:00:00Z").unwrap(),
            )),
            sleeps,
            retry,
            sink,
            log,
        }
    }

    pub fn http_for(&self, base: &str, key: Arc<AgentKey>) -> SyncHttpClient {
        let signer = RequestSigner::new(key, self.time.clone());
        SyncHttpClient::new(
            base,
            Duration::from_secs(30),
            Some(signer),
            self.retry.clone(),
            &self.log,
        )
        .unwrap()
    }

    pub fn sleeps(&self) -> Vec<Duration> {
        self.sleeps.lock().unwrap().clone()
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One request as the server saw it.
#[derive(Debug, Clone)]
pub struct SeenRequest {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl SeenRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Mutable server state (the C# properties).
pub struct State {
    pub valid_code: String,
    pub token_lifetime_seconds: i64,
    pub fail_next_batches: u32,
    pub fail_status: u16,
    pub retry_after: Option<String>,
    pub revoke_agent: bool,
    pub agent_key: Option<[u8; 32]>,
    pub token_counter: u32,
    pub nonces: HashSet<String>,
    pub rejections: Vec<String>,
    pub applied: Vec<Vec<u8>>,
    pub requests: Vec<SeenRequest>,
}

/// The fake cloud.
pub struct FakeSyncServer {
    pub base: String,
    pub agent_id: dl_uuid::Uuid,
    pub agent_token: String,
    pub state: Arc<Mutex<State>>,
    time: Arc<ManualClock>,
    stop: Arc<AtomicBool>,
    port: u16,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

fn respond(stream: &mut TcpStream, status: u16, body: &str, extra: &[(String, String)]) {
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\nConnection: close\r\nContent-Length: {}\r\n",
        reason(status),
        body.len()
    );
    if !body.is_empty() {
        head.push_str("Content-Type: application/json; charset=utf-8\r\n");
    }
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn read_request(stream: &TcpStream) -> Option<SeenRequest> {
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
    Some(SeenRequest {
        method,
        target,
        headers,
        body,
    })
}

impl FakeSyncServer {
    pub fn start(time: Arc<ManualClock>) -> FakeSyncServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let agent_id = dl_uuid::Uuid::new_v4();
        let agent_token = format!("agent-token-{}", dl_uuid::Uuid::new_v4().simple());
        let state = Arc::new(Mutex::new(State {
            valid_code: "CODE-1234".into(),
            token_lifetime_seconds: 900,
            fail_next_batches: 0,
            fail_status: 503,
            retry_after: None,
            revoke_agent: false,
            agent_key: None,
            token_counter: 0,
            nonces: HashSet::new(),
            rejections: Vec::new(),
            applied: Vec::new(),
            requests: Vec::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (st, tm, sp, id, tok) = (
            state.clone(),
            time.clone(),
            stop.clone(),
            agent_id,
            agent_token.clone(),
        );
        let thread = std::thread::spawn(move || {
            for conn in listener.incoming() {
                if sp.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = conn else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let Some(req) = read_request(&stream) else {
                    continue;
                };
                let (status, body, extra) = handle(&st, &tm, id, &tok, &req);
                st.lock().unwrap().requests.push(req);
                respond(&mut stream, status, &body, &extra);
            }
        });
        FakeSyncServer {
            base: format!("http://127.0.0.1:{port}/"),
            agent_id,
            agent_token,
            state,
            time,
            stop,
            port,
            thread: Some(thread),
        }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }

    pub fn rejections(&self) -> Vec<String> {
        self.with(|s| s.rejections.clone())
    }

    pub fn token_exchanges(&self) -> u32 {
        self.with(|s| s.token_counter)
    }
}

impl Drop for FakeSyncServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn reject(st: &mut State, why: &str) -> bool {
    st.rejections.push(why.to_owned());
    false
}

/// The S2 verification, written independently of `transport::signer`.
fn verify(st: &mut State, time: &ManualClock, req: &SeenRequest, key: &[u8; 32]) -> bool {
    let ts = req.header("X-Sync-Timestamp").unwrap_or("").to_owned();
    let nonce = req.header("X-Sync-Nonce").unwrap_or("").to_owned();
    let sig = req.header("X-Sync-Signature").unwrap_or("").to_owned();
    let Ok(unix) = ts.parse::<i64>() else {
        return reject(st, "stale");
    };
    if (time.now().unix_seconds() - unix).abs() > 300 {
        return reject(st, "stale");
    }
    if nonce.len() < 16 || !st.nonces.insert(nonce.clone()) {
        return reject(st, "replay");
    }
    let hash = dl_encoding::hex::encode(dl_hash::Sha256::digest(&req.body));
    let canonical = format!(
        "MM-SYNC1\n{}\n{}\n{ts}\n{nonce}\n{hash}",
        req.method.to_uppercase(),
        req.target
    );
    let Ok(sig) = dl_encoding::base64::STANDARD.decode(sig.as_bytes()) else {
        return reject(st, "badsig");
    };
    dl_ed25519::verify(key, canonical.as_bytes(), &sig) || reject(st, "badsig")
}

fn json_field<'a>(v: &'a dl_json::Value, name: &str) -> Option<&'a str> {
    v.get(name).and_then(dl_json::Value::as_str)
}

fn handle(
    state: &Mutex<State>,
    time: &ManualClock,
    agent_id: dl_uuid::Uuid,
    agent_token: &str,
    req: &SeenRequest,
) -> (u16, String, Vec<(String, String)>) {
    let mut st = state.lock().unwrap();
    let body: dl_json::Value = std::str::from_utf8(&req.body)
        .ok()
        .and_then(|s| dl_json::parse(s).ok())
        .unwrap_or(dl_json::Value::Null);
    let path = req.target.split('?').next().unwrap_or("");
    match (req.method.as_str(), path) {
        ("POST", "/sync/v1/enroll/redeem") => {
            let key = json_field(&body, "publicKey")
                .and_then(|k| dl_encoding::base64::STANDARD.decode(k.as_bytes()).ok())
                .and_then(|k| <[u8; 32]>::try_from(k).ok());
            let Some(key) = key else {
                return (401, String::new(), vec![]);
            };
            if !verify(&mut st, time, req, &key) {
                return (401, String::new(), vec![]);
            }
            if json_field(&body, "code") != Some(st.valid_code.as_str()) {
                return (400, "{\"result\":400}".into(), vec![]);
            }
            st.agent_key = Some(key);
            st.valid_code = "USED".into();
            (
                200,
                format!(
                    "{{\"result\":200,\"data\":{{\"agentId\":\"{agent_id}\",\"agentToken\":\"{agent_token}\",\"tenantId\":\"tenant-1\"}}}}"
                ),
                vec![],
            )
        }
        ("POST", "/sync/v1/token") => {
            let Some(key) = st.agent_key.filter(|_| !st.revoke_agent) else {
                return (401, "{\"result\":401}".into(), vec![]);
            };
            if json_field(&body, "agentToken") != Some(agent_token) {
                return (401, String::new(), vec![]);
            }
            if !verify(&mut st, time, req, &key) {
                return (401, String::new(), vec![]);
            }
            st.token_counter += 1;
            let n = st.token_counter;
            (
                200,
                format!(
                    "{{\"result\":200,\"data\":{{\"accessToken\":\"jwt-{n}\",\"tokenType\":\"Bearer\",\"expiresIn\":{},\"agentId\":\"{agent_id}\"}}}}",
                    st.token_lifetime_seconds
                ),
                vec![],
            )
        }
        ("POST", "/sync/v1/batches") => {
            let bearer = format!("Bearer jwt-{}", st.token_counter);
            let Some(key) = st.agent_key else {
                return (401, String::new(), vec![]);
            };
            if req.header("Authorization") != Some(bearer.as_str())
                || !verify(&mut st, time, req, &key)
            {
                return (401, String::new(), vec![]);
            }
            if st.fail_next_batches > 0 {
                st.fail_next_batches -= 1;
                let extra = st
                    .retry_after
                    .clone()
                    .map(|v| vec![("Retry-After".to_owned(), v)])
                    .unwrap_or_default();
                return (st.fail_status, String::new(), extra);
            }
            st.applied.push(req.body.clone());
            (
                200,
                "{\"result\":200,\"data\":{\"ack\":true}}".into(),
                vec![],
            )
        }
        _ => (404, String::new(), vec![]),
    }
}
