//! The dl-agent module: `modules.backup`.
//!
//! Not enrolled (no `backup-enroll` yet): degraded `not enrolled`, waiting for stop. Once
//! enrolled: a sync thread polls the hub and enqueues due schedules, `concurrency` worker
//! threads run the queue, and this thread answers commands, publishes metrics and turns the
//! hub link's health into the run state (running after the first config poll; degraded
//! when the hub stays unreachable past one back-off or rejects the agent). A new enrollment
//! (`backup-enroll --force`) is picked up without a restart.
//!
//! Stop: workers take no new job; a running backup gets the stop grace to finish, then its
//! statement is cancelled and the job stays queued for the next start.

pub mod selftest;
pub mod settings;

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dl_agent::Error as AgentError;
use dl_agent::command::Command;
use dl_agent::report::Severity;
use dl_agent::supervisor::{Factory, Module, ModuleContext};
use dl_json::Value;
use dl_winsvc::StopSignal;

use crate::Error;
use crate::core::{Core, Settings, lock};
use crate::enroll::Enrollment;
use crate::executor::{self, Gate};
use crate::hub::HubClient;
use crate::log;
use crate::report::Sink;
use crate::sql::{Connector, Tds};
use crate::sync::SyncLoop;
use crate::upload::{Real, Uploaders};

/// The module name (`modules.backup`, data under `<data>/modules/backup`).
pub const NAME: &str = "backup";

/// How often the module thread looks at commands, health and stop.
const TICK: Duration = Duration::from_millis(250);

/// How often the enrollment file is read again.
const RECHECK: Duration = Duration::from_secs(30);

/// How long cancelled work gets to unwind after the grace.
const CANCEL_WAIT: Duration = Duration::from_secs(10);

/// The module factory to register with `App::module`.
pub fn backup_module() -> Factory {
    Arc::new(|| Box::new(BackupModule::new(Arc::new(Tds), Arc::new(Real))) as Box<dyn Module>)
}

/// The module, over a SQL connector and destination uploaders.
pub struct BackupModule {
    sql: Arc<dyn Connector>,
    uploaders: Arc<dyn Uploaders>,
}

impl std::fmt::Debug for BackupModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackupModule").finish_non_exhaustive()
    }
}

impl BackupModule {
    /// A module using `sql` and `uploaders` (production: [`Tds`] and [`Real`]).
    pub fn new(sql: Arc<dyn Connector>, uploaders: Arc<dyn Uploaders>) -> BackupModule {
        BackupModule { sql, uploaders }
    }
}

impl Module for BackupModule {
    fn name(&self) -> &str {
        NAME
    }

    fn enabled_by_default(&self) -> bool {
        false
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), AgentError> {
        let settings = settings::from_config(ctx.config());
        loop {
            let Some(enrollment) = wait_for_enrollment(ctx) else {
                return Ok(());
            };
            match self.run_enrolled(ctx, &enrollment, settings.clone())? {
                Outcome::Stopped => return Ok(()),
                Outcome::Reload => log::info("backup module reloading its enrollment", &[]),
            }
        }
    }
}

/// Why `run_enrolled` returned without an error.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Stopped,
    Reload,
}

/// Waits until an enrollment exists; `None` when stopped first.
fn wait_for_enrollment(ctx: &ModuleContext) -> Option<Enrollment> {
    let mut detail = String::new();
    let mut next_check = Instant::now();
    loop {
        if Instant::now() >= next_check {
            next_check = Instant::now() + RECHECK;
            let now = match Enrollment::load(ctx.data_dir()) {
                Ok(Some(e)) => return Some(e),
                Ok(None) => "not enrolled".to_owned(),
                Err(e) => {
                    let msg = format!(
                        "enrollment unreadable ({e}); run `dl-agent backup-enroll --force`"
                    );
                    if msg != detail {
                        ctx.problem(Severity::Error, e.code(), &msg);
                    }
                    msg
                }
            };
            if now != detail {
                ctx.set_degraded(&now);
                detail = now;
            }
        }
        while let Some(cmd) = ctx.next_command() {
            let (ok, out) = answer(None, &cmd, &detail);
            ctx.complete(&cmd.id, ok, &out);
        }
        if ctx.stop().wait_timeout(TICK) {
            return None;
        }
    }
}

impl BackupModule {
    fn run_enrolled(
        &self,
        ctx: &ModuleContext,
        enrollment: &Enrollment,
        settings: Settings,
    ) -> Result<Outcome, AgentError> {
        let hub = HubClient::new(&enrollment.hub_url, &enrollment.token);
        let sink: Arc<dyn Sink> = Arc::new(ctx.clone());
        let workers = settings.concurrency.max(1);
        let running = format!(
            "{workers} worker(s); polling {} every {} s",
            hub.origin(),
            settings.poll.as_secs()
        );
        let core = Core::open(
            ctx.data_dir(),
            hub,
            self.sql.clone(),
            self.uploaders.clone(),
            sink,
            settings,
        )
        .map_err(|e| {
            ctx.problem(
                Severity::Error,
                e.code(),
                &format!("open the backup state: {e}"),
            );
            AgentError::from(e)
        })?;
        let core = Arc::new(core);
        let stop = StopSignal::new();
        let mut threads = Vec::new();
        let (c, s) = (core.clone(), stop.clone());
        threads.push(spawn(
            "backup-sync",
            move || SyncLoop::new(c).run(&s),
            &stop,
        )?);
        let gate = Arc::new(Gate::default());
        for i in 0..workers {
            let (c, s, g) = (core.clone(), stop.clone(), gate.clone());
            threads.push(spawn(
                &format!("backup-worker-{i}"),
                move || executor::work(&c, &g, &s),
                &stop,
            )?);
        }
        log::info("backup module started", &[("detail", &running)]);
        let result = supervise(ctx, &core, &mut threads, &running, enrollment);
        shutdown(&core, &stop, threads);
        result
    }
}

