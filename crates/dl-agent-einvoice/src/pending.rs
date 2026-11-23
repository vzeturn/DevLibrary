//! Pending selection split (RT-0784), port of MM-EINVOICE `collectsource_pending.go`.
//!
//! The select SPs take an optional `@Mode`: 0 (default) is the original query that
//! re-hashes every sent bill in the CollectFrom window on every call; 1 selects only
//! never-sent bills; 2 finds edited bills dated on or after `@ChangedFrom`; 3 pages
//! through every sent bill (key below `@After`) reporting which ones changed. The
//! planner calls mode 1 every cycle, mode 2 every [`RECENT_SCAN_EVERY`] over the last
//! [`RECENT_WINDOW`], and runs one full mode-3 sweep a night. An install whose SP has
//! no `@Mode` (the SP is only installed at enrolment) keeps the single mode-0 call.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

use dl_time::{Date, Duration, Offset, OffsetDateTime};

/// The original combined query (the only mode an old SP has).
pub const MODE_LEGACY: u8 = 0;
/// Never-sent bills only.
pub const MODE_NEW: u8 = 1;
/// Edited bills dated on or after `@ChangedFrom`.
pub const MODE_RECENT: u8 = 2;
/// One page of the night sweep over every sent bill.
pub const MODE_SWEEP: u8 = 3;

/// How often the recent-edit scan runs.
pub const RECENT_SCAN_EVERY: Duration = Duration::from_seconds(2 * 60);
/// How far back the recent-edit scan looks.
pub const RECENT_WINDOW: Duration = Duration::from_seconds(48 * 60 * 60);
/// Sent bills per sweep page (one page per cycle).
pub const SWEEP_PAGE_SIZE: i64 = 500;
/// Vietnam-time hour from which the nightly sweep may start; it runs once per
/// Vietnam calendar day, one page per cycle.
pub const SWEEP_START_HOUR: u8 = 2;

/// One select SP invocation.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingCall {
    /// `@Mode`; [`MODE_LEGACY`] sends only `@Limit` and `@CollectFrom`.
    pub mode: u8,
    /// `@Limit`.
    pub limit: i64,
    /// `@CollectFrom`.
    pub collect_from: Option<OffsetDateTime>,
    /// `@ChangedFrom` (mode 2), in Vietnam time like `InvDate`.
    pub changed_from: Option<OffsetDateTime>,
    /// Sweep cursor (last key of the previous page); `None` = from the top.
    pub after: Option<String>,
}

/// One row of a select SP result; `changed` is only set by mode 3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRow {
    /// The bill or check key.
    pub id: String,
    /// Mode 3: the source changed since it was sent.
    pub changed: bool,
}

/// Runs the select SP for a source.
pub trait PendingQuerier {
    /// Whether the installed SP takes `@Mode`.
    fn supports_split(&self, source: &str) -> Result<bool, String>;
    /// Runs one call and returns its rows.
    fn query(&self, source: &str, call: &PendingCall) -> Result<Vec<PendingRow>, String>;
}

/// One cycle's pending keys. `edit_error` is set when edit detection (mode 2 or 3)
/// failed: the keys are still valid and the caller logs the error.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Selection {
    /// Never-sent keys first, then edited ones, without duplicates.
    pub ids: Vec<String>,
    /// Why edit detection failed this cycle.
    pub edit_error: Option<String>,
}

#[derive(Default)]
struct SourceState {
    probed: bool,
    split: bool,
    last_recent: Option<OffsetDateTime>,
    sweep_done_day: Option<Date>,
    sweep_cursor: Option<String>,
    sweep_active: bool,
}

type Clock = Box<dyn Fn() -> OffsetDateTime + Send + Sync>;

/// Per-source selection state for one agent process. FO and POS each have their own
/// lock so their selects run in parallel.
pub struct PendingPlanner {
    now: Clock,
    fo: Mutex<SourceState>,
    pos: Mutex<SourceState>,
}

/// UTC+7.
pub fn vn_offset() -> Offset {
    Offset::from_hms(7, 0, 0).unwrap_or(Offset::UTC)
}

