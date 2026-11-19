//! How much of the vendor database's time capture may take (`Source/PollGovernor.cs`).

use std::time::{Duration, Instant};

/// How much of the vendor database's time capture may take.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PollBudget {
    /// Longest a single cycle may page before it stops and resumes next time (10 s).
    pub max_cycle_duration: Duration,
    /// Fraction of wall time spent reading (0.2 = the idle gap after a cycle is at least
    /// 4x its duration).
    pub max_duty_cycle: f64,
    /// Largest pause after failures (2 min).
    pub max_backoff: Duration,
}

impl Default for PollBudget {
    fn default() -> PollBudget {
        PollBudget {
            max_cycle_duration: Duration::from_secs(10),
            max_duty_cycle: 0.2,
            max_backoff: Duration::from_secs(120),
        }
    }
}

/// Per-cycle deadline; paging loops check it between pages. Measured on the monotonic
/// clock, like `TimeProvider.GetTimestamp`.
#[derive(Debug, Clone, Copy)]
pub struct CycleDeadline {
    start: Instant,
    budget: Option<Duration>,
}

impl CycleDeadline {
    /// A deadline `budget` from now.
    pub fn new(budget: Duration) -> CycleDeadline {
        CycleDeadline {
            start: Instant::now(),
            budget: Some(budget),
        }
    }

    /// A deadline that never expires (`CycleDeadline.None`).
    pub fn none() -> CycleDeadline {
        CycleDeadline {
            start: Instant::now(),
            budget: None,
        }
    }

    /// Whether the budget is spent.
    pub fn expired(&self) -> bool {
        self.budget.is_some_and(|b| self.start.elapsed() >= b)
    }

    /// Time since the start.
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}

fn ticks(d: Duration) -> i64 {
    i64::try_from(d.as_nanos() / 100).unwrap_or(i64::MAX)
}

fn from_ticks(t: i64) -> Duration {
    Duration::from_nanos(u64::try_from(t.max(0)).unwrap_or(0).saturating_mul(100))
}

/// Computes the pause before the next poll: base interval, duty-cycle cap, exponential
/// back-off on failures.
#[derive(Debug, Clone)]
pub struct PollGovernor {
    budget: PollBudget,
    failures: u32,
}

impl PollGovernor {
    /// A governor for `budget`.
    pub fn new(budget: PollBudget) -> PollGovernor {
        PollGovernor {
            budget,
            failures: 0,
        }
    }

    /// Failures in a row so far.
    pub fn consecutive_failures(&self) -> u32 {
        self.failures
    }

    /// The pause after a cycle of `last_duration` that `failed` or not.
    pub fn next_delay(
        &mut self,
        interval: Duration,
        last_duration: Duration,
        failed: bool,
    ) -> Duration {
        self.failures = if failed { self.failures + 1 } else { 0 };
        let duty = if self.budget.max_duty_cycle <= 0.0 || self.budget.max_duty_cycle > 1.0 {
            1.0
        } else {
            self.budget.max_duty_cycle
        };
        let min_gap = from_ticks((ticks(last_duration) as f64 * (1.0 / duty - 1.0)) as i64);
        let mut delay = if interval > min_gap {
            interval
        } else {
            min_gap
        };
        if self.failures > 0 {
            let factor = 2f64.powi(self.failures.min(16) as i32);
            let backoff = from_ticks(
                (ticks(interval) as f64 * factor).min(ticks(self.budget.max_backoff) as f64) as i64,
            );
            if backoff > delay {
                delay = backoff;
            }
        }
        let cap = if self.budget.max_backoff > interval {
            self.budget.max_backoff
        } else {
            interval
        };
        if delay > cap { cap } else { delay }
    }

    /// The deadline of a cycle starting now.
    pub fn start_cycle(&self) -> CycleDeadline {
        CycleDeadline::new(self.budget.max_cycle_duration)
    }
}

#[cfg(test)]
mod tests {
    //! `PollGovernorTests.cs`.
    use super::*;

    fn budget() -> PollBudget {
        PollBudget {
            max_duty_cycle: 0.2,
            max_backoff: Duration::from_secs(60),
            ..PollBudget::default()
        }
    }

    #[test]
    fn quick_cycle_keeps_the_base_interval() {
        let d = PollGovernor::new(budget()).next_delay(
            Duration::from_secs(2),
            Duration::from_millis(100),
            false,
        );
        assert_eq!(d, Duration::from_secs(2));
    }

    #[test]
    fn slow_cycle_is_followed_by_a_gap_that_caps_the_duty_cycle() {
        let d = PollGovernor::new(budget()).next_delay(
            Duration::from_secs(2),
            Duration::from_secs(3),
            false,
        );
        assert_eq!(d, Duration::from_secs(12));
    }

    #[test]
    fn failures_back_off_exponentially_up_to_the_cap_and_reset_on_success() {
        let mut g = PollGovernor::new(budget());
        let delays: Vec<Duration> = (0..8)
            .map(|_| g.next_delay(Duration::from_secs(2), Duration::ZERO, true))
            .collect();
        assert_eq!(delays[0], Duration::from_secs(4));
        assert_eq!(delays[1], Duration::from_secs(8));
        assert!(delays.windows(2).all(|w| w[0] <= w[1]));
        assert!(delays.iter().all(|d| *d <= Duration::from_secs(60)));
        assert_eq!(delays[7], Duration::from_secs(60));
        assert_eq!(
            g.next_delay(Duration::from_secs(2), Duration::ZERO, false),
            Duration::from_secs(2)
        );
        assert_eq!(g.consecutive_failures(), 0);
    }

    #[test]
    fn an_interval_longer_than_the_back_off_cap_is_not_clamped() {
        let d =
            PollGovernor::new(budget()).next_delay(Duration::from_secs(300), Duration::ZERO, false);
        assert_eq!(d, Duration::from_secs(300));
    }

    #[test]
    fn cycle_deadline_expires_with_the_clock() {
        let g = PollGovernor::new(PollBudget {
            max_cycle_duration: Duration::from_secs(10),
            ..PollBudget::default()
        });
        assert!(!g.start_cycle().expired());
        assert!(!CycleDeadline::none().expired());
        assert!(CycleDeadline::new(Duration::ZERO).expired());
    }
}
