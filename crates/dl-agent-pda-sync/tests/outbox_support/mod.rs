//! The batch and blob part of the C# `FakeSyncServer` for the uploader tests: token
//! exchange, `POST /sync/v1/batches` with the S3 semantics (duplicate seq acked, gap
//! answered 409 `expected_seq`, 424 `SYNC_PARENT_PENDING`, `?orphans=skip`), blob content
//! and refs, and the heartbeat. Signatures are checked for a fresh nonce only (the full S2
//! verification is covered by `tests/transport.rs`).

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::{SyncBatch, SyncBlobRef, SyncBlobRefBatch, SyncHeartbeat};
use dl_agent_pda_sync::security::{AgentKey, SharedSecretStore, names};
use dl_agent_pda_sync::transport::{SyncHttpClient, TokenProvider};
use dl_hash::{Digest, Sha256};

use super::support::TestEnv;

/// What the fake cloud saw and how it answers.
#[derive(Default)]
pub struct Cloud {
    pub token_counter: u32,
    pub token_lifetime_seconds: i64,
    pub nonces: HashSet<String>,
    pub rejections: Vec<String>,
    pub fail_next_batches: u32,
    pub fail_status: u16,
    pub fail_body: Option<String>,
    pub parent_pending: HashMap<String, String>,
    /// child stream -> parent stream: the child's next batch is answered 424 once, as when
    /// the cloud reads it while its parent's batch is still in flight (RT-1318 review).
    pub parent_race: HashMap<String, String>,
    pub honor_skip_orphans: bool,
    pub orphan_skips: Vec<SyncBatch>,
    pub parent_pending_answers: u32,
    pub last_seq: HashMap<String, i64>,
    pub applied: Vec<SyncBatch>,
    pub blobs: HashMap<String, Vec<u8>>,
    pub blob_refs: Vec<SyncBlobRef>,
    pub fail_next_blob_puts: u32,
    pub blob_fail_status: u16,
    pub forget_blobs: bool,
    pub blob_puts: u32,
    pub heartbeats: Vec<SyncHeartbeat>,
    pub config_json: Option<String>,
}

