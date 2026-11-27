//! The clock of the per-hop latency stamps (RT-1318, ADR 0011 "Each hop is measured"):
//! unix epoch microseconds that follow the wall clock, so the cloud can set them against
//! its own, and never step back within the process, so the agent's own spans (detected to
//! sent, received to finished) are never negative.
//!
//! Between two re-syncs a stamp is the anchored wall reading plus the monotonic time since
//! it. The wall clock is read on every stamp; once it is more than [`RESYNC_US`] away
//! (an NTP step, a monotonic clock that drifted) the clock re-anchors to it. A small step
//! back is held at the last stamp; a large one (the clock was set back) is followed, since
//! holding would freeze every stamp for as long as the step, so only the span across it is
//! off. Nothing here panics: a wall clock before 1970 gives 0, which callers send as "no
//! stamp" ([`field`]).

use std::cell::Cell;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// How far the monotonic estimate may drift from the wall clock before re-anchoring, and
/// the largest step back that is held instead of followed (microseconds).
pub const RESYNC_US: i64 = 2_000;

/// The state of the stamp clock, fed with readings (pure, so the rules can be tested).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HopClock {
    anchored: bool,
    anchor_wall: i64,
    anchor_mono: i64,
    last: i64,
}

impl HopClock {
    /// A clock that anchors on its first reading.
    pub const fn new() -> HopClock {
        HopClock {
            anchored: false,
            anchor_wall: 0,
            anchor_mono: 0,
            last: 0,
        }
    }

    /// The stamp for wall reading `wall` (epoch µs; 0 or less: unknown) taken at monotonic
    /// `mono` µs. 0 until a wall reading is known.
    pub fn stamp(&mut self, wall: i64, mono: i64) -> i64 {
        if !self.anchored {
            if wall <= 0 {
                return 0;
            }
            *self = HopClock {
                anchored: true,
                anchor_wall: wall,
                anchor_mono: mono,
                last: wall,
            };
            return wall;
        }
        let since = mono.saturating_sub(self.anchor_mono).max(0);
        let mut at = self.anchor_wall.saturating_add(since);
        if wall > 0 && wall.abs_diff(at) > RESYNC_US.unsigned_abs() {
            self.anchor_wall = wall;
            self.anchor_mono = mono;
            at = wall;
        }
        if at < self.last && self.last.saturating_sub(at) <= RESYNC_US {
            at = self.last;
        }
        self.last = at;
        at
    }
}

static CLOCK: Mutex<HopClock> = Mutex::new(HopClock::new());
static START: OnceLock<Instant> = OnceLock::new();

thread_local! {
    static PASS: Cell<i64> = const { Cell::new(0) };
}

/// The wall clock in epoch microseconds; 0 when it reads before 1970.
pub fn wall_micros() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_micros()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

/// The monotonic time since the first stamp of the process, in microseconds.
fn mono_micros() -> i64 {
    let start = *START.get_or_init(Instant::now);
    i64::try_from(Instant::now().saturating_duration_since(start).as_micros()).unwrap_or(i64::MAX)
}

/// A stamp now (epoch µs); 0 when the wall clock is unknown.
pub fn now_micros() -> i64 {
    let mono = mono_micros();
    let wall = wall_micros();
    CLOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .stamp(wall, mono)
}

/// A stamp as a wire field: 0 (no clock) is no stamp.
pub fn field(at: i64) -> Option<i64> {
    (at > 0).then_some(at)
}

/// A stamp now as a wire field.
pub fn now_field() -> Option<i64> {
    field(now_micros())
}

/// Marks the capture pass running on this thread (RT-1318): the batches it writes carry
/// the pass start as `t_detected`. Dropping it restores the pass around it.
#[must_use = "the pass ends when the stamp is dropped"]
pub struct PassStamp {
    outer: i64,
}

