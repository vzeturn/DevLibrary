//! Runs checks on their own threads with a deadline, so a hung SQL server or hub cannot hold
//! the self-test (or the service's `sys` module) forever.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::{Check, Outcome, SelfTest};
use crate::config::RemoteConfig;
use crate::sql::SqlPools;
use crate::supervisor::ModuleContext;

/// Why [`bounded`] has no result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stalled {
    /// Still running at the deadline (the thread is left to finish; its result is dropped).
    TimedOut,
    /// The work panicked; the panic message.
    Panicked(String),
    /// No thread could be started.
    NoThread(String),
}

impl std::fmt::Display for Stalled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Stalled::TimedOut => f.write_str("did not finish in time"),
            Stalled::Panicked(m) => write!(f, "crashed: {m}"),
            Stalled::NoThread(e) => write!(f, "cannot start a thread: {e}"),
        }
    }
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}

/// Starts `f` on a named thread; the receiver gets its result or its panic message.
fn spawn<T: Send + 'static>(
    name: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<mpsc::Receiver<Result<T, String>>, Stalled> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name(format!("selftest-{name}"))
        .spawn(move || {
            let r = catch_unwind(AssertUnwindSafe(f)).map_err(|p| panic_text(&*p));
            // The receiver is gone after a timeout; the result is dropped on purpose.
            let _ = tx.send(r);
        })
        .map_err(|e| Stalled::NoThread(e.to_string()))?;
    Ok(rx)
}

fn wait<T>(rx: &mpsc::Receiver<Result<T, String>>, deadline: Instant) -> Result<T, Stalled> {
    match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(m)) => Err(Stalled::Panicked(m)),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(Stalled::TimedOut),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(Stalled::Panicked("no result".into())),
    }
}

/// Runs `f` on its own thread and waits at most `timeout` for it.
pub fn bounded<T: Send + 'static>(
    name: &str,
    timeout: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Stalled> {
    let deadline = Instant::now() + timeout;
    wait(&spawn(name, f)?, deadline)
}

/// One registered module as the run sees it.
#[derive(Clone, Debug)]
pub(crate) struct ModulePlan {
    pub name: String,
    pub enabled: bool,
    pub test: Option<SelfTest>,
}

/// The plan for `modules` (registration order, each with its `enabled_by_default`): enabled
/// per the remote config, with the module's self-test when one is registered.
pub(crate) fn plan(
    modules: &[(String, bool)],
    tests: &[SelfTest],
    config: &RemoteConfig,
    only: Option<&str>,
) -> Vec<ModulePlan> {
    modules
        .iter()
        .filter(|(name, _)| only.is_none_or(|o| o == name))
        .map(|(name, default)| ModulePlan {
            name: name.clone(),
            enabled: config.module_enabled_or(name, *default),
            test: tests.iter().find(|t| t.module == name).copied(),
        })
        .collect()
}

/// Runs every enabled module's self-test at once, each on its own thread, and collects
/// them in plan order within `timeout`. Disabled modules and modules without a self-test
/// give one `skip`; a stuck or crashed self-test gives one `fail`.
pub(crate) fn run_modules(
    plans: &[ModulePlan],
    data_dir: &Path,
    config: &RemoteConfig,
    sql: &SqlPools,
    timeout: Duration,
) -> Vec<Check> {
    let deadline = Instant::now() + timeout;
    let started: Vec<_> = plans
        .iter()
        .map(|p| {
            let Some(test) = p.test.filter(|_| p.enabled) else {
                return (p, None);
            };
            let ctx = ModuleContext::standalone(
                &p.name,
                config.module(&p.name),
                data_dir.join("modules").join(&p.name),
            )
            .with_sql(sql.clone());
            (
                p,
                Some((Instant::now(), spawn(&p.name, move || (test.run)(&ctx)))),
            )
        })
        .collect();
    let mut out = Vec::new();
    for (p, job) in started {
        let Some((t0, job)) = job else {
            let reason = if p.enabled {
                "this module has no self-test"
            } else {
                "module disabled in the remote config"
            };
            out.push(Check::skip(&p.name, "module", "Module self-test", reason));
            continue;
        };
        match job.and_then(|rx| wait(&rx, deadline)) {
            Ok(checks) => out.extend(checks.into_iter().map(|mut c| {
                c.module.clone_from(&p.name);
                c
            })),
            Err(e) => out.push(Check::new(
                &p.name,
                "module",
                "Module self-test",
                Outcome::fail(format!("the self-test {e}"))
                    .remedy("send the agent log (logs/agent.log) to support"),
                t0.elapsed(),
            )),
        }
    }
    out
}
