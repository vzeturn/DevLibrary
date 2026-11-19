//! The agent runtime: boot and probation, supervisor ticks, heartbeat, remote config,
//! commands, self-update and report delivery (PROTOCOL.md §3–§8).

use std::path::{Path, PathBuf};
use std::time::Duration;

use dl_hash::{Digest, Sha256};
use dl_json::{Value, json};
use dl_winsvc::StopSignal;

use crate::command::{Command, CommandResult, Outbox};
use crate::config::{ConfigStore, RemoteConfig};
use crate::hub::{self, HubClient, Transport};
use crate::identity::Identity;
use crate::manifest::TrustedKeys;
use crate::report::{ProblemReport, Severity, Spool};
use crate::sql::Site;
use crate::supervisor::{Factory, Supervisor};
use crate::update::{BootAction, Installer, UpdateState, on_boot, try_commit};
use crate::watchdog::{self, Beat};
use crate::{Error, Version, agent_version, current_target, now};

/// Product name of the agent binary on the hub.
pub const PRODUCT: &str = "dl-agent";
/// Default probation after an update, in seconds.
pub const PROBATION_SECONDS: u64 = 600;
/// Probation on the canary channel (test machines, the fast lane of PROTOCOL.md §6.4).
pub const CANARY_PROBATION_SECONDS: u64 = 120;
/// Retry interval while the hub is unreachable, in seconds.
pub const RETRY_SECONDS: u64 = 30;
/// An unchanged site survey is sent again after this long (PROTOCOL.md §3).
pub const SURVEY_RESEND_SECONDS: u64 = 24 * 3600;
/// Least time between two update checks the hub asked for, in seconds (stable channel;
/// canary checks on every heartbeat that asks, PROTOCOL.md §6.4).
pub const UPDATE_CHECK_SECONDS: u64 = 300;
/// How long a module gets to stop on restart or shutdown.
pub const STOP_WAIT: Duration = Duration::from_secs(10);
/// How long each module gets to finish in-flight work before an update swaps the binary.
pub const DRAIN_WAIT: Duration = Duration::from_secs(30);
/// Reports sent per request.
const REPORT_BATCH: usize = 100;

/// Release keys built into this binary: `DL_AGENT_RELEASE_KEYS="id:base64,id2:base64"` at
/// compile time. With none, every update is refused (and reported), never installed unsigned.
pub fn release_keys() -> TrustedKeys {
    let mut keys = TrustedKeys::new();
    for entry in option_env!("DL_AGENT_RELEASE_KEYS")
        .unwrap_or("")
        .split(',')
    {
        if let Some((id, b64)) = entry.trim().split_once(':')
            && let Ok(k) = keys.clone().with_key(id.trim(), b64.trim())
        {
            keys = k;
        }
    }
    keys
}

/// Runtime settings.
#[derive(Clone, Debug)]
pub struct Options {
    /// State directory (identity, config, update state, spool, module data, logs).
    pub data_dir: PathBuf,
    /// The running executable (replaced by updates).
    pub exe: PathBuf,
    /// Product name on the hub: the release line this binary follows.
    pub product: String,
    /// Version of this binary on that release line.
    pub version: Version,
    /// Keys release manifests must be signed with.
    pub trusted: TrustedKeys,
    /// Probation after an update.
    pub probation_seconds: u64,
    /// Answers the `selftest` command (set by the CLI; `None` answers "no self-test").
    pub selftest: Option<crate::sys_module::SelfTestHook>,
    /// How long each module gets to finish in-flight work before an update swaps the binary.
    pub drain_wait: Duration,
    /// A fault compiled in for the rollback smoke test ([`crate::fault`]); `None` normally.
    pub fault: Option<&'static str>,
}

impl Options {
    /// Defaults for `data_dir` and `exe`.
    pub fn new(data_dir: PathBuf, exe: PathBuf) -> Options {
        Options {
            data_dir,
            exe,
            product: PRODUCT.into(),
            version: agent_version(),
            trusted: release_keys(),
            probation_seconds: PROBATION_SECONDS,
            selftest: None,
            drain_wait: DRAIN_WAIT,
            fault: crate::fault::COMPILED,
        }
    }
}

/// Why the run loop ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// Asked to stop (service stop, Ctrl-C).
    Stopped,
    /// Restart me: an update was installed or rolled back, or a `restart` command came.
    Restart,
    /// The hub revoked this agent; the identity is wiped.
    Revoked,
}

