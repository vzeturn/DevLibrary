//! Test doubles (port of SmilePms.DeviceAgent.Tests/Fakes): a manual clock, a list logger,
//! a temp outbox, the in-memory queue with the server semantics, a scriptable adapter, and
//! a std TcpListener HTTP server standing in for the API (instead of FakeHttpHandler).

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use dl_agent_pms_device::adapters::{AdapterError, Cx, DeviceAdapter};
use dl_agent_pms_device::agent::log::{Level, Logger, Record};
use dl_agent_pms_device::agent::{Clock, ResultOutbox, ResultReporter, StopToken, add_duration};
use dl_agent_pms_device::config::DotnetError;
use dl_agent_pms_device::contracts::{
    CancelAllRequest, DeviceCommand, DeviceError, EraseRequest, KeyCardRequest, PendingResult,
    ReadRequest, parse_json,
};
use dl_agent_pms_device::queue::{CommandQueue, QueueError, StaleClaim, StaleClaimReason};
use dl_json::{Value, json};
use dl_time::OffsetDateTime;
use dl_uuid::Uuid;

/// Polls `cond` every 10 ms for up to 20 s (headroom for a loaded machine; a passing
/// condition returns at once).
pub fn wait_until(what: &str, cond: impl Fn() -> bool) {
    for _ in 0..2000 {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(cond(), "condition not reached within 20 s: {what}");
}

/// Runs `work` on a thread while advancing `clock` 1 s every 5 ms (retry delays use the clock).
pub fn drive<T: Send>(clock: &ManualClock, work: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        let h = s.spawn(work);
        for _ in 0..4000 {
            if h.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
            clock.advance(Duration::from_secs(1));
        }
        h.join().expect("work panicked")
    })
}

/// `FakeTimeProvider`: time moves only through [`ManualClock::advance`].
pub struct ManualClock {
    now: Mutex<OffsetDateTime>,
}

impl ManualClock {
    pub fn new() -> Arc<ManualClock> {
        Arc::new(ManualClock {
            now: Mutex::new(OffsetDateTime::parse_rfc3339("2000-01-01T00:00:00Z").unwrap()),
        })
    }

    pub fn advance(&self, d: Duration) {
        let mut now = self.now.lock().unwrap();
        *now = add_duration(*now, d);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> OffsetDateTime {
        *self.now.lock().unwrap()
    }

    fn sleep(&self, duration: Duration, stop: &StopToken) -> bool {
        let until = add_duration(self.now(), duration);
        loop {
            if stop.is_stopped() {
                return false;
            }
            if self.now() >= until {
                return true;
            }
            stop.wait(Duration::from_millis(1));
        }
    }
}

/// One captured log entry.
#[derive(Debug, Clone)]
pub struct Entry {
    pub level: Level,
    pub category: String,
    pub message: String,
    pub exception: Option<DotnetError>,
}

/// `ListLogger<T>`.
#[derive(Default)]
pub struct ListLogger {
    entries: Mutex<Vec<Entry>>,
}

impl ListLogger {
    pub fn new() -> Arc<ListLogger> {
        Arc::new(ListLogger::default())
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.entries.lock().unwrap().clone()
    }

    pub fn at(&self, level: Level) -> Vec<Entry> {
        self.entries()
            .into_iter()
            .filter(|e| e.level == level)
            .collect()
    }
}

impl Logger for ListLogger {
    fn log(&self, r: &Record<'_>) {
        self.entries.lock().unwrap().push(Entry {
            level: r.level,
            category: r.category.to_owned(),
            message: r.message.to_owned(),
            exception: r.exception.cloned(),
        });
    }
}

/// A unique temp path.
pub fn temp_path(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()))
}

/// An outbox in its own temp directory, deleted on drop.
pub struct TempOutbox {
    pub dir: PathBuf,
}

impl TempOutbox {
    pub fn new() -> TempOutbox {
        TempOutbox {
            dir: temp_path("agent-outbox"),
        }
    }

    pub fn count(&self) -> usize {
        std::fs::read_dir(&self.dir).map_or(0, |d| {
            d.filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
                .count()
        })
    }

    pub fn open(&self, log: Arc<dyn Logger>) -> ResultOutbox {
        ResultOutbox::new(&self.dir, log)
    }

