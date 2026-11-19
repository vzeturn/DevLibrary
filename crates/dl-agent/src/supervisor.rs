//! Modules and the supervisor that keeps them running.
//!
//! Each module runs on its own thread. A module that returns an error or panics is restarted
//! with exponential backoff (1 s doubling to 5 min, reset after 10 min of healthy running),
//! and the failure becomes a problem report. Commands addressed to a module are queued in its
//! [`ModuleContext`]; the module answers them through [`ModuleContext::complete`].

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dl_json::{Map, Value, json};
use dl_winsvc::StopSignal;

use crate::command::{Command, CommandResult, Outbox};
use crate::config::RemoteConfig;
use crate::report::{ProblemReport, Severity, Spool};
use crate::sql::{SqlPool, SqlPools};
use crate::{Error, now};

/// First restart delay.
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
/// Longest restart delay.
pub const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// Healthy running time after which the backoff starts again from the minimum.
pub const BACKOFF_RESET: Duration = Duration::from_secs(600);

/// A product feature hosted by the agent.
pub trait Module: Send {
    /// Stable name (`einvoice`, `pms-site` …), also the config section and report module.
    fn name(&self) -> &str;

    /// Runs until `ctx.stop()` is signalled. Call [`ModuleContext::set_running`] once ready.
    /// Returning `Err` (or panicking) makes the supervisor restart the module with backoff.
    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error>;

    /// Whether the module runs when the remote config does not set
    /// `modules.<name>.enabled`. Product modules that ship in the shared binary but only
    /// belong on some sites return `false` (opt-in: they need `"enabled": true`).
    fn enabled_by_default(&self) -> bool {
        true
    }
}

/// Builds a fresh module instance for every (re)start.
pub type Factory = Arc<dyn Fn() -> Box<dyn Module> + Send + Sync>;

/// Module state as sent in the heartbeat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleState {
    /// Thread started, not ready yet.
    Starting,
    /// Working.
    Running,
    /// Working with problems.
    Degraded,
    /// Not running (failed, waiting for restart, or stopped).
    Stopped,
    /// Turned off by configuration.
    Disabled,
}

impl ModuleState {
    /// Wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            ModuleState::Starting => "starting",
            ModuleState::Running => "running",
            ModuleState::Degraded => "degraded",
            ModuleState::Stopped => "stopped",
            ModuleState::Disabled => "disabled",
        }
    }
}

/// Health of one module.
#[derive(Clone, Debug, PartialEq)]
pub struct ModuleStatus {
    /// Module name.
    pub name: String,
    /// State.
    pub state: ModuleState,
    /// Short explanation of the state.
    pub detail: String,
    /// Restarts since the agent started.
    pub restarts: u64,
    /// Last failure.
    pub last_error: String,
    /// Counters the module publishes.
    pub metrics: Map,
    /// Unix seconds when it entered `Degraded` (0 otherwise).
    pub degraded_since: u64,
    /// The module's own cloud link, when it has one ([`ModuleContext::set_cloud_link`]).
    pub link: Option<crate::watchdog::Link>,
    /// Files the module keeps outside its data directory that `collect_diagnostics` takes
    /// ([`ModuleContext::add_diagnostic_file`]).
    pub diagnostic_files: Vec<PathBuf>,
    /// Rolling log folders `collect_diagnostics` takes, newest files first
    /// ([`ModuleContext::add_diagnostic_folder`]).
    pub diagnostic_folders: Vec<PathBuf>,
}

impl ModuleStatus {
    fn new(name: &str) -> ModuleStatus {
        ModuleStatus {
            name: name.into(),
            state: ModuleState::Stopped,
            detail: String::new(),
            restarts: 0,
            last_error: String::new(),
            metrics: Map::new(),
            degraded_since: 0,
            link: None,
            diagnostic_files: Vec::new(),
            diagnostic_folders: Vec::new(),
        }
    }

    /// Heartbeat JSON; `detail` and `last_error` are cut to the hub's limits.
    pub fn to_json(&self) -> Value {
        json!({"name": self.name, "state": self.state.as_str(), "detail": clip(&self.detail, MAX_DETAIL),
               "restarts": self.restarts, "last_error": clip(&self.last_error, MAX_LAST_ERROR),
               "metrics": Value::Object(self.metrics.clone())})
    }
}

