//! A fake Check-In cloud on a loopback socket: opens the agent's sealed `agent.poll`
//! envelopes and answers what the test's script says, sealed the way the cloud seals
//! (`cloud.poll-response`). HTTP/1.1, one request per connection.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dl_agent_arrival::agent::secrets::Identity;
use dl_agent_arrival::contracts::messages::{
    HEADER_REFUSED, REFUSED_CLOCK, TYPE_POLL, TYPE_POLL_RESPONSE,
};
use dl_agent_arrival::contracts::{
    AgentPollResponse, Direction, EnvelopeCipher, PmsRequest, PmsResponse, SyncEnvelope,
};
use dl_json::Value;

/// The replay window of the real cloud.
pub const WINDOW_MS: i64 = 120_000;

/// What the cloud answers.
#[derive(Debug, Clone)]
pub enum Reply {
    /// A sealed reply carrying these requests.
    Requests(Vec<PmsRequest>),
    /// An empty sealed reply stamped this many milliseconds away from now.
    Skewed(i64),
    /// A bare status, with `X-Agent-Refused: clock` when `clock`.
    Refuse { status: u16, clock: bool },
}

/// One request the cloud received, opened.
#[derive(Debug, Clone)]
pub struct Exchange {
    /// `/api/agent/poll` or `/api/agent/results`.
    pub path: String,
    /// The opened `AgentPollRequest` JSON.
    pub body: Value,
}

/// The cloud's view when the script decides.
#[derive(Debug)]
pub struct Seen<'a> {
    /// This request.
    pub exchange: &'a Exchange,
    /// Polls before this one.
    pub polls: usize,
    /// Every answer the agent sent so far.
    pub answered: &'a [PmsResponse],
}

type Script = dyn Fn(&Seen<'_>) -> Reply + Send + Sync;

#[derive(Default)]
struct State {
    exchanges: Vec<Exchange>,
    answered: Vec<PmsResponse>,
    polls: usize,
}

/// A running fake cloud; stops on drop.
pub struct FakeCloud {
    /// The base URL for `CloudBaseUrl`.
    pub url: String,
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    done: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeCloud {
    /// Serves `identity`'s agent with `script`.
    pub fn start(
        identity: &Identity,
        script: impl Fn(&Seen<'_>) -> Reply + Send + Sync + 'static,
    ) -> FakeCloud {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let cipher = identity.cipher().expect("cipher");
        let state = Arc::new(Mutex::new(State::default()));
        let done = Arc::new(AtomicBool::new(false));
        let script: Arc<Script> = Arc::new(script);
        let (st, dn) = (Arc::clone(&state), Arc::clone(&done));
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if dn.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(stream) = stream else { continue };
                if let Err(e) = serve(stream, &cipher, &st, script.as_ref()) {
                    eprintln!("fake cloud: {e}");
                }
            }
        });
        FakeCloud {
            url: format!("http://{addr}"),
            addr,
            state,
            done,
            thread: Some(thread),
        }
    }

    /// Every opened request so far.
    pub fn exchanges(&self) -> Vec<Exchange> {
        lock(&self.state).exchanges.clone()
    }

    /// Every answer the agent posted so far.
    pub fn answered(&self) -> Vec<PmsResponse> {
        lock(&self.state).answered.clone()
    }

    /// The answer to `request_id`, once posted.
    pub fn answer(&self, request_id: &str) -> Option<PmsResponse> {
        self.answered()
            .into_iter()
            .find(|r| r.request_id == request_id)
    }

    /// Waits up to `limit` for `done`; `true` when it held.
    pub fn wait(&self, limit: Duration, done: impl Fn(&FakeCloud) -> bool) -> bool {
        wait_until(limit, || done(self))
    }
}

impl Drop for FakeCloud {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        // Wakes the accept loop.
        drop(TcpStream::connect(self.addr));
        if let Some(t) = self.thread.take()
            && t.join().is_err()
        {
            eprintln!("fake cloud thread panicked");
        }
    }
}

/// Polls `done` every 20 ms for up to `limit`.
pub fn wait_until(limit: Duration, done: impl Fn() -> bool) -> bool {
    let end = Instant::now() + limit;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn lock(m: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> i64 {
    dl_time::OffsetDateTime::now_utc().unix_millis()
}

fn serve(
    stream: TcpStream,
    cipher: &EnvelopeCipher,
    state: &Mutex<State>,
    script: &Script,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    let path = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).map_err(|e| e.to_string())?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value
                .trim()
                .parse()
                .map_err(|_| "bad content-length".to_string())?;
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    let text = String::from_utf8(body).map_err(|e| e.to_string())?;
    let envelope = SyncEnvelope::parse(&text).map_err(|e| e.to_string())?;

    // The real cloud refuses a sender whose clock is outside the window.
    if (envelope.timestamp - now_ms()).abs() > WINDOW_MS {
        return respond(stream, 401, &[(HEADER_REFUSED, REFUSED_CLOCK)], "");
    }
    let opened = cipher
        .open(Direction::AgentToCloud, TYPE_POLL, &envelope)
        .map_err(|e| e.to_string())?;
    let exchange = Exchange { path, body: opened };
    let reply = {
        let mut s = lock(state);
        if let Some(list) = exchange.body.get("responses").and_then(Value::as_array) {
            for r in list {
                s.answered.push(PmsResponse::from_json(r)?);
            }
        }
        let seen = Seen {
            exchange: &exchange,
            polls: s.polls,
            answered: &s.answered,
        };
        let reply = script(&seen);
        if exchange.path.ends_with("/poll") {
            s.polls += 1;
        }
        s.exchanges.push(exchange);
        reply
    };
    let (requests, stamp) = match reply {
        Reply::Requests(requests) => (requests, now_ms()),
        Reply::Skewed(offset) => (Vec::new(), now_ms() + offset),
        Reply::Refuse { status, clock } => {
            let headers: &[(&str, &str)] = if clock {
                &[(HEADER_REFUSED, REFUSED_CLOCK)]
            } else {
                &[]
            };
            return respond(stream, status, headers, "");
        }
    };
    let plain = AgentPollResponse {
        requests,
        next_poll_delay_ms: 100,
    }
    .to_json_string();
    let sealed = cipher
        .seal(Direction::CloudToAgent, TYPE_POLL_RESPONSE, &plain, stamp)
        .map_err(|e| e.to_string())?;
    respond(
        stream,
        200,
        &[("Content-Type", "application/json")],
        &sealed.to_json_string(),
    )
}

fn respond(
    mut stream: TcpStream,
    status: u16,
    headers: &[(&str, &str)],
    body: &str,
) -> Result<(), String> {
    let mut out = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(body);
    stream
        .write_all(out.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())
}
