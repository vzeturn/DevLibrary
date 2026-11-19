//! What the module thread derives from the runner's counters: the module state, the
//! metrics, and rate-limited problem reports.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use dl_agent::report::Severity;
use dl_json::{Map, Value};

use crate::run::{Runner, Stats};

/// How long SQL may stay unreachable at boot before the module shows it as degraded.
pub const SQL_WAIT_GRACE: Duration = Duration::from_secs(30);

/// The detail shown once the hub has given this hotel to another install (HTTP 403).
pub const SUPERSEDED: &str =
    "superseded: the hub accepts another install of this hotel (X-Instance-Id); this one stopped";

/// A snapshot of the runner's flags that decide the module state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signals {
    /// The hub answered 403.
    pub superseded: bool,
    /// The hub answered 401 since the last good poll.
    pub unauthorized: bool,
    /// `Breaker.State`.
    pub breaker: &'static str,
    /// Consecutive failed hub calls of the booking loop.
    pub hub_fail_streak: u32,
    /// Loops silent past their threshold.
    pub stalled: Vec<String>,
    /// SQL answered at boot.
    pub sql_ready: bool,
    /// Successful polls so far.
    pub polls_ok: u64,
}

impl Signals {
    /// Reads the runner.
    pub fn read(r: &Runner) -> Signals {
        let s = r.stats();
        Signals {
            superseded: s.superseded.load(Ordering::SeqCst),
            unauthorized: s.unauthorized.load(Ordering::SeqCst),
            breaker: r.breaker_state(),
            hub_fail_streak: s.hub_fail_streak.load(Ordering::SeqCst),
            stalled: s.status().stalled,
            sql_ready: s.sql_ready.load(Ordering::SeqCst),
            polls_ok: s.polls_ok.load(Ordering::SeqCst),
        }
    }
}

/// The module state the signals call for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Not up yet (no successful poll, nothing wrong for long enough to say so).
    Starting,
    /// Working.
    Running(String),
    /// Working badly or not at all, with the reason.
    Degraded(String),
}

/// Decides the module state. `up_for` is how long the runner has been running.
pub fn verdict(s: &Signals, up_for: Duration) -> Verdict {
    if s.superseded {
        return Verdict::Degraded(SUPERSEDED.into());
    }
    if s.unauthorized {
        return Verdict::Degraded(
            "hub rejected the token (HTTP 401): re-run hl-enroll with the hotel's token".into(),
        );
    }
    if s.breaker == "open" {
        return Verdict::Degraded(
            "PMS circuit breaker open: SQL Server writes keep failing".into(),
        );
    }
    if s.hub_fail_streak >= 2 {
        return Verdict::Degraded(format!(
            "hub unreachable ({} failed polls in a row)",
            s.hub_fail_streak
        ));
    }
    if !s.stalled.is_empty() {
        return Verdict::Degraded(format!("loops stalled: {}", s.stalled.join(",")));
    }
    if !s.sql_ready {
        return if up_for >= SQL_WAIT_GRACE {
            Verdict::Degraded("waiting for SQL Server (SMILE_FO unreachable)".into())
        } else {
            Verdict::Starting
        };
    }
    if s.polls_ok == 0 {
        return Verdict::Starting;
    }
    Verdict::Running(format!("polling the hub; breaker {}", s.breaker))
}

/// The module's metrics.
pub fn metrics(r: &Runner) -> Map {
    let s: std::sync::Arc<Stats> = r.stats();
    let st = s.status();
    let n = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::SeqCst) as i64;
    let mut m = Map::new();
    m.insert("jobs_polled", n(&s.jobs_polled));
    m.insert("jobs_applied", n(&s.jobs_applied));
    m.insert("jobs_acked", n(&s.jobs_acked));
    m.insert("jobs_failed", n(&s.jobs_failed));
    m.insert("manual_conflicts", n(&s.manual_conflicts));
    m.insert("availability_pushes", n(&s.availability_pushes));
    m.insert("polls_ok", n(&s.polls_ok));
    m.insert("queue_depth", r.queue_depth() as i64);
    m.insert("unacked", r.unacked() as i64);
    m.insert("mapping_hash", st.mapping_hash);
    m.insert("mapping_count", st.mapping_count);
    m.insert("last_sync_at", st.last_sync_at);
    m.insert("last_job_at", st.last_job_at);
    m.insert("breaker_state", r.breaker_state());
    m
}