/// Longest `detail` sent in a heartbeat (characters).
pub const MAX_DETAIL: usize = 2000;
/// Longest `last_error` sent in a heartbeat (characters).
pub const MAX_LAST_ERROR: usize = 4000;

/// `s` cut to `max` characters.
pub(crate) fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a running module can see and do.
#[derive(Clone)]
pub struct ModuleContext {
    name: String,
    stop: StopSignal,
    config: Value,
    data_dir: PathBuf,
    status: Arc<Mutex<ModuleStatus>>,
    inbox: Arc<Mutex<VecDeque<Command>>>,
    outbox: Outbox,
    reports: Spool,
    sql: SqlPools,
}

impl ModuleContext {
    /// A context for tests and tools that run one module by hand.
    pub fn standalone(name: &str, config: Value, data_dir: PathBuf) -> ModuleContext {
        ModuleContext {
            name: name.into(),
            stop: StopSignal::new(),
            config,
            data_dir,
            status: Arc::new(Mutex::new(ModuleStatus::new(name))),
            inbox: Arc::default(),
            outbox: Outbox::default(),
            reports: Spool::in_memory(),
            sql: SqlPools::default(),
        }
    }

    /// The same context with access to `pools` (tests and tools).
    pub fn with_sql(mut self, pools: SqlPools) -> ModuleContext {
        self.sql = pools;
        self
    }

    /// The site's shared connection pool for `database` (declared in `sql.json`).
    ///
    /// ```ignore
    /// let fo = ctx.sql("SMILE_FO")?;
    /// let rows = fo.with(|c| Ok(c.query("SELECT TOP 10 * FROM Room", &[])?))?;
    /// ```
    ///
    /// # Errors
    /// [`Error::Sql`] when the site does not declare that database.
    pub fn sql(&self, database: &str) -> Result<std::sync::Arc<SqlPool>, Error> {
        self.sql.get(database)
    }

    /// The stop signal; use `stop().wait_timeout(d)` as the loop sleep.
    pub fn stop(&self) -> &StopSignal {
        &self.stop
    }

    /// The module's config section (`Null` when absent).
    pub fn config(&self) -> &Value {
        &self.config
    }

    /// A directory only this module writes to (`<data>/modules/<name>`).
    pub fn data_dir(&self) -> &PathBuf {
        &self.data_dir
    }

    /// Marks the module ready.
    pub fn set_running(&self, detail: &str) {
        let mut s = lock(&self.status);
        s.state = ModuleState::Running;
        s.detail = detail.into();
        s.degraded_since = 0;
    }

    /// Marks the module working with problems.
    pub fn set_degraded(&self, detail: &str) {
        let mut s = lock(&self.status);
        if s.state != ModuleState::Degraded {
            s.degraded_since = now();
        }
        s.state = ModuleState::Degraded;
        s.detail = detail.into();
    }

    /// Publishes a metric for the heartbeat.
    pub fn set_metric(&self, key: &str, value: impl Into<Value>) {
        lock(&self.status).metrics.insert(key, value);
    }

    /// Declares the module's own cloud link (`url`, cut to its origin) and its last
    /// successful exchange (unix seconds, 0 when none yet); call it after every exchange.
    /// The external watchdog restarts the service when the link stays silent for
    /// [`crate::watchdog::STALE_SECONDS`] while `url` answers, even with the hub link up.
    /// Also published as the metric `cloud_link_ok_at`.
    pub fn set_cloud_link(&self, url: &str, ok_at: u64) {
        let url = crate::watchdog::origin(url);
        let mut s = lock(&self.status);
        let since = match &s.link {
            Some(l) if l.url == url => l.since,
            _ => now(),
        };
        s.link = Some(crate::watchdog::Link {
            module: self.name.clone(),
            url,
            ok_at,
            since,
        });
        s.metrics.insert("cloud_link_ok_at", ok_at);
    }

