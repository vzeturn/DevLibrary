//! Capture health for the heartbeat (port of the agent's `Capture/CaptureStatusTracker.cs`).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::engine::OPEN_STREAM;
use super::failure::CaptureFailureInfo;
use super::plan::CapturePlan;
use super::report::CaptureReport;
use crate::config::time::{SharedClock, Timestamp};
use crate::contracts::wire::StreamLag;
use crate::util::stj::eq_ignore_case;

/// The open-check job.
pub const OPEN_JOB: &str = "open";
/// The ledger tail job.
pub const LEDGER_TAIL_JOB: &str = "ledger-tail";
/// The ledger rescan job.
pub const LEDGER_RESCAN_JOB: &str = "ledger-rescan";
/// The master scan job.
pub const MASTERS_JOB: &str = "masters";
/// The initial load.
pub const SNAPSHOT_JOB: &str = "snapshot";

#[derive(Debug, Clone, Copy)]
struct StreamState {
    fresh: Timestamp,
    lag_rows: i64,
}

#[derive(Debug, Default)]
struct Inner {
    streams: BTreeMap<String, StreamState>,
    /// Vendor table -> wire stream (tables matched ignoring case).
    stream_of_table: Vec<(String, String)>,
    job_streams: Vec<(&'static str, Vec<String>)>,
    runs: HashMap<String, i32>,
    sql_reachable: bool,
    error: Option<String>,
    error_detail: Option<dl_json::Map>,
}

impl Inner {
    fn stream_of(&self, table: &str) -> String {
        self.stream_of_table
            .iter()
            .find(|(t, _)| eq_ignore_case(t, table))
            .map_or_else(|| table.to_owned(), |(_, s)| s.clone())
    }

    fn set_stream(&mut self, table: &str, stream: &str) {
        match self
            .stream_of_table
            .iter_mut()
            .find(|(t, _)| eq_ignore_case(t, table))
        {
            Some(slot) => slot.1 = stream.to_owned(),
            None => self
                .stream_of_table
                .push((table.to_owned(), stream.to_owned())),
        }
    }

    fn set_job(&mut self, job: &'static str, streams: Vec<String>) {
        match self.job_streams.iter_mut().find(|(j, _)| *j == job) {
            Some(slot) => slot.1 = streams,
            None => self.job_streams.push((job, streams)),
        }
    }
}

/// Thread-safe capture health for the heartbeat: SQL reachability, the current failure
/// code and per-stream lag. A stream's lag is the time since the job that feeds it last
/// completed a full pass (since start-up if never), so a failing or stalled capture shows
/// growing lag instead of an empty, healthy-looking list. Lag is whole seconds.
pub struct CaptureStatusTracker {
    clock: SharedClock,
    started: Timestamp,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for CaptureStatusTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureStatusTracker")
            .finish_non_exhaustive()
    }
}

