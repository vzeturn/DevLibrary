//! Time, behind a trait so tests can skew it (the cloud's ±120 s envelope window).

use std::time::Duration;

use dl_time::OffsetDateTime;

/// The wall clock the agent seals envelopes and stamps journals with.
pub trait Clock: Send + Sync {
    /// Now, in UTC.
    fn now(&self) -> OffsetDateTime;

    /// Now as Unix milliseconds.
    fn now_ms(&self) -> i64 {
        self.now().unix_millis()
    }
}

/// The machine's clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// A clock running `offset_ms` ahead of (or behind) the machine's: for tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct SkewedClock {
    /// Milliseconds added to the system time.
    pub offset_ms: i64,
}

impl Clock for SkewedClock {
    fn now(&self) -> OffsetDateTime {
        let ms = OffsetDateTime::now_utc()
            .unix_millis()
            .saturating_add(self.offset_ms);
        OffsetDateTime::from_unix_millis(ms).unwrap_or_else(|_| OffsetDateTime::now_utc())
    }
}

/// `CloudLinkWorker.Backoff`: full jitter in `[500 ms, min(60 s, 2^min(failures, 6) s))`, so
/// a cloud outage is not hammered by every hotel at once.
pub fn backoff(failures: u32) -> Duration {
    let cap = (1000u64 << failures.min(6)).min(60_000);
    Duration::from_millis(dl_rand::range(500..cap))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_stays_in_the_dotnet_window() {
        for f in 0..10 {
            let cap = (1000u64 << f.min(6)).min(60_000);
            for _ in 0..50 {
                let d = backoff(f).as_millis() as u64;
                assert!((500..cap).contains(&d), "{f}: {d}");
            }
        }
        let skewed = SkewedClock {
            offset_ms: -600_000,
        };
        assert!(SystemClock.now_ms() - skewed.now_ms() >= 599_000);
    }
}