    /// Names a state file (problem journal, enrol record) for the diagnostics bundle, where
    /// it goes scrubbed as `modules/<name>/<file>` (the last 512 KiB). Only `.json`, `.jsonl`,
    /// `.log` and `.txt` files whose name does not suggest a secret (`key`, `secret`,
    /// `identity`, `password`, `appsettings`) are taken; returns whether this one is.
    pub fn add_diagnostic_file(&self, path: impl Into<PathBuf>) -> bool {
        let path = path.into();
        if !crate::agent::diagnostic_file_allowed(&path) {
            dl_log::warn!("diagnostic file refused"; module = self.name.as_str(), file = %path.display());
            return false;
        }
        let mut s = lock(&self.status);
        if !s.diagnostic_files.contains(&path) {
            s.diagnostic_files.push(path);
        }
        true
    }

    /// Names a rolling log folder for the diagnostics bundle: its newest `.log`, `.txt`,
    /// `.json` and `.jsonl` files go in scrubbed as `modules/<name>/<folder>/<file>`, the end
    /// of each, within a share of the bundle's log budget. A folder whose name suggests a
    /// secret is refused, and so is each file inside whose name does
    /// ([`Self::add_diagnostic_file`] rules); returns whether this folder is taken.
    pub fn add_diagnostic_folder(&self, path: impl Into<PathBuf>) -> bool {
        let path = path.into();
        if !crate::agent::diagnostic_folder_allowed(&path) {
            dl_log::warn!("diagnostic folder refused"; module = self.name.as_str(), folder = %path.display());
            return false;
        }
        let mut s = lock(&self.status);
        if !s.diagnostic_folders.contains(&path) {
            s.diagnostic_folders.push(path);
        }
        true
    }

    /// Withdraws the cloud link (the module stopped using it: not enrolled, turned off).
    pub fn clear_cloud_link(&self) {
        let mut s = lock(&self.status);
        s.link = None;
        s.metrics.remove("cloud_link_ok_at");
    }

    /// Records a problem report for this module.
    pub fn report(&self, severity: Severity, code: &str, message: &str, context: Map) {
        let mut r = ProblemReport::new(severity, &self.name, code, message, now());
        r.context = context;
        // A full disk must not crash the module; the spool keeps the report in memory.
        let _ = self.reports.record(r);
    }

    /// Takes the next command addressed to this module.
    pub fn next_command(&self) -> Option<Command> {
        lock(&self.inbox).pop_front()
    }

    /// A snapshot of this module's status.
    pub fn status(&self) -> ModuleStatus {
        lock(&self.status).clone()
    }

    /// Answers a command.
    pub fn complete(&self, id: &str, ok: bool, output: &str) {
        self.outbox.push(CommandResult::new(id, ok, output, now()));
    }
}

struct Slot {
    factory: Factory,
    name: String,
    enabled_by_default: bool,
    status: Arc<Mutex<ModuleStatus>>,
    inbox: Arc<Mutex<VecDeque<Command>>>,
    stop: StopSignal,
    thread: Option<JoinHandle<Result<(), String>>>,
    started: Option<Instant>,
    backoff: Duration,
    next_start: Instant,
}

/// Next restart delay after a failure.
pub fn next_backoff(current: Duration, ran_for: Duration) -> Duration {
    if ran_for >= BACKOFF_RESET || current.is_zero() {
        BACKOFF_MIN
    } else {
        (current * 2).min(BACKOFF_MAX)
    }
}

/// Owns the module threads.
pub struct Supervisor {
    slots: Vec<Slot>,
    data_dir: PathBuf,
    outbox: Outbox,
    reports: Spool,
    sql: SqlPools,
}

impl Supervisor {
    /// A supervisor writing module data under `<data_dir>/modules`.
    pub fn new(data_dir: PathBuf, outbox: Outbox, reports: Spool) -> Supervisor {
        Supervisor {
            slots: Vec::new(),
            data_dir,
            outbox,
            reports,
            sql: SqlPools::default(),
        }
    }

    /// Gives every module access to the site's SQL pools.
    pub fn set_sql(&mut self, pools: SqlPools) {
        self.sql = pools;
    }

