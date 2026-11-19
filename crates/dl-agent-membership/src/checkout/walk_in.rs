//! The walk-in half of the checkout scan: `WalkInEarnState` (the heartbeat's
//! `WalkInEarnOk`) and the in-memory set of groups the hub already answered
//! (`_seenWalkIns`, `MarkWalkInSeen`, `ForgetWalkInsBefore`, `BuildWalkInExclusion`).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU8, Ordering};

use dl_decimal::Decimal;
use dl_time::OffsetDateTime;

use crate::log::{self, V};

/// `SqlServerInParamLimit`.
pub const IN_PARAM_LIMIT: usize = 500;
/// `SeenWalkInCapacity`.
pub const SEEN_CAPACITY: usize = 5_000;

const UNKNOWN: u8 = 0;
const HEALTHY: u8 = 1;
const DISABLED: u8 = 2;

/// `WalkInEarnState`: `None` until a walk-in scan ran (or while the feature is off),
/// `Some(true)` once one ran, `Some(false)` once the path turned itself off (for good).
#[derive(Debug, Default)]
pub struct WalkInEarnState(AtomicU8);

impl WalkInEarnState {
    /// `IsHealthy`.
    pub fn is_healthy(&self) -> Option<bool> {
        match self.0.load(Ordering::Acquire) {
            HEALTHY => Some(true),
            DISABLED => Some(false),
            _ => None,
        }
    }

    /// `MarkHealthy`: never overrides a disabled state.
    pub fn mark_healthy(&self) {
        let _ = self
            .0
            .compare_exchange(UNKNOWN, HEALTHY, Ordering::AcqRel, Ordering::Acquire);
    }

    /// `MarkDisabled`: one way, until the agent restarts.
    pub fn mark_disabled(&self) {
        self.0.store(DISABLED, Ordering::Release);
    }
}

/// The groups answered in this process, with their checkout time (oldest first).
#[derive(Debug, Default)]
pub struct SeenWalkIns {
    at: HashMap<Decimal, OffsetDateTime>,
    order: VecDeque<Decimal>,
    warned_truncated: bool,
}

impl SeenWalkIns {
    /// Whether the hub already answered `confirm_num`.
    pub fn contains(&self, confirm_num: &Decimal) -> bool {
        self.at.contains_key(confirm_num)
    }

    /// How many groups are remembered.
    pub fn len(&self) -> usize {
        self.at.len()
    }

    /// Whether nothing is remembered.
    pub fn is_empty(&self) -> bool {
        self.at.is_empty()
    }

    /// `MarkWalkInSeen`: FIFO-evicted at [`SEEN_CAPACITY`].
    pub fn mark(&mut self, confirm_num: Decimal, checked_out_at: OffsetDateTime) {
        if self.at.contains_key(&confirm_num) {
            return;
        }
        self.at.insert(confirm_num, checked_out_at);
        self.order.push_back(confirm_num);
        while self.order.len() > SEEN_CAPACITY {
            if let Some(old) = self.order.pop_front() {
                self.at.remove(&old);
            }
        }
    }

    /// `ForgetWalkInsBefore`: drops the groups that left the look-back window.
    pub fn forget_before(&mut self, since: OffsetDateTime) {
        while let Some(front) = self.order.front().copied() {
            if self.at.get(&front).is_some_and(|at| *at >= since) {
                break;
            }
            self.order.pop_front();
            self.at.remove(&front);
        }
    }

    /// `BuildWalkInExclusion`: every remembered group, or the oldest [`IN_PARAM_LIMIT`]
    /// (warned once per process) when there are more.
    pub fn exclusion(&mut self, max_per_scan: u32, lookback_days: i64) -> Vec<Decimal> {
        if self.order.len() > IN_PARAM_LIMIT && !self.warned_truncated {
            self.warned_truncated = true;
            log::warn(
                "walk-in backlog over the SQL parameter cap: the exclusion list is cut and the backlog drains slower; raise walk_in_max_per_scan or lower walk_in_lookback_days",
                &[
                    ("seen", V::I(self.order.len() as i64)),
                    ("limit", V::I(IN_PARAM_LIMIT as i64)),
                    ("walk_in_max_per_scan", V::I(i64::from(max_per_scan))),
                    ("walk_in_lookback_days", V::I(lookback_days)),
                ],
            );
        }
        self.order.iter().take(IN_PARAM_LIMIT).copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use dl_time::Duration;

    use super::*;

    #[test]
    fn the_state_has_three_values_and_disabled_sticks() {
        let s = WalkInEarnState::default();
        assert_eq!(s.is_healthy(), None);
        s.mark_healthy();
        assert_eq!(s.is_healthy(), Some(true));
        s.mark_disabled();
        s.mark_healthy();
        assert_eq!(s.is_healthy(), Some(false));
    }

    #[test]
    fn seen_groups_are_forgotten_by_time_and_capped() {
        let t0 = OffsetDateTime::now_utc();
        let mut seen = SeenWalkIns::default();
        for i in 0..(SEEN_CAPACITY as i64 + 3) {
            seen.mark(Decimal::from(i), t0 + Duration::from_seconds(i));
        }
        assert_eq!(seen.len(), SEEN_CAPACITY);
        assert!(!seen.contains(&Decimal::from(2)) && seen.contains(&Decimal::from(3)));
        let ex = seen.exclusion(100, 3);
        assert_eq!(ex.len(), IN_PARAM_LIMIT);
        assert_eq!(ex[0], Decimal::from(3), "the oldest first");
        seen.forget_before(t0 + Duration::from_seconds(10));
        assert!(!seen.contains(&Decimal::from(9)) && seen.contains(&Decimal::from(10)));
    }
}
