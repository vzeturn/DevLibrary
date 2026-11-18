//! The agent's clock (the C# `TimeProvider`): wall time for leases and the waits between
//! polls and result retries, replaceable by a manual clock in tests.

use std::time::Duration;

use dl_time::OffsetDateTime;

use super::StopToken;

/// Wall time and stoppable waits.
pub trait Clock: Send + Sync {
    /// The current UTC time (`TimeProvider.GetUtcNow()`).
    fn now(&self) -> OffsetDateTime;

    /// `Task.Delay(duration, time, stop)`: waits `duration` on this clock. Returns false,
    /// at once or early, when `stop` is set.
    fn sleep(&self, duration: Duration, stop: &StopToken) -> bool;
}

/// The system clock (`TimeProvider.System`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn sleep(&self, duration: Duration, stop: &StopToken) -> bool {
        !stop.wait(duration)
    }
}

/// `t + d`, keeping `t`'s offset; `t` itself when the sum is out of range.
pub fn add_duration(t: OffsetDateTime, d: Duration) -> OffsetDateTime {
    let nanos = t
        .unix_nanos()
        .saturating_add(i128::try_from(d.as_nanos()).unwrap_or(i128::MAX));
    OffsetDateTime::from_unix_nanos(nanos)
        .ok()
        .and_then(|s| s.checked_to_offset(t.offset()))
        .unwrap_or(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_duration_moves_the_instant() {
        let t = OffsetDateTime::parse_rfc3339("2026-10-14T07:00:00Z").unwrap();
        let later = add_duration(t, Duration::from_secs(55));
        assert_eq!(later.unix_nanos() - t.unix_nanos(), 55_000_000_000);
        assert!(SystemClock.now().unix_nanos() > t.unix_nanos() - 10_i128.pow(18));
        let stop = StopToken::new();
        stop.stop();
        assert!(!SystemClock.sleep(Duration::from_secs(60), &stop));
    }
}
