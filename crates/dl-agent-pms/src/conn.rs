//! The agent's outbound WebSocket to `/agent/v1/stream`: hello first, a pong for every
//! ping, ops handed to the executor (at most 8 in flight, each bounded by its deadline,
//! results written in order of completion), exponential reconnect with jitter capped at
//! 30 s. A supersede (4409), a revoked agent or an unsupported contract stops the loop
//! instead of reconnecting. Args and results are never logged. `update` hints are ignored:
//! dl-agent updates itself.
//!
//! The session thread owns the socket: it reads with a short timeout and, between reads,
//! sends the frames that op and migration workers queue through a [`Writer`].

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dl_http::Upgraded;
use dl_ws::{CloseFrame, ConnectOptions, Message, WebSocket};

use crate::attrs;
use crate::ctx::Ctx;
use crate::enroll::{EnrollError, TokenProvider};
use crate::exec::{self, Executor, SchemaVersion};
use crate::log::Logger;
use crate::telemetry::{DownKind, Telemetry};
use crate::wire::{
    self, Frame, Migrate, MigrateError, MigrateProgress, MigrateResult, Op, OpResult,
};

/// Why the stream loop ended for good.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnError {
    /// Another agent session for this site took over (4409).
    Superseded,
    /// The cloud refused this agent's credentials (revoked or unknown).
    Revoked,
    /// The cloud does not accept this agent's protocol or contract (4426).
    ContractUnsupported,
    /// A transient failure (reconnect).
    Other(String),
}

impl ConnError {
    /// Superseded, revoked or contract unsupported: do not reconnect.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, ConnError::Other(_))
    }
}

impl fmt::Display for ConnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnError::Superseded => f.write_str(
                "another agent session for this site took over (AGENT_SUPERSEDED)",
            ),
            ConnError::Revoked => {
                f.write_str("the cloud refused this agent's credentials (revoked or unknown)")
            }
            ConnError::ContractUnsupported => f.write_str(
                "the cloud does not accept this agent's protocol or contract (CONTRACT_UNSUPPORTED)",
            ),
            ConnError::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ConnError {}

/// The reason given when the stream refuses the bearer token (HTTP 401 or close 4401).
pub const TOKEN_REFUSED: &str = "stream refused the token";

fn other(m: impl Into<String>) -> ConnError {
    ConnError::Other(m.into())
}

/// Runs a cloud-requested migration; blocks until the run ends and returns its result.
/// `progress` sends a frame on the live stream (best effort). Enforces its own single
/// flight.
pub trait MigrationHandler: Send + Sync {
    /// Runs one request.
    fn handle(&self, ctx: &Ctx, req: &Migrate, progress: ProgressSink) -> MigrateResult;
}

/// Where a migration's progress frames go.
pub type ProgressSink = Arc<dyn Fn(MigrateProgress) + Send + Sync>;

/// Stream settings.
pub struct Options {
    /// Tenant base URL (`https://…`).
    pub cloud_url: String,
    /// Shared HTTP client (proxy, TLS).
    pub http: Arc<dl_http::Client>,
    /// Bearer tokens.
    pub tokens: Arc<dyn TokenProvider>,
    /// The op executor.
    pub exec: Arc<dyn Executor>,
    /// The schema version for hello and pong.
    pub schema: Arc<dyn SchemaVersion>,
    /// Logger.
    pub log: Logger,
    /// Build version.
    pub agent_version: String,
    /// Runs migrate frames; `None` answers each refused/MIGRATE_UNSUPPORTED.
    pub migrator: Option<Arc<dyn MigrationHandler>>,
    /// In-flight cap.
    pub inflight_cap: usize,
    /// No frame from the cloud this long: reconnect.
    pub idle_timeout: Duration,
    /// In-flight ops get this long on shutdown.
    pub shutdown_grace: Duration,
    /// Per-frame write limit.
    pub write_timeout: Duration,
    /// Reconnect back-off.
    pub backoff: fn(u32) -> Duration,
    /// Counters and reports for the module.
    pub telemetry: Arc<Telemetry>,
}

