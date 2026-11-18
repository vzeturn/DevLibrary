//! Remote commands (`pms.status`, `pms.reconnect`) and the bridge from telemetry reports
//! to dl-agent problem reports.

use std::sync::{Mutex, PoisonError};

use dl_agent::command::Command;
use dl_agent::report::Severity;
use dl_agent::supervisor::ModuleContext;
use dl_json::{Map, Value};

use crate::telemetry::{Level, Reporter, Telemetry};

/// What the module does after answering a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Nothing more.
    Done,
    /// Close the stream and start a new cycle.
    Reconnect,
}

/// Answers one command addressed to the module.
pub fn answer(ctx: &ModuleContext, cmd: &Command, tel: &Telemetry, detail: &str) -> Answer {
    let kind = cmd.kind.strip_prefix("pms.").unwrap_or(&cmd.kind);
    match kind {
        "status" => {
            ctx.complete(&cmd.id, true, &status_json(tel, detail));
            Answer::Done
        }
        "reconnect" => {
            ctx.complete(&cmd.id, true, "reconnecting");
            Answer::Reconnect
        }
        _ => {
            let msg = format!("unknown pms-site command {:?}", cmd.kind);
            ctx.complete(&cmd.id, false, &msg);
            Answer::Done
        }
    }
}

/// The `pms.status` answer: the counters plus the status detail.
pub fn status_json(tel: &Telemetry, detail: &str) -> String {
    let mut m = tel.snapshot().to_map();
    m.insert("detail", detail);
    dl_json::to_string(&Value::Object(m))
}

/// Publishes the counters as module metrics.
pub fn publish_metrics(ctx: &ModuleContext, tel: &Telemetry) {
    for (k, v) in tel.snapshot().to_map().iter() {
        ctx.set_metric(k, v.clone());
    }
}

/// Sends telemetry reports to dl-agent's report spool.
pub struct CtxReporter(Mutex<ModuleContext>);

impl CtxReporter {
    /// A reporter for `ctx`.
    pub fn new(ctx: ModuleContext) -> CtxReporter {
        CtxReporter(Mutex::new(ctx))
    }
}

impl Reporter for CtxReporter {
    fn report(&self, level: Level, code: &str, message: &str, context: Map) {
        let sev = match level {
            Level::Warning => Severity::Warning,
            Level::Error => Severity::Error,
            Level::Critical => Severity::Critical,
        };
        let ctx = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        ctx.report(sev, code, message, context);
    }
}