impl Exit {
    /// Process / service exit code: 75 makes the service manager restart the agent.
    pub fn code(self) -> u32 {
        match self {
            Exit::Restart => crate::update::RESTART_EXIT_CODE as u32,
            Exit::Stopped | Exit::Revoked => 0,
        }
    }
}

/// One running agent.
pub struct Agent {
    opts: Options,
    hub: HubClient,
    supervisor: Supervisor,
    config: ConfigStore,
    update: UpdateState,
    spool: Spool,
    outbox: Outbox,
    hostname: String,
    started_at: u64,
    channel: String,
    heartbeat_seconds: u64,
    next_heartbeat: u64,
    hub_ok: bool,
    last_update_check: u64,
    update_status: (&'static str, String),
    trial_restarts: Vec<(String, u64)>,
    sql: Site,
    host: Value,
    survey_sent: Option<(String, u64)>,
    hub_ok_at: u64,
    beat_at: u64,
    ignored_hub_urls: Vec<String>,
}

impl Agent {
    /// An agent with the built-in `sys` module plus `modules`.
    pub fn new(
        opts: Options,
        identity: Identity,
        transport: Box<dyn Transport>,
        modules: Vec<Factory>,
    ) -> Result<Agent, Error> {
        crate::error::io("create data dir", std::fs::create_dir_all(&opts.data_dir))?;
        let spool = Spool::open(opts.data_dir.join("reports.json"));
        let outbox = Outbox::default();
        let mut supervisor = Supervisor::new(opts.data_dir.clone(), outbox.clone(), spool.clone());
        supervisor.add(crate::sys_module::factory_with(opts.selftest.clone()));
        let sql = Site::load(&opts.data_dir);
        supervisor.set_sql(sql.pools.clone());
        if !sql.pools.is_empty() {
            supervisor.add(crate::sql::module::factory(sql.pools.clone()));
        }
        for m in modules {
            supervisor.add(m);
        }
        let config = ConfigStore::load(&opts.data_dir)?;
        let update = UpdateState::load(&opts.data_dir)?;
        let current = opts.version.to_string();
        let update_status = match (&update.pending, update.failed_versions().last()) {
            (Some(p), _) => ("probation", p.version.to_string()),
            (None, Some(f)) => ("rolled_back", f.to_string()),
            (None, None) => ("idle", current),
        };
        Ok(Agent {
            hub: HubClient::new(transport, identity),
            supervisor,
            config,
            update,
            spool,
            outbox,
            hostname: hub::hostname(),
            started_at: now(),
            channel: "stable".into(),
            heartbeat_seconds: 60,
            next_heartbeat: 0,
            hub_ok: false,
            last_update_check: 0,
            update_status,
            trial_restarts: Vec::new(),
            sql,
            host: crate::host::info(),
            survey_sent: None,
            hub_ok_at: 0,
            beat_at: 0,
            ignored_hub_urls: Vec::new(),
            opts,
        })
        .inspect(|agent| {
            if let Some(e) = &agent.sql.load_error {
                agent.report(Severity::Critical, "sql_config_invalid", e);
            }
            if let Some(why) = crate::optout::reason(&agent.opts.data_dir) {
                let d = crate::optout::describe(&agent.opts.data_dir, why);
                let msg = format!("automatic updates are off: {d} (update_now still works)");
                agent.report(Severity::Info, "update_blocked_local", &msg);
            }
        })
    }

    /// The reports spool (tests and diagnostics).
    pub fn spool(&self) -> &Spool {
        &self.spool
    }

    /// Current remote config.
    pub fn config(&self) -> &RemoteConfig {
        &self.config.current
    }

    fn installer(&self) -> Installer {
        Installer {
            exe: self.opts.exe.clone(),
        }
    }

    /// Applies the hub's `sql` section whenever the config version changes.
    fn tune_sql(&mut self) {
        self.sql.tune(&self.config.current);
    }

    fn report(&self, severity: Severity, code: &str, message: &str) {
        self.report_with(ProblemReport::new(severity, "agent", code, message, now()));
    }

    fn report_with(&self, r: ProblemReport) {
        if let Err(e) = self.spool.record(r) {
            dl_log::error!("cannot spool report"; error = %e);
        }
    }

