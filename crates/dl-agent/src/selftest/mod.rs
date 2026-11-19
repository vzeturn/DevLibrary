//! Self-test (RT-0164): every core part of the agent and every enrolled module checks itself
//! read-only and the run goes to the hub.
//!
//! A run is a list of [`Check`]s, each `pass`, `warn`, `fail` or `skip`. Core checks live
//! here; a product module adds its own through a [`SelfTest`] registered on the app
//! (`App::selftest`). A module that is disabled or not enrolled reports `skip`, never `fail`.
//! `dl-agent selftest` runs it from the command line (exit 1 on any `fail`); the `selftest`
//! command runs the same inside the service. Both upload the run (`POST /v1/agent/selftest`).
//!
//! Nothing secret goes into a check: details pass [`sanitize`] (connection-string passwords,
//! tokens, bearer values, URL credentials and long key-like blobs are masked).

pub(crate) mod checks_core;
mod checks_hub;
mod checks_state;
pub mod cli;
mod execute;
mod legacy;
mod runner;
mod sanitize;
#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use dl_json::{Map, Value, json};

use crate::supervisor::ModuleContext;

pub use execute::{command_output, execute, hook, upload};
pub use runner::{Stalled, bounded};
pub use sanitize::sanitize;

/// The remote command that runs the self-test in the service.
pub const COMMAND: &str = crate::sys_module::SELFTEST;
/// Module name of the agent's own checks.
pub const CORE: &str = "core";
/// The agent's report spool in the data directory.
pub(crate) const SPOOL_FILE: &str = "reports.json";
/// Most checks one run uploads (the hub caps it too).
pub const MAX_CHECKS: usize = 500;
/// Longest `detail` kept, in characters.
pub const DETAIL_MAX: usize = 600;
/// Longest `remedy` kept, in characters.
pub const REMEDY_MAX: usize = 300;
/// Longest `id` kept, in characters (the hub's cap).
pub const ID_MAX: usize = 100;
/// Longest `title` kept, in characters (the hub's cap).
pub const TITLE_MAX: usize = 300;
/// Largest upload body; details are cut further when a run is bigger.
pub const MAX_BODY: usize = 250_000;
/// How long one module's self-test may run before it is reported as stuck.
pub const MODULE_TIMEOUT: Duration = Duration::from_secs(120);
/// Timeout of one network call or SQL query inside a check.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// The result of one check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Works.
    Pass,
    /// Works, but something needs attention.
    Warn,
    /// Broken.
    Fail,
    /// Not applicable here (module disabled, not enrolled, other OS).
    Skip,
}

impl Status {
    /// Wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Warn => "warn",
            Status::Fail => "fail",
            Status::Skip => "skip",
        }
    }
}

/// What a check found, before it is timed and named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// Result.
    pub status: Status,
    /// What was found (no secrets).
    pub detail: String,
    /// What to do about it.
    pub remedy: Option<String>,
}

impl Outcome {
    fn of(status: Status, detail: impl Into<String>) -> Outcome {
        Outcome {
            status,
            detail: detail.into(),
            remedy: None,
        }
    }

    /// A passing check.
    pub fn pass(detail: impl Into<String>) -> Outcome {
        Outcome::of(Status::Pass, detail)
    }

    /// A check that works with a problem.
    pub fn warn(detail: impl Into<String>) -> Outcome {
        Outcome::of(Status::Warn, detail)
    }

    /// A failing check.
    pub fn fail(detail: impl Into<String>) -> Outcome {
        Outcome::of(Status::Fail, detail)
    }

    /// A check that does not apply.
    pub fn skip(detail: impl Into<String>) -> Outcome {
        Outcome::of(Status::Skip, detail)
    }

    /// The same outcome with what to do about it.
    pub fn remedy(mut self, remedy: impl Into<String>) -> Outcome {
        self.remedy = Some(remedy.into());
        self
    }
}

/// One check of a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// `core` or the module name.
    pub module: String,
    /// Stable id within the module, e.g. `identity` or `sql.SMILE_FO`.
    pub id: String,
    /// Short human title.
    pub title: String,
    /// Result.
    pub status: Status,
    /// What was found (sanitized).
    pub detail: String,
    /// What to do about it (sanitized).
    pub remedy: Option<String>,
    /// How long the check took.
    pub duration_ms: u64,
}

impl Check {
    /// A check from an outcome; detail and remedy are sanitized and clipped.
    pub fn new(module: &str, id: &str, title: &str, outcome: Outcome, took: Duration) -> Check {
        Check {
            module: module.into(),
            id: id.chars().take(ID_MAX).collect(),
            title: title.chars().take(TITLE_MAX).collect(),
            status: outcome.status,
            detail: sanitize(&outcome.detail, DETAIL_MAX),
            remedy: outcome.remedy.map(|r| sanitize(&r, REMEDY_MAX)),
            duration_ms: u64::try_from(took.as_millis()).unwrap_or(u64::MAX),
        }
    }

    /// Runs `f` and times it.
    pub fn timed(module: &str, id: &str, title: &str, f: impl FnOnce() -> Outcome) -> Check {
        let started = Instant::now();
        let outcome = f();
        Check::new(module, id, title, outcome, started.elapsed())
    }