impl Default for PendingPlanner {
    fn default() -> Self {
        PendingPlanner::new(Box::new(OffsetDateTime::now_utc))
    }
}

impl PendingPlanner {
    /// A planner reading the time from `now`.
    pub fn new(now: Clock) -> PendingPlanner {
        PendingPlanner {
            now,
            fo: Mutex::default(),
            pos: Mutex::default(),
        }
    }

    /// Returns the pending keys for one cycle. `Err` only when the new-bill select
    /// (or the legacy call) failed.
    pub fn select(
        &self,
        q: &dyn PendingQuerier,
        source: &str,
        limit: i64,
        from: Option<OffsetDateTime>,
    ) -> Result<Selection, String> {
        let lock = if source == "POS" { &self.pos } else { &self.fo };
        let mut st = lock.lock().unwrap_or_else(PoisonError::into_inner);

        if !st.probed {
            // A failed probe falls back to the old call this cycle and retries next cycle.
            if let Ok(split) = q.supports_split(source) {
                st.probed = true;
                st.split = split;
            }
        }
        let call = |mode, limit, changed_from, after| PendingCall {
            mode,
            limit,
            collect_from: from,
            changed_from,
            after,
        };
        if !st.split {
            let rows = q.query(source, &call(MODE_LEGACY, limit, None, None))?;
            return Ok(Selection {
                ids: row_ids(&rows, false),
                edit_error: None,
            });
        }

        let rows = q.query(source, &call(MODE_NEW, limit, None, None))?;
        let mut ids = row_ids(&rows, false);
        let mut seen: HashSet<String> = ids.iter().cloned().collect();
        let mut add = |more: Vec<String>, ids: &mut Vec<String>| {
            for id in more {
                if seen.insert(id.clone()) {
                    ids.push(id);
                }
            }
        };

        let now = (self.now)().to_offset(vn_offset());
        let mut errs = Vec::new();
        if st
            .last_recent
            .is_none_or(|t| now.duration_since(t) >= RECENT_SCAN_EVERY)
        {
            // InvDate is Vietnam wall-clock time and the SP parameter is a plain
            // datetime, so the window start is sent in Vietnam time.
            let mut changed_from = now - RECENT_WINDOW;
            if let Some(f) = from
                && f > changed_from
            {
                changed_from = f;
            }
            match q.query(source, &call(MODE_RECENT, limit, Some(changed_from), None)) {
                Ok(rows) => {
                    st.last_recent = Some(now);
                    add(row_ids(&rows, false), &mut ids);
                }
                Err(e) => errs.push(format!("recent edits: {e}")),
            }
        }

        let today = now.date();
        if !st.sweep_active && st.sweep_done_day != Some(today) && now.hour() >= SWEEP_START_HOUR {
            st.sweep_active = true;
            st.sweep_cursor = None;
        }
        if st.sweep_active {
            let after = st.sweep_cursor.clone();
            match q.query(
                source,
                &call(MODE_SWEEP, SWEEP_PAGE_SIZE, None, after.clone()),
            ) {
                Ok(rows) => {
                    add(row_ids(&rows, true), &mut ids);
                    if (rows.len() as i64) < SWEEP_PAGE_SIZE {
                        st.sweep_active = false;
                        st.sweep_cursor = None;
                        st.sweep_done_day = Some(today);
                    } else {
                        st.sweep_cursor = rows.last().map(|r| r.id.clone());
                    }
                }
                Err(e) => errs.push(format!(
                    "night sweep after {:?}: {e}",
                    after.unwrap_or_default()
                )),
            }
        }
        let edit_error = (!errs.is_empty()).then(|| {
            format!(
                "agent: edit detection failed ({source}): {}",
                errs.join("; ")
            )
        });
        Ok(Selection { ids, edit_error })
    }
}