    pub fn reporter(
        &self,
        queue: Arc<dyn CommandQueue>,
        clock: Arc<dyn Clock>,
        log: Arc<dyn Logger>,
    ) -> ResultReporter {
        ResultReporter::new(queue, self.open(log.clone()), clock, log)
    }
}

impl Drop for TempOutbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One row of [`FakeQueue`].
#[derive(Debug, Clone)]
pub struct Row {
    pub id: Uuid,
    pub kind: String,
    pub payload: Value,
    pub status: String,
    pub claim_token: Option<Uuid>,
    pub attempts: i32,
    pub result: Option<Value>,
    pub error: Option<DeviceError>,
}

/// `FakeCommandQueue`: claim token, stale claim, idempotent repeat.
pub struct FakeQueue {
    rows: Mutex<Vec<Row>>,
    calls: Mutex<Vec<String>>,
    clock: Arc<dyn Clock>,
    pub fail_next_claim: Mutex<Option<QueueError>>,
    /// Each send pops one of these and fails with it; when empty the send is applied.
    pub fail_sends: Mutex<VecDeque<QueueError>>,
    sent: Mutex<Vec<PendingResult>>,
}

impl FakeQueue {
    /// `FakeCommandQueue.Lease`.
    pub const LEASE: Duration = Duration::from_secs(60);

    pub fn new(clock: Arc<dyn Clock>) -> Arc<FakeQueue> {
        Arc::new(FakeQueue {
            rows: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            clock,
            fail_next_claim: Mutex::new(None),
            fail_sends: Mutex::new(VecDeque::new()),
            sent: Mutex::new(Vec::new()),
        })
    }

    pub fn enqueue(&self, kind: &str, payload_json: &str) -> Uuid {
        let id = Uuid::new_v4();
        self.rows.lock().unwrap().push(Row {
            id,
            kind: kind.to_owned(),
            payload: parse_json(payload_json.as_bytes()).unwrap(),
            status: "queued".into(),
            claim_token: None,
            attempts: 0,
            result: None,
            error: None,
        });
        id
    }

    pub fn read(&self, id: Uuid) -> Row {
        self.rows
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .expect("row exists")
    }

    pub fn fail_send(&self, e: QueueError) {
        self.fail_sends.lock().unwrap().push_back(e);
    }

    pub fn fail_claim(&self, e: QueueError) {
        *self.fail_next_claim.lock().unwrap() = Some(e);
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    pub fn sent(&self) -> Vec<PendingResult> {
        self.sent.lock().unwrap().clone()
    }
}

impl CommandQueue for FakeQueue {
    fn claim(&self) -> Result<Option<DeviceCommand>, QueueError> {
        self.calls.lock().unwrap().push("claim".into());
        if let Some(e) = self.fail_next_claim.lock().unwrap().take() {
            return Err(e);
        }
        let mut rows = self.rows.lock().unwrap();
        let Some(row) = rows.iter_mut().find(|r| r.status == "queued") else {
            return Ok(None);
        };
        row.status = "claimed".into();
        let token = Uuid::new_v4();
        row.claim_token = Some(token);
        row.attempts += 1;
        Ok(Some(DeviceCommand {
            id: row.id,
            kind: row.kind.clone(),
            payload: row.payload.clone(),
            claim_token: token,
            attempt: row.attempts,
            lease_until: add_duration(self.clock.now(), Self::LEASE),
        }))
    }

    fn send_result(&self, result: &PendingResult) -> Result<(), QueueError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("send:{}", result.command_id));
        self.sent.lock().unwrap().push(result.clone());
        if let Some(e) = self.fail_sends.lock().unwrap().pop_front() {
            return Err(e);
        }
        let mut rows = self.rows.lock().unwrap();
        let row = rows
            .iter_mut()
            .find(|r| r.id == result.command_id)
            .expect("row exists");
        let stale = |reason| {
            Err(QueueError::StaleClaim(StaleClaim {
                command_id: row_id(result),
                reason,
            }))
        };
        if row.claim_token != Some(result.body.claim_token) {
            return stale(StaleClaimReason::NotHeld);
        }
        if row.status == "done" || row.status == "failed" {
            return if row.status == result.body.outcome {
                Ok(())
            } else {
                stale(StaleClaimReason::OutcomeConflict)
            };
        }
        row.status = result.body.outcome.clone();
        row.result = result.body.result.clone();
        row.error = result.body.error.clone();
        Ok(())
    }
}

