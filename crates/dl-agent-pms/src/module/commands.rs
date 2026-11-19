//! Remote commands and the bridge from telemetry reports to dl-agent problem reports.
//!
//! * `pms.status`: the counters, the status detail, the SMILE databases as configured
//!   (name, source, auth; nothing connects), the ops this build executes and the journal.
//! * `pms.reconnect`: closes the stream and connects again.
//! * `pms.selftest`: the module's self-test rows (databases reachable as configured, auth
//!   mode, journal health, the read probe), as JSON; `ok` is false when a row failed.
//! * `pms.op_dry_run`: runs one read op (`args.op`, default `fo.hotelDate.get`, with
//!   `args.args`) through the local SQL executor, without the cloud and without the
//!   journal, and returns its result frame. Writes are refused.
//!
//! The slow ones run on their own thread so the module keeps its tick. Connection strings,
//! request keys and SQL text never appear in an answer.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use dl_agent::command::Command;
use dl_agent::report::Severity;
use dl_agent::selftest::Status;
use dl_agent::supervisor::ModuleContext;
use dl_json::{Map, Value, json};

use super::prepare::{Prepared, prepare};
use super::selftest::{self, PROBE_OP};
use super::settings::Settings;
use crate::config::Config;
use crate::ctx::Ctx;
use crate::db;
use crate::exec::{Executor, Limits, SqlExecutor};
use crate::journal;
use crate::log::Logger;
use crate::ops::{self, Manifest};
use crate::telemetry::{Level, Reporter, Telemetry};
use crate::wire::{self, Frame, Op};

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
            ctx.complete(&cmd.id, true, &status_json(ctx, tel, detail));
            Answer::Done
        }
        "reconnect" => {
            ctx.complete(&cmd.id, true, "reconnecting");
            Answer::Reconnect
        }
        "selftest" => {
            background(ctx, cmd, |c, _| {
                let checks = selftest::run(c);
                let failed = checks.iter().any(|ch| ch.status == Status::Fail);
                let rows: Vec<Value> = checks.iter().map(|ch| ch.to_json()).collect();
                (!failed, dl_json::to_string(&json!({ "checks": rows })))
            });
            Answer::Done
        }
        "op_dry_run" => {
            background(ctx, cmd, dry_run);
            Answer::Done
        }
        _ => {
            let msg = format!("unknown pms-site command {:?}", cmd.kind);
            ctx.complete(&cmd.id, false, &msg);
            Answer::Done
        }
    }
}

/// Runs `f` on its own thread and completes `cmd` with its answer.
fn background(
    ctx: &ModuleContext,
    cmd: &Command,
    f: impl FnOnce(&ModuleContext, &Value) -> (bool, String) + Send + 'static,
) {
    let (c, id, args) = (ctx.clone(), cmd.id.clone(), cmd.args.clone());
    let spawned = std::thread::Builder::new()
        .name("pms-site-command".into())
        .spawn(move || {
            let (ok, out) = f(&c, &args);
            c.complete(&id, ok, &out);
        });
    if let Err(e) = spawned {
        ctx.complete(&cmd.id, false, &format!("start the command: {e}"));
    }
}

/// The site config the module's settings point at, or why there is none.
fn site_config(ctx: &ModuleContext) -> Result<Config, String> {
    match prepare(&Settings::from_config(ctx.config())) {
        Prepared::Ready(cfg) => Ok(*cfg),
        Prepared::Waiting { detail, .. } => Err(detail),
    }
}

/// The `pms.status` answer.
pub fn status_json(ctx: &ModuleContext, tel: &Telemetry, detail: &str) -> String {
    let mut m = tel.snapshot().to_map();
    m.insert("detail", detail);
    if let Ok(man) = Manifest::load() {
        m.insert("ops", Value::from(man.implemented_ops()));
        m.insert("contract_max", man.contract_max());
    }
    match site_config(ctx) {
        Err(why) => {
            m.insert("site", why);
        }
        Ok(cfg) => {
            m.insert("databases", databases_json(ctx, &cfg));
            let j = match journal::inspect(&cfg.state_dir) {
                Ok(h) => h.to_json(),
                Err(e) => json!({ "error": e.to_string() }),
            };
            m.insert("journal", j);
        }
    }
    dl_json::to_string(&Value::Object(m))
}

