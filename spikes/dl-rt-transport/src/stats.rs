//! Percentiles of latency samples (nearest rank).

use std::time::Duration;

/// Summary of a sample set, microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// Number of samples.
    pub n: usize,
    /// Median.
    pub p50: u64,
    /// 95th percentile.
    pub p95: u64,
    /// 99th percentile.
    pub p99: u64,
    /// Largest sample.
    pub max: u64,
}

fn pct(sorted: &[u64], p: f64) -> u64 {
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// Summary, or `None` when there are no samples.
pub fn summarize(samples: &[Duration]) -> Option<Summary> {
    if samples.is_empty() {
        return None;
    }
    let mut us: Vec<u64> = samples.iter().map(|d| d.as_micros() as u64).collect();
    us.sort_unstable();
    Some(Summary {
        n: us.len(),
        p50: pct(&us, 50.0),
        p95: pct(&us, 95.0),
        p99: pct(&us, 99.0),
        max: us[us.len() - 1],
    })
}

/// Microseconds as milliseconds with one decimal.
pub fn ms(us: u64) -> String {
    format!("{:.1}", us as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank() {
        let s: Vec<Duration> = (1..=100).map(Duration::from_micros).collect();
        let m = summarize(&s).unwrap();
        assert_eq!((m.n, m.p50, m.p95, m.p99, m.max), (100, 50, 95, 99, 100));
        assert_eq!(summarize(&[]), None);
        assert_eq!(ms(1260), "1.3");
    }
}
