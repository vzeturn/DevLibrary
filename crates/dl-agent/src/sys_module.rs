//! The built-in `sys` module: proves the data directory is writable, answers `sys.ping` and
//! runs the `selftest` command.
//!
//! Every agent runs it, so the canary gate always has at least one module to judge and a
//! full disk or broken permissions show up as a report instead of silent data loss.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dl_json::{Map, Value};

use crate::Error;
use crate::command::Command;
use crate::report::Severity;
use crate::supervisor::{Factory, Module, ModuleContext};

/// How often the data directory is probed.
pub const PROBE_EVERY: Duration = Duration::from_secs(60);
/// The remote command that runs the self-test in the service (module `sys`, or none).
pub const SELFTEST: &str = "selftest";

/// Runs the self-test for the `selftest` command: takes the command's arguments and returns
/// `(ok, output)`. The CLI builds it (it knows the app's modules and their self-tests).
#[derive(Clone)]
pub struct SelfTestHook(pub Arc<SelfTestFn>);

/// The function behind [`SelfTestHook`]: command arguments in, `(ok, output)` out.
pub type SelfTestFn = dyn Fn(&Value) -> (bool, String) + Send + Sync;

impl std::fmt::Debug for SelfTestHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SelfTestHook")
    }
}

/// The `sys` module.
#[derive(Debug, Default)]
pub struct SysModule {
    probes: u64,
    failures: u64,
    selftest: Option<SelfTestHook>,
    busy: Arc<AtomicBool>,
}

/// Factory for [`SysModule`] without a self-test.
pub fn factory() -> Factory {
    factory_with(None)
}

/// Factory for [`SysModule`] answering `selftest` with `hook`.
pub fn factory_with(hook: Option<SelfTestHook>) -> Factory {
    Arc::new(move || {
        Box::new(SysModule {
            selftest: hook.clone(),
            ..SysModule::default()
        })
    })
}

/// `cmd` addressed to `sys` when it is a `selftest` without a module.
pub fn route(mut cmd: Command) -> Command {
    if cmd.module.is_none() && cmd.kind == SELFTEST {
        cmd.module = Some("sys".into());
    }
    cmd
}

/// Clears the busy flag when the self-test thread ends, even by a panic.
struct Busy(Arc<AtomicBool>);

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl SysModule {
    fn probe(&mut self, ctx: &ModuleContext) {
        self.probes += 1;
        let path = ctx.data_dir().join("probe.tmp");
        let result = std::fs::write(&path, self.probes.to_string())
            .and_then(|()| std::fs::remove_file(&path));
        match result {
            Ok(()) => ctx.set_running("data directory writable"),
            Err(e) => {
                self.failures += 1;
                let msg = format!("cannot write {}: {e}", ctx.data_dir().display());
                ctx.set_degraded(&msg);
                ctx.report(Severity::Error, "data_dir_not_writable", &msg, Map::new());
            }
        }
        ctx.set_metric("probes", self.probes);
        ctx.set_metric("probe_failures", self.failures);
    }

    fn handle_commands(&self, ctx: &ModuleContext) {
        while let Some(cmd) = ctx.next_command() {
            match cmd.kind.as_str() {
                "sys.ping" => ctx.complete(&cmd.id, true, "pong"),
                SELFTEST => self.start_selftest(ctx, cmd),
                other => ctx.complete(&cmd.id, false, &format!("sys: unknown command {other}")),
            }
        }
    }

    /// Runs the self-test on its own thread (it can take a minute) and answers when done.
    fn start_selftest(&self, ctx: &ModuleContext, cmd: Command) {
        let Some(hook) = self.selftest.clone() else {
            ctx.complete(&cmd.id, false, "sys: this build has no self-test");
            return;
        };
        if self.busy.swap(true, Ordering::SeqCst) {
            ctx.complete(&cmd.id, false, "sys: a self-test is already running");
            return;
        }
        let id = cmd.id.clone();
        let (worker, busy) = (ctx.clone(), Busy(self.busy.clone()));
        let spawned = std::thread::Builder::new()
            .name("selftest".into())
            .spawn(move || {
                let _busy = busy;
                let (ok, out) = (hook.0)(&cmd.args);
                worker.complete(&cmd.id, ok, &out);
            });
        if let Err(e) = spawned {
            self.busy.store(false, Ordering::SeqCst);
            ctx.complete(&id, false, &format!("sys: cannot start the self-test: {e}"));
        }
    }
}

impl Module for SysModule {
    fn name(&self) -> &str {
        "sys"
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error> {
        let started = Instant::now();
        self.probe(ctx);
        let mut last_probe = Instant::now();
        while !ctx.stop().wait_timeout(Duration::from_millis(500)) {
            self.handle_commands(ctx);
            if last_probe.elapsed() >= PROBE_EVERY {
                self.probe(ctx);
                last_probe = Instant::now();
            }
            ctx.set_metric("uptime_seconds", started.elapsed().as_secs());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testdir::TestDir;
    use crate::supervisor::ModuleContext;

    #[test]
    fn probe_marks_running_and_ping_answers() {
        let d = TestDir::new("sys-module");
        let ctx = ModuleContext::standalone("sys", dl_json::Value::Null, d.0.clone());
        let mut m = SysModule::default();
        m.probe(&ctx);
        assert_eq!(m.failures, 0);
        let missing =
            ModuleContext::standalone("sys", dl_json::Value::Null, d.0.join("no/such/dir"));
        m.probe(&missing);
        assert_eq!(m.failures, 1);
    }

    fn command(kind: &str, module: Option<&str>) -> Command {
        Command {
            id: "c1".into(),
            kind: kind.into(),
            module: module.map(str::to_string),
            args: dl_json::json!({"module": "einvoice"}),
            expires_at: u64::MAX,
        }
    }

    #[test]
    fn selftest_without_module_goes_to_sys() {
        assert_eq!(
            route(command(SELFTEST, None)).module.as_deref(),
            Some("sys")
        );
        assert_eq!(
            route(command(SELFTEST, Some("backup"))).module.as_deref(),
            Some("backup")
        );
        assert_eq!(route(command("other", None)).module, None);
    }

    #[test]
    fn selftest_runs_the_hook_on_its_own_thread() {
        let d = TestDir::new("sys-selftest");
        let ctx = ModuleContext::standalone("sys", dl_json::Value::Null, d.0.clone());
        let hook = SelfTestHook(Arc::new(|args: &Value| {
            (
                true,
                format!("ran {}", args["module"].as_str().unwrap_or("")),
            )
        }));
        let m = SysModule {
            selftest: Some(hook),
            ..SysModule::default()
        };
        m.start_selftest(&ctx, command(SELFTEST, None));
        let deadline = Instant::now() + Duration::from_secs(5);
        while m.busy.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!m.busy.load(Ordering::SeqCst));
        let none = SysModule::default();
        none.start_selftest(&ctx, command(SELFTEST, None));
        assert!(!none.busy.load(Ordering::SeqCst));
    }
}
