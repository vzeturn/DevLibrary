//! Shared test parts: a fake SMILE PMS cloud on a local socket (enroll, token, stream), a
//! fake op executor, and a rig that runs the module under the real dl-agent supervisor.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dl_agent::command::{Command, CommandResult, Outbox};
use dl_agent::config::RemoteConfig;
use dl_agent::report::Spool;
use dl_agent::supervisor::{Factory, ModuleState, ModuleStatus, Supervisor};
use dl_agent_pms::ctx::Ctx;
use dl_agent_pms::exec::{self, Executor};
use dl_agent_pms::module::{Hooks, pms_site_module_with};
use dl_agent_pms::wire::{self, Op, OpResult};
use dl_json::{Value, json};
use dl_ws::{CloseFrame, Config, Message, Role, WebSocket};

/// The agent id the fake cloud hands out on enroll.
pub const AGENT_ID: &str = "agent-1";

const CFG_FRAME: &str = include_str!("../../testdata/agent-protocol/cfg.json");
const OP_FRAME: &str = include_str!("../../testdata/agent-protocol/op.json");

/// A fresh directory under the system temp dir, removed on drop.
pub struct TestDir(pub PathBuf);

impl TestDir {
    pub fn new(tag: &str) -> TestDir {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("dl-pms-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        TestDir(p)
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The n-th test op id (a uuid, as the wire requires).
pub fn op_id(n: u32) -> String {
    format!("6f1c2b7e-8d4a-4c3b-9e2f-{n:012}")
}

/// Answers every op `applied` after `delay`, or `failed` when its context ends first.
pub struct FakeExec {
    delay: Duration,
    started: AtomicUsize,
}

impl FakeExec {
    pub fn new(delay: Duration) -> Arc<FakeExec> {
        Arc::new(FakeExec {
            delay,
            started: AtomicUsize::new(0),
        })
    }

    pub fn started(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }
}

impl Executor for FakeExec {
    fn execute(&self, ctx: &Ctx, op: &Op) -> OpResult {
        self.started.fetch_add(1, Ordering::SeqCst);
        if !ctx.sleep(self.delay) {
            return exec::failed(&op.id, "CANCELLED", "the op was cancelled");
        }
        OpResult {
            id: op.id.clone(),
            status: wire::STATUS_APPLIED.into(),
            sets: Vec::new(),
            rowcounts: vec![1],
            outputs: BTreeMap::new(),
            error: None,
            replayed: None,
        }
    }

    fn contract_max(&self) -> i64 {
        1
    }
}

/// How the fake cloud ends one stream session after its ops were answered.
#[derive(Debug, Clone, Copy)]
pub enum End {
    /// Close with this code.
    Close(u16),
    /// Keep the stream open until the agent closes it.
    Hold,
}

/// One scripted stream session: ops to send after `cfg`, then how it ends.
#[derive(Debug, Clone)]
pub struct Session {
    pub ops: Vec<String>,
    pub end: End,
}

impl Session {
    pub fn new(ops: &[String], end: End) -> Session {
        Session {
            ops: ops.to_vec(),
            end,
        }
    }
}

/// What the fake cloud saw.
#[derive(Debug, Default, Clone)]
pub struct Seen {
    pub enrolls: usize,
    pub refused_enrolls: usize,
    pub tokens: usize,
    pub streams: usize,
    /// `agent_version` of every hello.
    pub hellos: Vec<String>,
    /// (op id, status) of every result.
    pub results: Vec<(String, String)>,
    /// Close codes the agent sent.
    pub closes: Vec<u16>,
    /// Protocol surprises (the test fails on any).
    pub bad: Vec<String>,
}

/// A fake PMS cloud on 127.0.0.1.
pub struct FakeCloud {
    pub base: String,
    seen: Arc<Mutex<Seen>>,
}

struct Plan {
    code: String,
    sessions: Mutex<Vec<Session>>,
    seen: Arc<Mutex<Seen>>,
}

impl FakeCloud {
    /// Accepts `code` on enroll and runs `sessions` in order (later streams just hold).
    pub fn start(code: &str, sessions: Vec<Session>) -> FakeCloud {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let seen: Arc<Mutex<Seen>> = Arc::default();
        let plan = Arc::new(Plan {
            code: code.into(),
            sessions: Mutex::new(sessions.into_iter().rev().collect()),
            seen: seen.clone(),
        });
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(s) = s else { continue };
                let plan = plan.clone();
                std::thread::spawn(move || serve(s, &plan));
            }
        });
        FakeCloud { base, seen }
    }

    pub fn seen(&self) -> Seen {
        self.seen.lock().unwrap().clone()
    }

