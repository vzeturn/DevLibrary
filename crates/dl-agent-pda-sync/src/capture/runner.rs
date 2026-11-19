//! The agent's capture loop over the capture engine (port of the agent's
//! `Capture/SqlCaptureRunner.cs` and `ICaptureRunner.cs`).
//!
//! Reads SMILE_POS with SELECT only, diffs against its local state and enqueues batches in
//! the durable outbox. One session = one engine built from the current runtime config; it
//! runs the initial load until complete, then the scheduler (open checks, ledger
//! tail/rescan, masters) until the config changes or a resnapshot is requested. Any other
//! failure ends the session, is reported through [`CaptureStatus`] with a stable code,
//! logged, and retried with exponential back-off. Delivery is at-least-once: the capture
//! state is committed only after the outbox accepted the batch.

use std::borrow::Cow;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::engine::CaptureEngine;
use super::error::{CaptureError, ErrorKind, Result};
use super::failure::{CaptureFailureInfo, classify};
use super::governor::{PollBudget, PollGovernor};
use super::nudge::{CaptureNudge, UploadWake};
use super::options::CaptureOptions;
use super::plan::CapturePlan;
use super::report::{CaptureReport, RejectedRow};
use super::scheduler::{self, CaptureScheduler};
use super::sinks::NoEchoSuppressor;
use super::sinks_bridge::{
    BatchStore, BlobStore, OutboxBatchSink, QueueBlobSink, RuntimeConfigSource,
};
use super::source::CaptureSource;
use super::state::CaptureState;
use super::status::{self, CaptureStatusTracker};
use super::util::contains_ignore_case;
use crate::config::time::{SharedClock, Timestamp};
use crate::config::{RuntimeConfig, codes};
use crate::contracts::catalog::SyncCatalog;
use crate::contracts::time::TimeZone;
use crate::contracts::wire::StreamLag;
use crate::outbox::CaptureStatus;
use crate::reporting::log::{Level, Logger, Value};
use crate::reporting::sql_error;
use crate::transport::RetryPolicy;
use crate::util::ctx::Ctx;
use crate::util::stj::eq_ignore_case;

/// `ILogger<SqlCaptureRunner>`: the log category.
pub const RUNNER_CATEGORY: &str = "MM.PdaOrder.SyncAgent.Capture.SqlCaptureRunner";

/// How often a table that keeps failing with a schema error is reported again.
pub const SKIPPED_TABLE_REPORT_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Pacing of the capture jobs that is not cloud-tunable (`CaptureTimings`).
#[derive(Debug, Clone, Copy)]
pub struct CaptureTimings {
    /// Cycle deadline, duty-cycle cap and busy back-off cap shared by every job.
    pub budget: PollBudget,
    /// Base gap between initial-load passes (1 s).
    pub snapshot_pass_interval: Duration,
    /// Ledger tail interval (10 s).
    pub ledger_tail_interval: Duration,
    /// Ledger rescan interval (5 min).
    pub ledger_rescan_interval: Duration,
    /// Floor for the cloud-pushed open-check poll (500 ms).
    pub min_open_interval: Duration,
    /// Floor for the cloud-pushed master scan interval (10 s).
    pub min_master_interval: Duration,
}

impl Default for CaptureTimings {
    fn default() -> CaptureTimings {
        CaptureTimings {
            budget: PollBudget::default(),
            snapshot_pass_interval: Duration::from_secs(1),
            ledger_tail_interval: Duration::from_secs(10),
            ledger_rescan_interval: Duration::from_secs(300),
            min_open_interval: Duration::from_millis(500),
            min_master_interval: Duration::from_secs(10),
        }
    }
}

/// Builds the SMILE_POS reader of a session (production: [`super::SqlServerCaptureSource`]
/// as `pda_sync`).
pub type SourceFactory = Arc<dyn Fn() -> Result<Arc<dyn CaptureSource>> + Send + Sync>;
/// Opens the diff state of a session (production: `capture.db`); dropped at its end.
pub type StateFactory = Arc<dyn Fn() -> Result<Arc<dyn CaptureState>> + Send + Sync>;
/// Resolves the site time zone of a session.
pub type ZoneFactory = Arc<dyn Fn() -> Result<Arc<TimeZone>> + Send + Sync>;

