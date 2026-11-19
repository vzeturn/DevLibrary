//! What the stream loop tells the module: op counters, the stream's up/down state, and
//! problem reports with stable codes.
//!
//! Reports carry op names, error codes, SQL Server error numbers and op ids only, never
//! SQL text, bind values or error messages (a SQL Server message can quote a value).
//! Each code is reported at most once per [`REPORT_EVERY`]; the next report says how many
//! were held back.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dl_json::{Map, Value};

use crate::exec;
use crate::wire::{self, Op, OpResult};

/// The stream kept failing for longer than one back-off cycle, or the cloud stopped it.
pub const CODE_STREAM_FAILED: &str = "pms_stream_failed";
/// Enrollment, the token exchange or the stream refused this agent's credentials.
pub const CODE_AUTH_FAILED: &str = "pms_auth_failed";
/// An op failed in the site database.
pub const CODE_SQL_FAILED: &str = "pms_sql_failed";
/// The op journal could not record or answer a keyed write, or is at its cap.
pub const CODE_JOURNAL_FAILED: &str = "pms_journal_failed";
/// The agent rejected an op (unknown op, contract or argument mismatch).
pub const CODE_OP_REJECTED: &str = "pms_op_rejected";
/// The PMS config file cannot be read or is invalid.
pub const CODE_CONFIG_INVALID: &str = "pms_config_invalid";

/// The shortest gap between two reports with the same code.
pub const REPORT_EVERY: Duration = Duration::from_secs(600);

/// How bad a report is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Degraded but working.
    Warning,
    /// A feature failed.
    Error,
    /// The module stopped working until someone acts.
    Critical,
}

/// Where reports go (the module forwards them to dl-agent).
pub trait Reporter: Send + Sync {
    /// Records one report.
    fn report(&self, level: Level, code: &str, message: &str, context: Map);
}

/// Why the stream is down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownKind {
    /// Network, protocol or cloud failure.
    Stream,
    /// The cloud refused the token.
    Auth,
}

#[derive(Default)]
struct Down {
    since: Option<Instant>,
    reason: String,
    kind: Option<DownKind>,
    reported: bool,
}

/// Counters and state shared by the stream loop and the module.
pub struct Telemetry {
    inflight: AtomicI64,
    ok: AtomicU64,
    rejected: AtomicU64,
    failed: AtomicU64,
    last_op_unix: AtomicI64,
    reconnects: AtomicU64,
    connected: AtomicBool,
    ever_connected: AtomicBool,
    down: Mutex<Down>,
    limiter: Mutex<HashMap<String, (Instant, u64)>>,
    reporter: Option<Arc<dyn Reporter>>,
    clock: Clock,
}

/// The time source (tests advance it by hand).
pub type Clock = Box<dyn Fn() -> Instant + Send + Sync>;

impl std::fmt::Debug for Telemetry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Telemetry")
    }
}

/// A copy of the counters (`pms.status`, metrics).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    /// Ops running now.
    pub inflight: i64,
    /// Ops answered `applied`.
    pub ok: u64,
    /// Ops answered `rejected`.
    pub rejected: u64,
    /// Ops answered `failed`.
    pub failed: u64,
    /// Unix seconds of the last answered op (0: none yet).
    pub last_op_unix: i64,
    /// Times the stream came back up after it had been up.
    pub reconnects: u64,
    /// The stream is up.
    pub connected: bool,
}

impl Snapshot {
    /// The counters as a JSON object.
    pub fn to_map(&self) -> Map {
        let mut m = Map::new();
        m.insert("connected", self.connected);
        m.insert("ops_inflight", self.inflight);
        m.insert("ops_ok", self.ok);
        m.insert("ops_rejected", self.rejected);
        m.insert("ops_failed", self.failed);
        m.insert("last_op_unix", self.last_op_unix);
        m.insert("reconnects", self.reconnects);
        m
    }
}

/// The stream's health as the module shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// Up.
    Up,
    /// Not up yet, or down for less than the threshold.
    Pending,
    /// Down for longer than the threshold.
    Down {
        /// How long.
        for_secs: u64,
        /// The last failure.
        reason: String,
    },
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn unix_now() -> i64 {
    dl_time::OffsetDateTime::now_utc().unix_seconds()
}

const SQL_CODES: [&str; 4] = [
    exec::CODE_SQL_ERROR,
    exec::CODE_DEADLOCK,
    exec::CODE_LOCK_TIMEOUT,
    exec::CODE_DB_UNAVAILABLE,
];

impl Telemetry {
    /// Counters that report through `reporter` (`None`: count only).
    pub fn new(reporter: Option<Arc<dyn Reporter>>) -> Telemetry {
        Telemetry::with_clock(reporter, Box::new(Instant::now))
    }

    /// [`Telemetry::new`] with a clock (tests).
    pub fn with_clock(reporter: Option<Arc<dyn Reporter>>, clock: Clock) -> Telemetry {
        Telemetry {
            inflight: AtomicI64::new(0),
            ok: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            last_op_unix: AtomicI64::new(0),
            reconnects: AtomicU64::new(0),
            connected: AtomicBool::new(false),
            ever_connected: AtomicBool::new(false),
            down: Mutex::new(Down::default()),
            limiter: Mutex::new(HashMap::new()),
            reporter,
            clock,
        }
    }