fn row_ids(rows: &[PendingRow], changed_only: bool) -> Vec<String> {
    rows.iter()
        .filter(|r| !changed_only || r.changed)
        .map(|r| r.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::sync::Arc;

    type Answer = Box<dyn Fn(&PendingCall) -> Result<Vec<PendingRow>, String>>;

    #[derive(Default)]
    struct Fake {
        split: Cell<bool>,
        probe_err: RefCell<Option<String>>,
        probes: Cell<u32>,
        calls: RefCell<Vec<PendingCall>>,
        answer: RefCell<HashMap<u8, Answer>>,
    }

    impl PendingQuerier for Fake {
        fn supports_split(&self, _: &str) -> Result<bool, String> {
            self.probes.set(self.probes.get() + 1);
            match self.probe_err.borrow().clone() {
                Some(e) => Err(e),
                None => Ok(self.split.get()),
            }
        }
        fn query(&self, _: &str, c: &PendingCall) -> Result<Vec<PendingRow>, String> {
            self.calls.borrow_mut().push(c.clone());
            match self.answer.borrow().get(&c.mode) {
                Some(f) => f(c),
                None => Ok(Vec::new()),
            }
        }
    }

    impl Fake {
        fn split() -> Fake {
            let f = Fake::default();
            f.split.set(true);
            f
        }
        fn on(
            &self,
            mode: u8,
            f: impl Fn(&PendingCall) -> Result<Vec<PendingRow>, String> + 'static,
        ) {
            self.answer.borrow_mut().insert(mode, Box::new(f));
        }
        fn take(&self) -> Vec<PendingCall> {
            std::mem::take(&mut *self.calls.borrow_mut())
        }
        fn modes(&self) -> Vec<u8> {
            self.take().iter().map(|c| c.mode).collect()
        }
    }

    fn rows(ids: &[&str]) -> Vec<PendingRow> {
        ids.iter()
            .map(|id| PendingRow {
                id: (*id).to_owned(),
                changed: false,
            })
            .collect()
    }

    fn vn(day: u8, hour: u8, minute: u8) -> OffsetDateTime {
        let d = Date::new(2026, 11, day).unwrap();
        let t = dl_time::Time::from_hms(hour, minute, 0).unwrap();
        OffsetDateTime::new(dl_time::DateTime::new(d, t), vn_offset())
    }

    struct Clk(Arc<Mutex<OffsetDateTime>>);
    impl Clk {
        fn new(t: OffsetDateTime) -> (Clk, PendingPlanner) {
            let shared = Arc::new(Mutex::new(t));
            let c = shared.clone();
            (
                Clk(shared),
                PendingPlanner::new(Box::new(move || *c.lock().unwrap())),
            )
        }
        fn set(&self, t: OffsetDateTime) {
            *self.0.lock().unwrap() = t;
        }
        fn add(&self, d: Duration) {
            let mut g = self.0.lock().unwrap();
            *g += d;
        }
        fn now(&self) -> OffsetDateTime {
            *self.0.lock().unwrap()
        }
    }

    #[test]
    fn old_sp_keeps_the_single_legacy_call() {
        let q = Fake::default();
        q.on(MODE_LEGACY, |_| Ok(rows(&["9", "8"])));
        let (clk, p) = Clk::new(vn(23, 3, 0));
        let from = Some(vn(1, 0, 0));
        for cycle in 0..3 {
            let s = p.select(&q, "FO", 50, from).unwrap();
            assert_eq!(s.ids, ["9", "8"], "cycle {cycle}");
            let calls = q.take();
            assert_eq!(calls.len(), 1, "cycle {cycle}: {calls:?}");
            assert_eq!(
                (calls[0].mode, calls[0].limit, calls[0].collect_from),
                (MODE_LEGACY, 50, from)
            );
            clk.add(Duration::from_seconds(600));
        }
        assert_eq!(q.probes.get(), 1, "probe once per process");
    }

    #[test]
    fn probe_failure_falls_back_and_retries() {
        let q = Fake::default();
        *q.probe_err.borrow_mut() = Some("permission denied".into());
        let (_clk, p) = Clk::new(vn(23, 1, 0));
        p.select(&q, "POS", 10, None).unwrap();
        assert_eq!(q.modes(), [MODE_LEGACY]);
        *q.probe_err.borrow_mut() = None;
        q.split.set(true);
        p.select(&q, "POS", 10, None).unwrap();
        assert_eq!(q.probes.get(), 2);
        assert_eq!(q.modes(), [MODE_NEW, MODE_RECENT]);
    }

    #[test]
    fn recent_edits_window_and_cadence() {
        let q = Fake::split();
        q.on(MODE_NEW, |_| Ok(rows(&["30", "29"])));
        q.on(MODE_RECENT, |_| Ok(rows(&["29", "12"])));
        let (clk, p) = Clk::new(vn(23, 1, 0));
        let old = Some(vn(1, 0, 0));

        let s = p.select(&q, "FO", 50, old).unwrap();
        assert_eq!(s.ids, ["30", "29", "12"], "deduplicated, new first");
        let calls = q.take();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].changed_from, Some(clk.now() - RECENT_WINDOW));
        assert_eq!(
            calls[1].changed_from.unwrap().offset(),
            vn_offset(),
            "sent in Vietnam time"
        );

        clk.add(Duration::from_seconds(60));
        p.select(&q, "FO", 50, old).unwrap();
        assert_eq!(q.modes(), [MODE_NEW], "one minute later");

        clk.add(RECENT_SCAN_EVERY);
        let recent = clk.now() - Duration::from_seconds(3600);
        p.select(&q, "FO", 50, Some(recent)).unwrap();
        let calls = q.take();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            (calls[1].mode, calls[1].changed_from),
            (MODE_RECENT, Some(recent))
        );
    }

    #[test]
    fn night_sweep_pages_once_per_day() {
        // 1200 sent bills, keys 1200..1; 7 and 800 were edited.
        let q = Fake::split();
        q.on(MODE_SWEEP, |c| {
            let top = match &c.after {
                Some(a) => a.parse::<i64>().unwrap() - 1,
                None => 1200,
            };
            Ok((1..=top)
                .rev()
                .take(c.limit as usize)
                .map(|k| PendingRow {
                    id: k.to_string(),
                    changed: k == 7 || k == 800,
                })
                .collect())
        });
        let (clk, p) = Clk::new(vn(23, 1, 59));
        p.select(&q, "POS", 50, None).unwrap();
        assert_eq!(q.modes(), [MODE_NEW, MODE_RECENT], "no sweep before 02:00");

        let mut found = Vec::new();
        let mut afters = Vec::new();
        for cycle in 0..3 {
            clk.set(vn(23, 2, cycle));
            found.extend(p.select(&q, "POS", 50, None).unwrap().ids);
            for c in q.take() {
                if c.mode == MODE_SWEEP {
                    assert_eq!(c.limit, SWEEP_PAGE_SIZE);
                    afters.push(c.after.unwrap_or_default());
                }
            }
        }
        assert_eq!(afters, ["", "701", "201"]);
        assert_eq!(found, ["800", "7"], "only edited bills");

        clk.set(vn(23, 23, 0));
        p.select(&q, "POS", 50, None).unwrap();
        assert!(
            !q.modes().contains(&MODE_SWEEP),
            "sweep ran twice on the same day"
        );
        clk.set(vn(24, 2, 0));
        p.select(&q, "POS", 50, None).unwrap();
        let last = q.take().pop().unwrap();
        assert_eq!(
            (last.mode, last.after),
            (MODE_SWEEP, None),
            "next night from the top"
        );
    }

    #[test]
    fn edit_detection_failure_keeps_new_bills() {
        let q = Fake::split();
        q.on(MODE_NEW, |_| Ok(rows(&["5"])));
        q.on(MODE_RECENT, |_| Err("timeout".into()));
        q.on(MODE_SWEEP, |_| Err("timeout".into()));
        let (clk, p) = Clk::new(vn(23, 3, 0));
        let s = p.select(&q, "FO", 50, None).unwrap();
        assert_eq!(s.ids, ["5"]);
        let e = s.edit_error.expect("edit error");
        assert!(
            e.contains("recent edits: timeout") && e.contains("night sweep"),
            "{e}"
        );
        q.take();
        clk.add(Duration::from_seconds(30));
        let _ = p.select(&q, "FO", 50, None);
        assert_eq!(
            q.modes(),
            [MODE_NEW, MODE_RECENT, MODE_SWEEP],
            "retried next cycle"
        );

        q.on(MODE_NEW, |_| Err("boom".into()));
        assert_eq!(p.select(&q, "FO", 50, None), Err("boom".to_owned()));
    }
}