/// What the runner builds each capture session from (`CaptureRunnerDependencies`).
/// Factories run per session, so a re-enroll's new SQL password, a reopened local store
/// or a corrected time zone is picked up on the next retry without a restart.
#[derive(Clone)]
pub struct CaptureRunnerDependencies {
    /// Read-only SMILE_POS reader.
    pub source: SourceFactory,
    /// Diff state (shadows, watermarks, cursors).
    pub state: StateFactory,
    /// The blob queue.
    pub blobs: Arc<dyn BlobStore>,
    /// The sync catalog.
    pub catalog: Arc<SyncCatalog>,
    /// Agent id (batch envelope).
    pub agent_id: String,
    /// The site time zone.
    pub site_time_zone: ZoneFactory,
    /// Pacing.
    pub timings: CaptureTimings,
    /// Woken after an open-check run a nudge asked for (ADR 0104 §4); `None`: the uploader
    /// keeps its own pace.
    pub upload_wake: Option<Arc<UploadWake>>,
}

#[derive(Default)]
struct Problems {
    failures: i32,
    rejected_seen: Vec<(String, String)>,
    missing_seen: Vec<(String, String)>,
    skipped_tables: Vec<(String, Timestamp)>,
}

struct Shared {
    deps: CaptureRunnerDependencies,
    retry: RetryPolicy,
    clock: SharedClock,
    log: Logger,
    status: CaptureStatusTracker,
    problems: Mutex<Problems>,
    scheduler: Mutex<Option<Arc<CaptureScheduler>>>,
    open_nudges: AtomicI32,
}

/// The capture runner (`SqlCaptureRunner`). Cheap to clone (shared state).
#[derive(Clone)]
pub struct SqlCaptureRunner(Arc<Shared>);

impl std::fmt::Debug for SqlCaptureRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlCaptureRunner").finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Value identity of the runtime config fields that shape a session (`ConfigKey`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfigKey {
    open_ms: i32,
    master_seconds: i32,
    rescan_days: i32,
    history_months: i32,
    allowlist: String,
}

impl ConfigKey {
    /// A `null` allowlist entry fails like the C# `t.ToUpperInvariant()` on null
    /// (`NullReferenceException`, reported as `CAPTURE_FAILED`).
    fn of(c: &RuntimeConfig) -> Result<ConfigKey> {
        let mut names: Vec<String> = Vec::new();
        for t in c.table_allowlist.iter().flatten() {
            let Some(t) = t else {
                return Err(CaptureError::new(
                    ErrorKind::NullReference,
                    "Object reference not set to an instance of an object.",
                ));
            };
            let up = t.to_uppercase();
            if !names.contains(&up) {
                names.push(up);
            }
        }
        names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        Ok(ConfigKey {
            open_ms: c.open_check_poll_ms,
            master_seconds: c.master_scan_seconds,
            rescan_days: c.closed_rescan_days,
            history_months: c.history_months,
            allowlist: names.join("\n"),
        })
    }
}

