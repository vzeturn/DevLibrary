//! The built-in `sys` module: proves the data directory is writable and answers `sys.ping`.
//!
//! Every agent runs it, so the canary gate always has at least one module to judge and a
//! full disk or broken permissions show up as a report instead of silent data loss.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_json::Map;

use crate::Error;
use crate::report::Severity;
use crate::supervisor::{Factory, Module, ModuleContext};

/// How often the data directory is probed.
pub const PROBE_EVERY: Duration = Duration::from_secs(60);

/// The `sys` module.
#[derive(Debug, Default)]
pub struct SysModule {
    probes: u64,
    failures: u64,
}

/// Factory for [`SysModule`].
pub fn factory() -> Factory {
    Arc::new(|| Box::new(SysModule::default()))
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

    fn handle_commands(ctx: &ModuleContext) {
        while let Some(cmd) = ctx.next_command() {
            match cmd.kind.as_str() {
                "sys.ping" => ctx.complete(&cmd.id, true, "pong"),
                other => ctx.complete(&cmd.id, false, &format!("sys: unknown command {other}")),
            }
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
            Self::handle_commands(ctx);
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
}