impl CaptureStatusTracker {
    /// A tracker starting now with `initial_plan`.
    pub fn new(clock: SharedClock, initial_plan: &CapturePlan) -> CaptureStatusTracker {
        let started = clock.now();
        let t = CaptureStatusTracker {
            clock,
            started,
            inner: Mutex::new(Inner::default()),
        };
        t.use_plan(initial_plan);
        t
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the SQL Server was last known usable.
    pub fn sql_reachable(&self) -> bool {
        self.lock().sql_reachable
    }

    /// The code of the failure that currently stops capture.
    pub fn error(&self) -> Option<String> {
        self.lock().error.clone()
    }

    /// Safe facts about the current failure (schema terms and SQL numbers, never row
    /// data); a copy.
    pub fn error_detail(&self) -> Option<dl_json::Map> {
        self.lock().error_detail.clone()
    }

    /// Current lag of every planned stream, ordered by name.
    pub fn stream_lags(&self) -> Vec<StreamLag> {
        let now = self.clock.now();
        let inner = self.lock();
        inner
            .streams
            .iter()
            .map(|(name, s)| {
                let ticks = (now.unix_nanos() - s.fresh.unix_nanos()) / 100;
                let seconds = (ticks as f64 / 10_000_000.0).floor();
                StreamLag {
                    stream: name.clone(),
                    lag_rows: s.lag_rows,
                    lag_seconds: seconds.max(0.0),
                }
            })
            .collect()
    }

    /// Successful passes of `job` so far.
    pub fn runs(&self, job: &str) -> i32 {
        self.lock().runs.get(job).copied().unwrap_or(0)
    }

    /// Maps the session's plan to wire streams; streams that left the plan are dropped,
    /// kept ones keep their lag.
    pub fn use_plan(&self, plan: &CapturePlan) {
        let mut inner = self.lock();
        inner.stream_of_table.clear();
        for p in &plan.open {
            inner.set_stream(p.name(), OPEN_STREAM);
        }
        for p in plan.ledger.iter().chain(&plan.masters) {
            inner.set_stream(p.name(), p.name());
        }
        let open: Vec<String> = if plan.open.is_empty() {
            Vec::new()
        } else {
            vec![OPEN_STREAM.to_owned()]
        };
        let masters: Vec<String> = plan.masters.iter().map(|p| p.name().to_owned()).collect();
        inner.set_job(OPEN_JOB, open.clone());
        inner.set_job(
            LEDGER_TAIL_JOB,
            plan.ledger.iter().map(|p| p.name().to_owned()).collect(),
        );
        inner.set_job(LEDGER_RESCAN_JOB, Vec::new());
        inner.set_job(MASTERS_JOB, masters.clone());
        inner.set_job(SNAPSHOT_JOB, open.into_iter().chain(masters).collect());

        let wanted: Vec<String> = inner
            .job_streams
            .iter()
            .flat_map(|(_, s)| s.iter().cloned())
            .collect();
        inner.streams.retain(|k, _| wanted.contains(k));
        for s in wanted {
            let started = self.started;
            inner.streams.entry(s).or_insert(StreamState {
                fresh: started,
                lag_rows: 0,
            });
        }
    }

    /// A job finished a pass: the SQL Server is usable and any failure is over. Its streams
    /// become fresh unless the pass stopped at the cycle deadline with work left. Lag rows
    /// = rows the mapper rejected (+ open checks deferred as unstable).
    pub fn succeeded(&self, job: &str, report: &CaptureReport) {
        let now = self.clock.now();
        let mut inner = self.lock();
        inner.sql_reachable = true;
        inner.error = None;
        inner.error_detail = None;
        *inner.runs.entry(job.to_owned()).or_insert(0) += 1;
        let mut rejected: HashMap<String, i64> = HashMap::new();
        for r in &report.rejected {
            *rejected.entry(inner.stream_of(&r.table)).or_insert(0) += 1;
        }
        // A table skipped after a schema error (AD1) did not run: its stream keeps its last fresh time.
        let skipped: Vec<String> = report
            .table_faults
            .iter()
            .map(|f| inner.stream_of(&f.table))
            .collect();
        let streams = inner
            .job_streams
            .iter()
            .find(|(j, _)| *j == job)
            .map(|(_, s)| s.clone())
            .unwrap_or_default();
        for stream in streams {
            if skipped.contains(&stream) {
                continue;
            }
            let Some(state) = inner.streams.get_mut(&stream) else {
                continue;
            };
            if !report.deadline_hit {
                state.fresh = now;
            }
            state.lag_rows = rejected.get(&stream).copied().unwrap_or(0)
                + if stream == OPEN_STREAM {
                    i64::from(report.unstable_checks_deferred)
                } else {
                    0
                };
        }
    }

    /// Capture is failing with `failure`; reachability changes only when the failure
    /// proves it.
    pub fn failed(&self, failure: &CaptureFailureInfo, detail: Option<&dl_json::Map>) {
        let mut inner = self.lock();
        inner.error = Some(failure.code.clone());
        inner.error_detail = detail.cloned();
        if let Some(reachable) = failure.sql_reachable {
            inner.sql_reachable = reachable;
        }
    }
}
