//! Counters the collector and the relay worker share with the module thread, which
//! publishes them as heartbeat metrics and answers `einvoice.status` with them.

use std::sync::atomic::{AtomicU64, Ordering};

use dl_json::Map;

/// Running totals since the module started.
#[derive(Debug, Default)]
pub struct Stats {
    /// Collect cycles run.
    pub cycles: AtomicU64,
    /// Invoices read from SQL.
    pub read_total: AtomicU64,
    /// Invoices the platform accepted.
    pub pushed_total: AtomicU64,
    /// Invoices the platform answered with an error.
    pub errored_total: AtomicU64,
    /// Items waiting in the outbox after the last cycle.
    pub outbox_pending: AtomicU64,
    /// Unix seconds of the last push the platform answered (0: never).
    pub last_push_at: AtomicU64,
    /// Unix seconds when the last cycle ended (0: never).
    pub last_cycle_at: AtomicU64,
    /// Relayed calls received from the hub.
    pub relay_calls: AtomicU64,
    /// Relayed calls answered with an error (refused, failed or too large).
    pub relay_failures: AtomicU64,
    /// Relayed calls refused by the allowlist.
    pub relay_refused: AtomicU64,
    /// Failed polls of `/relay/next`.
    pub relay_poll_failures: AtomicU64,
}

impl Stats {
    /// Adds `n` to a counter.
    pub fn bump(c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::Relaxed);
    }

    /// Sets a gauge.
    pub fn set(c: &AtomicU64, v: u64) {
        c.store(v, Ordering::Relaxed);
    }

    /// Every counter by name, as published in the heartbeat.
    pub fn to_map(&self) -> Map {
        let mut m = Map::new();
        for (k, c) in [
            ("outbox_pending", &self.outbox_pending),
            ("pushed_total", &self.pushed_total),
            ("errored_total", &self.errored_total),
            ("read_total", &self.read_total),
            ("cycles", &self.cycles),
            ("last_push_at", &self.last_push_at),
            ("last_cycle_at", &self.last_cycle_at),
            ("relay_calls", &self.relay_calls),
            ("relay_failures", &self.relay_failures),
            ("relay_refused", &self.relay_refused),
            ("relay_poll_failures", &self.relay_poll_failures),
        ] {
            m.insert(k, c.load(Ordering::Relaxed));
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_show_up_by_name() {
        let s = Stats::default();
        Stats::bump(&s.pushed_total, 3);
        Stats::bump(&s.pushed_total, 2);
        Stats::set(&s.outbox_pending, 7);
        let m = s.to_map();
        assert_eq!(m.get("pushed_total").and_then(|v| v.as_u64()), Some(5));
        assert_eq!(m.get("outbox_pending").and_then(|v| v.as_u64()), Some(7));
        assert_eq!(m.get("relay_calls").and_then(|v| v.as_u64()), Some(0));
    }
}
