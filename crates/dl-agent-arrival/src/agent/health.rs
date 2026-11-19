//! What the link tells the supervisor: the module state (running or degraded, from the
//! cloud's and the PMS's health together), heartbeat metrics, and rate-limited problem
//! reports. Reports carry codes, counts and operation names only: never guest data, SQL text
//! or keys.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dl_agent::report::Severity;
use dl_json::{Map, Value};

use crate::contracts::Status;

/// The cloud could not be reached (or answered with an error) for longer than one backoff.
pub const CLOUD_FAILED: &str = "arrival_cloud_failed";
/// The cloud refused this agent's key, or an envelope did not open.
pub const AUTH_FAILED: &str = "arrival_auth_failed";
/// The cloud refused this agent because the machine's clock is wrong.
pub const CLOCK_SKEW: &str = "arrival_clock_skew";
/// The PMS database cannot be reached.
pub const PMS_FAILED: &str = "arrival_pms_failed";
/// A request was refused before reaching the PMS.
pub const OP_REFUSED: &str = "arrival_op_refused";
/// The PMS procedures could not be installed.
pub const PROC_INSTALL_FAILED: &str = "arrival_proc_install_failed";

/// Default interval between two reports with the same code.
pub const REPORT_INTERVAL: Duration = Duration::from_secs(600);

/// Where state, metrics and reports go (the module context, or a test fake).
pub trait Sink: Send + Sync {
    /// The module works.
    fn running(&self, detail: &str);
    /// The module works with problems.
    fn degraded(&self, detail: &str);
    /// A heartbeat metric.
    fn metric(&self, key: &str, value: Value);
    /// A problem report.
    fn report(&self, severity: Severity, code: &str, message: &str, context: Map);
}

/// Counters published as metrics.
#[derive(Debug, Default)]
pub struct Metrics {
    /// Requests answered `Ok`, `NotModified` or `Delta`.
    pub requests_ok: AtomicU64,
    /// Requests answered `Failed`.
    pub requests_failed: AtomicU64,
    /// Requests answered `Rejected`.
    pub requests_refused: AtomicU64,
    /// Requests being worked on.
    pub in_flight: AtomicI64,
    /// Snapshots held.
    pub snapshots: AtomicU64,
    /// Unix seconds of the last authenticated poll (0 = none yet).
    pub last_poll: AtomicI64,
    /// Milliseconds the last PMS call took.
    pub pms_latency_ms: AtomicU64,
}

/// How a cloud exchange failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudFailure {
    /// Not reached, or an error status: retried with backoff.
    Transport,
    /// The cloud refused the key, or a reply did not open.
    Auth,
    /// The machine's clock is off.
    ClockSkew,
}

#[derive(Debug, Default)]
struct State {
    polled: bool,
    transport_failures: u32,
    cloud: Option<String>,
    pms: Option<String>,
    install: Option<String>,
    shown: Option<(bool, String)>,
    last_report: HashMap<&'static str, Instant>,
}

/// The link's health.
pub struct Health {
    sink: Arc<dyn Sink>,
    interval: Duration,
    /// Counters.
    pub metrics: Metrics,
    state: Mutex<State>,
}

impl std::fmt::Debug for Health {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Health")
            .field("metrics", &self.metrics)
            .finish_non_exhaustive()
    }
}

