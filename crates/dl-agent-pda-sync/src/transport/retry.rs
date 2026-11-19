//! Exponential backoff with full jitter (port of `Transport/RetryPolicy.cs`). The sleeper
//! and the jitter are injectable so tests never wait.

use crate::util::ctx::Ctx;
use std::sync::Arc;
use std::time::Duration;

/// Sleeps for a delay unless the context ends first; `false` when it ended.
pub type Sleeper = Arc<dyn Fn(Duration, &Ctx) -> bool + Send + Sync>;
/// A value in `[0, 1)`.
pub type Jitter = Arc<dyn Fn() -> f64 + Send + Sync>;

/// Attempts and delays of the cloud calls.
#[derive(Clone)]
pub struct RetryPolicy {
    /// Attempts in total (6).
    pub max_attempts: u32,
    /// Delay before the first retry, before jitter (500 ms).
    pub base_delay: Duration,
    /// Largest delay before jitter (60 s).
    pub max_delay: Duration,
    /// How a delay is waited for (`Task.Delay`).
    pub sleep: Sleeper,
    /// The jitter source (`Random.Shared.NextDouble`).
    pub jitter: Jitter,
}

impl std::fmt::Debug for RetryPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryPolicy")
            .field("max_attempts", &self.max_attempts)
            .field("base_delay", &self.base_delay)
            .field("max_delay", &self.max_delay)
            .finish_non_exhaustive()
    }
}

impl Default for RetryPolicy {
    fn default() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 6,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(60),
            sleep: Arc::new(|d, ctx| ctx.sleep(d)),
            jitter: Arc::new(dl_rand::f64),
        }
    }
}

/// 408, 429, 502, 503, 504 and every 5xx.
pub fn is_transient(status: u16) -> bool {
    matches!(status, 408 | 429 | 502 | 503 | 504) || status >= 500
}

/// `TimeSpan.FromMilliseconds(double)`: whole ticks (100 ns), truncated.
pub fn from_millis_f64(ms: f64) -> Duration {
    if !ms.is_finite() || ms <= 0.0 {
        return Duration::ZERO;
    }
    let ticks = (ms * 10_000.0) as u64;
    Duration::from_nanos(ticks.saturating_mul(100))
}

impl RetryPolicy {
    /// A policy with `max_attempts` and the defaults otherwise.
    pub fn with_attempts(max_attempts: u32) -> RetryPolicy {
        RetryPolicy {
            max_attempts,
            ..RetryPolicy::default()
        }
    }

    /// Delay before attempt number `attempt + 1` (`attempt` is the 1-based retry index),
    /// honouring a `Retry-After` delta that is longer.
    pub fn delay_for(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        let base = self.base_delay.as_secs_f64() * 1000.0;
        let max = self.max_delay.as_secs_f64() * 1000.0;
        let exp = max.min(base * 2f64.powi(i32::try_from(attempt).unwrap_or(i32::MAX) - 1));
        let jittered = from_millis_f64(exp * (0.5 + (self.jitter)() * 0.5));
        match retry_after {
            Some(ra) if ra > jittered => ra,
            _ => jittered,
        }
    }
}

/// `RetryConditionHeaderValue.Delta`: a non-negative whole number of seconds; an HTTP date
/// or anything else gives `None`.
pub fn parse_retry_after(value: Option<&str>) -> Option<Duration> {
    let v = value?.trim();
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let secs: i32 = v.parse().ok()?;
    Some(Duration::from_secs(u64::try_from(secs).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_grow_and_honour_retry_after() {
        let p = RetryPolicy {
            jitter: Arc::new(|| 1.0),
            ..RetryPolicy::default()
        };
        assert_eq!(p.delay_for(1, None), Duration::from_millis(500));
        assert_eq!(p.delay_for(2, None), Duration::from_secs(1));
        assert_eq!(p.delay_for(9, None), Duration::from_secs(60));
        assert_eq!(
            p.delay_for(1, Some(Duration::from_secs(3))),
            Duration::from_secs(3)
        );
        let half = RetryPolicy {
            jitter: Arc::new(|| 0.0),
            ..RetryPolicy::default()
        };
        assert_eq!(half.delay_for(1, None), Duration::from_millis(250));
        // .NET 10 `RetryPolicy { Jitter = () => 0.37 }.DelayFor(1..9)` in ticks (`sa-core\b2b`).
        let p37 = RetryPolicy {
            jitter: Arc::new(|| 0.37),
            ..RetryPolicy::default()
        };
        let ticks: Vec<u128> = (1..=9)
            .map(|i| p37.delay_for(i, None).as_nanos() / 100)
            .collect();
        assert_eq!(
            ticks,
            [
                3425000, 6850000, 13700000, 27400000, 54800000, 109600000, 219200000, 411000000,
                411000000
            ]
        );
        assert_eq!(
            p37.delay_for(1, Some(Duration::from_secs(3))).as_nanos() / 100,
            30000000
        );
        assert_eq!(
            p37.delay_for(9, Some(Duration::from_secs(3))).as_nanos() / 100,
            411000000
        );
        assert_eq!(parse_retry_after(Some(" 7 ")), Some(Duration::from_secs(7)));
        assert_eq!(
            parse_retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT")),
            None
        );
        assert_eq!(parse_retry_after(Some("-1")), None);
        assert!(is_transient(500) && is_transient(408) && is_transient(429) && !is_transient(404));
    }
}
