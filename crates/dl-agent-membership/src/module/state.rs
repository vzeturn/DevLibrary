//! What the module thread, the sync thread and the scan thread share: the latest
//! sync-config, the health that decides running/degraded, the metrics, and the reports
//! waiting to be sent (rate-limited per code by the module thread).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use dl_agent::report::Severity;
use dl_json::Map;
use dl_time::OffsetDateTime;

use crate::folio::FolioScanState;
use crate::log::{self, V};
use crate::wire::parse::SyncConfig;
use crate::writer::special_rows::HubSpecialCode;

/// Stable report codes.
pub mod codes {
    /// The hub stayed unreachable past one back-off.
    pub const HUB_FAILED: &str = "membership_hub_failed";
    /// The hub rejected the agent token (401).
    pub const AUTH_FAILED: &str = "membership_auth_failed";
    /// SMILE did not answer (or the connection string is unusable).
    pub const SMILE_FAILED: &str = "membership_smile_failed";
    /// SMILE now shows another database than the one pinned at start.
    pub const DB_IDENTITY_CHANGED: &str = "membership_db_identity_changed";
    /// A scanner failed.
    pub const SCAN_FAILED: &str = "membership_scan_failed";
}

/// One report waiting for the module thread. The message and context never carry guest
/// data, SQL text, a connection string or the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReport {
    /// Severity.
    pub severity: Severity,
    /// One of [`codes`].
    pub code: &'static str,
    /// One line.
    pub message: String,
    /// Extra keys.
    pub context: Vec<(&'static str, String)>,
}

/// What decides running or degraded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Health {
    /// A heartbeat reached the hub at least once in this run.
    pub heartbeat_ok: bool,
    /// Hub failures in a row (heartbeat, sync-config or job pull).
    pub hub_failures: u32,
    /// The last hub failure.
    pub hub_error: Option<String>,
    /// The hub rejected the token and no call has succeeded since.
    pub auth_rejected: bool,
    /// The last SMILE failure, until a ping succeeds.
    pub smile_error: Option<String>,
    /// Set while SMILE shows another database than the pinned one; scans are paused.
    pub identity_changed: Option<String>,
}

impl Health {
    /// The degraded reason, if any. Before the first heartbeat a single hub failure is
    /// not a problem yet (one back-off is allowed).
    pub fn problem(&self) -> Option<String> {
        if self.auth_rejected {
            return Some(
                "hub rejected the agent token (401): enrolled again on another machine or revoked; run membership-enroll here".into(),
            );
        }
        if let Some(why) = &self.identity_changed {
            return Some(format!("scans paused: {why}"));
        }
        if self.hub_failures >= 2 {
            let e = self.hub_error.as_deref().unwrap_or("unknown error");
            return Some(format!(
                "hub unreachable ({} failures in a row): {e}",
                self.hub_failures
            ));
        }
        self.smile_error
            .as_ref()
            .map(|e| format!("SMILE unreachable: {e}"))
    }
}

/// Per-scanner counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanStat {
    /// Passes run.
    pub runs: u64,
    /// When the last pass ended.
    pub last_run: Option<OffsetDateTime>,
    /// Rows (lines, rooms or folios) the last pass sent.
    pub rows: u64,
    /// Failed passes.
    pub errors: u64,
    /// What the last pass did, one short line.
    pub last: String,
}

/// Counters for the metrics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    /// Heartbeats the hub accepted.
    pub heartbeats: u64,
    /// When the last one was accepted.
    pub heartbeat_last: Option<OffsetDateTime>,
    /// Heartbeats that failed.
    pub heartbeat_failures: u64,
    /// Jobs pulled.
    pub jobs_seen: u64,
    /// Jobs in the last pull, left for the hub to lease again.
    pub jobs_left: u64,
    /// Jobs executed (written or failed) and sent to the hub.
    pub jobs_executed: u64,
    /// Job acks the hub accepted.
    pub jobs_acked: u64,
    /// Per scanner.
    pub scans: BTreeMap<&'static str, ScanStat>,
}

/// The shared state.
#[derive(Debug, Default)]
pub struct Shared {
    sync_config: Mutex<Option<SyncConfig>>,
    health: Mutex<Health>,
    stats: Mutex<Stats>,
    reports: Mutex<Vec<PendingReport>>,
    pinned_identity: Mutex<Option<String>>,
    /// The folio scanner's cursor reading, for the heartbeat.
    pub folio: Arc<FolioScanState>,
    /// The walk-in earn health (`WalkInEarnState`), for the heartbeat.
    pub walk_in: Arc<crate::checkout::WalkInEarnState>,
    /// The hub's extra-bed special code (sync-config), shared with the writer.
    pub hub_special_code: HubSpecialCode,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    /// The latest sync-config, if one was read.
    pub fn sync_config(&self) -> Option<SyncConfig> {
        lock(&self.sync_config).clone()
    }

    /// Stores a fresh sync-config.
    pub fn set_sync_config(&self, c: SyncConfig) {
        if self
            .hub_special_code
            .set(c.extra_bed_special_code.as_deref())
        {
            log::info(
                "hub picked the extra-bed special (extra_bed_special_code is the fallback only)",
                &[(
                    "code",
                    V::S(c.extra_bed_special_code.as_deref().unwrap_or("(none)")),
                )],
            );
        }
        *lock(&self.sync_config) = Some(c);
    }

    /// A copy of the health.
    pub fn health(&self) -> Health {
        lock(&self.health).clone()
    }

    /// Changes the health.
    pub fn update_health(&self, f: impl FnOnce(&mut Health)) {
        f(&mut lock(&self.health));
    }