impl Health {
    /// Health reporting into `sink`, at most one report per code per `interval`.
    pub fn new(sink: Arc<dyn Sink>, interval: Duration) -> Health {
        Health {
            sink,
            interval,
            metrics: Metrics::default(),
            state: Mutex::new(State::default()),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let r = f(&mut s);
        self.publish_state(&mut s);
        r
    }

    fn publish_state(&self, s: &mut State) {
        let problems: Vec<&str> = [&s.cloud, &s.pms, &s.install]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        let next = if problems.is_empty() {
            if !s.polled {
                return;
            }
            (true, "connected to the cloud".to_string())
        } else {
            (false, problems.join("; "))
        };
        if s.shown.as_ref() == Some(&next) {
            return;
        }
        if next.0 {
            self.sink.running(&next.1);
        } else {
            self.sink.degraded(&next.1);
        }
        s.shown = Some(next);
    }

    /// Sends a report unless one with the same code went out within the interval.
    pub fn report(&self, severity: Severity, code: &'static str, message: &str, context: Map) {
        let due = {
            let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let now = Instant::now();
            match s.last_report.get(code) {
                Some(at) if now.duration_since(*at) < self.interval => false,
                _ => {
                    s.last_report.insert(code, now);
                    true
                }
            }
        };
        if due {
            self.sink.report(severity, code, message, context);
        }
    }

    /// Shows a problem before anything ran (not enrolled, bad settings).
    pub fn blocked(&self, detail: &str) {
        self.sink.degraded(detail);
    }

    /// A poll was answered and opened.
    pub fn cloud_ok(&self, unix_seconds: i64) {
        self.metrics
            .last_poll
            .store(unix_seconds, Ordering::Relaxed);
        self.with(|s| {
            s.polled = true;
            s.transport_failures = 0;
            s.cloud = None;
        });
    }

    /// A cloud exchange failed with `message` (never secret).
    pub fn cloud_failed(&self, kind: CloudFailure, message: &str) {
        let report = self.with(|s| match kind {
            CloudFailure::Transport => {
                s.transport_failures += 1;
                if s.transport_failures >= 2 {
                    s.cloud = Some(message.to_string());
                    Some((Severity::Error, CLOUD_FAILED))
                } else {
                    None
                }
            }
            CloudFailure::Auth => {
                s.cloud = Some(message.to_string());
                Some((Severity::Critical, AUTH_FAILED))
            }
            CloudFailure::ClockSkew => {
                s.cloud = Some(message.to_string());
                Some((Severity::Critical, CLOCK_SKEW))
            }
        });
        if let Some((severity, code)) = report {
            self.report(severity, code, message, Map::new());
        }
    }

    /// A PMS call went through.
    pub fn pms_ok(&self, latency: Duration) {
        let ms = u64::try_from(latency.as_millis()).unwrap_or(u64::MAX);
        self.metrics.pms_latency_ms.store(ms, Ordering::Relaxed);
        let changed = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pms
            .is_some();
        if changed {
            self.with(|s| s.pms = None);
        }
    }

    /// The PMS database cannot be reached; `message` names no data.
    pub fn pms_failed(&self, message: &str) {
        self.with(|s| s.pms = Some(format!("PMS unavailable: {message}")));
        self.report(Severity::Error, PMS_FAILED, message, Map::new());
    }

    /// The procedures could not be installed (the link keeps working on what is there).
    pub fn install_failed(&self, message: &str) {
        self.with(|s| s.install = Some(format!("PMS procedures not installed: {message}")));
        self.report(Severity::Error, PROC_INSTALL_FAILED, message, Map::new());
    }

    /// The procedures are in place.
    pub fn install_ok(&self) {
        self.with(|s| s.install = None);
    }

    /// A request was refused before reaching the PMS.
    pub fn op_refused(&self, operation: &str, reason: &str) {
        let mut ctx = Map::new();
        ctx.insert("operation", Value::from(truncate(operation, 64)));
        self.report(Severity::Warning, OP_REFUSED, &truncate(reason, 200), ctx);
    }

    /// Counts an answer by its status.
    pub fn count(&self, status: Status) {
        let counter = match status {
            Status::Ok | Status::NotModified | Status::Delta => &self.metrics.requests_ok,
            Status::Failed => &self.metrics.requests_failed,
            Status::Rejected => &self.metrics.requests_refused,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Publishes the counters.
    pub fn publish_metrics(&self) {
        let m = &self.metrics;
        let load = |a: &AtomicU64| Value::from(a.load(Ordering::Relaxed));
        self.sink.metric("requests_ok", load(&m.requests_ok));
        self.sink
            .metric("requests_failed", load(&m.requests_failed));
        self.sink
            .metric("requests_refused", load(&m.requests_refused));
        self.sink.metric(
            "in_flight",
            Value::from(m.in_flight.load(Ordering::Relaxed).max(0)),
        );
        self.sink.metric("snapshots", load(&m.snapshots));
        self.sink.metric(
            "last_poll",
            Value::from(m.last_poll.load(Ordering::Relaxed)),
        );
        self.sink.metric("pms_latency_ms", load(&m.pms_latency_ms));
    }
}

/// At most `max` characters of `s`, control characters replaced.
pub fn truncate(s: &str, max: usize) -> String {
    s.chars()
        .take(max)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// A [`Sink`] that records everything: for tests.
#[derive(Debug, Default)]
pub struct RecordingSink {
    /// `(running, detail)` in order.
    pub states: Mutex<Vec<(bool, String)>>,
    /// `(code, message)` in order.
    pub reports: Mutex<Vec<(String, String)>>,
    /// Last value of each metric.
    pub metrics: Mutex<HashMap<String, Value>>,
}

impl RecordingSink {
    /// The last state shown.
    pub fn last_state(&self) -> Option<(bool, String)> {
        self.states
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last()
            .cloned()
    }

    /// The codes reported so far.
    pub fn codes(&self) -> Vec<String> {
        let r = self.reports.lock().unwrap_or_else(PoisonError::into_inner);
        r.iter().map(|(c, _)| c.clone()).collect()
    }
}

impl Sink for RecordingSink {
    fn running(&self, detail: &str) {
        self.states
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((true, detail.into()));
    }
    fn degraded(&self, detail: &str) {
        self.states
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((false, detail.into()));
    }
    fn metric(&self, key: &str, value: Value) {
        self.metrics
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.into(), value);
    }
    fn report(&self, _severity: Severity, code: &str, message: &str, _context: Map) {
        let mut r = self.reports.lock().unwrap_or_else(PoisonError::into_inner);
        r.push((code.into(), message.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_transport_failure_is_tolerated_and_reports_are_rate_limited() {
        let sink = Arc::new(RecordingSink::default());
        let h = Health::new(sink.clone(), Duration::from_secs(600));
        h.cloud_ok(1);
        assert_eq!(
            sink.last_state(),
            Some((true, "connected to the cloud".into()))
        );
        h.cloud_failed(CloudFailure::Transport, "Cannot reach the cloud: x");
        assert!(sink.last_state().unwrap().0);
        h.cloud_failed(CloudFailure::Transport, "Cannot reach the cloud: x");
        h.cloud_failed(CloudFailure::Transport, "Cannot reach the cloud: x");
        assert!(!sink.last_state().unwrap().0);
        assert_eq!(sink.codes(), vec![CLOUD_FAILED.to_string()]);
        h.pms_failed("connect failed");
        h.cloud_ok(2);
        assert!(sink.last_state().unwrap().1.starts_with("PMS unavailable"));
        h.pms_ok(Duration::from_millis(5));
        assert!(sink.last_state().unwrap().0);
        h.cloud_failed(CloudFailure::ClockSkew, "clock");
        assert_eq!(sink.last_state(), Some((false, "clock".into())));
        h.publish_metrics();
        assert_eq!(
            sink.metrics.lock().unwrap().get("pms_latency_ms"),
            Some(&Value::from(5u64))
        );
    }
}
