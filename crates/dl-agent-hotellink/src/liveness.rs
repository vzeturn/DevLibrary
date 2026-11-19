//! Loop liveness (Go `liveness.go`): every loop beats once per round; a loop whose last
//! beat is older than its threshold is reported as stalled. A loop that never beat is not.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The supervised loops.
pub const LOOPS: [&str; 3] = ["booking", "availability", "heartbeat"];

/// `stallThreshold`.
pub fn stall_threshold(name: &str, avail_interval: Duration) -> Duration {
    match name {
        "booking" => Duration::from_secs(5 * 60),
        "heartbeat" => Duration::from_secs(3 * 60),
        "availability" => avail_interval * 2 + Duration::from_secs(5 * 60),
        _ => Duration::from_secs(10 * 60),
    }
}

/// `loopBeats`.
#[derive(Debug, Default)]
pub struct LoopBeats {
    last: Mutex<[Option<Instant>; 3]>,
}

impl LoopBeats {
    /// `beat`: unknown names are ignored.
    pub fn beat(&self, name: &str, t: Instant) {
        if let Some(i) = LOOPS.iter().position(|n| *n == name) {
            self.last.lock().unwrap_or_else(|p| p.into_inner())[i] = Some(t);
        }
    }

    /// `stalled`: one threshold for every loop.
    pub fn stalled(&self, now: Instant, threshold: Duration) -> Vec<String> {
        self.stalled_by(now, |_| threshold)
    }

    /// `stalledLoops`: the per-loop thresholds.
    pub fn stalled_loops(&self, now: Instant, avail_interval: Duration) -> Vec<String> {
        self.stalled_by(now, |n| stall_threshold(n, avail_interval))
    }

    fn stalled_by(&self, now: Instant, threshold: impl Fn(&str) -> Duration) -> Vec<String> {
        let last = *self.last.lock().unwrap_or_else(|p| p.into_inner());
        let mut out: Vec<String> = LOOPS
            .iter()
            .zip(last)
            .filter_map(|(name, t)| {
                let t = t?;
                (now.saturating_duration_since(t) > threshold(name)).then(|| (*name).to_owned())
            })
            .collect();
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    fn base() -> Instant {
        Instant::now() + 60 * MIN
    }

    #[test]
    fn reports_stalled_loop() {
        let b = base();
        let lb = LoopBeats::default();
        for n in LOOPS {
            lb.beat(n, b);
        }
        lb.beat("booking", b - 10 * MIN);
        assert_eq!(lb.stalled(b, 5 * MIN), ["booking"]);
    }

    #[test]
    fn ignores_never_beaten() {
        let b = base();
        let lb = LoopBeats::default();
        lb.beat("booking", b);
        assert!(lb.stalled(b, 5 * MIN).is_empty());
    }

    #[test]
    fn per_loop_threshold() {
        let b = base();
        let lb = LoopBeats::default();
        lb.beat("availability", b - 20 * MIN);
        lb.beat("booking", b - 10 * MIN);
        lb.beat("heartbeat", b);
        assert_eq!(lb.stalled_loops(b, 30 * MIN), ["booking"]);
    }

    #[test]
    fn all_fresh() {
        let b = base();
        let lb = LoopBeats::default();
        lb.beat("booking", b);
        lb.beat("availability", b - MIN);
        lb.beat("heartbeat", b - 2 * MIN);
        assert!(lb.stalled(b, 5 * MIN).is_empty());
    }

    #[test]
    fn sorted_output() {
        let b = base();
        let lb = LoopBeats::default();
        for n in LOOPS {
            lb.beat(n, b - 100 * MIN);
        }
        assert_eq!(lb.stalled(b, MIN), ["availability", "booking", "heartbeat"]);
    }
}