    /// Runs the blocking probe `f` on its own thread and times it: a probe that outlives
    /// `timeout` or panics is a `fail`, and the run goes on without it.
    pub fn bounded(
        module: &str,
        id: &str,
        title: &str,
        timeout: Duration,
        f: impl FnOnce() -> Outcome + Send + 'static,
    ) -> Check {
        Check::timed(module, id, title, || {
            bounded(&format!("{module}-{id}"), timeout, f)
                .unwrap_or_else(|e| Outcome::fail(format!("the probe {e}")))
        })
    }

    /// A `skip` with its reason.
    pub fn skip(module: &str, id: &str, title: &str, reason: &str) -> Check {
        Check::new(module, id, title, Outcome::skip(reason), Duration::ZERO)
    }

    /// Wire JSON (`remedy` only when set).
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("module", self.module.clone());
        o.insert("id", self.id.clone());
        o.insert("title", self.title.clone());
        o.insert("status", self.status.as_str());
        o.insert("detail", self.detail.clone());
        if let Some(r) = &self.remedy {
            o.insert("remedy", r.clone());
        }
        o.insert("duration_ms", self.duration_ms);
        Value::Object(o)
    }
}

/// A module's self-test, registered with `App::selftest`. It gets the module's context as
/// the service would build it (its config section, its data directory, the site's SQL
/// pools) and must stay read-only and bounded: every network call or query uses
/// [`PROBE_TIMEOUT`], and the run as a whole is cut at [`MODULE_TIMEOUT`].
#[derive(Clone, Copy)]
pub struct SelfTest {
    /// Module name, the same as `Module::name`.
    pub module: &'static str,
    /// The checks. Return a `skip` ("not enrolled") when the module has no config yet.
    pub run: fn(&ModuleContext) -> Vec<Check>,
}

impl std::fmt::Debug for SelfTest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelfTest")
            .field("module", &self.module)
            .finish()
    }
}

/// What started a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// `dl-agent selftest`.
    Cli,
    /// The `selftest` remote command.
    Command,
    /// The installer (`dl-agent selftest --trigger install`).
    Install,
}

impl Trigger {
    /// Wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Cli => "cli",
            Trigger::Command => "command",
            Trigger::Install => "install",
        }
    }

    /// Parses a wire name.
    pub fn parse(s: &str) -> Option<Trigger> {
        match s {
            "cli" => Some(Trigger::Cli),
            "command" => Some(Trigger::Command),
            "install" => Some(Trigger::Install),
            _ => None,
        }
    }
}

/// One self-test run.
#[derive(Clone, Debug)]
pub struct Run {
    /// Random id.
    pub run_id: String,
    /// What started it.
    pub trigger: Trigger,
    /// Unix seconds.
    pub started_at: u64,
    /// Unix seconds.
    pub finished_at: u64,
    /// Version of this binary.
    pub agent_version: String,
    /// `windows`, `linux`, ...
    pub os: String,
    /// This machine's name.
    pub hostname: String,
    /// The checks, core first.
    pub checks: Vec<Check>,
}

impl Run {
    /// How many checks ended with `status`.
    pub fn count(&self, status: Status) -> usize {
        self.checks.iter().filter(|c| c.status == status).count()
    }

    /// Whether any check failed.
    pub fn failed(&self) -> bool {
        self.count(Status::Fail) > 0
    }

    /// `{"pass": n, "warn": n, "fail": n, "skip": n}`.
    pub fn summary(&self) -> Value {
        json!({
            "pass": self.count(Status::Pass), "warn": self.count(Status::Warn),
            "fail": self.count(Status::Fail), "skip": self.count(Status::Skip),
        })
    }

    fn body(&self, checks: Vec<Value>) -> Value {
        json!({
            "run_id": self.run_id, "trigger": self.trigger.as_str(),
            "started_at": self.started_at, "finished_at": self.finished_at,
            "agent_version": self.agent_version, "os": self.os, "hostname": self.hostname,
            "checks": checks,
        })
    }

    /// The upload body: at most [`MAX_CHECKS`] checks and under [`MAX_BODY`] bytes (details
    /// are cut to 200 characters when the full run is bigger).
    pub fn to_json(&self) -> Value {
        let checks = self.checks.iter().take(MAX_CHECKS);
        let full = self.body(checks.clone().map(Check::to_json).collect());
        if dl_json::to_string(&full).len() <= MAX_BODY {
            return full;
        }
        self.body(
            checks
                .map(|c| {
                    let mut short = c.clone();
                    short.detail = sanitize(&c.detail, 200);
                    short.to_json()
                })
                .collect(),
        )
    }
}

/// A random run id (32 hex digits).
pub fn run_id() -> String {
    match dl_rand::secure_bytes::<16>() {
        Ok(bytes) => dl_encoding::hex::encode(bytes),
        // No OS randomness: the seeded thread generator is unique enough for a run id.
        Err(_) => format!("{:016x}{:016x}", dl_rand::u64(), dl_rand::u64()),
    }
}