    /// Start-up: counts a probation boot and rolls back after too many. Returns
    /// `Some(Exit::Restart)` when the previous binary was restored.
    pub fn boot(&mut self) -> Result<Option<Exit>, Error> {
        match watchdog::drain_reports(&self.opts.data_dir, &self.spool) {
            Ok(0) => {}
            Ok(n) => dl_log::info!("watchdog reports queued"; count = n),
            Err(e) => self.report(
                Severity::Warning,
                "watchdog_reports_unreadable",
                &e.to_string(),
            ),
        }
        let action = on_boot(&mut self.update, &self.opts.version);
        self.update.save(&self.opts.data_dir)?;
        match action {
            BootAction::RollBack => {
                let reason = format!(
                    "{} starts without a successful probation",
                    crate::update::MAX_PROBATION_BOOTS
                );
                self.roll_back(&reason)?;
                Ok(Some(Exit::Restart))
            }
            BootAction::Probation => {
                dl_log::info!("update on probation"; version = %self.opts.version);
                Ok(None)
            }
            BootAction::Normal => Ok(None),
        }
    }

    /// Runs until stopped, revoked or a restart is needed. Modules are stopped on return.
    pub fn run(&mut self, stop: &StopSignal) -> Exit {
        let exit = match self.boot() {
            Ok(Some(exit)) => exit,
            Ok(None) => loop {
                if let Some(exit) = self.step(now()) {
                    break exit;
                }
                if stop.wait_timeout(Duration::from_secs(1)) {
                    break Exit::Stopped;
                }
            },
            Err(e) => {
                // Without update state the agent still runs its modules; the problem is reported.
                self.report(Severity::Critical, "boot_failed", &e.to_string());
                Exit::Restart
            }
        };
        dl_log::info!("agent stopping"; reason = ?exit);
        self.supervisor.stop_all(STOP_WAIT);
        self.flush_results();
        exit
    }

    /// One loop iteration at `now`.
    pub fn step(&mut self, now: u64) -> Option<Exit> {
        self.write_beat(now);
        self.tune_sql();
        self.supervisor.tick(&self.config.current);
        self.watch_config(now);
        if let Some(exit) = self.watch_probation(now) {
            return Some(exit);
        }
        if let Some(exit) = self.local_update(now) {
            return Some(exit);
        }
        if now >= self.next_heartbeat {
            return self.heartbeat(now);
        }
        None
    }

    /// Liveness for the external watchdog, once a minute.
    fn write_beat(&mut self, now: u64) {
        if now < self.beat_at + watchdog::BEAT_EVERY_SECONDS && now >= self.beat_at {
            return;
        }
        self.put_beat(now);
    }

    fn put_beat(&mut self, now: u64) {
        self.beat_at = now;
        let beat = Beat {
            at: now,
            hub_at: self.hub_ok_at,
            version: self.opts.version.to_string(),
            pid: std::process::id(),
            channel: self.channel.clone(),
            skew: self.hub.skew(),
            links: self.module_links(),
        };
        if let Err(e) = watchdog::write_beat(&self.opts.data_dir, &beat) {
            dl_log::warn!("cannot write the heartbeat file"; error = %e);
        }
    }

    /// The cloud links of the modules that run now (a stopped one's link is not watched).
    fn module_links(&self) -> Vec<watchdog::Link> {
        use crate::supervisor::ModuleState as S;
        self.supervisor
            .statuses()
            .into_iter()
            .filter(|s| matches!(s.state, S::Starting | S::Running | S::Degraded))
            .filter_map(|s| s.link)
            .collect()
    }

    fn watch_config(&mut self, now: u64) {
        if self.config.trial_since.is_none() {
            return;
        }
        if !self.config.in_trial(now) {
            if let Err(e) = self.config.confirm_if_due(now) {
                self.report(Severity::Error, "config_save_failed", &e.to_string());
            }
            self.trial_restarts.clear();
            return;
        }
        let counts = self.supervisor.restart_counts();
        let failed: Vec<String> = self
            .trial_restarts
            .iter()
            .filter(|(n, before)| counts.iter().any(|(m, c)| m == n && c > before))
            .map(|(n, _)| n.clone())
            .collect();
        if failed.is_empty() {
            return;
        }
        let bad = self.config.current.version;
        match self.config.revert() {
            Ok(changed) => {
                for m in &changed {
                    self.supervisor.restart(m, STOP_WAIT);
                }
                let msg = format!(
                    "config {bad} reverted: {} failed after the change",
                    failed.join(", ")
                );
                dl_log::warn!("{}", msg);
                let r = ProblemReport::new(Severity::Error, "agent", "config_reverted", &msg, now)
                    .with("config_version", bad)
                    .with("modules", failed);
                self.report_with(r);
            }
            Err(e) => self.report(Severity::Error, "config_revert_failed", &e.to_string()),
        }
        self.trial_restarts.clear();
    }

