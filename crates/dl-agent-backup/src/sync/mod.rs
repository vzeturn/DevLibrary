//! Go `syncloop`: polls the hub's config, enqueues the schedules that are due, runs restore
//! requests and commands, reports discovered databases and applies retention. Reports go
//! through the outbox ([`Core::deliver`]) so a hub outage loses none of them.
//!
//! Differences from Go, on purpose:
//! - A schedule whose server or destination credential the hub could not open (the hub's
//!   `credentialError`), or that no longer decrypts after one master key re-fetch, gets a
//!   Failed backup report with the reason and counts as run for this period; Go logged it and
//!   tried again every minute without telling the hub.
//! - A schedule whose previous job is still queued or running is not queued twice; the
//!   period counts as run.
//! - A restore or command is run once per request: Go ran it again on every poll until the
//!   hub saw the report.
//! - A 401/403 keeps the loop polling (every 5 minutes) with the module degraded, so a
//!   re-enrollment takes effect without restarting the service; Go stopped the loop.

mod work;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_agent::report::Severity;
use dl_json::Value;
use dl_winsvc::StopSignal;

use crate::Error;
use crate::backup::{BackupType, Params, Step, Steps};
use crate::core::{Core, Health, lock};
use crate::hub::backup_report;
use crate::hub::types::{ConfigResponse, ScheduleConfig};
use crate::log;
use crate::schedule::is_due;
use crate::sql::Target;
use crate::state::now_ms;
use crate::state::queue::Job;
use crate::upload::Destination;

/// The login the hub's servers use (Go `hubUsername`).
pub const HUB_USERNAME: &str = "sa";

/// The slowest the loop polls after failures (Go `maxInterval`).
pub const MAX_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Discovery runs on the first poll and then every this many polls (Go).
pub const DISCOVERY_EVERY: u64 = 10;

/// The sync loop.
#[derive(Debug)]
pub struct SyncLoop {
    core: Arc<Core>,
    ticks: u64,
    failures: u32,
    /// Restore requests and commands already run in this process.
    handled: HashSet<String>,
}

impl SyncLoop {
    /// A loop over `core`.
    pub fn new(core: Arc<Core>) -> SyncLoop {
        SyncLoop {
            core,
            ticks: 0,
            failures: 0,
            handled: HashSet::new(),
        }
    }