fn row_id(result: &PendingResult) -> Uuid {
    result.command_id
}

type IssueHook = Arc<dyn Fn(&Cx) -> Result<Value, AdapterError> + Send + Sync>;

/// `FakeAdapter`: `{"ok":true}` for every verb unless `on_issue` is set (issue, duplicate
/// and extend use it).
#[derive(Default)]
pub struct FakeAdapter {
    on_issue: Mutex<Option<IssueHook>>,
}

impl FakeAdapter {
    pub fn new() -> Arc<FakeAdapter> {
        Arc::new(FakeAdapter::default())
    }

    pub fn on_issue(&self, f: impl Fn(&Cx) -> Result<Value, AdapterError> + Send + Sync + 'static) {
        *self.on_issue.lock().unwrap() = Some(Arc::new(f));
    }

    fn ok() -> Result<Value, AdapterError> {
        Ok(json!({"ok": true}))
    }
}

impl DeviceAdapter for FakeAdapter {
    fn family(&self) -> &str {
        "fake"
    }

    fn issue(&self, _: &KeyCardRequest, cx: &Cx) -> Result<Value, AdapterError> {
        let hook = self.on_issue.lock().unwrap().clone();
        match hook {
            Some(f) => f(cx),
            None => Self::ok(),
        }
    }

    fn duplicate(&self, r: &KeyCardRequest, cx: &Cx) -> Result<Value, AdapterError> {
        self.issue(r, cx)
    }

    fn read(&self, _: &ReadRequest, _: &Cx) -> Result<Value, AdapterError> {
        Self::ok()
    }

    fn erase(&self, _: &EraseRequest, _: &Cx) -> Result<Value, AdapterError> {
        Self::ok()
    }

    fn extend(&self, r: &KeyCardRequest, cx: &Cx) -> Result<Value, AdapterError> {
        self.issue(r, cx)
    }

    fn cancel_all(&self, _: &CancelAllRequest, _: &Cx) -> Result<Value, AdapterError> {
        Self::ok()
    }
}

/// A request the fake API received.
#[derive(Debug, Clone)]
pub struct SentRequest {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl SentRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

type Script = Arc<Mutex<VecDeque<(u16, Option<String>)>>>;

/// The API on 127.0.0.1: answers each request from a script (status, JSON body) with
/// `Connection: close`, and records what was sent. Unscripted requests get 500.
pub struct FakeApi {
    addr: SocketAddr,
    script: Script,
    requests: Arc<Mutex<Vec<SentRequest>>>,
    done: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FakeApi {
    pub fn start() -> FakeApi {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let script: Script = Arc::default();
        let requests: Arc<Mutex<Vec<SentRequest>>> = Arc::default();
        let done = Arc::new(AtomicBool::new(false));
        let (s, r, d) = (script.clone(), requests.clone(), done.clone());
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if d.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(stream) = stream {
                    serve(stream, &s, &r);
                }
            }
        });
        FakeApi {
            addr,
            script,
            requests,
            done,
            handle: Some(handle),
        }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn then(&self, status: u16, body: Option<&str>) -> &FakeApi {
        self.script
            .lock()
            .unwrap()
            .push_back((status, body.map(str::to_owned)));
        self
    }

    pub fn requests(&self) -> Vec<SentRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for FakeApi {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn serve(mut stream: TcpStream, script: &Script, requests: &Mutex<Vec<SentRequest>>) {
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    requests.lock().unwrap().push(request);
    let (status, body) = script
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or((500, Some("unscripted".into())));
    respond(&mut stream, status, &body.unwrap_or_default());
}

/// Reads one HTTP/1.1 request (head and `Content-Length` body); `None` when the peer
/// closed or stalled for 5 s before the head was complete.
pub fn read_request(stream: &mut TcpStream) -> Option<SentRequest> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_owned();
    let target = first.next().unwrap_or("").to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end..].to_vec();
    while body.len() < length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    Some(SentRequest {
        method,
        target,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// Writes a JSON response with `Connection: close`.
pub fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let response = format!(
        "HTTP/1.1 {status} Scripted\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}
