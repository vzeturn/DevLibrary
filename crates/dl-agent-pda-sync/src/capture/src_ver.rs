//! Strictly increasing `src_ver` source (`Strategies/SrcVerClock.cs`).

use std::sync::atomic::{AtomicI64, Ordering};

use crate::config::time::SharedClock;

/// Wall-clock Unix milliseconds, bumped when two calls land in the same millisecond.
pub struct SrcVerClock {
    clock: SharedClock,
    last: AtomicI64,
}

impl std::fmt::Debug for SrcVerClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SrcVerClock")
            .field("last", &self.last.load(Ordering::Relaxed))
            .finish()
    }
}

impl SrcVerClock {
    /// A clock over `clock`.
    pub fn new(clock: SharedClock) -> SrcVerClock {
        SrcVerClock {
            clock,
            last: AtomicI64::new(0),
        }
    }

    /// The next version: `max(previous + 1, now_ms)`.
    pub fn next(&self) -> i64 {
        // ToUnixTimeMilliseconds floors toward negative infinity.
        let now = i64::try_from(self.clock.now().unix_nanos().div_euclid(1_000_000)).unwrap_or(0);
        let mut previous = self.last.load(Ordering::SeqCst);
        loop {
            let next = previous.saturating_add(1).max(now);
            match self
                .last
                .compare_exchange(previous, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return next,
                Err(actual) => previous = actual,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::time::{ManualClock, Timestamp};
    use std::sync::Arc;

    #[test]
    fn src_ver_is_strictly_increasing_even_within_one_millisecond() {
        let clock = Arc::new(ManualClock::new(Timestamp::from_unix_seconds(86_400)));
        let v = SrcVerClock::new(clock);
        let values: Vec<i64> = (0..5).map(|_| v.next()).collect();
        assert!(values.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(values[0], 86_400_000);
    }
}