/// `rule=count` pairs, most frequent first, e.g. `missing_key:PrintType=783`
/// (`RuleCounts`).
pub fn rule_counts<'a>(rows: impl IntoIterator<Item = &'a RejectedRow>) -> String {
    let mut groups: Vec<(&str, usize)> = Vec::new();
    for r in rows {
        match groups.iter_mut().find(|(k, _)| *k == r.rule) {
            Some(g) => g.1 += 1,
            None => groups.push((&r.rule, 1)),
        }
    }
    groups.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.encode_utf16().cmp(b.0.encode_utf16()))
    });
    groups
        .iter()
        .map(|(k, n)| format!("{k}={n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Safe facts of a session failure for its site event (`FailureDetail`): exception type,
/// SQL number/class, and for a schema error the identifier SQL Server named and the table
/// being read. Never the message (it can quote values).
pub fn failure_detail(error: &CaptureError) -> dl_json::Map {
    let mut detail = dl_json::Map::new();
    detail.insert("exception", error.short_type_name());
    sql_error::add_to(&mut detail, Some(&error.to_exception()));
    if !detail.contains_key("capture_table")
        && let Some(table) = error.table_of()
    {
        detail.insert("capture_table", table);
    }
    detail
}

fn millis(ms: i32) -> Duration {
    Duration::from_millis(u64::try_from(ms).unwrap_or(0))
}

/// The capture options of a runtime config, clamped to safe values (`OptionsFor`).
pub fn options_for(
    runtime: &RuntimeConfig,
    zone: Arc<TimeZone>,
    agent_id: &str,
    timings: &CaptureTimings,
) -> CaptureOptions {
    let window = runtime.history_months.clamp(1, 120);
    let mut o = CaptureOptions::new(agent_id, zone);
    o.budget = timings.budget;
    o.rescan_days = runtime.closed_rescan_days.clamp(1, 31);
    o.snapshot_window_months = window;
    o.log_snapshot_window_months = window.min(6);
    o.open_interval = millis(runtime.open_check_poll_ms).max(timings.min_open_interval);
    o.master_interval =
        Duration::from_secs(u64::try_from(runtime.master_scan_seconds).unwrap_or(0))
            .max(timings.min_master_interval);
    o.ledger_tail_interval = timings.ledger_tail_interval;
    o.ledger_rescan_interval = timings.ledger_rescan_interval;
    o
}

/// The catalog limited to the cloud-pushed allowlist (`None` or empty = every catalog
/// table, borrowed as is). Tables match ignoring case.
pub fn allowlisted<'a>(
    catalog: &'a SyncCatalog,
    allowlist: Option<&[Option<String>]>,
) -> Cow<'a, SyncCatalog> {
    let Some(list) = allowlist.filter(|l| !l.is_empty()) else {
        return Cow::Borrowed(catalog);
    };
    let wanted: Vec<&str> = list.iter().filter_map(|t| t.as_deref()).collect();
    let mut c = catalog.clone();
    c.tables.retain(|t| contains_ignore_case(&wanted, &t.table));
    Cow::Owned(c)
}

fn restart_reason(
    queue: &dyn BatchStore,
    config: &dyn RuntimeConfigSource,
    key: &ConfigKey,
) -> Result<Option<&'static str>> {
    if queue.needs_resnapshot()? {
        return Ok(Some("resnapshot requested"));
    }
    if !queue.pending_stream_resnapshot()?.is_empty() {
        return Ok(Some("stream resnapshot requested"));
    }
    Ok((ConfigKey::of(&config.current())? != *key).then_some("runtime config changed"))
}