    fn apply_config(&mut self, new: RemoteConfig, now: u64) {
        let version = new.version;
        match self.config.apply(new, now) {
            Ok(changed) => {
                let counts = self.supervisor.restart_counts();
                self.trial_restarts = counts
                    .into_iter()
                    .filter(|(n, _)| changed.contains(n))
                    .collect();
                for m in &changed {
                    self.supervisor.restart(m, STOP_WAIT);
                }
                set_log_level(self.config.current.log_level());
                dl_log::info!("config applied"; version, restarted = changed);
            }
            Err(e) => self.report(
                Severity::Error,
                "config_apply_failed",
                &format!("config {version}: {e}"),
            ),
        }
    }

    fn watch_probation(&mut self, now: u64) -> Option<Exit> {
        let pending = self.update.pending.as_ref()?;
        let until = pending.probation_until;
        let skip = pending.unhealthy_before.clone();
        let fault = crate::fault::probation_failure(self.opts.fault);
        if fault.is_none()
            && try_commit(
                &mut self.update,
                now,
                self.supervisor.all_running_except(&skip),
                self.hub_ok,
            )
        {
            if let Err(e) = self.update.save(&self.opts.data_dir) {
                self.report(Severity::Error, "update_state_save_failed", &e.to_string());
            }
            dl_log::info!("update committed"; version = %self.opts.version);
            self.update_status = ("idle", self.opts.version.to_string());
            return None;
        }
        if now >= until
            && (fault.is_some()
                || self
                    .supervisor
                    .any_unhealthy_for_except(self.probation_seconds(), now, &skip))
        {
            let why = fault.unwrap_or("a module stayed unhealthy through probation");
            if let Err(e) = self.roll_back(why) {
                self.report(Severity::Critical, "rollback_failed", &e.to_string());
            }
            return Some(Exit::Restart);
        }
        None
    }

    fn heartbeat_body(&self, now: u64) -> Value {
        let id = self.hub.identity();
        let modules: Vec<Value> = self
            .supervisor
            .statuses()
            .iter()
            .map(|s| s.to_json())
            .collect();
        let mut body = json!({
            "agent_id": id.agent_id, "product": self.opts.product, "version": self.opts.version.to_string(),
            "channel": self.channel,
            "target": current_target(), "hostname": self.hostname, "started_at": self.started_at,
            "uptime_seconds": now.saturating_sub(self.started_at), "config_version": self.config.current.version,
            "update": {"state": self.update_status.0, "version": self.update_status.1}, "modules": modules,
        });
        if let Some(map) = body.as_object_mut() {
            for (k, v) in crate::optout::heartbeat_fields(&self.opts.data_dir) {
                map.insert(k, v);
            }
        }
        body
    }

    /// The site survey and its hash when it changed or is due again, else `None`.
    fn survey_due(&self, now: u64) -> Option<(Value, String)> {
        let survey = json!({"host": self.host, "databases": self.sql.pools.surveys()});
        let hash = dl_encoding::hex::encode(Sha256::digest(dl_json::to_string(&survey).as_bytes()));
        match &self.survey_sent {
            Some((h, at)) if *h == hash && now < at + SURVEY_RESEND_SECONDS => None,
            _ => Some((survey, hash)),
        }
    }