    /// A copy of the counters.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            inflight: self.inflight.load(Ordering::SeqCst),
            ok: self.ok.load(Ordering::SeqCst),
            rejected: self.rejected.load(Ordering::SeqCst),
            failed: self.failed.load(Ordering::SeqCst),
            last_op_unix: self.last_op_unix.load(Ordering::SeqCst),
            reconnects: self.reconnects.load(Ordering::SeqCst),
            connected: self.connected.load(Ordering::SeqCst),
        }
    }

    /// Reports `code` unless it was reported less than [`REPORT_EVERY`] ago.
    pub fn report(&self, level: Level, code: &str, message: &str, mut context: Map) {
        let Some(r) = &self.reporter else { return };
        let now = (self.clock)();
        let held = {
            let mut lim = lock(&self.limiter);
            match lim.get_mut(code) {
                Some((at, held)) if now.duration_since(*at) < REPORT_EVERY => {
                    *held += 1;
                    return;
                }
                Some((at, held)) => {
                    *at = now;
                    std::mem::take(held)
                }
                None => {
                    lim.insert(code.to_owned(), (now, 0));
                    0
                }
            }
        };
        if held > 0 {
            context.insert("suppressed", held);
        }
        r.report(level, code, message, context);
    }

    /// An op was handed to a worker.
    pub fn op_started(&self) {
        self.inflight.fetch_add(1, Ordering::SeqCst);
    }

    /// An op worker ended (after [`Telemetry::op_done`]).
    pub fn op_ended(&self) {
        self.inflight.fetch_sub(1, Ordering::SeqCst);
    }

    /// Counts an answered op and reports a rejection or a database failure.
    pub fn op_done(&self, op: &Op, res: &OpResult) {
        self.last_op_unix.store(unix_now(), Ordering::SeqCst);
        let code = res.error.as_ref().map_or("", |e| e.code.as_str());
        let mut ctx = Map::new();
        ctx.insert("op", op.op.as_str());
        ctx.insert("op_id", op.id.as_str());
        ctx.insert("error_code", code);
        if let Some(n) = res.error.as_ref().and_then(|e| e.sql_number) {
            ctx.insert("sql_number", n);
        }
        match res.status.as_str() {
            wire::STATUS_APPLIED => {
                self.ok.fetch_add(1, Ordering::SeqCst);
            }
            wire::STATUS_REJECTED => {
                self.rejected.fetch_add(1, Ordering::SeqCst);
                let msg = format!("op {} rejected: {code}", op.op);
                self.report(Level::Warning, CODE_OP_REJECTED, &msg, ctx);
            }
            _ => {
                self.failed.fetch_add(1, Ordering::SeqCst);
                if SQL_CODES.contains(&code) {
                    let msg = format!("op {} failed in the database: {code}", op.op);
                    self.report(Level::Error, CODE_SQL_FAILED, &msg, ctx);
                }
            }
        }
    }

    /// The cloud answered the hello; counts a reconnect when it was up before.
    pub fn connected(&self) {
        if self.ever_connected.swap(true, Ordering::SeqCst) {
            self.reconnects.fetch_add(1, Ordering::SeqCst);
        }
        self.connected.store(true, Ordering::SeqCst);
        *lock(&self.down) = Down::default();
    }

    /// The stream was closed on purpose (shutdown, reconnect command): not a failure.
    pub fn stopped(&self) {
        self.connected.store(false, Ordering::SeqCst);
        *lock(&self.down) = Down::default();
    }

    /// A session ended with `reason` (empty: the cloud closed it normally).
    pub fn disconnected(&self, kind: DownKind, reason: &str) {
        self.connected.store(false, Ordering::SeqCst);
        let mut d = lock(&self.down);
        if d.since.is_none() {
            d.since = Some((self.clock)());
            d.reported = false;
        }
        if !reason.is_empty() || d.reason.is_empty() {
            d.reason = if reason.is_empty() {
                "stream closed by the cloud".into()
            } else {
                reason.to_owned()
            };
        }
        d.kind = Some(kind);
    }

    /// Up, pending, or down for longer than `threshold`; the first time it is seen down,
    /// reports `pms_stream_failed` (or `pms_auth_failed` when the token was refused).
    pub fn health(&self, threshold: Duration) -> Health {
        if self.connected.load(Ordering::SeqCst) {
            return Health::Up;
        }
        let now = (self.clock)();
        let mut d = lock(&self.down);
        let Some(since) = d.since else {
            return Health::Pending;
        };
        let down_for = now.duration_since(since);
        if down_for <= threshold {
            return Health::Pending;
        }
        let reason = d.reason.clone();
        if !d.reported {
            d.reported = true;
            let (code, what) = match d.kind {
                Some(DownKind::Auth) => (CODE_AUTH_FAILED, "the cloud refuses this agent's token"),
                _ => (CODE_STREAM_FAILED, "the stream to the cloud is down"),
            };
            drop(d);
            let mut ctx = Map::new();
            ctx.insert("down_secs", down_for.as_secs());
            ctx.insert("reason", reason.as_str());
            let msg = format!("{what} for {} s: {reason}", down_for.as_secs());
            self.report(Level::Error, code, &msg, ctx);
        }
        Health::Down {
            for_secs: down_for.as_secs(),
            reason,
        }
    }
}

/// A [`Reporter`] that keeps reports in memory (tests).
#[derive(Default)]
pub struct MemoryReporter(pub Mutex<Vec<(Level, String, String, Map)>>);

impl MemoryReporter {
    /// The codes reported so far.
    pub fn codes(&self) -> Vec<String> {
        lock(&self.0).iter().map(|r| r.1.clone()).collect()
    }

    /// Every report's context, as JSON text.
    pub fn contexts(&self) -> Vec<String> {
        lock(&self.0)
            .iter()
            .map(|r| dl_json::to_string(&Value::Object(r.3.clone())))
            .collect()
    }
}

impl Reporter for MemoryReporter {
    fn report(&self, level: Level, code: &str, message: &str, context: Map) {
        lock(&self.0).push((level, code.to_owned(), message.to_owned(), context));
    }
}

#[cfg(test)]
mod tests;