fn spawn(
    name: &str,
    f: impl FnOnce() + Send + 'static,
    stop: &StopSignal,
) -> Result<JoinHandle<()>, AgentError> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map_err(|e| {
            stop.stop();
            AgentError::from(Error::io(&format!("spawn {name}"), &e))
        })
}

/// The module thread: commands, metrics, run state, crashed threads, enrollment changes.
fn supervise(
    ctx: &ModuleContext,
    core: &Core,
    threads: &mut Vec<JoinHandle<()>>,
    running: &str,
    enrollment: &Enrollment,
) -> Result<Outcome, AgentError> {
    let mut next_check = Instant::now() + RECHECK;
    loop {
        let health = lock(&core.health).clone();
        let detail = match &health.problem {
            Some(p) => {
                ctx.set_degraded(p);
                p.clone()
            }
            None if health.polled => {
                ctx.set_running(running);
                running.to_owned()
            }
            None => "waiting for the first config from the hub".to_owned(),
        };
        while let Some(cmd) = ctx.next_command() {
            let (ok, out) = answer(Some(core), &cmd, &detail);
            ctx.complete(&cmd.id, ok, &out);
        }
        core.publish();
        if let Some(i) = threads.iter().position(JoinHandle::is_finished) {
            let h = threads.remove(i);
            let name = h.thread().name().unwrap_or("backup thread").to_owned();
            let msg = match h.join() {
                Err(_) => format!("{name} panicked"),
                Ok(()) => format!("{name} ended unexpectedly"),
            };
            log::error(&msg, &[]);
            return Err(AgentError::Module(msg));
        }
        if Instant::now() >= next_check {
            next_check = Instant::now() + RECHECK;
            match Enrollment::load(ctx.data_dir()) {
                Ok(Some(e)) if &e == enrollment => {}
                Ok(_) => return Ok(Outcome::Reload),
                Err(e) => log::warn(
                    "backup enrollment unreadable; keeping the loaded one",
                    &[("error", &e.to_string())],
                ),
            }
        }
        if ctx.stop().wait_timeout(TICK) {
            return Ok(Outcome::Stopped);
        }
    }
}

/// Stops the threads: the grace for running work, then cancel, then a short wait. A thread
/// still inside a blocking call after that is left to end on its own.
fn shutdown(core: &Core, stop: &StopSignal, threads: Vec<JoinHandle<()>>) {
    stop.stop();
    let grace_end = Instant::now() + core.settings.stop_grace;
    wait_all(&threads, grace_end);
    if threads.iter().any(|h| !h.is_finished()) {
        log::info(
            "backup work still running after the stop grace; cancelling it",
            &[],
        );
        core.cancel.cancel();
        wait_all(&threads, Instant::now() + CANCEL_WAIT);
    }
    for h in threads {
        let name = h.thread().name().unwrap_or("backup thread").to_owned();
        if !h.is_finished() {
            log::warn(
                "backup thread still finishing a call; it ends on its own",
                &[("thread", &name)],
            );
        } else if h.join().is_err() {
            log::error(
                "backup thread panicked while stopping",
                &[("thread", &name)],
            );
        }
    }
    core.publish();
    log::info("backup module stopped", &[]);
}

fn wait_all(threads: &[JoinHandle<()>], until: Instant) {
    while threads.iter().any(|h| !h.is_finished()) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Answers `backup.status` and `backup.run-now <scheduleId>`; `core` is `None` while not
/// enrolled.
pub fn answer(core: Option<&Core>, cmd: &Command, detail: &str) -> (bool, String) {
    let kind = cmd.kind.strip_prefix("backup.").unwrap_or(&cmd.kind);
    match (kind, core) {
        ("status", core) => {
            let mut v = core.map_or_else(|| Value::Object(dl_json::Map::new()), Core::status);
            if let Value::Object(m) = &mut v {
                m.insert("enrolled", core.is_some());
                m.insert("detail", detail);
            }
            (true, dl_json::to_string(&v))
        }
        ("run-now" | "run_now", None) => (false, detail.to_owned()),
        ("run-now" | "run_now", Some(core)) => run_now(core, &cmd.args),
        _ => (false, format!("unknown backup command {:?}", cmd.kind)),
    }
}

fn run_now(core: &Core, args: &Value) -> (bool, String) {
    let id = args
        .as_str()
        .or_else(|| args.get("scheduleId").and_then(Value::as_str))
        .or_else(|| args.get("schedule_id").and_then(Value::as_str))
        .map(str::trim)
        .unwrap_or_default();
    if id.is_empty() {
        return (false, "backup.run-now needs a scheduleId".into());
    }
    let known = match lock(&core.config).as_ref() {
        None => {
            return (
                false,
                "no config from the hub yet; try again after the first poll".into(),
            );
        }
        Some(cfg) => cfg.schedules.iter().any(|s| s.id == id),
    };
    if !known {
        return (false, format!("unknown schedule {id:?}"));
    }
    let mut queue = lock(&core.run_now);
    if !queue.iter().any(|s| s == id) {
        queue.push(id.to_owned());
    }
    (
        true,
        format!("schedule {id} will be queued on the next poll"),
    )
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