    fn heartbeat(&mut self, now: u64) -> Option<Exit> {
        let mut body = self.heartbeat_body(now);
        let survey = self.survey_due(now);
        if let (Some((s, _)), Some(o)) = (&survey, body.as_object_mut()) {
            o.insert("survey", s.clone());
        }
        let answer = match self.hub.heartbeat(&body) {
            Ok(a) => a,
            Err(e) => {
                if hub::error_code(&e).as_deref() == Some("revoked") {
                    return Some(self.revoke("the hub revoked this agent"));
                }
                self.next_heartbeat = now + RETRY_SECONDS.min(self.heartbeat_seconds);
                dl_log::warn!("heartbeat failed"; error = %e);
                self.report(Severity::Warning, "hub_unreachable", &e.to_string());
                return None;
            }
        };
        self.hub_ok = true;
        if self.hub_ok_at < now {
            self.hub_ok_at = now;
            self.put_beat(now); // tell the watchdog at once
        }
        if let Some((_, hash)) = survey {
            self.survey_sent = Some((hash, now));
        }
        self.adopt_hub_urls(answer.hub_urls.as_ref());
        // A channel chosen on this machine (dl-agent install stable|canary) wins.
        self.channel = crate::optout::channel(&self.opts.data_dir).unwrap_or(answer.channel);
        self.heartbeat_seconds = answer.heartbeat_seconds;
        self.next_heartbeat = now + answer.heartbeat_seconds;
        if let Some(c) = answer.config {
            self.apply_config(c, now);
        }
        for cmd in answer.commands {
            if let Some(exit) = self.run_command(cmd, now) {
                self.flush_results();
                return Some(exit);
            }
        }
        if answer.update_check
            && !crate::optout::blocked(&self.opts.data_dir)
            && self.update_check_due(now)
        {
            self.last_update_check = now;
            match self.try_update(now, false) {
                Ok(update_flow::Checked::Installed) => return Some(Exit::Restart),
                Ok(_) => {}
                Err(e) => self.report(Severity::Error, "update_failed", &e.to_string()),
            }
        }
        self.flush_reports();
        self.flush_results();
        None
    }

    fn revoke(&mut self, why: &str) -> Exit {
        dl_log::warn!("agent revoked, wiping identity"; reason = why);
        if let Err(e) = Identity::wipe(&self.opts.data_dir) {
            dl_log::error!("cannot wipe identity"; error = %e);
        }
        Exit::Revoked
    }

    fn answer(&self, id: &str, ok: bool, output: impl Into<String>) {
        self.outbox.push(CommandResult::new(id, ok, output, now()));
    }

    /// Runs one command; returns an exit when the command ends the process.
    fn run_command(&mut self, cmd: Command, now: u64) -> Option<Exit> {
        if now > cmd.expires_at {
            self.answer(&cmd.id, false, "expired");
            return None;
        }
        if !cmd.is_built_in() {
            let cmd = crate::sys_module::route(cmd);
            if cmd.module.is_none() {
                self.answer(&cmd.id, false, format!("unknown command {}", cmd.kind));
            } else if let Err(c) = self.supervisor.deliver(cmd) {
                let target = c.module.as_deref().unwrap_or_default();
                self.answer(&c.id, false, format!("module {target} is not running"));
            }
            return None;
        }
        let arg = |k: &str| cmd.args.get(k).and_then(Value::as_str).map(str::to_string);
        match cmd.kind.as_str() {
            "restart" => {
                self.answer(&cmd.id, true, "restarting");
                Some(Exit::Restart)
            }
            "restart_module" => {
                let name = cmd
                    .module
                    .clone()
                    .or_else(|| arg("module"))
                    .unwrap_or_default();
                let ok = self.supervisor.restart(&name, STOP_WAIT);
                self.answer(
                    &cmd.id,
                    ok,
                    if ok {
                        format!("{name} restarted")
                    } else {
                        format!("no module {name}")
                    },
                );
                None
            }
            "update_now" => {
                let (ok, out, exit) = self.update_now(now);
                self.answer(&cmd.id, ok, out);
                exit
            }
            "retry_update" => {
                let (ok, out) = self.retry_update(arg("version").as_deref());
                self.answer(&cmd.id, ok, out);
                None
            }
            "set_log_level" => {
                let level = arg("level").unwrap_or_default();
                let ok = set_log_level(&level);
                self.answer(
                    &cmd.id,
                    ok,
                    if ok {
                        format!("log level {level}")
                    } else {
                        format!("unknown level {level:?}")
                    },
                );
                None
            }
            "collect_diagnostics" => {
                let (ok, out) = self.collect_diagnostics(&cmd.id, now);
                self.answer(&cmd.id, ok, out);
                None
            }
            "reload_config" => {
                for m in self.supervisor.names() {
                    self.supervisor.restart(&m, STOP_WAIT);
                }
                set_log_level(self.config.current.log_level());
                self.answer(
                    &cmd.id,
                    true,
                    format!(
                        "modules restarted with config {}",
                        self.config.current.version
                    ),
                );
                None
            }
            "revoke" => {
                self.answer(&cmd.id, true, "identity wiped, stopping");
                self.flush_results();
                Some(self.revoke("revoke command"))
            }
            other => {
                self.answer(&cmd.id, false, format!("unknown command {other}"));
                None
            }
        }
    }