fn distinct(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in items {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

impl SqlCaptureRunner {
    /// A runner; the initial status plan is the whole catalog with the default config.
    pub fn new(
        deps: CaptureRunnerDependencies,
        retry: RetryPolicy,
        clock: SharedClock,
        log: &Logger,
    ) -> Result<SqlCaptureRunner> {
        let options = options_for(
            &RuntimeConfig::default(),
            Arc::new(TimeZone::utc()),
            &deps.agent_id,
            &deps.timings,
        );
        let plan = CapturePlan::build(&deps.catalog, &options)?;
        let status = CaptureStatusTracker::new(clock.clone(), &plan);
        Ok(SqlCaptureRunner(Arc::new(Shared {
            deps,
            retry,
            clock,
            log: log.for_category(RUNNER_CATEGORY),
            status,
            problems: Mutex::new(Problems::default()),
            scheduler: Mutex::new(None),
            open_nudges: AtomicI32::new(0),
        })))
    }

    /// The status tracker.
    pub fn status(&self) -> &CaptureStatusTracker {
        &self.0.status
    }

    /// Safe facts about the current capture error for its site event.
    pub fn capture_error_detail(&self) -> Option<dl_json::Map> {
        self.0.status.error_detail()
    }

    /// Runs capture until `ctx` is cancelled (`RunAsync`). Every failure is reported and
    /// retried; this returns only on cancellation.
    pub fn run(&self, queue: Arc<dyn BatchStore>, config: Arc<dyn RuntimeConfigSource>, ctx: &Ctx) {
        let s = &self.0;
        while !ctx.is_done() {
            match s.run_session(&queue, &config, ctx) {
                Ok(()) => {}
                Err(e) if ctx.is_done() => {
                    if !e.is_cancelled() {
                        s.log.write(
                            Level::Debug,
                            Some(e.to_exception()),
                            "Capture stopped while a read was in flight",
                            &[],
                        );
                    }
                    return;
                }
                Err(e) => {
                    if !s.back_off(&e, ctx) {
                        return;
                    }
                }
            }
        }
    }
}

impl CaptureStatus for SqlCaptureRunner {
    fn sql_reachable(&self) -> bool {
        self.0.status.sql_reachable()
    }

    fn streams(&self) -> Vec<StreamLag> {
        self.0.status.stream_lags()
    }

    fn capture_error(&self) -> Option<String> {
        self.0.status.error()
    }
}

impl crate::reporting::CaptureHealth for SqlCaptureRunner {
    fn capture_error_detail(&self) -> Option<dl_json::Map> {
        self.0.status.error_detail()
    }
}

impl CaptureNudge for SqlCaptureRunner {
    /// ADR 0104 §4: an order command committed, so the open checks are read now and the
    /// uploader is woken once they are queued. Two open-job runs pulse the uploader: the
    /// run in flight may have read before the commit. Outside a scheduled session there is
    /// nothing to hurry.
    fn nudge_open_checks(&self) {
        self.0.open_nudges.store(2, Ordering::SeqCst);
        let scheduler = lock(&self.0.scheduler).clone();
        if let Some(s) = scheduler {
            s.nudge_open_checks();
        }
    }
}

impl Shared {
    /// Records and logs a failed session, then waits; false when the wait was cancelled.
    fn back_off(&self, e: &CaptureError, ctx: &Ctx) -> bool {
        let failure = classify(e);
        self.status.failed(&failure, Some(&failure_detail(e)));
        let failures = {
            let mut p = lock(&self.problems);
            p.failures += 1;
            p.failures
        };
        let attempt = u32::try_from(failures.min(10)).unwrap_or(10);
        let delay = self.retry.delay_for(attempt, None);
        if matches!(e.kind, ErrorKind::Backpressure(_)) {
            self.log.warn(
                "Capture paused ({Code}): {Reason} Retry {Failures} in {Delay}",
                &[
                    Value::from(&failure.code),
                    Value::from(&e.message),
                    Value::from(failures),
                    Value::from(delay),
                ],
            );
        } else {
            self.log.error_ex(
                e.to_exception(),
                "Capture failed with {Code} ({Failures} in a row); retrying in {Delay}",
                &[
                    Value::from(&failure.code),
                    Value::from(failures),
                    Value::from(delay),
                ],
            );
        }
        (self.retry.sleep)(delay, ctx)
    }

    fn run_session(
        self: &Arc<Self>,
        queue: &Arc<dyn BatchStore>,
        config: &Arc<dyn RuntimeConfigSource>,
        ctx: &Ctx,
    ) -> Result<()> {
        let runtime = config.current();
        let key = ConfigKey::of(&runtime)?;
        let zone = (self.deps.site_time_zone)()?;
        let options = options_for(&runtime, zone, &self.deps.agent_id, &self.deps.timings);
        let catalog = allowlisted(&self.deps.catalog, runtime.table_allowlist.as_deref());
        let state = (self.deps.state)()?;
        // NoEchoSuppressor (ADR 0029): the effects of replayed site commands must reach the
        // cloud mirror through normal capture.
        let engine = Arc::new(
            CaptureEngine::new(
                options.clone(),
                &catalog,
                (self.deps.source)()?,
                state,
                Arc::new(OutboxBatchSink::new(queue.clone())),
                Some(Arc::new(QueueBlobSink::new(self.deps.blobs.clone()))),
                Some(Arc::new(NoEchoSuppressor)),
                Some(self.clock.clone()),
            )?
            .with_key_repairs(queue.key_repairs()),
        );
        let plan = engine.plan();
        self.status.use_plan(plan);
        self.log.info(
            "Capture session started: {Open} open-check, {Ledger} ledger and {Masters} master tables; history {Months} months, rescan {Days} days",
            &[
                Value::from(plan.open.len()),
                Value::from(plan.ledger.len()),
                Value::from(plan.masters.len()),
                Value::from(options.snapshot_window_months),
                Value::from(options.rescan_days),
            ],
        );

        if queue.needs_resnapshot()? {
            self.log.warn(
                "Resnapshot requested: capture forgets what it sent and sends every row in scope again",
                &[],
            );
            engine.reset(ctx)?;
            queue.clear_needs_resnapshot()?;
            // Pending targeted ones are covered too, or they would restart the session forever.
            let streams = distinct(
                engine
                    .streams()
                    .into_iter()
                    .chain(queue.pending_stream_resnapshot()?),
            );
            queue.begin_stream_resnapshot(&streams)?;
        } else {
            let streams = queue.pending_stream_resnapshot()?;
            if !streams.is_empty() {
                self.reset_streams(&engine, &**queue, &streams, ctx)?;
            }
        }

        if self.initial_load(&engine, &**queue, &**config, &key, ctx)? {
            queue.stream_resnapshot_captured()?;
            self.schedule(&engine, &options, queue, config, &key, ctx)?;
        }
        Ok(())
    }

    /// Runs initial-load passes until complete (true) or the session must restart (false).
    fn initial_load(
        &self,
        engine: &CaptureEngine,
        queue: &dyn BatchStore,
        config: &dyn RuntimeConfigSource,
        key: &ConfigKey,
        ctx: &Ctx,
    ) -> Result<bool> {
        let timings = &self.deps.timings;
        let mut governor = PollGovernor::new(timings.budget);
        let mut pass = 0;
        loop {
            pass += 1;
            let started = Instant::now();
            let mut busy = false;
            match engine.run_initial_snapshot(ctx) {
                Ok(result) => {
                    self.on_job_completed(status::SNAPSHOT_JOB, Some(&result.report), None);
                    if result.complete {
                        if pass > 1 {
                            self.log.info(
                                "Initial load complete after {Passes} passes",
                                &[Value::from(pass)],
                            );
                        }
                        return Ok(true);
                    }
                }
                Err(e) if e.is_busy() => {
                    busy = true;
                    self.on_job_completed(status::SNAPSHOT_JOB, None, Some(&e));
                }
                Err(e) => return Err(e),
            }
            if let Some(reason) = restart_reason(queue, config, key)? {
                self.log.info(
                    "Capture session restarts during the initial load: {Reason}",
                    &[Value::from(reason)],
                );
                return Ok(false);
            }
            let delay =
                governor.next_delay(timings.snapshot_pass_interval, started.elapsed(), busy);
            if !ctx.sleep(delay) {
                return Err(CaptureError::cancelled());
            }
        }
    }

    fn schedule(
        self: &Arc<Self>,
        engine: &Arc<CaptureEngine>,
        options: &CaptureOptions,
        queue: &Arc<dyn BatchStore>,
        config: &Arc<dyn RuntimeConfigSource>,
        key: &ConfigKey,
        ctx: &Ctx,
    ) -> Result<()> {
        let session = ctx.child();
        let restart: Arc<Mutex<Option<&'static str>>> = Arc::default();
        let restart_error: Arc<Mutex<Option<CaptureError>>> = Arc::default();
        let completed: scheduler::Completed = {
            let me = self.clone();
            let queue = queue.clone();
            let config = config.clone();
            let key = key.clone();
            let session = session.clone();
            let restart = restart.clone();
            let restart_error = restart_error.clone();
            Box::new(move |job, report, busy| {
                me.on_job_completed(job, report, busy);
                if job == scheduler::OPEN_JOB
                    && me.take_open_nudge()
                    && let Some(w) = &me.deps.upload_wake
                {
                    w.pulse();
                }
                let mut r = lock(&restart);
                if r.is_none() {
                    match restart_reason(&*queue, &*config, &key) {
                        Ok(reason) => *r = reason,
                        Err(e) => {
                            // C#: the handler's exception ends the scheduler (and the session).
                            *r = Some("");
                            *lock(&restart_error) = Some(e);
                        }
                    }
                }
                if r.is_some() {
                    session.cancel();
                }
            })
        };
        let scheduler = Arc::new(CaptureScheduler::new(
            engine.clone(),
            options.clone(),
            Some(completed),
        ));
        *lock(&self.scheduler) = Some(scheduler.clone());
        let result = scheduler.run(&session);
        {
            let mut slot = lock(&self.scheduler);
            if slot.as_ref().is_some_and(|s| Arc::ptr_eq(s, &scheduler)) {
                *slot = None;
            }
        }
        // Break the cycle scheduler -> callback -> session before returning.
        drop(scheduler);
        if let Some(e) = lock(&restart_error).take() {
            return Err(e);
        }
        let restart = *lock(&restart);
        match result {
            // The handler cancelled the session to rebuild it; logged below.
            Err(e) if e.is_cancelled() && restart.is_some() && !ctx.is_done() => {}
            Err(e) => return Err(e),
            Ok(()) => {}
        }
        if let Some(reason) = restart
            && !ctx.is_done()
        {
            self.log
                .info("Capture session restarts: {Reason}", &[Value::from(reason)]);
        }
        Ok(())
    }

    fn take_open_nudge(&self) -> bool {
        let mut left = self.open_nudges.load(Ordering::SeqCst);
        while left > 0 {
            match self.open_nudges.compare_exchange(
                left,
                left - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(now) => left = now,
            }
        }
        false
    }

    /// Targeted resnapshot (ADR 0102): capture forgets what it sent for the pending streams
    /// only. A stream the plan does not know turns it into a resnapshot of everything.
    fn reset_streams(
        &self,
        engine: &CaptureEngine,
        queue: &dyn BatchStore,
        streams: &[String],
        ctx: &Ctx,
    ) -> Result<()> {
        let joined = streams.join(", ");
        if engine.reset_streams(streams, ctx)? {
            self.log.info(
                "Stream resnapshot: capture forgets what it sent for {Streams} and sends their rows again",
                &[Value::from(joined)],
            );
            return queue.begin_stream_resnapshot(streams);
        }
        self.log.warn(
            "Stream resnapshot of {Streams} names a stream this capture plan does not have ({Code}); sending every row in scope again instead",
            &[Value::from(joined), Value::from(codes::SYNC_STREAM_RESNAPSHOT)],
        );
        engine.reset(ctx)?;
        queue.begin_stream_resnapshot(&distinct(
            engine.streams().into_iter().chain(streams.iter().cloned()),
        ))?;
        queue.clear_needs_resnapshot()
    }

    fn on_job_completed(
        &self,
        job: &str,
        report: Option<&CaptureReport>,
        busy: Option<&CaptureError>,
    ) {
        if let Some(b) = busy {
            self.status
                .failed(&CaptureFailureInfo::new(codes::SQL_BUSY, Some(true)), None);
            self.log.info(
                "Capture {Job} backed off, SMILE_POS is busy: {Reason}",
                &[Value::from(job), Value::from(&b.message)],
            );
            return;
        }
        let Some(report) = report else { return };
        let mut p = lock(&self.problems);
        if p.failures > 0 {
            self.log.info(
                "Capture recovered after {Failures} failed attempts",
                &[Value::from(p.failures)],
            );
            p.failures = 0;
        }
        self.status.succeeded(job, report);
        self.log.debug(
            "Capture {Job}: {Batches} batches, {Rows} rows, {Tombstones} tombstones, {Blobs} blobs, {Deferred} checks deferred, deadline hit {DeadlineHit}",
            &[
                Value::from(job),
                Value::from(report.batches),
                Value::from(report.rows_sent),
                Value::from(report.tombstones_sent),
                Value::from(report.blobs_sent),
                Value::from(report.unstable_checks_deferred),
                Value::from(report.deadline_hit),
            ],
        );
        if self.log.is_enabled(Level::Debug) {
            for note in distinct(report.notes.iter().cloned()) {
                self.log.debug(
                    "Capture {Job}: {Note}",
                    &[Value::from(job), Value::from(note)],
                );
            }
        }
        self.log_problem_changes(&mut p, job, report);
        self.track_skipped_tables(&mut p, job, report);
    }

    /// Tables skipped after a schema error (AD1): reported when first seen and then once
    /// per [`SKIPPED_TABLE_REPORT_INTERVAL`]; the first clean run is reported as recovered.
    /// The heartbeat shows `SQL_SCHEMA_MISMATCH` while any is skipped.
    fn track_skipped_tables(&self, p: &mut Problems, job: &str, report: &CaptureReport) {
        let now = self.clock.now();
        let mut current: Vec<String> = Vec::new();
        for fault in &report.table_faults {
            let key = format!("{job}/{}", fault.table);
            if contains_ignore_case(&current, &key) {
                continue;
            }
            current.push(key.clone());
            let slot = p
                .skipped_tables
                .iter()
                .position(|(k, _)| eq_ignore_case(k, &key));
            if let Some(i) = slot {
                let reported = p.skipped_tables[i].1;
                let since = now.unix_nanos() - reported.unix_nanos();
                if since < SKIPPED_TABLE_REPORT_INTERVAL.as_nanos() as i128 {
                    self.log.debug(
                        "Capture {Job}: table {Table} still skipped after a schema error",
                        &[Value::from(job), Value::from(&fault.table)],
                    );
                    continue;
                }
                p.skipped_tables[i].1 = now;
            } else {
                p.skipped_tables.push((key, now));
            }
            self.log.error_ex(
                fault.error.to_exception(),
                "Capture {Job}: table {Table} skipped ({Code}) after a schema error; the other tables keep capturing and it is retried on the next run",
                &[
                    Value::from(job),
                    Value::from(&fault.table),
                    Value::from(codes::CAPTURE_TABLE_SKIPPED),
                ],
            );
        }
        // A run cut short by its deadline may not have reached a skipped table: no recovery claimed then.
        if !report.deadline_hit {
            let prefix = format!("{job}/");
            let mut i = 0;
            while i < p.skipped_tables.len() {
                let key = &p.skipped_tables[i].0;
                if key.starts_with(&prefix) && !contains_ignore_case(&current, key) {
                    let (key, _) = p.skipped_tables.remove(i);
                    self.log.warn(
                        "Capture {Job}: table {Table} reads again ({Code})",
                        &[
                            Value::from(job),
                            Value::from(&key[prefix.len()..]),
                            Value::from(codes::CAPTURE_TABLE_RECOVERED),
                        ],
                    );
                } else {
                    i += 1;
                }
            }
        }
        if !p.skipped_tables.is_empty() {
            let mut keys: Vec<&str> = p.skipped_tables.iter().map(|(k, _)| k.as_str()).collect();
            keys.sort_by_key(|k| k.to_uppercase());
            let mut detail = dl_json::Map::new();
            detail.insert("skipped_tables", keys.join(", "));
            self.status.failed(
                &CaptureFailureInfo::new(codes::SQL_SCHEMA_MISMATCH, Some(true)),
                Some(&detail),
            );
        }
    }

    /// Warns when the rejected rows or missing tables of a job change. A reject is logged
    /// as its table, count and per-rule counts: schema terms only.
    fn log_problem_changes(&self, p: &mut Problems, job: &str, report: &CaptureReport) {
        let mut groups: Vec<(String, Vec<&RejectedRow>)> = Vec::new();
        for r in &report.rejected {
            let k = format!("{job}/{}", r.table);
            match groups.iter_mut().find(|(g, _)| eq_ignore_case(g, &k)) {
                Some(g) => g.1.push(r),
                None => groups.push((k, vec![r])),
            }
        }
        for (k, rows) in &groups {
            let rules = rule_counts(rows.iter().copied());
            let seen = format!("{}|{rules}", rows.len());
            match p.rejected_seen.iter_mut().find(|(s, _)| s == k) {
                Some(slot) if slot.1 == seen => continue,
                Some(slot) => slot.1 = seen,
                None => p.rejected_seen.push((k.clone(), seen)),
            }
            self.log.warn(
                "Capture {Job}: {Count} rows of {Table} rejected by the mapper and not sent ({RejectRules})",
                &[
                    Value::from(job),
                    Value::from(rows.len()),
                    Value::from(&rows[0].table),
                    Value::from(rules),
                ],
            );
        }
        let prefix = format!("{job}/");
        p.rejected_seen.retain(|(k, _)| {
            !k.starts_with(&prefix) || groups.iter().any(|(g, _)| eq_ignore_case(g, k))
        });

        let missing = report.missing_tables.join(", ");
        let last = p
            .missing_seen
            .iter()
            .find(|(j, _)| j == job)
            .map_or("", |(_, m)| m.as_str());
        if last != missing {
            match p.missing_seen.iter_mut().find(|(j, _)| j == job) {
                Some(slot) => slot.1 = missing.clone(),
                None => p.missing_seen.push((job.to_owned(), missing.clone())),
            }
            if report.tables_missing() > 0 {
                self.log.warn(
                    "Capture {Job}: {Count} planned tables do not exist in SMILE_POS and are skipped: {MissingTables}",
                    &[
                        Value::from(job),
                        Value::from(report.tables_missing()),
                        Value::from(missing),
                    ],
                );
            }
        }
    }
}