    /// Polls until `stop` fires, backing off from the configured interval to
    /// [`MAX_INTERVAL`] while the hub fails. A `backup.run-now` cuts the wait short.
    pub fn run(&mut self, stop: &StopSignal) {
        let base = self.core.settings.poll;
        let mut interval = base;
        loop {
            interval = match self.poll(now_ms()) {
                Ok(()) => base,
                Err(Error::Auth(_)) => MAX_INTERVAL.max(base),
                Err(_) => next_interval(interval, base),
            };
            self.core.publish();
            let deadline = Instant::now() + interval;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                if stop.wait_timeout(left.min(Duration::from_millis(250))) {
                    return;
                }
                if self.failures == 0 && !lock(&self.core.run_now).is_empty() {
                    break;
                }
            }
        }
    }

    /// One poll: [`SyncLoop::tick`], then the outbox, and the hub link's health. Returns the
    /// tick's error after recording it.
    pub fn poll(&mut self, now_ms: u64) -> Result<(), Error> {
        let res = self.tick(now_ms);
        let core = &self.core;
        match &res {
            Ok(()) => {
                self.failures = 0;
                *lock(&core.health) = Health {
                    polled: true,
                    problem: None,
                };
                if let Err(e) = core.drain_outbox() {
                    log::warn("backup outbox drain stopped", &[("error", &e.to_string())]);
                }
            }
            Err(e @ Error::Auth(_)) => {
                self.failures += 1;
                lock(&core.health).problem = Some(format!(
                    "the backup hub rejected this agent ({e}); enroll it again with `dl-agent backup-enroll --force`"
                ));
                core.reports.error("hub", e);
            }
            Err(e) => {
                self.failures += 1;
                // One failure only backs off; still failing after the back-off degrades.
                if self.failures >= 2 {
                    lock(&core.health).problem = Some(format!("backup hub unreachable: {e}"));
                    core.reports.error("hub", e);
                }
            }
        }
        res
    }

    /// Go `tick`: everything one config poll does. Fails only when the config cannot be
    /// fetched or the hub rejects the agent; a schedule, restore or command that fails is
    /// reported and the rest carry on.
    pub fn tick(&mut self, now_ms: u64) -> Result<(), Error> {
        let cfg = self.core.hub.fetch_config()?;
        let run_now = std::mem::take(&mut *lock(&self.core.run_now));
        for sched in &cfg.schedules {
            self.schedule(&cfg, sched, run_now.contains(&sched.id), now_ms)?;
        }
        self.restores(&cfg)?;
        self.commands(&cfg)?;
        self.ticks += 1;
        if self.ticks % DISCOVERY_EVERY == 1 {
            self.discovery(&cfg)?;
        }
        self.retention(&cfg, now_ms)?;
        let live: HashSet<&str> = cfg
            .restore_requests
            .iter()
            .map(|r| r.id.as_str())
            .chain(cfg.commands.iter().map(|c| c.id.as_str()))
            .collect();
        self.handled.retain(|id| live.contains(id.as_str()));
        *lock(&self.core.config) = Some(cfg);
        Ok(())
    }

    /// Enqueues `sched` when it is due (or asked for by `backup.run-now`).
    fn schedule(
        &self,
        cfg: &ConfigResponse,
        sched: &ScheduleConfig,
        forced: bool,
        now_ms: u64,
    ) -> Result<(), Error> {
        let core = &self.core;
        let last = lock(&core.runs)
            .last_run(&sched.id)
            .map(|ms| (ms / 1000) as i64);
        let due = match is_due(
            &sched.cron_expression,
            &sched.time_zone_id,
            last,
            (now_ms / 1000) as i64,
        ) {
            Ok(due) => due,
            Err(e) => {
                core.reports.error(&sched.id, &e);
                return Ok(());
            }
        };
        if !due && !forced {
            return Ok(());
        }
        if lock(&core.queue).has_schedule(&sched.id) {
            return self.mark(&sched.id, now_ms);
        }
        match self.enqueue(cfg, sched, now_ms) {
            Ok(()) => self.mark(&sched.id, now_ms),
            Err(e @ Error::Auth(_)) => Err(e),
            Err(e @ Error::Credential(_)) => {
                self.credential_failure(sched, &e)?;
                self.mark(&sched.id, now_ms)
            }
            Err(e) => {
                // Not marked: the next poll tries again, as Go.
                core.reports.error(&sched.id, &e);
                Ok(())
            }
        }
    }

    fn mark(&self, schedule_id: &str, now_ms: u64) -> Result<(), Error> {
        if let Err(e) = lock(&self.core.runs).mark(schedule_id, now_ms) {
            self.core.reports.error(schedule_id, &e);
        }
        Ok(())
    }

    /// Go `enqueueSchedule` + `buildParams`.
    fn enqueue(
        &self,
        cfg: &ConfigResponse,
        sched: &ScheduleConfig,
        now_ms: u64,
    ) -> Result<(), Error> {
        let core = &self.core;
        let (server, db) = cfg.server_for_database(&sched.database_id).ok_or_else(|| {
            Error::Schedule(format!(
                "no configured server owns database {}",
                sched.database_id
            ))
        })?;
        let what = format!("password of server {}", server.host);
        let password =
            core.open_credential(&server.encrypted_password, server.credential_error, &what)?;
        let destination = self.destination(sched)?;
        let history_id = new_id()?;
        let params = Params {
            history_id: history_id.clone(),
            schedule_id: sched.id.clone(),
            database_name: db.database_name.clone(),
            backup_type: BackupType::from_hub(&sched.backup_type),
            target: Target {
                host: server.host.clone(),
                port: server.port,
                auth_type: server.auth_type.clone(),
                username: HUB_USERNAME.into(),
                password,
            },
            destination,
            backup_dir: core.settings.backup_dir.clone(),
        };
        let job = Job::new(&history_id, &sched.id, params.to_json(), now_ms);
        lock(&core.queue).enqueue(job)?;
        core.queued
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        log::info(
            "backup queued",
            &[("schedule", &sched.id), ("database", &db.database_name)],
        );
        Ok(())
    }

    /// The schedule's destination; `None` without stored tokens (the file stays on the
    /// server, as Go).
    fn destination(&self, sched: &ScheduleConfig) -> Result<Option<Destination>, Error> {
        let c = &sched.cloud;
        let what = format!("credentials of destination {}", c.provider);
        if !c.credential_error && c.encrypted_tokens.is_empty() {
            return Ok(None);
        }
        let plain = self
            .core
            .open_credential(&c.encrypted_tokens, c.credential_error, &what)?;
        Destination::from_plain(&c.provider, &plain, c.endpoint.as_deref()).map(Some)
    }

    /// A schedule that cannot run for a credential: a Failed backup on the hub with the
    /// reason, and one problem report per window.
    fn credential_failure(&self, sched: &ScheduleConfig, err: &Error) -> Result<(), Error> {
        let core = &self.core;
        core.reports.error(&sched.id, err);
        let history_id = new_id()?;
        let mut steps = Steps::new();
        steps.set("credentials", "error", err.to_string(), None);
        let body = backup_report(&sched.id, &history_id, false, steps.to_json(), "", "");
        deliver(core, &format!("backup:{history_id}"), body);
        let mut stats = lock(&core.stats);
        stats.jobs_failed += 1;
        Ok(())
    }
}

/// Sends `body` through the outbox; a hub failure leaves it there for the next drain.
pub fn deliver(core: &Core, key: &str, body: Value) {
    match core.deliver(key, body) {
        Ok(()) => {}
        Err(e @ (Error::Hub(_) | Error::Auth(_))) => {
            log::warn(
                "backup report kept in the outbox",
                &[("key", key), ("error", &e.to_string())],
            );
        }
        Err(e) => {
            core.reports.send(
                Severity::Error,
                e.code(),
                "outbox",
                &format!("report {key} not kept: {e}"),
            );
        }
    }
}

/// Step details with anything that looks like a secret removed, for the hub.
pub fn scrub_steps(steps: &[Step]) -> Vec<Value> {
    let mut clean = Steps::new();
    for s in steps {
        clean.set(&s.step, &s.status, crate::report::scrub(&s.detail), None);
    }
    let mut out = clean.to_json();
    for (v, s) in out.iter_mut().zip(steps) {
        if let Value::Object(m) = v {
            m.insert("durationMs", s.duration_ms);
        }
    }
    out
}

/// A new history id (a v4 UUID, as Go `uuid.NewString`).
pub fn new_id() -> Result<String, Error> {
    dl_uuid::Uuid::try_new_v4()
        .map(|u| u.to_string())
        .map_err(|e| Error::Io(format!("generate an id: {e}")))
}

/// Go `nextInterval`: doubles up to [`MAX_INTERVAL`].
pub fn next_interval(current: Duration, base: Duration) -> Duration {
    current.saturating_mul(2).min(MAX_INTERVAL.max(base))
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
