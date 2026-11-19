//! Go `executor`: workers that take jobs off the queue, run the backup pipeline and report
//! the outcome. One backup at a time per SQL Server instance; the job's lease is renewed
//! while it runs so a long backup is not taken by another worker; jobs out of attempts are
//! reported once ("abandoned after …") and dropped.
//!
//! Differences from Go, on purpose:
//! - A failure that another attempt cannot fix (SIMPLE recovery for a log backup, an
//!   unsupported destination, a credential the hub could not open, a bad schedule) is
//!   reported and the job dropped at once instead of being tried three times.
//! - On stop a running job gets [`crate::core::Settings::stop_grace`] to finish; then its
//!   statement is cancelled (TDS attention) and the job goes back to the queue without
//!   spending an attempt.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dl_winsvc::StopSignal;

use crate::Error;
use crate::backup::{BackupResult, Params, Steps, pipeline};
use crate::core::{Core, lock};
use crate::hub::backup_report;
use crate::log;
use crate::state::history::{BackupRecord, Outcome};
use crate::state::now_ms;
use crate::state::queue::{Job, LEASE};
use crate::sync::{deliver, scrub_steps};

/// One lock per SQL Server instance (Go `instanceGate`).
#[derive(Debug, Default)]
pub struct Gate {
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl Gate {
    fn lock_for(&self, key: &str) -> Arc<Mutex<()>> {
        lock(&self.locks).entry(key.to_owned()).or_default().clone()
    }
}

/// Whether another attempt could change the outcome of `err`.
pub fn is_permanent(err: &Error) -> bool {
    matches!(
        err,
        Error::RecoveryModel(_) | Error::Unsupported(_) | Error::Credential(_) | Error::Schedule(_)
    )
}

/// A worker: runs jobs until `stop` fires. Go `Executor.work`.
pub fn work(core: &Core, gate: &Gate, stop: &StopSignal) {
    let poll = core.settings.worker_poll;
    while !stop.is_stopped() {
        let next = lock(&core.queue).dequeue(now_ms());
        match next {
            Ok(Some(job)) => execute(core, gate, stop, &job),
            Ok(None) => {
                drain_dead(core);
                if idle(core, stop, poll) {
                    return;
                }
            }
            Err(e) => {
                core.reports.error("queue", &e);
                if stop.wait_timeout(poll) {
                    return;
                }
            }
        }
    }
}

/// Waits up to `poll` for new work; true when stopped.
fn idle(core: &Core, stop: &StopSignal, poll: Duration) -> bool {
    let seen = core.queued.load(Ordering::Relaxed);
    let deadline = Instant::now() + poll;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || core.queued.load(Ordering::Relaxed) != seen {
            return false;
        }
        if stop.wait_timeout(left.min(Duration::from_millis(100))) {
            return true;
        }
    }
}

/// Runs one leased job and settles it in the queue.
pub fn execute(core: &Core, gate: &Gate, stop: &StopSignal, job: &Job) {
    let Some(params) = Params::from_json(&job.params) else {
        // A job this version cannot read cannot run on any attempt.
        let err = Error::Schedule(format!("queued job {} has unreadable parameters", job.id));
        core.reports.error(&job.schedule_id, &err);
        settle(core, job, Err(&err));
        return;
    };
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let result = std::thread::scope(|s| {
        s.spawn(move || keep_lease(core, &job.id, &done_rx));
        let res = run(core, gate, stop, &params);
        drop(done_tx);
        res
    });
    match result {
        None => {
            // Stopping before it started, or cancelled after the grace: it runs again on
            // the next start, without spending an attempt.
            if let Err(e) = lock(&core.queue).release(&job.id) {
                core.reports.error("queue", &e);
            }
            log::info("backup job returned to the queue", &[("job", &job.id)]);
        }
        Some((steps, Ok(res))) => {
            succeeded(core, &params, &steps, &res);
            settle(core, job, Ok(()));
        }
        Some((steps, Err(e))) => {
            failed(core, &params, steps, &e);
            settle(core, job, Err(&e));
        }
    }
}

type RunResult = Option<(Steps, Result<BackupResult, Error>)>;

/// The pipeline under the instance's gate; `None` when the job did not run to an outcome.
fn run(core: &Core, gate: &Gate, stop: &StopSignal, params: &Params) -> RunResult {
    let key = format!(
        "{}:{}",
        params.target.host.to_ascii_lowercase(),
        params.target.port
    );
    let instance = gate.lock_for(&key);
    let _held = lock(&instance);
    if stop.is_stopped() || core.cancel.is_cancelled() {
        return None;
    }
    let mut steps = Steps::new();
    let res = pipeline::run(
        core.sql.as_ref(),
        core.uploaders.as_ref(),
        params,
        &core.cancel,
        &mut steps,
    );
    match res {
        Err(_) if core.cancel.is_cancelled() => None,
        Err(Error::Cancelled) => None,
        other => Some((steps, other)),
    }
}

