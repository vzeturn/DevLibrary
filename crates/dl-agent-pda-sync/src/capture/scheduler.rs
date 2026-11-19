//! Runs the capture jobs at their intervals on one thread (`Strategies/CaptureScheduler.cs`),
//! so POS Win never sees concurrent capture queries, pausing between cycles per
//! [`PollGovernor`]: the duty-cycle cap and exponential back-off when the source is busy.
//! Due times use the monotonic clock (C#: the injected `TimeProvider`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::engine::CaptureEngine;
use super::error::{CaptureError, Result};
use super::governor::PollGovernor;
use super::options::CaptureOptions;
use super::report::{CaptureReport, TableFault};
use crate::util::ctx::Ctx;

/// Name of the open-check job ([`CaptureScheduler::nudge_open_checks`]).
pub const OPEN_JOB: &str = "open";

type JobFn = fn(&CaptureEngine, &Ctx) -> Result<CaptureReport>;

/// Called after every job run with its report, or with the busy error of a busy source.
pub type Completed = Box<dyn Fn(&str, Option<&CaptureReport>, Option<&CaptureError>) + Send + Sync>;

struct Job {
    name: &'static str,
    interval: Duration,
    run: JobFn,
    due: Instant,
    governor: PollGovernor,
}

#[derive(Default)]
struct Wake {
    flag: Mutex<bool>,
    cv: Condvar,
}

impl Wake {
    fn notify(&self) {
        *self.flag.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.cv.notify_all();
    }
}

/// The scheduler.
pub struct CaptureScheduler {
    engine: Arc<CaptureEngine>,
    options: CaptureOptions,
    open_nudged: AtomicBool,
    wake: Arc<Wake>,
    completed: Option<Completed>,
}

impl std::fmt::Debug for CaptureScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureScheduler").finish_non_exhaustive()
    }
}

impl CaptureScheduler {
    /// A scheduler of `engine`'s jobs.
    pub fn new(
        engine: Arc<CaptureEngine>,
        options: CaptureOptions,
        completed: Option<Completed>,
    ) -> CaptureScheduler {
        CaptureScheduler {
            engine,
            options,
            open_nudged: AtomicBool::new(false),
            wake: Arc::new(Wake::default()),
            completed,
        }
    }

    /// Makes the open-check job due now (ADR 0104 §4); it still runs on the scheduler's
    /// thread, after the job in flight.
    pub fn nudge_open_checks(&self) {
        self.open_nudged.store(true, Ordering::SeqCst);
        self.wake.notify();
    }

    fn emit(&self, name: &str, report: Option<&CaptureReport>, busy: Option<&CaptureError>) {
        if let Some(c) = &self.completed {
            c(name, report, busy);
        }
    }

    /// Runs until `ctx` is cancelled. Errors other than a busy source or a table schema
    /// failure end the loop.
    pub fn run(&self, ctx: &Ctx) -> Result<()> {
        let o = &self.options;
        let now = Instant::now();
        let job = |name, interval, run: JobFn| Job {
            name,
            interval,
            run,
            due: now,
            governor: PollGovernor::new(o.budget),
        };
        let mut jobs = [
            job(OPEN_JOB, o.open_interval, CaptureEngine::run_open_checks),
            job(
                "ledger-tail",
                o.ledger_tail_interval,
                CaptureEngine::run_ledger_tail,
            ),
            job(
                "ledger-rescan",
                o.ledger_rescan_interval,
                CaptureEngine::run_ledger_rescan,
            ),
            job("masters", o.master_interval, CaptureEngine::run_masters),
            job(
                "ledger-load",
                o.ledger_rescan_interval,
                CaptureEngine::run_ledger_load_resume,
            ),
            job(
                "key-repair",
                o.master_interval,
                CaptureEngine::run_key_repairs,
            ),
        ];
        let wake = self.wake.clone();
        ctx.on_cancel(move || wake.notify());

        while !ctx.is_done() {
            if self.open_nudged.swap(false, Ordering::SeqCst) {
                jobs[0].due = Instant::now();
            }
            // OrderBy(Due).First(): the earliest, first in list order on ties.
            let mut next = 0;
            for (i, j) in jobs.iter().enumerate() {
                if j.due < jobs[next].due {
                    next = i;
                }
            }
            let wait = jobs[next].due.saturating_duration_since(Instant::now());
            if !wait.is_zero() && self.wait_or_nudged(wait, ctx)? {
                continue;
            }
            self.run_job(&mut jobs[next], ctx)?;
        }
        Err(CaptureError::cancelled())
    }

    /// Waits for `wait`; true when a nudge cut it short.
    fn wait_or_nudged(&self, wait: Duration, ctx: &Ctx) -> Result<bool> {
        let end = Instant::now() + wait;
        let mut flag = self
            .wake
            .flag
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *flag = false;
        loop {
            if ctx.is_done() {
                return Err(CaptureError::cancelled());
            }
            if self.open_nudged.load(Ordering::SeqCst) {
                return Ok(true);
            }
            let now = Instant::now();
            if now >= end {
                return Ok(false);
            }
            let (f, _) = self
                .wake
                .cv
                .wait_timeout(flag, end - now)
                .unwrap_or_else(PoisonError::into_inner);
            flag = f;
            *flag = false;
        }
    }

    fn run_job(&self, job: &mut Job, ctx: &Ctx) -> Result<()> {
        let started = Instant::now();
        let mut failed = false;
        match (job.run)(&self.engine, ctx) {
            Ok(report) => self.emit(job.name, Some(&report), None),
            Err(e) if e.is_busy() => {
                failed = true;
                self.emit(job.name, None, Some(&e));
            }
            Err(e) if !ctx.is_done() && e.is_schema_fault() => {
                // A schema failure outside a per-table guard: this job is skipped and
                // backs off, the other jobs keep running (AD1).
                failed = true;
                let mut report = CaptureReport::new();
                report.table_faults.push(TableFault {
                    table: e.table_of().unwrap_or(job.name).to_owned(),
                    error: e,
                });
                self.emit(job.name, Some(&report), None);
            }
            Err(e) => return Err(e),
        }
        job.due = Instant::now()
            + job
                .governor
                .next_delay(job.interval, started.elapsed(), failed);
        Ok(())
    }
}