    /// Registers a module (not started until [`Supervisor::tick`]).
    pub fn add(&mut self, factory: Factory) {
        let probe = factory();
        let name = probe.name().to_string();
        let enabled_by_default = probe.enabled_by_default();
        drop(probe);
        self.slots.push(Slot {
            status: Arc::new(Mutex::new(ModuleStatus::new(&name))),
            name,
            enabled_by_default,
            factory,
            inbox: Arc::default(),
            stop: StopSignal::new(),
            thread: None,
            started: None,
            backoff: Duration::ZERO,
            next_start: Instant::now(),
        });
    }

    /// Registered module names.
    pub fn names(&self) -> Vec<String> {
        self.slots.iter().map(|s| s.name.clone()).collect()
    }

    /// Reaps finished threads, schedules restarts, starts due modules and stops disabled
    /// ones. Call about once a second.
    pub fn tick(&mut self, config: &RemoteConfig) {
        for i in 0..self.slots.len() {
            self.reap(i);
            let enabled =
                config.module_enabled_or(&self.slots[i].name, self.slots[i].enabled_by_default);
            let slot = &mut self.slots[i];
            if !enabled {
                if slot.thread.is_some() {
                    slot.stop.stop();
                } else {
                    let mut st = lock(&slot.status);
                    st.state = ModuleState::Disabled;
                    st.detail = "disabled by configuration".into();
                }
                continue;
            }
            if slot.thread.is_none() && Instant::now() >= slot.next_start {
                self.spawn(i, config);
            }
        }
    }

