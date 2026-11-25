//! The agent's self-update flow (PROTOCOL.md §6): fetch and verify the manifest, apply the
//! site policy (window, auto off) and the failure cap, install, and roll back.

use super::{Agent, CANARY_PROBATION_SECONDS, Exit, UPDATE_CHECK_SECONDS, now};
use crate::Error;
use crate::channel::Channel;
use crate::current_target;
use crate::manifest::Manifest;
use crate::report::{ProblemReport, Severity};
use crate::update::{Decision, Pending, Policy};

/// How an update check ended when it did not fail.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Checked {
    /// An update was installed: restart into it.
    Installed,
    /// Nothing to install, with the reason.
    Nothing(String),
    /// An update is due but waits (probation, window, failure cap), with the reason.
    Held(String),
}

impl Agent {
    /// Canary agents run on test machines and take the fast lane (PROTOCOL.md §6.4): an
    /// update check on every heartbeat that asks for one, a short probation, and no hold by
    /// the site's window or `auto: false`. The failure cap and rollback are the same on both
    /// channels, so a bad build still rolls back.
    pub(super) fn fast_lane(&self) -> bool {
        self.channel == Channel::Canary.as_str()
    }

    /// Probation for an update installed now, and how long a module may stay unhealthy in it.
    pub(super) fn probation_seconds(&self) -> u64 {
        if self.fast_lane() {
            self.opts.probation_seconds.min(CANARY_PROBATION_SECONDS)
        } else {
            self.opts.probation_seconds
        }
    }

    /// Whether to act on the hub's `update_check` now: stable checks at most once per
    /// `UPDATE_CHECK_SECONDS`, canary on every heartbeat.
    pub(super) fn update_check_due(&self, now: u64) -> bool {
        self.fast_lane() || now >= self.last_update_check + UPDATE_CHECK_SECONDS
    }

    /// Fetches, verifies and installs the release for this agent's channel. `forced` (the
    /// `update_now` command) skips the site's window and `auto: false`, not the failure cap;
    /// so does the canary channel.
    pub(super) fn try_update(&mut self, now: u64, forced: bool) -> Result<Checked, Error> {
        if self.update.pending.is_some() {
            return Ok(Checked::Held("an update is still on probation".into()));
        }
        let Some(text) = self.hub.manifest(&self.opts.product, &self.channel)? else {
            return Ok(Checked::Nothing("no release on this channel".into()));
        };
        let m = Manifest::parse(&text)?;
        m.verify(&self.opts.trusted)?;
        if m.product != self.opts.product || m.channel.as_str() != self.channel {
            return Err(Error::Manifest(format!(
                "manifest is for {}/{}",
                m.product, m.channel
            )));
        }
        let current = self.opts.version.clone();
        let agent_id = self.hub.identity().agent_id.clone();
        let (artifact, version) =
            match self
                .update
                .decide(&m, &current, &agent_id, &current_target(), now)
            {
                Decision::UpToDate | Decision::NotInRollout => {
                    self.update.accept(&m);
                    self.update.save(&self.opts.data_dir)?;
                    return Ok(Checked::Nothing("no update to install".into()));
                }
                Decision::Deferred(reason) => return Ok(self.defer(reason)),
                Decision::Refused(reason) => return Err(Error::Manifest(reason)),
                Decision::Install(artifact, version) => (artifact, version),
            };
        let (policy, bad) = Policy::from_config(&self.config.current.body);
        if let Some(e) = bad {
            self.report(Severity::Warning, "update_policy_invalid", &e.to_string());
        }
        if let Some(reason) = policy.hold(&m, now, forced || self.fast_lane()) {
            return Ok(self.defer(format!("{version} waits: {reason}")));
        }
        let bytes = self.hub.download(&artifact.url)?;
        // Record the probation before the swap: a crash in between then rolls back
        // or (if the swap never happened) is forgotten at the next boot.
        self.update.accept(&m);
        self.update.pending = Some(Pending {
            version: version.clone(),
            previous_version: current.clone(),
            probation_until: now + self.probation_seconds(),
            boots: 0,
            unhealthy_before: self.supervisor.unhealthy_names(),
        });
        self.update.save(&self.opts.data_dir)?;
        self.drain();
        if let Err(e) = self.installer().install(&bytes, &artifact) {
            // A bad download (stale CDN copy, truncated file) or a failed swap counts
            // towards the failure cap; network errors above do not. The drained modules
            // start again on the next tick.
            self.update.pending = None;
            self.update.record_failure(&version, now);
            self.update.save(&self.opts.data_dir)?;
            return Err(e);
        }
        let msg = format!("installed {version} over {current}, restarting");
        dl_log::info!("{}", msg);
        self.report_with(
            ProblemReport::new(Severity::Info, "agent", "update_installed", &msg, now)
                .with_command("update.install"),
        );
        Ok(Checked::Installed)
    }

    /// Stops every module before the swap so in-flight work (a SQL batch, an upload)
    /// finishes on the old binary (PROTOCOL.md §6). Each module gets `opts.drain_wait`; one
    /// that does not stop is reported as `module_stuck` and the update goes on, since the
    /// restart that follows ends it anyway.
    pub(super) fn drain(&mut self) {
        let started = std::time::Instant::now();
        self.supervisor.stop_all(self.opts.drain_wait);
        let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        dl_log::info!("modules drained before the update"; ms = ms);
    }

