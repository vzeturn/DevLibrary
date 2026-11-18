//! Doubles for the module tests: a stateful PMS API on 127.0.0.1, an install folder with
//! the old service's appsettings, a door-lock adapter that blocks until released, and the
//! module under a real dl-agent supervisor. Waits allow 20 s so a loaded machine passes.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dl_agent::command::{Command, CommandResult, Outbox};
use dl_agent::config::RemoteConfig;
use dl_agent::report::{ProblemReport, Spool};
use dl_agent::supervisor::{Factory, ModuleState, ModuleStatus, Supervisor};
use dl_agent_pms_device::adapters::{AdapterError, Cx, DeviceAdapter};
use dl_agent_pms_device::contracts::{
    CancelAllRequest, EraseRequest, KeyCardRequest, ReadRequest, parse_json,
};
use dl_json::{Value, json};
use dl_uuid::Uuid;

use crate::support::{read_request, respond};

/// The agent token the install folder holds.
pub const TOKEN: &str = "tok-pmsdev-0123456789abcdef0123456789abcdef";
/// How long a test waits for something that should happen.
pub const WAIT: Duration = Duration::from_secs(20);

/// Polls `cond` every 20 ms for up to [`WAIT`].
pub fn eventually(what: &str, cond: impl Fn() -> bool) {
    let end = Instant::now() + WAIT;
    while !cond() {
        assert!(Instant::now() < end, "{what} did not happen within 20 s");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A fresh folder under the temp directory, removed on drop.
pub struct TestDir(pub PathBuf);

impl TestDir {
    pub fn new(tag: &str) -> TestDir {
        let p = std::env::temp_dir().join(format!(
            "pms-device-{tag}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
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

/// What the fake API was asked and how it answers.
#[derive(Clone, Default)]
pub struct ApiState {
    /// Commands waiting to be claimed: id, kind, payload JSON.
    pub queue: VecDeque<(Uuid, String, String)>,
    /// `leaseSeconds` of the claims (0: 60).
    pub lease_seconds: u64,
    /// Answer every claim with this status instead (e.g. 503, 401).
    pub claim_status: Option<u16>,
    /// The status for results (0: 200) and its problem detail.
    pub result_status: u16,
    pub result_detail: String,
    /// Claim requests seen.
    pub claims: usize,
    /// Result requests seen: command id, status answered, parsed body.
    pub results: Vec<(String, u16, Value)>,
    /// Requests whose `Authorization` header did not carry [`TOKEN`].
    pub bad_auth: usize,
}

/// The PMS device-agent API on 127.0.0.1 over plain http (allowed on loopback).
pub struct PmsApi {
    addr: SocketAddr,
    state: Arc<Mutex<ApiState>>,
    done: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl PmsApi {
    pub fn start() -> PmsApi {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state: Arc<Mutex<ApiState>> = Arc::default();
        let done = Arc::new(AtomicBool::new(false));
        let (s, d) = (state.clone(), done.clone());
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if d.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(stream) = stream {
                    serve(stream, &s);
                }
            }
        });
        PmsApi {
            addr,
            state,
            done,
            handle: Some(handle),
        }
    }

    /// `http://127.0.0.1:<port>/api`.
    pub fn base_url(&self) -> String {
        format!("http://{}/api", self.addr)
    }

    /// Queues a command and returns its id.
    pub fn push(&self, kind: &str, payload: &str) -> Uuid {
        let id = Uuid::new_v4();
        self.with(|s| s.queue.push_back((id, kind.into(), payload.into())));
        id
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut ApiState) -> T) -> T {
        f(&mut self.state.lock().unwrap())
    }

    pub fn snapshot(&self) -> ApiState {
        self.with(|s| s.clone())
    }

    /// Waits until `cond` holds and returns the state it held in.
    pub fn wait(&self, what: &str, cond: impl Fn(&ApiState) -> bool) -> ApiState {
        eventually(what, || cond(&self.snapshot()));
        self.snapshot()
    }
}

impl Drop for PmsApi {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn serve(mut stream: TcpStream, state: &Mutex<ApiState>) {
    let Some(req) = read_request(&mut stream) else {
        return;
    };
    let (status, body) = {
        let mut s = state.lock().unwrap();
        if req.header("Authorization") != Some(&format!("DeviceAgent {TOKEN}")) {
            s.bad_auth += 1;
        }
        let path = req.target.trim_start_matches("/api/device-agent/commands/");
        if path == "claim" {
            s.claims += 1;
            claim(&mut s)
        } else if let Some(id) = path.strip_suffix("/result") {
            let status = if s.result_status == 0 {
                200
            } else {
                s.result_status
            };
            let body = parse_json(req.body.as_bytes()).unwrap_or(Value::Null);
            s.results.push((id.to_owned(), status, body));
            let detail = json!({"detail": s.result_detail.clone()});
            (status, dl_json::to_string(&detail))
        } else {
            (404, String::new())
        }
    };
    respond(&mut stream, status, &body);
}

fn claim(s: &mut ApiState) -> (u16, String) {
    if let Some(status) = s.claim_status {
        return (status, r#"{"title":"scripted failure"}"#.into());
    }
    let Some((id, kind, payload)) = s.queue.pop_front() else {
        return (204, String::new());
    };
    let lease = if s.lease_seconds == 0 {
        60
    } else {
        s.lease_seconds
    };
    let body = format!(
        r#"{{"id":"{id}","kind":"{kind}","payload":{payload},"claimToken":"{}","attempt":1,"leaseSeconds":{lease}}}"#,
        Uuid::new_v4()
    );
    (200, body)
}

/// The old service's install folder: appsettings.json, the token file and the data
/// folder with its outbox.
pub struct Install {
    pub dir: TestDir,
}

impl Install {
    /// An install pointing at `api` with `adapter`; the token file is written unless
    /// `token` is false.
    pub fn new(tag: &str, api: &str, adapter: &str, token: bool) -> Install {
        let dir = TestDir::new(tag);
        let settings = json!({"Agent": {
            "ApiBaseUrl": api,
            "AgentId": "FD01",
            "TokenFile": dir.path("agent.token").to_string_lossy().into_owned(),
            "DataDirectory": dir.path("data").to_string_lossy().into_owned(),
            "Adapter": adapter,
            "PollInterval": "00:00:00.25",
        }});
        std::fs::write(dir.path("appsettings.json"), dl_json::to_string(&settings)).unwrap();
        let install = Install { dir };
        if token {
            install.write_token();
        }
        install
    }

    pub fn write_token(&self) {
        std::fs::write(self.dir.path("agent.token"), format!("{TOKEN}\n")).unwrap();
    }

    /// The module section that reads this install.
    pub fn section(&self) -> Value {
        json!({"enabled": true, "appsettings_dir": self.dir.0.to_string_lossy().into_owned()})
    }

    pub fn outbox(&self) -> PathBuf {
        self.dir.path("data").join("outbox")
    }

    /// Result files waiting in the outbox.
    pub fn outbox_files(&self) -> usize {
        count_json(&self.outbox())
    }
}

fn count_json(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .count()
    })
}

/// A door-lock adapter whose `issue` blocks, ignoring the cancellation, until released.
#[derive(Default)]
pub struct GateAdapter {
    state: Mutex<(usize, bool)>,
    cv: Condvar,
}

impl GateAdapter {
    pub fn new() -> Arc<GateAdapter> {
        Arc::default()
    }

    /// Calls that entered `issue`.
    pub fn entered(&self) -> usize {
        self.state.lock().unwrap().0
    }

    pub fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.cv.notify_all();
    }
}

impl DeviceAdapter for GateAdapter {
    fn family(&self) -> &str {
        "gate"
    }

    fn issue(&self, _: &KeyCardRequest, _: &Cx) -> Result<Value, AdapterError> {
        let mut s = self.state.lock().unwrap();
        s.0 += 1;
        while !s.1 {
            s = self.cv.wait(s).unwrap();
        }
        Ok(json!({"cards": [{"room": "101", "cardNumber": "GATE-1"}]}))
    }

    fn duplicate(&self, r: &KeyCardRequest, cx: &Cx) -> Result<Value, AdapterError> {
        self.issue(r, cx)
    }

    fn read(&self, _: &ReadRequest, _: &Cx) -> Result<Value, AdapterError> {
        Ok(json!({}))
    }

    fn erase(&self, _: &EraseRequest, _: &Cx) -> Result<Value, AdapterError> {
        Ok(json!({}))
    }

    fn extend(&self, r: &KeyCardRequest, cx: &Cx) -> Result<Value, AdapterError> {
        self.issue(r, cx)
    }

    fn cancel_all(&self, _: &CancelAllRequest, _: &Cx) -> Result<Value, AdapterError> {
        Ok(json!({}))
    }
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

/// A supervisor with `factory` and `modules.pms-device = section`.
pub fn rig(factory: Factory, section: Value) -> Rig {
    let data = TestDir::new("agent-data");
    let outbox = Outbox::default();
    let spool = Spool::in_memory();
    let mut sup = Supervisor::new(data.0.clone(), outbox.clone(), spool.clone());
    sup.add(factory);
    let cfg = RemoteConfig::from_json(&json!({
        "version": 2,
        "body": {"modules": {"pms-device": section}}
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

    /// Ticks the supervisor until `done` holds; fails after [`WAIT`].
    pub fn until(&mut self, what: &str, done: impl Fn(&ModuleStatus) -> bool) -> ModuleStatus {
        let end = Instant::now() + WAIT;
        loop {
            self.sup.tick(&self.cfg);
            let st = self.status();
            if done(&st) {
                return st;
            }
            assert!(Instant::now() < end, "{what} within 20 s: {st:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Sends a command to the module and waits for its answer.
    pub fn command(&mut self, kind: &str) -> CommandResult {
        self.next += 1;
        let id = format!("c-{kind}-{}", self.next);
        self.sup
            .deliver(Command {
                id: id.clone(),
                kind: kind.into(),
                module: Some("pms-device".into()),
                args: Value::Null,
                expires_at: u64::MAX,
            })
            .expect("module running");
        let end = Instant::now() + WAIT;
        loop {
            if let Some(r) = self.outbox.take().into_iter().find(|r| r.id == id) {
                return r;
            }
            assert!(Instant::now() < end, "no answer to {kind} within 20 s");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn reports(&self) -> Vec<ProblemReport> {
        self.spool.peek(100)
    }

    pub fn codes(&self) -> Vec<String> {
        self.reports().into_iter().map(|r| r.code).collect()
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

/// `metrics[group][kind]` of `st`.
pub fn counted(st: &ModuleStatus, group: &str, kind: &str) -> u64 {
    st.metrics
        .get(group)
        .and_then(|g| g.get(kind))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}