impl Options {
    /// Options with the protocol defaults.
    pub fn new(
        cloud_url: &str,
        http: Arc<dl_http::Client>,
        tokens: Arc<dyn TokenProvider>,
        exec: Arc<dyn Executor>,
    ) -> Options {
        Options {
            cloud_url: cloud_url.to_owned(),
            http,
            tokens,
            exec,
            schema: Arc::new(exec::UnknownSchema),
            log: Logger::discard(),
            agent_version: String::new(),
            migrator: None,
            inflight_cap: wire::INFLIGHT_CAP,
            idle_timeout: wire::HEARTBEAT * wire::MISSED_PONGS_LIMIT + Duration::from_secs(5),
            shutdown_grace: Duration::from_secs(5),
            write_timeout: Duration::from_secs(10),
            backoff,
            telemetry: Arc::new(Telemetry::new(None)),
        }
    }
}

/// 1, 2, 4 … s capped at 30 s, with jitter in [d/2, d].
pub fn backoff(attempt: u32) -> Duration {
    let attempt = attempt.max(1);
    let d = if attempt < 6 {
        Duration::from_secs(1u64 << (attempt - 1)).min(wire::BACKOFF_MAX)
    } else {
        wire::BACKOFF_MAX
    };
    let half = u64::try_from(d.as_nanos() / 2).unwrap_or(u64::MAX);
    Duration::from_nanos(half + dl_rand::range(0..=half))
}

const SCHEMA_REFRESH: Duration = Duration::from_secs(300);
/// How long one socket read waits before queued frames get their turn: the latency a
/// worker's result can see on an otherwise idle stream.
const READ_SLICE: Duration = Duration::from_millis(25);

/// The stream loop.
pub struct Client {
    o: Options,
    started: Instant,
    schema: Mutex<(i64, Option<Instant>)>,
}

type Ws = WebSocket<Upgraded>;
type Reply = SyncSender<Result<(), String>>;
type Queue = Arc<Mutex<Option<Sender<(Frame, Reply)>>>>;

/// Queues frames for the session thread to send.
#[derive(Clone)]
pub struct Writer {
    tx: Queue,
    timeout: Duration,
}

impl Writer {
    /// Sends `f` and waits until it is written (or the connection is gone).
    pub fn write(&self, f: Frame) -> Result<(), String> {
        let name = f.frame_type();
        let (rtx, rrx) = mpsc::sync_channel(1);
        {
            let guard = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(tx) = guard.as_ref() else {
                return Err(format!("write {name}: connection closed"));
            };
            tx.send((f, rtx))
                .map_err(|_| format!("write {name}: connection closed"))?;
        }
        match rrx.recv_timeout(self.timeout + Duration::from_secs(1)) {
            Ok(r) => r,
            Err(_) => Err(format!("write {name}: connection closed")),
        }
    }

