//! What the module tells dl-agent: problem reports with stable codes (rate-limited, scrubbed
//! of anything that looks like a secret) and metrics. [`Sink`] is the seam: dl-agent's
//! `ModuleContext` in production, a recorder in tests.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use dl_agent::report::Severity;
use dl_agent::supervisor::ModuleContext;
use dl_json::{Map, Value};

use crate::Error;
use crate::state::now_ms;

/// The same code for the same subject is reported at most once per this window.
pub const REPORT_WINDOW_MS: u64 = 15 * 60 * 1000;

/// Where problem reports and metrics go.
pub trait Sink: Send + Sync {
    /// One problem report.
    fn problem(&self, severity: Severity, code: &str, message: &str);
    /// One metric.
    fn metric(&self, key: &str, value: Value);
}

impl Sink for ModuleContext {
    fn problem(&self, severity: Severity, code: &str, message: &str) {
        self.report(severity, code, message, Map::new());
    }

    fn metric(&self, key: &str, value: Value) {
        self.set_metric(key, value);
    }
}

/// Problem reports with a per code + subject rate limit.
pub struct Reports {
    sink: std::sync::Arc<dyn Sink>,
    last: Mutex<HashMap<String, u64>>,
    window_ms: u64,
}

impl std::fmt::Debug for Reports {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reports")
            .field("window_ms", &self.window_ms)
            .finish_non_exhaustive()
    }
}

impl Reports {
    /// Reports into `sink`, at most once per [`REPORT_WINDOW_MS`] per code + subject.
    pub fn new(sink: std::sync::Arc<dyn Sink>) -> Reports {
        Reports {
            sink,
            last: Mutex::new(HashMap::new()),
            window_ms: REPORT_WINDOW_MS,
        }
    }

    /// The sink, for metrics.
    pub fn sink(&self) -> &dyn Sink {
        self.sink.as_ref()
    }

    /// Reports `err` about `subject` (a schedule, server or request id) unless the same
    /// code and subject were reported within the window. Returns whether it was sent.
    pub fn error(&self, subject: &str, err: &Error) -> bool {
        let severity = match err {
            Error::Auth(_) => Severity::Critical,
            Error::Cancelled => return false,
            _ => Severity::Error,
        };
        self.send(severity, err.code(), subject, &err.to_string())
    }

    /// Reports `message` under `code` about `subject`, rate-limited like [`Reports::error`].
    pub fn send(&self, severity: Severity, code: &str, subject: &str, message: &str) -> bool {
        let key = format!("{code}\u{1f}{subject}");
        let now = now_ms();
        {
            let mut last = self
                .last
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if last
                .get(&key)
                .is_some_and(|t| now.saturating_sub(*t) < self.window_ms)
            {
                return false;
            }
            last.insert(key, now);
        }
        let text = if subject.is_empty() {
            scrub(message)
        } else {
            format!("{subject}: {}", scrub(message))
        };
        self.sink.problem(severity, code, &text);
        true
    }
}

/// Words that start a secret in a message; what follows them, up to the next separator, is
/// replaced. Errors are written not to carry secrets; this is the second line of defence.
const SECRET_MARKERS: [&str; 10] = [
    "password=",
    "pwd=",
    "accountkey=",
    "sharedaccesssignature=",
    "sig=",
    "bearer ",
    "secretaccesskey",
    "x-amz-signature=",
    "token=",
    "client_secret=",
];