    fn defer(&mut self, reason: String) -> Checked {
        dl_log::info!("update deferred"; reason = reason.as_str());
        self.update_status = ("deferred", reason.clone());
        Checked::Held(reason)
    }

    /// An agent report that names the command or operation that failed.
    pub(super) fn report_cmd(&self, severity: Severity, command: &str, code: &str, message: &str) {
        self.report_with(
            ProblemReport::new(severity, "agent", code, message, now()).with_command(command),
        );
    }

    /// Hands a command the runtime does not handle itself to its module; one that cannot
    /// run is answered and reported with its kind as `command`.
    pub(super) fn route_to_module(&mut self, cmd: crate::command::Command) {
        let cmd = crate::sys_module::route(cmd);
        if cmd.module.is_none() {
            let msg = format!("unknown command {}", cmd.kind);
            self.report_cmd(Severity::Warning, &cmd.kind, "command_unknown", &msg);
            self.answer(&cmd.id, false, msg);
        } else if let Err(c) = self.supervisor.deliver(cmd) {
            let target = c.module.as_deref().unwrap_or_default();
            let msg = format!("module {target} is not running");
            self.report_cmd(Severity::Warning, &c.kind, "command_module_down", &msg);
            self.answer(&c.id, false, msg);
        }
    }

    /// Restores the previous binary and counts the failure.
    pub(super) fn roll_back(&mut self, reason: &str) -> Result<(), Error> {
        let Some(p) = self.update.pending.take() else {
            return Ok(());
        };
        if let Err(e) = self.installer().rollback() {
            self.report_cmd(
                Severity::Critical,
                "update.rollback",
                "rollback_failed",
                &format!("cannot restore {}: {e}", p.previous_version),
            );
        }
        self.update.record_failure(&p.version, now());
        self.update.save(&self.opts.data_dir)?;
        let msg = format!(
            "rolled back {} to {}: {reason}",
            p.version, p.previous_version
        );
        dl_log::error!("{}", msg);
        let r = ProblemReport::new(
            Severity::Critical,
            "agent",
            "update_rolled_back",
            &msg,
            now(),
        )
        .with_command("update.rollback")
        .with("version", p.version.to_string())
        .with("previous_version", p.previous_version.to_string());
        self.report_with(r);
        self.update_status = ("rolled_back", p.version.to_string());
        Ok(())
    }

    /// The `update_now` command: `(ok, output, exit)`.
    pub(super) fn update_now(&mut self, now: u64) -> (bool, String, Option<Exit>) {
        self.last_update_check = now;
        match self.try_update(now, true) {
            Ok(Checked::Installed) => (
                true,
                "update installed, restarting".into(),
                Some(Exit::Restart),
            ),
            Ok(Checked::Nothing(why)) => (true, why, None),
            Ok(Checked::Held(why)) => (false, why, None),
            Err(e) => (false, e.to_string(), None),
        }
    }

    /// `dl-agent update` on this machine (PROTOCOL.md §6.5): a request file in the data
    /// directory runs the `update_now` path at once, on the channel chosen here when one is;
    /// the answer goes to `update-now.result.json` for the CLI that waits for it.
    pub(super) fn local_update(&mut self, now: u64) -> Option<Exit> {
        let dir = self.opts.data_dir.clone();
        match crate::optout::take_update_request(&dir) {
            Ok(true) => {}
            Ok(false) => return None,
            Err(e) => {
                self.report(Severity::Error, "update_request_failed", &e.to_string());
                return None;
            }
        }
        if let Some(c) = crate::optout::channel(&dir) {
            self.channel = c;
        }
        let from = self.opts.version.to_string();
        let (ok, output, exit) = self.update_now(now);
        let to = match (&exit, &self.update.pending) {
            (Some(_), Some(p)) => p.version.to_string(),
            _ => String::new(),
        };
        dl_log::info!("local update request"; ok = ok, output = output.as_str());
        let r = crate::optout::UpdateResult {
            at: now,
            ok,
            installed: exit.is_some(),
            from,
            to,
            channel: self.channel.clone(),
            output,
        };
        if let Err(e) = crate::optout::write_update_result(&dir, &r) {
            self.report(Severity::Error, "update_request_failed", &e.to_string());
        }
        exit
    }

    /// The `retry_update` command: forgets failed attempts (of `version`, or all).
    pub(super) fn retry_update(&mut self, version: Option<&str>) -> (bool, String) {
        let before = self.update.failures.len();
        match version {
            Some(v) => self.update.failures.retain(|(f, _)| f.to_string() != v),
            None => self.update.failures.clear(),
        }
        let cleared = before - self.update.failures.len();
        match self.update.save(&self.opts.data_dir) {
            Ok(()) => {
                if self.update_status.0 != "probation" {
                    self.update_status = ("idle", self.opts.version.to_string());
                }
                (true, format!("{cleared} failed attempts forgotten"))
            }
            Err(e) => (false, e.to_string()),
        }
    }
}

#[cfg(test)]
#[path = "agent_drain_tests.rs"]
mod drain_tests;