/// The severity of each report code.
pub fn severity(code: &str) -> Severity {
    match code {
        "hl_auth_failed" | "hl_pms_failed" | "hl_reconcile_failed" => Severity::Error,
        _ => Severity::Warning,
    }
}

/// Lets one report per code through per window and counts the ones held back.
#[derive(Debug)]
pub struct Limiter {
    window: Duration,
    last: HashMap<&'static str, (Instant, u32)>,
}

impl Limiter {
    /// A limiter with the given window.
    pub fn new(window: Duration) -> Limiter {
        Limiter {
            window,
            last: HashMap::new(),
        }
    }

    /// `Some(held back since the last report)` when a report of `code` may go out now.
    pub fn admit(&mut self, code: &'static str, now: Instant) -> Option<u32> {
        match self.last.get_mut(code) {
            Some((at, held)) if now.duration_since(*at) < self.window => {
                *held += 1;
                None
            }
            Some((at, held)) => {
                let n = *held;
                *at = now;
                *held = 0;
                Some(n)
            }
            None => {
                self.last.insert(code, (now, 0));
                Some(0)
            }
        }
    }
}

/// The context of a report: how many of the same code were held back.
pub fn report_context(held_back: u32) -> Map {
    let mut m = Map::new();
    if held_back > 0 {
        m.insert("held_back", Value::from(i64::from(held_back)));
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> Signals {
        Signals {
            breaker: "closed",
            sql_ready: true,
            polls_ok: 1,
            ..Signals::default()
        }
    }

    #[test]
    fn running_after_the_first_poll() {
        assert!(matches!(
            verdict(&ok(), Duration::ZERO),
            Verdict::Running(_)
        ));
        let s = Signals {
            polls_ok: 0,
            ..ok()
        };
        assert_eq!(verdict(&s, Duration::from_secs(100)), Verdict::Starting);
    }

    #[test]
    fn degraded_reasons_in_order() {
        let up = Duration::ZERO;
        let d = |s: Signals| match verdict(&s, up) {
            Verdict::Degraded(m) => m,
            v => panic!("not degraded: {v:?}"),
        };
        assert!(
            d(Signals {
                superseded: true,
                unauthorized: true,
                ..ok()
            })
            .starts_with("superseded")
        );
        assert!(
            d(Signals {
                unauthorized: true,
                ..ok()
            })
            .contains("401")
        );
        assert!(
            d(Signals {
                breaker: "open",
                ..ok()
            })
            .contains("breaker")
        );
        assert!(
            d(Signals {
                hub_fail_streak: 2,
                ..ok()
            })
            .contains("unreachable")
        );
        assert!(
            d(Signals {
                stalled: vec!["heartbeat".into()],
                ..ok()
            })
            .contains("heartbeat")
        );
        let one = Signals {
            hub_fail_streak: 1,
            ..ok()
        };
        assert!(
            matches!(verdict(&one, up), Verdict::Running(_)),
            "one failure is within the back-off"
        );
    }

    #[test]
    fn sql_wait_degrades_after_grace() {
        let s = Signals {
            sql_ready: false,
            polls_ok: 0,
            ..ok()
        };
        assert_eq!(verdict(&s, Duration::from_secs(1)), Verdict::Starting);
        assert!(matches!(verdict(&s, SQL_WAIT_GRACE), Verdict::Degraded(_)));
    }

    #[test]
    fn limiter_lets_one_through_per_window() {
        let t0 = Instant::now();
        let w = Duration::from_secs(60);
        let mut l = Limiter::new(w);
        assert_eq!(l.admit("hl_hub_failed", t0), Some(0));
        assert_eq!(l.admit("hl_hub_failed", t0 + Duration::from_secs(1)), None);
        assert_eq!(l.admit("hl_hub_failed", t0 + Duration::from_secs(2)), None);
        assert_eq!(
            l.admit("hl_pms_failed", t0),
            Some(0),
            "codes are limited apart"
        );
        assert_eq!(l.admit("hl_hub_failed", t0 + w), Some(2));
        assert!(report_context(0).is_empty());
        assert_eq!(
            report_context(2).get("held_back").and_then(Value::as_i64),
            Some(2)
        );
    }
}