    /// Everything support needs, without secrets.
    pub fn diagnostics(&self, now: u64) -> Value {
        let id = self.hub.identity();
        let modules: Vec<Value> = self
            .supervisor
            .statuses()
            .iter()
            .map(|s| s.to_json())
            .collect();
        let pending = self.update.pending.as_ref().map(|p| json!({"version": p.version.to_string(), "previous_version": p.previous_version.to_string(), "probation_until": p.probation_until, "boots": u64::from(p.boots)}));
        let failed: Vec<Value> = self
            .update
            .failures
            .iter()
            .map(|(v, at)| json!({"version": v.to_string(), "at": *at}))
            .collect();
        json!({
            "agent_id": id.agent_id, "tenant": id.tenant, "site": id.site, "hub_urls": self.hub.hub_urls(),
            "product": self.opts.product, "version": self.opts.version.to_string(), "target": current_target(), "hostname": self.hostname,
            "channel": self.channel, "uptime_seconds": now.saturating_sub(self.started_at), "clock_skew": self.hub.skew(),
            "config_version": self.config.current.version, "config_on_trial": self.config.trial_since.is_some(),
            "update": {"pending": pending, "failures": failed}, "spooled_reports": self.spool.len() as u64,
            "modules": modules, "log_tail": log_tail(&self.opts.data_dir.join("logs"), 100),
        })
    }

    /// Sends spooled reports, oldest first. A rejected batch (4xx) is dropped so it cannot
    /// block the spool forever.
    fn flush_reports(&mut self) {
        for _ in 0..5 {
            let batch = self.spool.peek(REPORT_BATCH);
            if batch.is_empty() {
                return;
            }
            let ack = match self.hub.reports(&batch, &self.hostname) {
                Ok(_) => true,
                Err(
                    e @ Error::Status {
                        code: 400..=499, ..
                    },
                ) => {
                    dl_log::error!("hub rejected reports, dropping batch"; error = %e, count = batch.len());
                    true
                }
                Err(e) => {
                    dl_log::warn!("cannot send reports"; error = %e);
                    false
                }
            };
            if !ack {
                return;
            }
            if let Err(e) = self.spool.ack(&batch) {
                dl_log::error!("cannot update report spool"; error = %e);
                return;
            }
        }
    }

    /// Sends command results; keeps the ones that could not be delivered.
    fn flush_results(&mut self) {
        let mut pending = self.outbox.take().into_iter();
        while let Some(r) = pending.next() {
            match self.hub.command_result(&r) {
                Ok(()) => {}
                Err(
                    e @ Error::Status {
                        code: 400..=499, ..
                    },
                ) => dl_log::warn!("hub refused command result"; id = r.id, error = %e),
                Err(e) => {
                    dl_log::warn!("cannot send command result"; error = %e);
                    let mut rest = vec![r];
                    rest.extend(pending);
                    self.outbox.requeue(rest);
                    return;
                }
            }
        }
    }
}

/// Sets the global log level from its name; `false` for an unknown name.
pub fn set_log_level(name: &str) -> bool {
    match name.parse::<dl_log::LevelFilter>() {
        Ok(l) => {
            dl_log::set_level(l);
            true
        }
        Err(_) => false,
    }
}

/// The last `lines` lines of the newest file in `dir`.
pub fn log_tail(dir: &Path, lines: usize) -> Vec<String> {
    let newest = std::fs::read_dir(dir).ok().and_then(|rd| {
        rd.filter_map(Result::ok)
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .filter(|(_, p)| p.is_file())
            .max()
            .map(|(_, p)| p)
    });
    let Some(path) = newest else {
        return Vec::new();
    };
    let text = std::fs::read(&path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[path = "agent_status.rs"]
mod status;
pub use status::local_status;

#[path = "agent_update.rs"]
mod update_flow;

#[path = "agent_hub.rs"]
mod hub_list;

#[path = "agent_diag.rs"]
mod diag;
pub use diag::{diagnostic_file_allowed, diagnostic_folder_allowed};

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "agent_lane_tests.rs"]
mod lane_tests;

#[cfg(test)]
#[path = "agent_rollback_tests.rs"]
mod rollback_tests;
