#![doc = include_str!("../README.md")]

use std::hint::black_box;
use std::time::{Duration, Instant};

/// Benchmark runner. Each case is warmed up, then sampled for a fixed time budget.
pub struct Bench {
    filter: Option<String>,
    warmup: Duration,
    measure: Duration,
    samples: usize,
}

/// Result of one benchmark case.
#[derive(Debug, Clone, Copy)]
pub struct Report {
    /// Median nanoseconds per iteration.
    pub median_ns: f64,
    /// Fastest sample, nanoseconds per iteration.
    pub min_ns: f64,
    /// Iterations measured in total.
    pub iterations: u64,
}

impl Default for Bench {
    fn default() -> Self {
        Self {
            filter: None,
            warmup: Duration::from_millis(200),
            measure: Duration::from_secs(1),
            samples: 30,
        }
    }
}

impl Bench {
    /// Creates a runner. The first non-flag command-line argument is used as a name filter
    /// (cargo passes `--bench` itself, which is ignored).
    pub fn from_args() -> Self {
        let filter = std::env::args().skip(1).find(|a| !a.starts_with('-'));
        Self {
            filter,
            ..Self::default()
        }
    }

    /// Sets the measurement time budget per case.
    pub fn measure_time(mut self, d: Duration) -> Self {
        self.measure = d;
        self
    }

    /// Benchmarks `f` and prints time per iteration.
    pub fn run<R>(&mut self, name: &str, f: impl FnMut() -> R) -> Option<Report> {
        self.case(name, None, f)
    }

    /// Benchmarks `f`, which processes `bytes` bytes per call, and prints throughput.
    pub fn throughput<R>(
        &mut self,
        name: &str,
        bytes: u64,
        f: impl FnMut() -> R,
    ) -> Option<Report> {
        self.case(name, Some(bytes), f)
    }

    fn case<R>(
        &mut self,
        name: &str,
        bytes: Option<u64>,
        mut f: impl FnMut() -> R,
    ) -> Option<Report> {
        if let Some(filter) = &self.filter
            && !name.contains(filter.as_str())
        {
            return None;
        }
        // Warm up and estimate the cost of one call.
        let start = Instant::now();
        let mut warm_iters = 0u64;
        while start.elapsed() < self.warmup {
            black_box(f());
            warm_iters += 1;
        }
        let per_iter = start.elapsed().as_secs_f64() / warm_iters as f64;
        let budget = self.measure.as_secs_f64() / self.samples as f64;
        let batch = ((budget / per_iter) as u64).max(1);

        let mut times = Vec::with_capacity(self.samples);
        for _ in 0..self.samples {
            let t = Instant::now();
            for _ in 0..batch {
                black_box(f());
            }
            times.push(t.elapsed().as_secs_f64() * 1e9 / batch as f64);
        }
        times.sort_by(f64::total_cmp);
        let report = Report {
            median_ns: times[times.len() / 2],
            min_ns: times[0],
            iterations: batch * self.samples as u64,
        };
        let extra = match bytes {
            Some(b) => format!(
                "{:>10.1} MiB/s",
                b as f64 / report.median_ns * 1e9 / 1048576.0
            ),
            None => String::new(),
        };
        println!("{name:<40} {:>12} {extra}", fmt_ns(report.median_ns));
        Some(report)
    }
}

fn fmt_ns(ns: f64) -> String {
    if ns < 1e3 {
        format!("{ns:.1} ns")
    } else if ns < 1e6 {
        format!("{:.2} µs", ns / 1e3)
    } else if ns < 1e9 {
        format!("{:.2} ms", ns / 1e6)
    } else {
        format!("{:.2} s", ns / 1e9)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_and_filters() {
        let mut b = Bench {
            filter: Some("keep".into()),
            ..Bench::default()
        }
        .measure_time(Duration::from_millis(30));
        b.warmup = Duration::from_millis(5);
        assert!(b.run("skip me", || 1).is_none());
        let r = b.run("keep me", || black_box(2u64).pow(3)).unwrap();
        assert!(r.median_ns >= 0.0 && r.iterations > 0);
    }
}