    /// Whether scans are paused (wrong database).
    pub fn scans_paused(&self) -> bool {
        lock(&self.health).identity_changed.is_some()
    }

    /// A copy of the counters.
    pub fn stats(&self) -> Stats {
        lock(&self.stats).clone()
    }

    /// Changes the counters.
    pub fn update_stats(&self, f: impl FnOnce(&mut Stats)) {
        f(&mut lock(&self.stats));
    }

    /// Records a scanner pass: `Ok(rows)` or `Err(what failed)`.
    pub fn record_scan(&self, scanner: &'static str, result: Result<(u64, String), String>) {
        let mut s = lock(&self.stats);
        let stat = s.scans.entry(scanner).or_default();
        stat.runs += 1;
        stat.last_run = Some(OffsetDateTime::now_utc());
        match result {
            Ok((rows, what)) => {
                stat.rows = rows;
                stat.last = what;
            }
            Err(e) => {
                stat.errors += 1;
                stat.last = e;
            }
        }
    }

    /// Remembers the identity fingerprint pinned at start, for the metrics.
    pub fn set_pinned_identity(&self, fp: Option<String>) {
        *lock(&self.pinned_identity) = fp;
    }

    /// Queues a report.
    pub fn report(
        &self,
        severity: Severity,
        code: &'static str,
        message: impl Into<String>,
        context: Vec<(&'static str, String)>,
    ) {
        lock(&self.reports).push(PendingReport {
            severity,
            code,
            message: message.into(),
            context,
        });
    }

    /// Takes the queued reports.
    pub fn take_reports(&self) -> Vec<PendingReport> {
        std::mem::take(&mut *lock(&self.reports))
    }

    /// The metrics.
    pub fn metrics(&self) -> Map {
        let s = self.stats();
        let h = self.health();
        let mut m = Map::new();
        m.insert("heartbeat.count", s.heartbeats);
        m.insert("heartbeat.failures", s.heartbeat_failures);
        if let Some(t) = s.heartbeat_last {
            m.insert("heartbeat.last", t.to_rfc3339());
        }
        m.insert("hub.failures_in_a_row", u64::from(h.hub_failures));
        m.insert("smile.ok", h.smile_error.is_none());
        m.insert("scans.paused", h.identity_changed.is_some());
        m.insert("jobs.seen", s.jobs_seen);
        m.insert("jobs.left", s.jobs_left);
        m.insert("jobs.executed", s.jobs_executed);
        m.insert("jobs.acked", s.jobs_acked);
        for (name, st) in &s.scans {
            m.insert(format!("{name}.runs"), st.runs);
            m.insert(format!("{name}.rows"), st.rows);
            m.insert(format!("{name}.errors"), st.errors);
            m.insert(format!("{name}.last"), st.last.as_str());
            if let Some(t) = st.last_run {
                m.insert(format!("{name}.last_run"), t.to_rfc3339());
            }
        }
        if let Some(c) = self.folio.snapshot() {
            m.insert("folio.cursor", crate::stj::format_datetime(c.watermark));
            m.insert("folio.lag_seconds", c.lag_seconds);
        }
        if let Some(fp) = lock(&self.pinned_identity).as_deref() {
            m.insert("identity.pinned", fp);
        }
        m
    }
}

/// Lets one report per code through every `interval`.
#[derive(Debug)]
pub struct RateLimit {
    interval: Duration,
    last: HashMap<&'static str, Instant>,
}

impl RateLimit {
    /// A limit of one report per code per `interval`.
    pub fn new(interval: Duration) -> RateLimit {
        RateLimit {
            interval,
            last: HashMap::new(),
        }
    }

    /// Whether a report with `code` may go out at `now`.
    pub fn allow(&mut self, code: &'static str, now: Instant) -> bool {
        match self.last.get(code) {
            Some(at) if now.saturating_duration_since(*at) < self.interval => false,
            _ => {
                self.last.insert(code, now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_hub_failure_is_tolerated_and_auth_wins() {
        let mut h = Health::default();
        assert_eq!(h.problem(), None);
        h.hub_failures = 1;
        h.hub_error = Some("hub heartbeat answered 503".into());
        assert_eq!(h.problem(), None, "one back-off is allowed");
        h.hub_failures = 2;
        assert!(
            h.problem()
                .unwrap()
                .starts_with("hub unreachable (2 failures")
        );
        h.auth_rejected = true;
        assert!(h.problem().unwrap().contains("401"));
    }

    #[test]
    fn reports_are_rate_limited_per_code() {
        let mut r = RateLimit::new(Duration::from_secs(900));
        let t = Instant::now();
        assert!(r.allow(codes::HUB_FAILED, t));
        assert!(!r.allow(codes::HUB_FAILED, t + Duration::from_secs(60)));
        assert!(r.allow(codes::SCAN_FAILED, t + Duration::from_secs(60)));
        assert!(r.allow(codes::HUB_FAILED, t + Duration::from_secs(901)));
    }

    #[test]
    fn metrics_carry_scanner_counters() {
        let s = Shared::default();
        s.record_scan("rooms", Ok((12, "pushed".into())));
        s.record_scan("rooms", Err("room scan: SMILE unreachable".into()));
        s.update_stats(|st| st.jobs_seen = 3);
        let m = s.metrics();
        assert_eq!(
            m.get("rooms.runs").and_then(dl_json::Value::as_u64),
            Some(2)
        );
        assert_eq!(
            m.get("rooms.errors").and_then(dl_json::Value::as_u64),
            Some(1)
        );
        assert_eq!(
            m.get("rooms.rows").and_then(dl_json::Value::as_u64),
            Some(12)
        );
        assert_eq!(m.get("jobs.seen").and_then(dl_json::Value::as_u64), Some(3));
    }
}
