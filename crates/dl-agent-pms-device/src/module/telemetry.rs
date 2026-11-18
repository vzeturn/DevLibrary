//! What the worker tells the module: commands claimed / ok / failed by kind, the API's
//! health, and problem reports with stable codes.
//!
//! Reports carry kinds, error codes, HTTP statuses and command ids only: never payloads,
//! results (card data), adapter messages or the token. Each code is reported at most once
//! per [`REPORT_EVERY`]; the next report says how many were held back.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use dl_json::{Map, Value};

use crate::agent::{AgentError, Observer};
use crate::contracts::{CommandOutcome, DeviceCommand, codes, kinds};
use crate::queue::QueueError;

/// The API kept failing (unreachable, 5xx, results not delivered) past the grace.
pub const CODE_API_FAILED: &str = "pms_device_api_failed";
/// The API refused the agent token (401 / 403).
pub const CODE_AUTH_FAILED: &str = "pms_device_auth_failed";
/// An adapter failed on a command (DEVICE_ADAPTER_FAILED).
pub const CODE_ADAPTER_FAILED: &str = "pms_device_adapter_failed";
/// A command needed a vendor SDK this workstation lacks.
pub const CODE_SDK_MISSING: &str = "pms_device_sdk_missing";

/// The shortest gap between two reports with the same code.
pub const REPORT_EVERY: Duration = Duration::from_secs(600);

/// The kinds counted under their own name; anything else counts as `other`.
const KNOWN_KINDS: [&str; 11] = [
    kinds::LOCK_ISSUE,
    kinds::LOCK_DUPLICATE,
    kinds::LOCK_READ,
    kinds::LOCK_EXTEND,
    kinds::LOCK_CANCEL_ALL,
    kinds::LOCK_ERASE,
    kinds::POS_PRINT,
    kinds::POS_KICK,
    kinds::POS_DISPLAY,
    kinds::POS_READ,
    kinds::POS_SPEAK,
];

/// The longest message text taken from an error into a report or status detail.
const MAX_REASON: usize = 200;

/// How bad a report is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Degraded but working.
    Warning,
    /// A feature failed.
    Error,
}

/// Where reports go (the module forwards them to dl-agent).
pub trait Reporter: Send + Sync {
    /// Records one report.
    fn report(&self, level: Level, code: &str, message: &str, context: Map);
}

/// The API's health as the module shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// No poll has finished yet.
    Pending,
    /// The last poll reached the API.
    Up,
    /// The API has been failing for `for_secs` (or refuses the token).
    Down {
        /// Seconds since the first failure in a row.
        for_secs: u64,
        /// The last failure, without secrets.
        reason: String,
    },
}

#[derive(Default)]
struct Api {
    ever_ok: bool,
    failing_since: Option<Instant>,
    reason: String,
    auth: bool,
    reported: bool,
}

#[derive(Default)]
struct Counts {
    claimed: BTreeMap<&'static str, u64>,
    ok: BTreeMap<&'static str, u64>,
    failed: BTreeMap<&'static str, u64>,
}

/// Counters and state shared by the worker (as its [`Observer`]) and the module.
pub struct Telemetry {
    counts: Mutex<Counts>,
    api: Mutex<Api>,
    last_command_unix: AtomicI64,
    held: Mutex<HashMap<&'static str, (Instant, u64)>>,
    reporter: Option<Arc<dyn Reporter>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn kind_key(kind: &str) -> &'static str {
    KNOWN_KINDS
        .iter()
        .copied()
        .find(|k| *k == kind)
        .unwrap_or("other")
}