    fn reap(&mut self, i: usize) {
        let slot = &mut self.slots[i];
        if !slot.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            return;
        }
        let Some(handle) = slot.thread.take() else {
            return;
        };
        let outcome = handle
            .join()
            .unwrap_or_else(|_| Err("module thread panicked".into()));
        let ran_for = slot.started.take().map_or(Duration::ZERO, |t| t.elapsed());
        let stopping = slot.stop.is_stopped();
        let mut st = lock(&slot.status);
        st.state = ModuleState::Stopped;
        if stopping {
            st.detail = "stopped".into();
            slot.next_start = Instant::now();
            return;
        }
        let msg = match outcome {
            Ok(()) => "module returned without being stopped".to_string(),
            Err(e) => e,
        };
        slot.backoff = next_backoff(slot.backoff, ran_for);
        slot.next_start = Instant::now() + slot.backoff;
        st.detail = format!("restarting in {} s", slot.backoff.as_secs());
        st.last_error = msg.clone();
        st.restarts += 1;
        if st.degraded_since == 0 {
            st.degraded_since = now();
        }
        let severity = if slot.backoff >= BACKOFF_MAX {
            Severity::Critical
        } else {
            Severity::Error
        };
        drop(st);
        let r = ProblemReport::new(severity, &slot.name, "module_failed", &msg, now())
            .with("backoff_seconds", slot.backoff.as_secs());
        let _ = self.reports.record(r); // see ModuleContext::report
    }

    fn spawn(&mut self, i: usize, config: &RemoteConfig) {
        let slot = &mut self.slots[i];
        slot.stop = StopSignal::new();
        {
            // `degraded_since` survives restarts: only `set_running` clears it.
            let mut st = lock(&slot.status);
            st.state = ModuleState::Starting;
            st.detail = "starting".into();
        }
        let ctx = ModuleContext {
            name: slot.name.clone(),
            stop: slot.stop.clone(),
            config: config.module(&slot.name),
            data_dir: self.data_dir.join("modules").join(&slot.name),
            status: slot.status.clone(),
            inbox: slot.inbox.clone(),
            outbox: self.outbox.clone(),
            reports: self.reports.clone(),
            sql: self.sql.clone(),
        };
        let factory = slot.factory.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("mod-{}", slot.name))
            .spawn(move || {
                let _ = std::fs::create_dir_all(ctx.data_dir());
                match catch_unwind(AssertUnwindSafe(|| factory().run(&ctx))) {
                    Ok(r) => r.map_err(|e| e.to_string()),
                    Err(p) => Err(panic_message(&p)),
                }
            });
        match spawned {
            Ok(h) => {
                slot.thread = Some(h);
                slot.started = Some(Instant::now());
            }
            Err(e) => {
                slot.backoff = next_backoff(slot.backoff, Duration::ZERO);
                slot.next_start = Instant::now() + slot.backoff;
                let mut st = lock(&slot.status);
                st.state = ModuleState::Stopped;
                st.last_error = format!("cannot start thread: {e}");
            }
        }
    }

    /// Stops a module and starts it again on the next tick, without backoff.
    pub fn restart(&mut self, name: &str, wait: Duration) -> bool {
        let Some(i) = self.slots.iter().position(|s| s.name == name) else {
            return false;
        };
        self.stop_slot(i, wait);
        self.slots[i].backoff = Duration::ZERO;
        self.slots[i].next_start = Instant::now();
        true
    }

    fn stop_slot(&mut self, i: usize, wait: Duration) {
        let slot = &mut self.slots[i];
        slot.stop.stop();
        let deadline = Instant::now() + wait;
        while slot.thread.as_ref().is_some_and(|h| !h.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if slot.thread.as_ref().is_some_and(|h| !h.is_finished()) {
            // Leave the stuck thread behind; it is reported, and a process restart clears it.
            slot.thread = None;
            let msg = format!("did not stop within {} s", wait.as_secs());
            let _ = self.reports.record(ProblemReport::new(
                Severity::Error,
                &slot.name,
                "module_stuck",
                &msg,
                now(),
            ));
        }
        self.reap(i);
    }

    /// Stops every module, waiting up to `wait` for each.
    pub fn stop_all(&mut self, wait: Duration) {
        for s in &self.slots {
            s.stop.stop();
        }
        for i in 0..self.slots.len() {
            self.stop_slot(i, wait);
        }
    }

    /// Queues a command for a module; gives it back when there is no such module or it is
    /// not running (the caller answers the command).
    pub fn deliver(&self, cmd: Command) -> Result<(), Command> {
        let target = cmd.module.as_deref().unwrap_or("");
        match self
            .slots
            .iter()
            .find(|s| s.name == target && s.thread.is_some())
        {
            Some(s) => {
                lock(&s.inbox).push_back(cmd);
                Ok(())
            }
            None => Err(cmd),
        }
    }

    /// Current health of every module.
    pub fn statuses(&self) -> Vec<ModuleStatus> {
        self.slots.iter().map(|s| lock(&s.status).clone()).collect()
    }

    /// Whether every enabled module is running (disabled modules do not count against).
    pub fn all_running(&self) -> bool {
        self.all_running_except(&[])
    }

    /// [`Self::all_running`], ignoring the modules named in `skip`.
    pub fn all_running_except(&self, skip: &[String]) -> bool {
        self.statuses().iter().all(|s| {
            skip.contains(&s.name)
                || matches!(s.state, ModuleState::Running | ModuleState::Disabled)
        })
    }

    /// Enabled modules that are degraded or stopped right now.
    pub fn unhealthy_names(&self) -> Vec<String> {
        self.statuses()
            .into_iter()
            .filter(|s| matches!(s.state, ModuleState::Degraded | ModuleState::Stopped))
            .map(|s| s.name)
            .collect()
    }

    /// Whether an enabled module has been degraded or failing for at least `secs`.
    pub fn any_unhealthy_for(&self, secs: u64, now: u64) -> bool {
        self.any_unhealthy_for_except(secs, now, &[])
    }

    /// [`Self::any_unhealthy_for`], ignoring the modules named in `skip`.
    pub fn any_unhealthy_for_except(&self, secs: u64, now: u64, skip: &[String]) -> bool {
        self.statuses().iter().any(|s| {
            s.state != ModuleState::Disabled
                && !skip.contains(&s.name)
                && s.degraded_since > 0
                && now.saturating_sub(s.degraded_since) >= secs
        })
    }

    /// Restart counts per module (to notice failures after a config change).
    pub fn restart_counts(&self) -> Vec<(String, u64)> {
        self.statuses()
            .into_iter()
            .map(|s| (s.name, s.restarts))
            .collect()
    }
}

fn panic_message(p: &Box<dyn core::any::Any + Send>) -> String {
    let text = p
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned());
    format!("panic: {}", text.unwrap_or_else(|| "unknown".into()))
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
