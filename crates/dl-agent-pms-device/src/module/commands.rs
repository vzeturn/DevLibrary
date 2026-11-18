//! Remote commands (`pms-device.status`, `pms-device.flush_outbox`), the module metrics and
//! the bridge from telemetry reports to dl-agent problem reports.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use dl_agent::command::Command;
use dl_agent::report::Severity;
use dl_agent::supervisor::ModuleContext;
use dl_json::{Map, Value};

use crate::agent::{ResultOutbox, ResultReporter, StopToken};

use super::telemetry::{Level, Reporter, Telemetry, describe};

/// Command kinds, after the optional `pms-device.` prefix.
pub const STATUS: &str = "status";
/// Sends the results waiting in the outbox now instead of at the next poll.
pub const FLUSH_OUTBOX: &str = "flush_outbox";

/// Sends the outbox on demand while the worker runs (one flush at a time).
pub struct Flusher {
    reporter: Arc<ResultReporter>,
    outbox: ResultOutbox,
    stop: StopToken,
    busy: Arc<AtomicBool>,
}

impl Flusher {
    /// A flusher sending through `reporter`; `stop` is the worker's stop token, so a flush
    /// ends with the cycle.
    pub fn new(reporter: ResultReporter, outbox: ResultOutbox, stop: StopToken) -> Flusher {
        Flusher {
            reporter: Arc::new(reporter),
            outbox,
            stop,
            busy: Arc::default(),
        }
    }

    /// Flushes on a helper thread and answers `id` when done; the module thread keeps
    /// answering and can stop meanwhile (a stop ends the flush between two sends).
    fn start(&self, ctx: &ModuleContext, id: &str) {
        if self.busy.swap(true, Ordering::AcqRel) {
            ctx.complete(id, false, "a flush is already running");
            return;
        }
        let (reporter, outbox, stop, busy) = (
            self.reporter.clone(),
            self.outbox.clone(),
            self.stop.clone(),
            self.busy.clone(),
        );
        let (worker_ctx, worker_id) = (ctx.clone(), id.to_owned());
        let spawned = thread::Builder::new()
            .name("pms-device-flush".into())
            .spawn(move || {
                let (ok, text) = flush(&reporter, &outbox, &stop);
                worker_ctx.complete(&worker_id, ok, &text);
                busy.store(false, Ordering::Release);
            });
        if let Err(e) = spawned {
            self.busy.store(false, Ordering::Release);
            ctx.complete(id, false, &format!("cannot start a flush: {e}"));
        }
    }
}

/// One flush: `(ok, answer)`; the answer is JSON with what was sent and what is left.
fn flush(reporter: &ResultReporter, outbox: &ResultOutbox, stop: &StopToken) -> (bool, String) {
    let before = match outbox.pending_count() {
        Ok(n) => n,
        Err(e) => return (false, format!("outbox not readable: {e}")),
    };
    let res = reporter.flush(stop);
    let left = match outbox.pending_count() {
        Ok(n) => n,
        Err(e) => return (false, format!("outbox not readable: {e}")),
    };
    let mut m = Map::new();
    m.insert("sent", before.saturating_sub(left) as u64);
    m.insert("pending", left as u64);
    match res {
        Ok(()) => (true, dl_json::to_string(&Value::Object(m))),
        Err(e) => {
            let reason = describe(&e).map_or_else(|| "stopped".to_owned(), |(r, _)| r);
            m.insert("error", reason);
            (false, dl_json::to_string(&Value::Object(m)))
        }
    }
}

/// What the module shows while it answers commands.
#[derive(Clone, Copy)]
pub struct View<'a> {
    /// The status detail.
    pub detail: &'a str,
    /// Results waiting in the outbox (`None`: not counted yet or unreadable).
    pub outbox_pending: Option<u64>,
    /// The running worker's flusher (`None` while the worker is not running).
    pub flusher: Option<&'a Flusher>,
}

/// Answers one command addressed to the module.
pub fn answer(ctx: &ModuleContext, cmd: &Command, tel: &Telemetry, view: View<'_>) {
    let kind = cmd.kind.strip_prefix("pms-device.").unwrap_or(&cmd.kind);
    match (kind, view.flusher) {
        (STATUS, _) => {
            let text = status_json(tel, view.detail, view.outbox_pending);
            ctx.complete(&cmd.id, true, &text);
        }
        (FLUSH_OUTBOX, Some(f)) => f.start(ctx, &cmd.id),
        (FLUSH_OUTBOX, None) => {
            let msg = format!("the agent is not running: {}", view.detail);
            ctx.complete(&cmd.id, false, &msg);
        }
        _ => {
            let msg = format!("unknown pms-device command {:?}", cmd.kind);
            ctx.complete(&cmd.id, false, &msg);
        }
    }
}

/// The `pms-device.status` answer: the counters, the outbox and the status detail.
pub fn status_json(tel: &Telemetry, detail: &str, outbox_pending: Option<u64>) -> String {
    let mut m = tel.to_map();
    if let Some(n) = outbox_pending {
        m.insert("outbox_pending", n);
    }
    m.insert("detail", detail);
    dl_json::to_string(&Value::Object(m))
}

/// Publishes the counters (and the outbox size when known) as module metrics.
pub fn publish_metrics(ctx: &ModuleContext, tel: &Telemetry, outbox_pending: Option<u64>) {
    for (k, v) in tel.to_map().iter() {
        ctx.set_metric(k, v.clone());
    }
    if let Some(n) = outbox_pending {
        ctx.set_metric("outbox_pending", n);
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
        };
        let ctx = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        ctx.report(sev, code, message, context);
    }
}