fn cut(s: &str) -> String {
    s.chars().take(MAX_REASON).collect()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// A poll failure in words, without payloads, problem bodies or the token; `None` for a
/// stop. The second value is true when the API refused the token.
pub fn describe(e: &AgentError) -> Option<(String, bool)> {
    Some(match e {
        AgentError::Stopped => return None,
        AgentError::PendingResults(p) => (p.to_string(), false),
        AgentError::Io { message, .. } => (format!("outbox: {}", cut(message)), false),
        AgentError::Queue(q) => match q {
            QueueError::Api(api) => (
                format!("API answered HTTP {}", api.status),
                api.status == 401 || api.status == 403,
            ),
            QueueError::Timeout => ("API did not answer within 15 s".to_owned(), false),
            QueueError::Transport(t) => (format!("API unreachable: {}", cut(&t.message)), false),
            QueueError::StaleClaim(s) => (format!("result refused: {}", s.reason), false),
            QueueError::Other(d) => (cut(&d.message), false),
        },
    })
}

impl Telemetry {
    /// Telemetry sending reports to `reporter` (none: counters only).
    pub fn new(reporter: Option<Arc<dyn Reporter>>) -> Telemetry {
        Telemetry {
            counts: Mutex::default(),
            api: Mutex::default(),
            last_command_unix: AtomicI64::new(0),
            held: Mutex::default(),
            reporter,
        }
    }

    /// Forgets the API state (a new cycle starts with a new worker); counters stay.
    pub fn reset_api(&self) {
        *lock(&self.api) = Api::default();
    }

    /// The API health; files the API or auth report once the failure lasts `down_after`
    /// (a refused token at once).
    pub fn health(&self, down_after: Duration) -> Health {
        let mut api = lock(&self.api);
        let Some(since) = api.failing_since else {
            return if api.ever_ok {
                Health::Up
            } else {
                Health::Pending
            };
        };
        let elapsed = since.elapsed();
        if elapsed < down_after && !api.auth {
            return if api.ever_ok {
                Health::Up
            } else {
                Health::Pending
            };
        }
        let reason = api.reason.clone();
        if !api.reported {
            api.reported = true;
            let code = if api.auth {
                CODE_AUTH_FAILED
            } else {
                CODE_API_FAILED
            };
            drop(api);
            self.report(Level::Error, code, &reason, Map::new());
        }
        Health::Down {
            for_secs: elapsed.as_secs(),
            reason,
        }
    }

    /// Files a report unless the same code was reported less than [`REPORT_EVERY`] ago.
    pub fn report(&self, level: Level, code: &'static str, message: &str, mut context: Map) {
        let Some(reporter) = &self.reporter else {
            return;
        };
        let held_back = {
            let mut held = lock(&self.held);
            let now = Instant::now();
            match held.get_mut(code) {
                Some((last, count)) if now.duration_since(*last) < REPORT_EVERY => {
                    *count += 1;
                    return;
                }
                Some((last, count)) => {
                    *last = now;
                    std::mem::take(count)
                }
                None => {
                    held.insert(code, (now, 0));
                    0
                }
            }
        };
        if held_back > 0 {
            context.insert("held_back", held_back);
        }
        reporter.report(level, code, message, context);
    }

    /// The counters as metrics: `claimed`, `ok`, `failed` (objects by kind) and
    /// `last_command_unix`.
    pub fn to_map(&self) -> Map {
        let by_kind = |m: &BTreeMap<&'static str, u64>| {
            let mut o = Map::new();
            for (k, v) in m {
                o.insert(*k, *v);
            }
            Value::Object(o)
        };
        let c = lock(&self.counts);
        let mut m = Map::new();
        m.insert("claimed", by_kind(&c.claimed));
        m.insert("ok", by_kind(&c.ok));
        m.insert("failed", by_kind(&c.failed));
        m.insert(
            "last_command_unix",
            self.last_command_unix.load(Ordering::Relaxed),
        );
        m
    }

    /// Commands claimed, all kinds.
    pub fn claimed_total(&self) -> u64 {
        lock(&self.counts).claimed.values().sum()
    }

    /// A claim round trip worked: the API is up.
    fn api_ok(&self) {
        let mut api = lock(&self.api);
        api.ever_ok = true;
        api.failing_since = None;
        api.auth = false;
        api.reported = false;
    }
}

impl Observer for Telemetry {
    fn claimed(&self, command: &DeviceCommand) {
        *lock(&self.counts)
            .claimed
            .entry(kind_key(&command.kind))
            .or_default() += 1;
        self.last_command_unix.store(unix_now(), Ordering::Relaxed);
        // The claim reached the API, even if the device then blocks for a while.
        self.api_ok();
    }

    fn finished(&self, command: &DeviceCommand, outcome: &CommandOutcome) {
        let key = kind_key(&command.kind);
        {
            let mut c = lock(&self.counts);
            let map = if outcome.ok() {
                &mut c.ok
            } else {
                &mut c.failed
            };
            *map.entry(key).or_default() += 1;
        }
        let Some(err) = &outcome.error else { return };
        let mut ctx = Map::new();
        ctx.insert("kind", key);
        ctx.insert("error_code", err.code.as_str());
        ctx.insert("command_id", command.id.to_string());
        match err.code.as_str() {
            codes::ADAPTER_FAILED => {
                // "{ExceptionType}: {message}": only the type, the message may quote device data.
                let what = err.message.split_once(": ").map_or("exception", |(t, _)| t);
                let msg = format!("{key} failed in the adapter ({})", cut(what));
                self.report(Level::Error, CODE_ADAPTER_FAILED, &msg, ctx);
            }
            codes::SDK_NOT_INSTALLED | codes::VENDOR_SDK_MISSING => {
                let msg = format!("{key}: {}", cut(&err.message));
                self.report(Level::Warning, CODE_SDK_MISSING, &msg, ctx);
            }
            _ => {}
        }
    }

    fn polled(&self, result: Result<bool, &AgentError>) {
        let e = match result {
            Ok(_) => return self.api_ok(),
            Err(e) => e,
        };
        let Some((reason, auth)) = describe(e) else {
            return;
        };
        let mut api = lock(&self.api);
        api.failing_since.get_or_insert_with(Instant::now);
        api.reason = reason;
        api.auth = auth;
    }
}

#[cfg(test)]
mod tests;
