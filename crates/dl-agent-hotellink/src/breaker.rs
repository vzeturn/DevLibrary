//! Circuit breaker for PMS writes (Go `agent/breaker`): opens after `threshold`
//! consecutive failures, admits one half-open probe after the cooldown, and doubles the
//! cooldown (up to `max`) every time the probe fails.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A monotonic clock, swappable in tests.
pub type Now = Arc<dyn Fn() -> Instant + Send + Sync>;

struct St {
    fails: u32,
    open: bool,
    half_open: bool,
    opened_at: Instant,
    cooldown: Duration,
}

/// `Breaker`.
pub struct Breaker {
    threshold: u32,
    base: Duration,
    max: Duration,
    now: Now,
    st: Mutex<St>,
}

impl std::fmt::Debug for Breaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Breaker")
            .field("state", &self.state())
            .finish()
    }
}

impl Breaker {
    /// `New`.
    pub fn new(threshold: u32, base: Duration, max: Duration) -> Breaker {
        Breaker::with_clock(threshold, base, max, Arc::new(Instant::now))
    }

    /// `New` with a clock.
    pub fn with_clock(threshold: u32, base: Duration, max: Duration, now: Now) -> Breaker {
        let t = now();
        Breaker {
            threshold,
            base,
            max,
            now,
            st: Mutex::new(St {
                fails: 0,
                open: false,
                half_open: false,
                opened_at: t,
                cooldown: base,
            }),
        }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, St> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// `Allow`: closed, or open past the cooldown (which turns it half-open).
    pub fn allow(&self) -> bool {
        let mut st = self.st();
        if !st.open {
            return true;
        }
        if (self.now)().saturating_duration_since(st.opened_at) >= st.cooldown {
            st.half_open = true;
            return true;
        }
        false
    }

    /// `OnSuccess`: closes and resets the cooldown.
    pub fn on_success(&self) {
        let mut st = self.st();
        st.fails = 0;
        st.open = false;
        st.half_open = false;
        st.cooldown = self.base;
    }

    /// `OnFailure`.
    pub fn on_failure(&self) {
        let mut st = self.st();
        if st.half_open {
            st.half_open = false;
            st.opened_at = (self.now)();
            st.cooldown = (st.cooldown * 2).min(self.max);
            return;
        }
        st.fails += 1;
        if st.fails >= self.threshold {
            st.open = true;
            st.opened_at = (self.now)();
        }
    }

    /// `State`: `half-open`, `open` or `closed`.
    pub fn state(&self) -> &'static str {
        let st = self.st();
        if st.half_open {
            "half-open"
        } else if st.open {
            "open"
        } else {
            "closed"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock() -> (Arc<Mutex<Duration>>, Now) {
        let base = Instant::now();
        let off = Arc::new(Mutex::new(Duration::ZERO));
        let o = off.clone();
        (off, Arc::new(move || base + *o.lock().expect("clock")))
    }

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn trips_after_n() {
        let (_, now) = clock();
        let b = Breaker::with_clock(3, MIN, 10 * MIN, now);
        for i in 0..2 {
            b.on_failure();
            assert!(b.allow(), "tripped too early at {i}");
        }
        b.on_failure();
        assert!(!b.allow(), "open after 3 failures");
        assert_eq!(b.state(), "open");
    }

    #[test]
    fn half_open_probe_recovers() {
        let (off, now) = clock();
        let b = Breaker::with_clock(1, MIN, 10 * MIN, now);
        b.on_failure();
        assert!(!b.allow());
        *off.lock().expect("clock") += 2 * MIN;
        assert!(b.allow(), "probe after cooldown");
        assert_eq!(b.state(), "half-open");
        b.on_success();
        assert!(b.allow() && b.state() == "closed");
    }

    #[test]
    fn half_open_probe_fails_reopens_with_longer_cooldown() {
        let (off, now) = clock();
        let b = Breaker::with_clock(1, MIN, 3 * MIN, now);
        b.on_failure();
        *off.lock().expect("clock") += 2 * MIN;
        assert!(b.allow());
        b.on_failure();
        assert!(!b.allow(), "re-open after failed probe");
        *off.lock().expect("clock") += MIN + Duration::from_secs(1);
        assert!(!b.allow(), "cooldown doubled to 2 min");
        *off.lock().expect("clock") += MIN;
        assert!(b.allow());
        b.on_failure();
        *off.lock().expect("clock") += 3 * MIN;
        assert!(b.allow(), "cooldown capped at max");
    }
}