    /// Polls until `done` holds or `ms` passed; the last snapshot either way.
    pub fn wait(&self, ms: u64, done: impl Fn(&Seen) -> bool) -> Seen {
        let end = Instant::now() + Duration::from_millis(ms);
        loop {
            let s = self.seen();
            if done(&s) || Instant::now() >= end {
                return s;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

struct Request {
    line: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn path(&self) -> &str {
        self.line.split(' ').nth(1).unwrap_or("")
    }
}

fn read_request(s: &mut TcpStream) -> Option<Request> {
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
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.to_owned(), v.trim().to_owned()))
        .collect();
    let mut req = Request {
        line,
        headers,
        body: Vec::new(),
    };
    let len: usize = req
        .header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    req.body = vec![0; len];
    s.read_exact(&mut req.body).ok()?;
    Some(req)
}

fn respond(s: &mut TcpStream, status: &str, body: &str) {
    let _ = write!(
        s,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn serve(mut s: TcpStream, plan: &Plan) {
    let Some(req) = read_request(&mut s) else {
        return;
    };
    let seen = &plan.seen;
    match req.path() {
        wire::ENROLL_PATH => {
            let body = dl_json::parse(&String::from_utf8_lossy(&req.body)).unwrap_or(Value::Null);
            let code = body.get("code").and_then(Value::as_str).unwrap_or("");
            if code == plan.code {
                seen.lock().unwrap().enrolls += 1;
                let out = json!({"agentId": AGENT_ID, "siteId": "site-1", "tenantId": "tenant-1"});
                respond(&mut s, "201 Created", &dl_json::to_string(&out));
            } else {
                seen.lock().unwrap().refused_enrolls += 1;
                respond(&mut s, "401 Unauthorized", r#"{"error":"invalid_code"}"#);
            }
        }
        wire::TOKEN_PATH => {
            if req.header("x-agent-id") != Some(AGENT_ID)
                || req.header("x-agent-signature").is_none()
            {
                seen.lock()
                    .unwrap()
                    .bad
                    .push("unsigned token request".into());
                respond(&mut s, "401 Unauthorized", r#"{"error":"unauthorized"}"#);
                return;
            }
            let n = {
                let mut g = seen.lock().unwrap();
                g.tokens += 1;
                g.tokens
            };
            let out = json!({"token": format!("tok-{n}"), "expiresAt": "2099-01-01T00:00:00Z"});
            respond(&mut s, "200 OK", &dl_json::to_string(&out));
        }
        wire::STREAM_PATH => {
            if !req
                .header("authorization")
                .is_some_and(|a| a.starts_with("Bearer tok-"))
            {
                respond(&mut s, "401 Unauthorized", r#"{"error":"unauthorized"}"#);
                return;
            }
            let fields: Vec<(&str, &str)> = req
                .headers
                .iter()
                .map(|(n, v)| (n.as_str(), v.as_str()))
                .collect();
            let method = req.line.split(' ').next().unwrap_or("");
            let Ok(accept) = dl_ws::check_request(method, &fields) else {
                respond(&mut s, "400 Bad Request", r#"{"error":"not an upgrade"}"#);
                return;
            };
            let _ = write!(
                s,
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            );
            seen.lock().unwrap().streams += 1;
            let session = plan.sessions.lock().unwrap().pop().unwrap_or(Session {
                ops: Vec::new(),
                end: End::Hold,
            });
            stream(
                WebSocket::from_raw(s, Role::Server, Config::default()),
                &session,
                seen,
            );
        }
        _ => respond(&mut s, "404 Not Found", "{}"),
    }
}

fn op_frame(id: &str) -> String {
    let mut v = dl_json::parse(OP_FRAME).unwrap();
    if let Value::Object(o) = &mut v {
        o.insert("id", id);
        o.insert("request_key", format!("rk-{id}"));
    }
    dl_json::to_string(&v)
}

fn stream(mut ws: WebSocket<TcpStream>, plan: &Session, seen: &Mutex<Seen>) {
    let hello = match ws.read() {
        Ok(Message::Text(t)) => dl_json::parse(&t).unwrap_or(Value::Null),
        other => {
            seen.lock()
                .unwrap()
                .bad
                .push(format!("first frame {other:?}"));
            return;
        }
    };
    if hello.get("type").and_then(Value::as_str) != Some("hello") {
        seen.lock()
            .unwrap()
            .bad
            .push(format!("first frame {hello:?}"));
        return;
    }
    let version = hello
        .get("agent_version")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    seen.lock().unwrap().hellos.push(version);
    if ws.send_text(CFG_FRAME).is_err() {
        return;
    }
    let mut pending: HashSet<String> = plan.ops.iter().cloned().collect();
    for id in &plan.ops {
        if ws.send_text(&op_frame(id)).is_err() {
            return;
        }
    }
    let mut closed_by_us = false;
    maybe_close(&mut ws, &pending, plan.end, &mut closed_by_us);
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => {
                let v = dl_json::parse(&t).unwrap_or(Value::Null);
                if v.get("type").and_then(Value::as_str) == Some("result") {
                    let id = v.get("id").and_then(Value::as_str).unwrap_or("").to_owned();
                    let status = v.get("status").and_then(Value::as_str).unwrap_or("");
                    pending.remove(&id);
                    seen.lock().unwrap().results.push((id, status.to_owned()));
                    maybe_close(&mut ws, &pending, plan.end, &mut closed_by_us);
                }
            }
            Ok(Message::Close(f)) => {
                if !closed_by_us {
                    seen.lock().unwrap().closes.push(f.map_or(1005, |f| f.code));
                }
                return;
            }
            Ok(_) => {}
            Err(_) => return,
        }
    }
}

/// Closes with the scripted code once every op was answered.
fn maybe_close(
    ws: &mut WebSocket<TcpStream>,
    pending: &HashSet<String>,
    end: End,
    done: &mut bool,
) {
    if let (true, End::Close(code), false) = (pending.is_empty(), end, *done) {
        *done = true;
        let _ = ws.close(Some(CloseFrame::new(code, "test")));
    }
}

/// An HTTP client without proxies (a developer proxy must not see the fake cloud).
pub fn http() -> Arc<dl_http::Client> {
    Arc::new(dl_http::Client::with_options(dl_http::ClientOptions {
        proxy: dl_http::Proxy::None,
        timeout: Some(Duration::from_secs(5)),
        connect_timeout: Some(Duration::from_secs(2)),
        ..dl_http::ClientOptions::default()
    }))
}

fn fast(_: u32) -> Duration {
    Duration::from_millis(100)
}

/// The module with the fake executor, a direct HTTP client and a fast back-off.
pub fn module(exec: Arc<FakeExec>) -> Factory {
    pms_site_module_with(Hooks {
        exec: Some(exec),
        http: Some(http()),
        backoff: Some(fast),
    })
}

/// Writes a PMS agent config for `cloud` (with `code` when given) and returns its path.
pub fn write_config(dir: &TestDir, cloud: &str, code: Option<&str>) -> PathBuf {
    let path = dir.path("config.json");
    let mut m = dl_json::Map::new();
    m.insert("cloud_url", cloud);
    m.insert(
        "state_dir",
        dir.path("state").to_string_lossy().into_owned(),
    );
    if let Some(c) = code {
        m.insert("enrollment_code", c);
    }
    std::fs::write(&path, dl_json::to_string(&Value::Object(m))).unwrap();
    path
}

/// The module section that points at `config`.
pub fn section(config: &Path) -> Value {
    json!({"enabled": true, "config_path": config.to_string_lossy().into_owned()})
}

/// The module under a real supervisor.
pub struct Rig {
    pub sup: Supervisor,
    pub outbox: Outbox,
    pub spool: Spool,
    pub cfg: RemoteConfig,
    next: u32,
    _data: TestDir,
}

pub fn rig(factory: Factory, section: Value) -> Rig {
    let data = TestDir::new("data");
    let outbox = Outbox::default();
    let spool = Spool::in_memory();
    let mut sup = Supervisor::new(data.0.clone(), outbox.clone(), spool.clone());
    sup.add(factory);
    let cfg = RemoteConfig::from_json(&json!({
        "version": 2,
        "body": {"modules": {"pms-site": section}}
    }))
    .unwrap();
    Rig {
        sup,
        outbox,
        spool,
        cfg,
        next: 0,
        _data: data,
    }
}

impl Rig {
    pub fn status(&self) -> ModuleStatus {
        self.sup.statuses()[0].clone()
    }

    pub fn tick_until(&mut self, ms: u64, done: impl Fn(&ModuleStatus) -> bool) -> ModuleStatus {
        let end = Instant::now() + Duration::from_millis(ms);
        loop {
            self.sup.tick(&self.cfg);
            let st = self.status();
            if done(&st) || Instant::now() >= end {
                return st;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn command(&mut self, kind: &str) -> CommandResult {
        self.next += 1;
        let id = format!("c-{kind}-{}", self.next);
        self.sup
            .deliver(Command {
                id: id.clone(),
                kind: kind.into(),
                module: Some("pms-site".into()),
                args: Value::Null,
                expires_at: u64::MAX,
            })
            .expect("module running");
        let end = Instant::now() + Duration::from_secs(3);
        while Instant::now() < end {
            if let Some(r) = self.outbox.take().into_iter().find(|r| r.id == id) {
                return r;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("no answer to {kind}");
    }

    pub fn codes(&self) -> Vec<String> {
        self.spool.peek(100).into_iter().map(|r| r.code).collect()
    }

    /// Stops the module and returns how long it took.
    pub fn stop(&mut self) -> Duration {
        let t = Instant::now();
        self.sup.stop_all(Duration::from_secs(10));
        let took = t.elapsed();
        assert_eq!(self.status().state, ModuleState::Stopped);
        took
    }
}

/// A numeric metric of `st`.
pub fn metric(st: &ModuleStatus, key: &str) -> Option<u64> {
    st.metrics.get(key).and_then(Value::as_u64)
}