impl PassStamp {
    /// Starts a pass now.
    pub fn begin() -> PassStamp {
        let now = now_micros();
        let outer = PASS.try_with(|p| p.replace(now)).unwrap_or(0);
        PassStamp { outer }
    }
}

impl Drop for PassStamp {
    fn drop(&mut self) {
        let _ = PASS.try_with(|p| p.set(self.outer));
    }
}

/// The start of the capture pass running on this thread; `None` outside a pass.
pub fn pass_started() -> Option<i64> {
    field(PASS.try_with(Cell::get).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_764_144_000_000_000;

    #[test]
    fn follows_monotonic_time_between_resyncs() {
        let mut c = HopClock::new();
        assert_eq!(c.stamp(T0, 0), T0);
        // The wall clock reads 500 µs fast: within RESYNC_US the monotonic time wins.
        assert_eq!(c.stamp(T0 + 1_500, 1_000), T0 + 1_000);
        assert_eq!(c.stamp(T0 + 2_000, 2_000), T0 + 2_000);
    }

    #[test]
    fn re_anchors_on_a_forward_step() {
        let mut c = HopClock::new();
        c.stamp(T0, 0);
        assert_eq!(c.stamp(T0 + 1_000_000, 10), T0 + 1_000_000);
        assert_eq!(c.stamp(T0 + 1_000_100, 110), T0 + 1_000_100);
    }

    #[test]
    fn holds_a_small_step_back() {
        let mut c = HopClock::new();
        c.stamp(T0, 0);
        assert_eq!(c.stamp(T0 + 10_000, 10_000), T0 + 10_000);
        // The monotonic clock ran 2.5 ms ahead of the wall clock: re-anchored 2.5 ms back,
        // held at the last stamp until the clock passes it.
        assert_eq!(c.stamp(T0 + 9_000, 11_500), T0 + 10_000);
        assert_eq!(c.stamp(T0 + 9_600, 12_100), T0 + 10_000);
        assert_eq!(c.stamp(T0 + 10_500, 13_000), T0 + 10_500);
    }

    #[test]
    fn follows_a_large_step_back_without_panicking() {
        let mut c = HopClock::new();
        c.stamp(T0, 0);
        let back = T0 - 3_600_000_000;
        assert_eq!(c.stamp(back, 1_000), back);
        assert_eq!(c.stamp(back + 1_000, 2_000), back + 1_000);
    }

    #[test]
    fn an_unknown_wall_clock_gives_no_stamp() {
        let mut c = HopClock::new();
        assert_eq!(c.stamp(0, 0), 0);
        assert_eq!(c.stamp(-5, 10), 0);
        assert_eq!(field(0), None);
        c.stamp(T0, 20);
        // Once anchored, a wall clock that stops reading keeps the monotonic estimate.
        assert_eq!(c.stamp(0, 1_020), T0 + 1_000);
    }

    #[test]
    fn monotonic_going_back_saturates() {
        let mut c = HopClock::new();
        c.stamp(T0, 5_000);
        assert_eq!(c.stamp(T0, 1_000), T0);
        assert_eq!(c.stamp(i64::MAX, i64::MAX), i64::MAX);
    }

    #[test]
    fn real_stamps_never_go_back() {
        let mut last = now_micros();
        assert!(last > T0, "the wall clock reads after 2025-11-26");
        for _ in 0..1_000 {
            let now = now_micros();
            assert!(now >= last);
            last = now;
        }
    }

    #[test]
    fn a_pass_stamp_is_scoped_to_its_thread_and_nests() {
        assert_eq!(pass_started(), None);
        let outer = PassStamp::begin();
        let first = pass_started().expect("in a pass");
        {
            let _inner = PassStamp::begin();
            assert!(pass_started().expect("in the inner pass") >= first);
            std::thread::spawn(|| assert_eq!(pass_started(), None))
                .join()
                .expect("thread ends");
        }
        assert_eq!(pass_started(), Some(first));
        drop(outer);
        assert_eq!(pass_started(), None);
    }
}