    fn close(&self) {
        *self.tx.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// State shared by the session thread and its workers.
struct Shared {
    closing: AtomicBool,
    inflight: AtomicUsize,
    /// Ops run under this; it outlives a shutdown request by the grace period.
    ops: Ctx,
    writer: Writer,
}

fn encode(f: &Frame) -> Result<wire::Message, String> {
    wire::encode(f).map_err(|e| e.to_string())
}

fn send_ws(ws: &mut Ws, f: &Frame) -> Result<(), String> {
    let msg = encode(f)?;
    let r = if msg.binary {
        ws.send_binary(&msg.data)
    } else {
        // Encoded JSON is UTF-8 by construction.
        ws.send_text(&String::from_utf8_lossy(&msg.data))
    };
    r.map_err(|e| format!("write {}: {e}", f.frame_type()))
}

fn pump(ws: &mut Ws, rx: &Receiver<(Frame, Reply)>, wait: Duration) {
    let mut first = true;
    loop {
        let item = if first && !wait.is_zero() {
            match rx.recv_timeout(wait) {
                Ok(i) => i,
                Err(_) => return,
            }
        } else {
            match rx.try_recv() {
                Ok(i) => i,
                Err(_) => return,
            }
        };
        first = false;
        let (f, reply) = item;
        let _ = reply.send(send_ws(ws, &f));
    }
}

impl Client {
    /// A client over `o`.
    pub fn new(o: Options) -> Client {
        Client {
            o,
            started: Instant::now(),
            schema: Mutex::new((0, None)),
        }
    }

    /// The schema for a pong: cached, re-read at most every 5 minutes; a failed read keeps
    /// the last known value.
    fn schema_version(&self, life: &Ctx) -> i64 {
        let mut s = self.schema.lock().unwrap_or_else(PoisonError::into_inner);
        if s.1.is_some_and(|at| at.elapsed() < SCHEMA_REFRESH) {
            return s.0;
        }
        s.1 = Some(Instant::now());
        let q = life.with_timeout(Duration::from_secs(2));
        match self.o.schema.schema_version(&q) {
            Ok(v) if v >= 0 => {
                s.0 = v;
                v
            }
            r => {
                let reason = r.err().unwrap_or_default();
                self.o
                    .log
                    .warn("agent.schema_version.unknown", attrs!["reason" => reason]);
                s.0
            }
        }
    }

    fn pong(&self, life: &Ctx, ts: i64) -> Frame {
        Frame::Pong(wire::Pong {
            ts,
            agent_version: self.o.agent_version.clone(),
            schema_version: self.schema_version(life),
            uptime_s: i64::try_from(self.started.elapsed().as_secs()).unwrap_or(i64::MAX),
        })
    }

    /// Keeps the stream up until `ctx` ends (`Ok`) or a terminal error.
    pub fn run(&self, ctx: &Ctx) -> Result<(), ConnError> {
        let mut attempt = 0u32;
        loop {
            let (established, res) = self.session(ctx);
            let tel = &self.o.telemetry;
            if ctx.is_done() {
                tel.stopped();
                return Ok(());
            }
            let err = match res {
                Err(e) if e.is_terminal() => {
                    let kind = if e == ConnError::Revoked {
                        DownKind::Auth
                    } else {
                        DownKind::Stream
                    };
                    tel.disconnected(kind, &e.to_string());
                    return Err(e);
                }
                Err(e) => e.to_string(),
                Ok(()) => String::new(),
            };
            let kind = if err == TOKEN_REFUSED {
                DownKind::Auth
            } else {
                DownKind::Stream
            };
            tel.disconnected(kind, &err);
            if established {
                attempt = 0;
            }
            attempt += 1;
            let wait = (self.o.backoff)(attempt);
            self.o.log.warn(
                "agent.stream.reconnect",
                attrs![
                    "attempt" => attempt,
                    "in_ms" => i64::try_from(wait.as_millis()).unwrap_or(i64::MAX),
                    "reason" => err
                ],
            );
            if !ctx.sleep(wait) {
                return Ok(());
            }
        }
    }

    fn dial(&self, ctx: &Ctx) -> Result<Ws, ConnError> {
        let token = match self.o.tokens.token(ctx) {
            Ok(t) => t,
            Err(EnrollError::Refused) => return Err(ConnError::Revoked),
            Err(e) => return Err(other(format!("get token: {e}"))),
        };
        let opts = ConnectOptions {
            headers: vec![("Authorization".into(), format!("Bearer {token}"))],
            timeout: Some(Duration::from_secs(30)),
            config: dl_ws::Config {
                max_message_size: wire::MAX_FRAME_BYTES,
                ..dl_ws::Config::default()
            },
            ..ConnectOptions::default()
        };
        let url = format!("{}{}", self.o.cloud_url, wire::STREAM_PATH);
        match dl_ws::connect_with(&self.o.http, &url, &opts) {
            Ok((ws, _)) => Ok(ws),
            Err(dl_ws::Error::Rejected { status: 401, .. }) => {
                self.o.tokens.invalidate();
                Err(other(TOKEN_REFUSED))
            }
            Err(dl_ws::Error::Rejected { status, .. }) => {
                Err(other(format!("dial stream: HTTP {status}")))
            }
            Err(e) => Err(other(format!("dial stream: {e}"))),
        }
    }

    fn classify_close(&self, code: u16) -> ConnError {
        match code {
            wire::CLOSE_AGENT_SUPERSEDED => ConnError::Superseded,
            wire::CLOSE_CONTRACT_UNSUPPORTED => ConnError::ContractUnsupported,
            wire::CLOSE_UNAUTHORIZED => {
                self.o.tokens.invalidate();
                other(TOKEN_REFUSED)
            }
            c => other(format!("stream closed with code {c}")),
        }
    }

    /// One connection; the bool is true once the cloud answered the hello.
    fn session(&self, ctx: &Ctx) -> (bool, Result<(), ConnError>) {
        let mut ws = match self.dial(ctx) {
            Ok(ws) => ws,
            Err(e) => return (false, Err(e)),
        };
        let _ = ws.set_read_timeout(Some(READ_SLICE));
        let _ = ws.set_write_timeout(Some(self.o.write_timeout));
        let schema = match self.o.schema.schema_version(ctx) {
            Ok(v) if v >= 0 => v,
            r => {
                let reason = r.err().unwrap_or_default();
                self.o
                    .log
                    .warn("agent.schema_version.unknown", attrs!["reason" => reason]);
                wire::SCHEMA_VERSION_UNKNOWN
            }
        };
        let hello = Frame::Hello(wire::Hello {
            protocol: wire::PROTOCOL_VERSION,
            agent_version: self.o.agent_version.clone(),
            schema_version: schema,
            contract_max: self.o.exec.contract_max(),
        });
        if let Err(e) = send_ws(&mut ws, &hello) {
            return (false, Err(other(e)));
        }
        self.o.log.info("agent.stream.connected", &[]);
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            closing: AtomicBool::new(false),
            inflight: AtomicUsize::new(0),
            ops: Ctx::background(),
            writer: Writer {
                tx: Arc::new(Mutex::new(Some(tx))),
                timeout: self.o.write_timeout,
            },
        });
        let mut ready = false;
        let res = self.read_loop(ctx, &mut ws, &rx, &shared, &mut ready);
        match res {
            Some(err) => {
                // abort: in-flight results cannot be sent any more.
                shared.closing.store(true, Ordering::SeqCst);
                shared.writer.close();
                drop(rx);
                shared.ops.cancel();
                wait_idle(&shared, None);
                (ready, Err(err))
            }
            None => {
                self.drain(&mut ws, &rx, &shared);
                let _ = ws.close(Some(CloseFrame::new(1000, "agent shutdown")));
                let until = Instant::now() + Duration::from_secs(2);
                while Instant::now() < until {
                    match ws.read() {
                        Err(e) if e.is_timeout() => {}
                        Ok(Message::Close(_)) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
                shared.writer.close();
                self.o.log.info("agent.stream.shutdown", &[]);
                (ready, Ok(()))
            }
        }
    }

    /// Reads until the context ends (`None`) or the connection fails (`Some`).
    fn read_loop(
        &self,
        ctx: &Ctx,
        ws: &mut Ws,
        rx: &Receiver<(Frame, Reply)>,
        shared: &Arc<Shared>,
        ready: &mut bool,
    ) -> Option<ConnError> {
        let mut last = Instant::now();
        loop {
            pump(ws, rx, Duration::ZERO);
            if ctx.is_done() {
                return None;
            }
            let (data, binary) = match ws.read() {
                Ok(Message::Text(s)) => (s.into_bytes(), false),
                Ok(Message::Binary(b)) => (b, true),
                Ok(Message::Close(f)) => {
                    return Some(self.classify_close(f.map_or(1005, |f| f.code)));
                }
                Ok(_) => continue,
                Err(e) if e.is_timeout() => {
                    if last.elapsed() >= self.o.idle_timeout {
                        return Some(other("no frame from the cloud: idle timeout"));
                    }
                    continue;
                }
                Err(e) => return Some(other(e.to_string())),
            };
            last = Instant::now();
            let frame = match wire::decode_cloud_frame(&data, binary) {
                Ok(f) => f,
                Err(e) => {
                    self.o
                        .log
                        .warn("agent.frame.bad", attrs!["reason" => e.to_string()]);
                    let _ = ws.close(Some(CloseFrame::new(wire::CLOSE_BAD_FRAME, "BAD_FRAME")));
                    return Some(other(format!("bad frame from the cloud: {e}")));
                }
            };
            if !*ready {
                *ready = true;
                self.o.telemetry.connected();
            }
            match frame {
                Frame::Ping(p) => {
                    let pong = self.pong(ctx, p.ts);
                    if let Err(e) = send_ws(ws, &pong) {
                        return Some(other(e));
                    }
                }
                Frame::Cfg(c) => self.o.log.info(
                    "agent.stream.cfg",
                    attrs!["inflight_cap" => c.inflight_cap, "heartbeat_ms" => c.heartbeat_ms],
                ),
                Frame::Op(op) => self.dispatch(ws, shared, op),
                Frame::Migrate(m) => self.migrate(ctx, ws, shared, m),
                Frame::Update(u) => self.o.log.info(
                    "agent.update.ignored",
                    attrs!["version" => u.version, "reason" => "dl-agent updates itself"],
                ),
                other_frame => self.o.log.warn(
                    "agent.frame.unsupported",
                    attrs!["type" => other_frame.frame_type()],
                ),
            }
        }
    }

    fn dispatch(&self, ws: &mut Ws, shared: &Arc<Shared>, op: Op) {
        if !shared.closing.load(Ordering::SeqCst) {
            let mut taken = false;
            let mut n = shared.inflight.load(Ordering::SeqCst);
            while n < self.o.inflight_cap {
                match shared
                    .inflight
                    .compare_exchange(n, n + 1, Ordering::SeqCst, Ordering::SeqCst)
                {
                    Ok(_) => {
                        taken = true;
                        break;
                    }
                    Err(cur) => n = cur,
                }
            }
            if taken {
                let sh = shared.clone();
                let exec = self.o.exec.clone();
                let log = self.o.log.clone();
                let tel = self.o.telemetry.clone();
                tel.op_started();
                std::thread::spawn(move || {
                    run_op(&sh, exec, &log, &tel, op);
                    tel.op_ended();
                    sh.inflight.fetch_sub(1, Ordering::SeqCst);
                });
                return;
            }
        }
        self.o
            .log
            .warn("agent.op.busy", attrs!["op_id" => &op.id, "op" => &op.op]);
        let res = exec::failed(
            &op.id,
            wire::ERR_AGENT_BUSY,
            "agent is at its in-flight limit or shutting down",
        );
        self.o.telemetry.op_done(&op, &res);
        let r = send_ws(ws, &Frame::Result(res.clone()));
        log_sent(&self.o.log, &op, &res, Duration::ZERO, r);
    }

    fn migrate(&self, life: &Ctx, ws: &mut Ws, shared: &Arc<Shared>, req: Migrate) {
        self.o.log.info(
            "agent.migrate.requested",
            attrs!["run_id" => &req.id, "bundle_id" => &req.bundle_id, "dry_run" => req.dry_run],
        );
        let Some(h) = self.o.migrator.clone() else {
            let res = MigrateResult {
                id: req.id.clone(),
                status: wire::MIGRATE_REFUSED.into(),
                bundle_id: req.bundle_id.clone(),
                error: Some(MigrateError {
                    code: wire::MIGRATE_UNSUPPORTED.into(),
                    ..MigrateError::default()
                }),
                ..MigrateResult::default()
            };
            self.o.telemetry.migration_done(&res);
            if let Err(e) = send_ws(ws, &Frame::MigrateResult(res.clone())) {
                self.o.log.warn(
                    "agent.migrate.result_unsent",
                    attrs!["run_id" => &res.id, "status" => &res.status, "reason" => e],
                );
            }
            return;
        };
        let writer = shared.writer.clone();
        let log = self.o.log.clone();
        let tel = self.o.telemetry.clone();
        let life = life.clone();
        std::thread::spawn(move || {
            let w = writer.clone();
            let l = log.clone();
            let id = req.id.clone();
            let progress: ProgressSink = Arc::new(move |p: MigrateProgress| {
                if let Err(e) = w.write(Frame::MigrateProgress(p)) {
                    l.warn(
                        "agent.migrate.progress_unsent",
                        attrs!["run_id" => &id, "reason" => e],
                    );
                }
            });
            let res = h.handle(&life, &req, progress);
            tel.migration_done(&res);
            if let Err(e) = writer.write(Frame::MigrateResult(res.clone())) {
                log.warn(
                    "agent.migrate.result_unsent",
                    attrs!["run_id" => &res.id, "status" => &res.status, "reason" => e],
                );
            }
        });
    }

    /// Stops new ops and gives in-flight ones the grace period to finish and report,
    /// sending their results meanwhile.
    fn drain(&self, ws: &mut Ws, rx: &Receiver<(Frame, Reply)>, shared: &Arc<Shared>) {
        shared.closing.store(true, Ordering::SeqCst);
        let until = Instant::now() + self.o.shutdown_grace;
        while shared.inflight.load(Ordering::SeqCst) > 0 && Instant::now() < until {
            pump(ws, rx, Duration::from_millis(20));
        }
        shared.ops.cancel();
        while shared.inflight.load(Ordering::SeqCst) > 0 {
            pump(ws, rx, Duration::from_millis(20));
        }
        pump(ws, rx, Duration::ZERO);
    }
}

fn wait_idle(shared: &Shared, limit: Option<Duration>) {
    let start = Instant::now();
    while shared.inflight.load(Ordering::SeqCst) > 0 {
        if limit.is_some_and(|l| start.elapsed() >= l) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn log_sent(log: &Logger, op: &Op, res: &OpResult, took: Duration, r: Result<(), String>) {
    match r {
        Err(e) => log.warn(
            "agent.op.result_unsent",
            attrs!["op_id" => &op.id, "op" => &op.op, "reason" => e],
        ),
        Ok(()) => {
            let code = res.error.as_ref().map_or("", |e| e.code.as_str());
            log.info(
                "agent.op.done",
                attrs![
                    "op_id" => &op.id,
                    "op" => &op.op,
                    "status" => &res.status,
                    "code" => code,
                    "ms" => i64::try_from(took.as_millis()).unwrap_or(i64::MAX)
                ],
            );
        }
    }
}

/// One op on a worker: the executor runs on its own thread so a deadline (or a forced
/// shutdown) answers OP_TIMEOUT at once even when the database call is still running.
fn run_op(shared: &Arc<Shared>, exec: Arc<dyn Executor>, log: &Logger, tel: &Telemetry, op: Op) {
    let started = Instant::now();
    let deadline = Duration::from_millis(u64::try_from(op.deadline_ms).unwrap_or(0));
    let ctx = shared.ops.with_timeout(deadline);
    let (tx, rx) = mpsc::sync_channel(1);
    {
        let ctx = ctx.clone();
        let op = op.clone();
        std::thread::spawn(move || {
            let _ = tx.send(exec.execute(&ctx, &op));
        });
    }
    let mut res = loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(r) => break r,
            Err(RecvTimeoutError::Disconnected) => {
                break exec::failed(&op.id, exec::CODE_DB_UNAVAILABLE, "database_unavailable");
            }
            Err(RecvTimeoutError::Timeout) if ctx.is_done() => {
                // The cloud has already timed the op out: informational only.
                break exec::failed(
                    &op.id,
                    wire::ERR_OP_TIMEOUT,
                    "deadline passed before the op finished",
                );
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    };
    res.id = op.id.clone();
    tel.op_done(&op, &res);
    let r = shared.writer.write(Frame::Result(res.clone()));
    log_sent(log, &op, &res, started.elapsed(), r);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_with_jitter_and_caps() {
        for (attempt, max) in [(1u32, 1u64), (2, 2), (3, 4), (5, 16), (6, 30), (40, 30)] {
            for _ in 0..20 {
                let d = backoff(attempt);
                let max = Duration::from_secs(max);
                assert!(d >= max / 2 && d <= max, "{attempt}: {d:?}");
            }
        }
        assert!(backoff(0) <= Duration::from_secs(1));
    }
}