/// `message` with anything after a secret marker replaced by `[redacted]`.
pub fn scrub(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    let mut out = String::with_capacity(message.len());
    let mut i = 0;
    while i < message.len() {
        let hit = SECRET_MARKERS.iter().find(|m| lower[i..].starts_with(*m));
        match hit {
            Some(m) => {
                out.push_str(&message[i..i + m.len()]);
                out.push_str("[redacted]");
                i += m.len();
                while i < message.len() {
                    let c = message[i..].chars().next().unwrap_or(' ');
                    if c.is_whitespace() || ";&\"',".contains(c) {
                        break;
                    }
                    i += c.len_utf8();
                }
            }
            None => {
                let c = message[i..].chars().next().unwrap_or(' ');
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

/// The module's counters, published as dl-agent metrics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    /// Backups that succeeded.
    pub jobs_ok: u64,
    /// Backup attempts that failed.
    pub jobs_failed: u64,
    /// Jobs given up after their last attempt.
    pub dead_lettered: u64,
    /// Bytes sent to destinations.
    pub bytes_uploaded: u64,
    /// Last success per database (ms).
    pub last_success: BTreeMap<String, u64>,
}

impl Stats {
    /// The counters as a JSON object (also the `backup.status` answer).
    pub fn to_map(&self, queue_depth: usize, outbox: usize) -> Map {
        let mut last = Map::new();
        for (db, at) in &self.last_success {
            last.insert(db.as_str(), *at);
        }
        let mut m = Map::new();
        m.insert("queue_depth", queue_depth as u64);
        m.insert("outbox_depth", outbox as u64);
        m.insert("jobs_ok", self.jobs_ok);
        m.insert("jobs_failed", self.jobs_failed);
        m.insert("dead_lettered", self.dead_lettered);
        m.insert("bytes_uploaded", self.bytes_uploaded);
        m.insert("last_success", Value::Object(last));
        m
    }

    /// Publishes every counter.
    pub fn publish(&self, sink: &dyn Sink, queue_depth: usize, outbox: usize) {
        for (k, v) in self.to_map(queue_depth, outbox) {
            sink.metric(&k, v);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use super::*;

    /// Records what the module reported.
    #[derive(Default)]
    pub struct Recorder {
        pub problems: Mutex<Vec<(String, String)>>,
        pub metrics: Mutex<BTreeMap<String, Value>>,
    }

    impl Recorder {
        pub fn codes(&self) -> Vec<String> {
            self.problems
                .lock()
                .unwrap()
                .iter()
                .map(|(c, _)| c.clone())
                .collect()
        }
    }

    impl Sink for Recorder {
        fn problem(&self, _severity: Severity, code: &str, message: &str) {
            self.problems
                .lock()
                .unwrap()
                .push((code.to_owned(), message.to_owned()));
        }

        fn metric(&self, key: &str, value: Value) {
            self.metrics.lock().unwrap().insert(key.to_owned(), value);
        }
    }

    #[test]
    fn same_code_and_subject_are_rate_limited() {
        let rec = Arc::new(Recorder::default());
        let r = Reports::new(rec.clone());
        assert!(r.error("sched-1", &Error::Upload("HTTP 503".into())));
        assert!(!r.error("sched-1", &Error::Upload("HTTP 503 again".into())));
        assert!(r.error("sched-2", &Error::Upload("HTTP 503".into())));
        assert!(r.error("sched-1", &Error::Sql("login failed".into())));
        assert!(!r.error("sched-1", &Error::Cancelled));
        assert_eq!(
            rec.codes(),
            [
                "backup_upload_failed",
                "backup_upload_failed",
                "backup_sql_failed"
            ]
        );
        assert_eq!(rec.problems.lock().unwrap()[0].1, "sched-1: HTTP 503");
    }

    #[test]
    fn secrets_are_scrubbed() {
        assert_eq!(
            scrub("conn AccountName=a;AccountKey=abc+/=;EndpointSuffix=x"),
            "conn AccountName=a;AccountKey=[redacted];EndpointSuffix=x"
        );
        assert_eq!(
            scrub("url https://h/c?sv=1&sig=AbC%2F&se=2"),
            "url https://h/c?sv=1&sig=[redacted]&se=2"
        );
        assert_eq!(
            scrub("Authorization: Bearer eyJhbGci.x.y done"),
            "Authorization: Bearer [redacted] done"
        );
        assert_eq!(
            scrub("grant_type=refresh_token&client_secret=S3CR3T&refresh_token=R"),
            "grant_type=refresh_token&client_secret=[redacted]&refresh_token=[redacted]"
        );
        assert_eq!(scrub("Password=hunter2 ok"), "Password=[redacted] ok");
        assert_eq!(scrub("Bán hàng: lỗi 3201"), "Bán hàng: lỗi 3201");
    }

    #[test]
    fn stats_publish_every_counter() {
        let rec = Recorder::default();
        let mut s = Stats {
            jobs_ok: 2,
            bytes_uploaded: 10,
            ..Stats::default()
        };
        s.last_success.insert("Sales".into(), 5);
        s.publish(&rec, 3, 1);
        let m = rec.metrics.lock().unwrap();
        assert_eq!(m.get("queue_depth").and_then(Value::as_u64), Some(3));
        assert_eq!(m.get("jobs_ok").and_then(Value::as_u64), Some(2));
        assert_eq!(
            m.get("last_success")
                .and_then(|v| v.get("Sales"))
                .and_then(Value::as_u64),
            Some(5)
        );
    }
}