/// The SMILE databases as configured, without connecting.
fn databases_json(ctx: &ModuleContext, cfg: &Config) -> Value {
    let site = match db::open(
        &db::shared_pools(ctx),
        &cfg.state_dir.join(db::DB_CONFIG_FILE),
    ) {
        Ok(s) => s,
        Err(e) => return json!({ "error": e.to_string() }),
    };
    let rows: Vec<Value> = site
        .pools
        .values()
        .map(|p| json!({ "name": p.name(), "source": p.source().as_str(), "auth": p.auth() }))
        .collect();
    db::close_all(&site.pools);
    json!({ "configured": rows, "ignored": site.ignored })
}

/// `pms.op_dry_run`: one read op through the local executor.
fn dry_run(ctx: &ModuleContext, args: &Value) -> (bool, String) {
    let fail = |m: String| (false, dl_json::to_string(&json!({ "error": m })));
    let name = match args.get("op") {
        None | Some(Value::Null) => PROBE_OP,
        Some(Value::String(s)) => s.as_str(),
        Some(_) => return fail("op: not a string".into()),
    };
    let op_args = match args.get("args") {
        None | Some(Value::Null) => Value::Object(Map::new()),
        Some(v @ Value::Object(_)) => v.clone(),
        Some(_) => return fail("args: not an object".into()),
    };
    let manifest = match Manifest::load() {
        Ok(m) => Arc::new(m),
        Err(e) => return fail(e),
    };
    let Some(spec) = manifest.lookup(name) else {
        return fail(format!("unknown op {name:?}"));
    };
    if spec.kind != ops::KIND_READ || spec.calls.is_empty() {
        return fail(format!(
            "{name} is not a read this agent runs; op_dry_run runs reads only"
        ));
    }
    let Some(target_db) = spec.target_db.clone() else {
        return fail(format!("{name} has no site database"));
    };
    let cfg = match site_config(ctx) {
        Ok(c) => c,
        Err(why) => return fail(why),
    };
    let site = match db::open(
        &db::shared_pools(ctx),
        &cfg.state_dir.join(db::DB_CONFIG_FILE),
    ) {
        Ok(s) => s,
        Err(e) => return fail(e.to_string()),
    };
    let exec = SqlExecutor {
        manifest: Arc::clone(&manifest),
        dbs: site.pools.clone(),
        limits: Limits::default(),
        log: Logger::discard(),
    };
    let op = Op {
        id: dl_uuid::Uuid::new_v4().to_string(),
        op: name.to_owned(),
        contract_ver: spec.contract_ver,
        target_db: target_db.clone(),
        args: op_args,
        request_key: None,
        expected_stamp: None,
        deadline_ms: spec.timeout_ms,
        replay: None,
    };
    let started = Instant::now();
    let deadline = std::time::Duration::from_millis(u64::try_from(spec.timeout_ms).unwrap_or(0));
    let res = exec.execute(&Ctx::background().with_timeout(deadline), &op);
    db::close_all(&site.pools);
    let ok = res.status == wire::STATUS_APPLIED;
    let mut out = Map::new();
    out.insert("op", name);
    out.insert("target_db", target_db);
    out.insert("status", res.status.as_str());
    out.insert(
        "took_ms",
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    );
    if ok && name == PROBE_OP {
        // The decoded answer the cloud would return: `{hotelDate:'YYYY-MM-DD'}`.
        if let Some(Value::String(d)) = res
            .sets
            .first()
            .and_then(|s| s.first())
            .and_then(|r| r.get("HotelDate"))
        {
            out.insert("hotelDate", d.get(..10).unwrap_or(d));
        }
    }
    let frame = dl_json::parse(&wire::to_json(&Frame::Result(res))).unwrap_or(Value::Null);
    out.insert("result", frame);
    (ok, dl_json::to_string(&Value::Object(out)))
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