/// The server thread and its state.
pub struct FakeCloud {
    pub base: String,
    pub agent_id: dl_uuid::Uuid,
    pub agent_token: String,
    pub state: Arc<Mutex<Cloud>>,
    stop: Arc<AtomicBool>,
    port: u16,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct Req {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

fn read_request(stream: &TcpStream) -> Option<Req> {
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
    Some(Req {
        method,
        target,
        headers,
        body,
    })
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
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

fn ok(data: &str) -> (u16, String) {
    (200, format!("{{\"result\":200,\"data\":{data}}}"))
}

fn sha(content: &[u8]) -> String {
    dl_encoding::hex::encode(Sha256::digest(content))
}

fn authorized(st: &mut Cloud, req: &Req) -> bool {
    if req.header("Authorization") != Some(format!("Bearer jwt-{}", st.token_counter).as_str()) {
        return false;
    }
    let nonce = req.header("X-Sync-Nonce").unwrap_or("").to_owned();
    if nonce.len() < 16 || !st.nonces.insert(nonce) {
        st.rejections.push("replay".into());
        return false;
    }
    true
}

fn handle(st: &mut Cloud, agent_id: dl_uuid::Uuid, agent_token: &str, req: &Req) -> (u16, String) {
    let text = String::from_utf8_lossy(&req.body).into_owned();
    let (path, query) = req
        .target
        .split_once('?')
        .unwrap_or((req.target.as_str(), ""));
    match (req.method.as_str(), path) {
        ("POST", "/sync/v1/token") => {
            if !text.contains(agent_token) {
                return (401, String::new());
            }
            st.token_counter += 1;
            let lifetime = if st.token_lifetime_seconds == 0 {
                900
            } else {
                st.token_lifetime_seconds
            };
            ok(&format!(
                "{{\"accessToken\":\"jwt-{}\",\"tokenType\":\"Bearer\",\"expiresIn\":{lifetime},\"agentId\":\"{agent_id}\"}}",
                st.token_counter
            ))
        }
        ("POST", "/sync/v1/batches") => {
            if !authorized(st, req) {
                return (401, String::new());
            }
            if st.fail_next_batches > 0 {
                st.fail_next_batches -= 1;
                return (st.fail_status, st.fail_body.clone().unwrap_or_default());
            }
            let Ok(batch) = SyncBatch::from_json(&text) else {
                return (400, "{\"result\":400}".into());
            };
            let skip = st.honor_skip_orphans && query == "orphans=skip";
            if skip {
                st.orphan_skips.push(batch.clone());
            }
            let raced = st.parent_race.remove(&batch.stream);
            if let Some(parent) = raced.or_else(|| {
                (!skip)
                    .then(|| st.parent_pending.get(&batch.stream).cloned())
                    .flatten()
            }) {
                st.parent_pending_answers += 1;
                return (
                    424,
                    format!(
                        "{{\"result\":424,\"code\":\"SYNC_PARENT_PENDING\",\"message\":\"{}: waiting for rows of stream {parent}\",\"stream\":\"{}\",\"parent_stream\":\"{parent}\"}}",
                        batch.table, batch.stream
                    ),
                );
            }
            let last = st.last_seq.get(&batch.stream).copied().unwrap_or(0);
            if batch.seq <= last {
                return ok("{\"ack\":true,\"duplicate\":true}");
            }
            if batch.seq != last + 1 {
                return (
                    409,
                    format!("{{\"result\":409,\"expected_seq\":{}}}", last + 1),
                );
            }
            st.last_seq.insert(batch.stream.clone(), batch.seq);
            st.applied.push(batch);
            ok("{\"ack\":true}")
        }
        ("POST", "/sync/v1/blobs/refs") => {
            if !authorized(st, req) {
                return (401, String::new());
            }
            let Ok(batch) = SyncBlobRefBatch::from_json(&text) else {
                return (400, "{\"result\":400}".into());
            };
            if batch.agent_id != agent_id.to_string() || batch.contract_version != 1 {
                return (400, "{\"result\":400}".into());
            }
            let mut missing: Vec<String> = Vec::new();
            let mut applied = 0;
            for r in batch.refs {
                if st.blobs.contains_key(&r.sha256) {
                    applied += 1;
                    st.blob_refs.push(r);
                } else if !missing.contains(&r.sha256) {
                    missing.push(r.sha256);
                }
            }
            let list: Vec<String> = missing.iter().map(|m| format!("\"{m}\"")).collect();
            ok(&format!(
                "{{\"applied\":{applied},\"missing\":[{}]}}",
                list.join(",")
            ))
        }
        ("PUT", p) if p.starts_with("/sync/v1/blobs/") => {
            if !authorized(st, req) {
                return (401, String::new());
            }
            st.blob_puts += 1;
            if st.fail_next_blob_puts > 0 {
                st.fail_next_blob_puts -= 1;
                return (
                    st.blob_fail_status,
                    format!("{{\"result\":{}}}", st.blob_fail_status),
                );
            }
            if req.header("Content-Type") != Some("application/octet-stream") {
                return (415, "{\"result\":415}".into());
            }
            let hash = &p["/sync/v1/blobs/".len()..];
            if sha(&req.body) != hash {
                return (400, "{\"result\":400}".into());
            }
            let stored = !st.forget_blobs && !st.blobs.contains_key(hash);
            if stored {
                st.blobs.insert(hash.to_owned(), req.body.clone());
            }
            ok(&format!("{{\"sha256\":\"{hash}\",\"stored\":{stored}}}"))
        }
        ("POST", "/sync/v1/heartbeat") => {
            if !authorized(st, req) {
                return (401, String::new());
            }
            match SyncHeartbeat::from_json(&text) {
                Ok(hb) => st.heartbeats.push(hb),
                Err(_) => return (400, "{\"result\":400}".into()),
            }
            match &st.config_json {
                None => ok("{}"),
                Some(c) => ok(&format!("{{\"config\":{c}}}")),
            }
        }
        _ => (404, String::new()),
    }
}

impl FakeCloud {
    /// Starts the server.
    pub fn start() -> FakeCloud {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let agent_id = dl_uuid::Uuid::new_v4();
        let agent_token = format!("agent-token-{}", dl_uuid::Uuid::new_v4().simple());
        let state = Arc::new(Mutex::new(Cloud {
            fail_status: 503,
            blob_fail_status: 503,
            ..Cloud::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (st, sp, tok) = (state.clone(), stop.clone(), agent_token.clone());
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
                let (status, body) = handle(&mut st.lock().unwrap(), agent_id, &tok, &req);
                respond(&mut stream, status, &body);
            }
        });
        FakeCloud {
            base: format!("http://127.0.0.1:{port}/"),
            agent_id,
            agent_token,
            state,
            stop,
            port,
            thread: Some(thread),
        }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut Cloud) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }

    pub fn applied_seqs(&self) -> Vec<i64> {
        self.with(|s| s.applied.iter().map(|b| b.seq).collect())
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

/// An "enrolled" agent against the fake cloud: signing key and agent token stored, the
/// signed client and the token provider wired like the service does.
pub struct Agent {
    pub http: Arc<SyncHttpClient>,
    pub tokens: Arc<TokenProvider>,
}

pub fn agent(env: &TestEnv, cloud: &FakeCloud) -> Agent {
    let secrets: &SharedSecretStore = &env.secrets;
    let (key, _) = AgentKey::load_or_create(secrets.as_ref()).unwrap();
    secrets
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
    Agent { http, tokens }
}