/// Go `keepLeaseAlive`: renews every third of the lease until `done` closes.
fn keep_lease(core: &Core, id: &str, done: &mpsc::Receiver<()>) {
    let every = LEASE / 3;
    loop {
        match done.recv_timeout(every) {
            Err(RecvTimeoutError::Timeout) => {
                if let Err(e) = lock(&core.queue).renew(id, now_ms()) {
                    core.reports.error("queue", &e);
                    return;
                }
            }
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn succeeded(core: &Core, p: &Params, steps: &Steps, res: &BackupResult) {
    let now = now_ms();
    let provider = p
        .destination
        .as_ref()
        .map(|d| d.provider.clone())
        .unwrap_or_default();
    let record = BackupRecord {
        schedule_id: p.schedule_id.clone(),
        hub_history_id: p.history_id.clone(),
        database_name: p.database_name.clone(),
        local_file_path: res.local_file_path.clone(),
        cloud_file_path: res.cloud_file_path.clone(),
        provider,
        created_at: now,
    };
    {
        let mut history = lock(&core.history);
        if let Err(e) = history.record(record) {
            core.reports.error(&p.schedule_id, &e);
        }
        history.push_outcome(Outcome {
            history_id: p.history_id.clone(),
            schedule_id: p.schedule_id.clone(),
            database: p.database_name.clone(),
            ok: true,
            error: String::new(),
            bytes: res.bytes,
            finished_at: now,
        });
    }
    {
        let mut stats = lock(&core.stats);
        stats.jobs_ok += 1;
        if !res.cloud_file_path.is_empty() {
            stats.bytes_uploaded += res.bytes;
        }
        stats.last_success.insert(p.database_name.clone(), now);
    }
    let body = backup_report(
        &p.schedule_id,
        &p.history_id,
        true,
        scrub_steps(steps.list()),
        &res.checksum,
        &res.cloud_file_path,
    );
    deliver(core, &format!("backup:{}", p.history_id), body);
    log::info(
        "backup succeeded",
        &[
            ("schedule", &p.schedule_id),
            ("database", &p.database_name),
            ("options", &res.options),
        ],
    );
}

fn failed(core: &Core, p: &Params, mut steps: Steps, err: &Error) {
    if !steps.list().iter().any(|s| s.status == "error") {
        steps.set("job", "error", err.to_string(), None);
    }
    core.reports.error(&p.schedule_id, err);
    lock(&core.stats).jobs_failed += 1;
    lock(&core.history).push_outcome(Outcome {
        history_id: p.history_id.clone(),
        schedule_id: p.schedule_id.clone(),
        database: p.database_name.clone(),
        ok: false,
        error: err.to_string(),
        bytes: 0,
        finished_at: now_ms(),
    });
    // Every attempt is reported under the same history id; the hub keeps the latest.
    let body = backup_report(
        &p.schedule_id,
        &p.history_id,
        false,
        scrub_steps(steps.list()),
        "",
        "",
    );
    deliver(core, &format!("backup:{}", p.history_id), body);
    log::warn(
        "backup failed",
        &[
            ("schedule", &p.schedule_id),
            ("database", &p.database_name),
            ("error", &err.to_string()),
        ],
    );
}

/// Completes the job, or records the failed attempt (dropping it when no attempt can help).
fn settle(core: &Core, job: &Job, outcome: Result<(), &Error>) {
    let mut queue = lock(&core.queue);
    let res = match outcome {
        Ok(()) => queue.complete(&job.id),
        Err(e) if is_permanent(e) => queue.complete(&job.id),
        Err(e) => queue.fail(&job.id, &e.to_string()),
    };
    drop(queue);
    if let Err(e) = res {
        core.reports.error("queue", &e);
    }
}

/// Go `DrainDeadLetter`: one Failed report per job out of attempts, then the job is dropped.
pub fn drain_dead(core: &Core) {
    let dead = lock(&core.queue).dead();
    for job in dead {
        let cause = if job.last_error.is_empty() {
            "no error recorded"
        } else {
            job.last_error.as_str()
        };
        let reason = format!(
            "abandoned after {}/{} attempts: {cause}",
            job.attempts, job.max_attempts
        );
        if let Some(p) = Params::from_json(&job.params) {
            let mut steps = Steps::new();
            steps.set("queue", "error", reason.as_str(), None);
            let body = backup_report(
                &p.schedule_id,
                &p.history_id,
                false,
                scrub_steps(steps.list()),
                "",
                "",
            );
            deliver(core, &format!("backup:{}", p.history_id), body);
        }
        lock(&core.stats).dead_lettered += 1;
        log::warn(
            "backup job abandoned",
            &[("job", &job.id), ("reason", &reason)],
        );
        if let Err(e) = lock(&core.queue).discard(&job.id) {
            core.reports.error("queue", &e);
        }
    }
}

#[cfg(test)]
#[path = "executor_tests.rs"]
mod tests;
