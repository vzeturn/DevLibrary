//! The dl-agent module: wires the sealed config, the collector thread and the relay worker
//! into [`dl_agent::supervisor::Module`].
//!
//! The module is opt-in (`modules.einvoice.enabled = true`). Without `config.enc` it stays
//! degraded ("not enrolled") and looks for the file again every 30 s; it does not fail and
//! restart. Blocking calls (SQL, platform, provider) cannot be interrupted, so the workers
//! run on their own threads and the module thread answers commands, publishes metrics and
//! returns within a few seconds of `stop`, leaving a worker that is still finishing a call
//! to end on its own.

pub mod collector;
pub mod settings;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dl_agent::Error;
use dl_agent::command::Command;
use dl_agent::report::Severity;
use dl_agent::supervisor::{Factory, Module, ModuleContext};
use dl_json::{Map, Value};
use dl_winsvc::StopSignal;

use crate::agent_config::AgentConfig;
use crate::log::{self, V};
use crate::outbox::Outbox;
use crate::platform::Platform;
use crate::relay::transport::HttpTransport;
use crate::relay::{Exit, Worker};
use crate::runner::Runner;
use crate::sql::Sql;
use collector::{Collector, RELAY_BUSY, Shared, exclusive};
use settings::Settings;

/// The module name: config section, report module and command prefix.
pub const NAME: &str = "einvoice";
/// The outbox file inside the module's data directory.
pub const OUTBOX_FILE: &str = "outbox.json";

/// How often the module thread answers commands and publishes metrics.
const TICK: Duration = Duration::from_secs(1);
/// How often a module without a usable `config.enc` looks again.
const RECHECK: Duration = Duration::from_secs(30);
/// How long a stop waits for the worker threads before leaving them to finish.
const WORKER_STOP_WAIT: Duration = Duration::from_secs(5);
/// The poll interval when the sealed config has none.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(30);

/// The Azolo E-Invoice module.
#[derive(Debug, Default)]
pub struct EinvoiceModule;

/// The factory to register with `App::module`.
pub fn einvoice_module() -> Factory {
    Arc::new(|| Box::new(EinvoiceModule) as Box<dyn Module>)
}

impl Module for EinvoiceModule {
    fn name(&self) -> &str {
        NAME
    }

    fn enabled_by_default(&self) -> bool {
        false
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error> {
        let settings = Settings::from_config(ctx.config());
        match wait_for_config(ctx, &settings.config_dir) {
            Some(cfg) => run_enrolled(ctx, cfg, &settings),
            None => Ok(()),
        }
    }
}

/// Loads `config.enc`, staying degraded until it exists and opens; `None` when stopped.
fn wait_for_config(ctx: &ModuleContext, dir: &Path) -> Option<AgentConfig> {
    let mut detail = String::new();
    let mut next_check = Instant::now();
    loop {
        if Instant::now() >= next_check {
            next_check = Instant::now() + RECHECK;
            let now = match AgentConfig::load(dir) {
                Ok(Some(cfg)) => return Some(cfg),
                Ok(None) => format!("not enrolled: no config.enc in {}", dir.display()),
                Err(e) => format!("config.enc unreadable: {e}"),
            };
            if now != detail {
                log::warn(
                    "einvoice module waiting for config",
                    &[("detail", V::S(&now))],
                );
                if !now.starts_with("not enrolled") {
                    let mut m = Map::new();
                    m.insert("config_dir", dir.display().to_string());
                    ctx.report(Severity::Error, "einvoice_config_unreadable", &now, m);
                }
                ctx.set_degraded(&now);
                detail = now;
            }
        }
        while let Some(cmd) = ctx.next_command() {
            answer(ctx, &cmd, None, &detail);
        }
        if ctx.stop().wait_timeout(TICK) {
            return None;
        }
    }
}

fn log_config(cfg: &AgentConfig, interval: Duration) {
    let r = cfg.redacted();
    log::info(
        "config loaded",
        &[
            ("platform_base_url", V::S(&r.platform_base_url)),
            ("device_id", V::S(&r.device_id.to_string())),
            ("api_key", V::S(&r.api_key)),
            ("collect_sources", V::S(&r.collect_sources)),
            ("batch_size", V::I(r.batch_size)),
            ("recalc_tax_into_sc", V::B(r.recalc_tax_into_sc)),
            ("poll_interval_s", V::I(interval.as_secs() as i64)),
        ],
    );
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, Error> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map_err(|e| Error::Io("spawn einvoice worker", e))
}

/// Runs the collector and the relay worker until `ctx.stop()`.
fn run_enrolled(
    ctx: &ModuleContext,
    mut cfg: AgentConfig,
    settings: &Settings,
) -> Result<(), Error> {
    if let Some(p) = settings.poll_interval {
        cfg.poll_interval = p;
    }
    let interval = if cfg.poll_interval.is_zero() {
        DEFAULT_INTERVAL
    } else {
        cfg.poll_interval
    };
    log_config(&cfg, interval);
    let outbox_path = ctx.data_dir().join(OUTBOX_FILE);
    let outbox = Outbox::open(&outbox_path).map_err(|e| {
        let mut m = Map::new();
        m.insert("path", outbox_path.display().to_string());
        ctx.report(Severity::Error, "einvoice_outbox_failed", &e, m);
        Error::Module(format!("open outbox: {e}"))
    })?;
    let relay_hub = Platform::new(&cfg.platform_base_url, &cfg.api_key, cfg.device_id);
    let cli = Platform::new(&cfg.platform_base_url, &cfg.api_key, cfg.device_id);
    let sql = Sql::new(&cfg.sql_dsn, &cfg.pos_sql_dsn, cfg.sp_names());
    let runner = Runner::new(sql, cli, outbox, cfg);

    let shared = Arc::new(Shared::default());
    let stop = StopSignal::new();
    let collector = Collector::new(runner, interval, stop.clone(), shared.clone(), ctx.clone());
    let mut workers = vec![(
        "collector",
        spawn("einvoice-collector", move || collector.run())?,
    )];
    let (rstop, rshared, rctx) = (stop.clone(), shared.clone(), ctx.clone());
    let relay = spawn("einvoice-relay", move || {
        let Some(_one) = exclusive(&RELAY_BUSY, &rstop) else {
            return;
        };
        let stats = rshared.stats.clone();
        let mut w = Worker::new(relay_hub, HttpTransport::new(), rstop, stats, rctx);
        if w.run() == Exit::Replaced {
            rshared.replaced.store(true, Ordering::Relaxed);
        }
    });
    match relay {
        Ok(h) => workers.push(("relay", h)),
        Err(e) => {
            stop.stop();
            return Err(e);
        }
    }
    let running = format!("collecting every {} s; relay on", interval.as_secs());
    ctx.set_running(&running);
    log::info("einvoice module started", &[("detail", V::S(&running))]);

    let result = supervise(ctx, &shared, &stop, &mut workers, &running);
    stop.stop();
    finish(workers);
    result
}

/// The module thread's loop: commands, metrics, health and worker crashes.
fn supervise(
    ctx: &ModuleContext,
    shared: &Shared,
    stop: &StopSignal,
    workers: &mut Vec<(&'static str, JoinHandle<()>)>,
    running: &str,
) -> Result<(), Error> {
    let mut replaced = false;
    loop {
        let detail = if replaced {
            REPLACED.to_owned()
        } else {
            shared.problem().unwrap_or_else(|| running.to_owned())
        };
        while let Some(cmd) = ctx.next_command() {
            answer(ctx, &cmd, Some(shared), &detail);
        }
        for (k, v) in shared.stats.to_map().iter() {
            ctx.set_metric(k, v.clone());
        }
        if !replaced && shared.replaced.load(Ordering::Relaxed) {
            replaced = true;
            stop.stop();
            ctx.set_degraded(REPLACED);
            ctx.report(
                Severity::Critical,
                "einvoice_device_replaced",
                REPLACED,
                Map::new(),
            );
            log::error(
                "einvoice module stopped its workers",
                &[("reason", V::S(REPLACED))],
            );
        } else if !replaced {
            match shared.problem() {
                Some(p) => ctx.set_degraded(&p),
                None => ctx.set_running(running),
            }
        }
        // A worker that ends while not stopped crashed (the replaced case is handled above,
        // on the next tick when its flag lands between the two checks).
        if !stop.is_stopped()
            && !shared.replaced.load(Ordering::Relaxed)
            && let Some(i) = workers.iter().position(|(_, h)| h.is_finished())
        {
            let (name, h) = workers.remove(i);
            let msg = match h.join() {
                Err(_) => format!("einvoice {name} thread panicked"),
                Ok(()) => format!("einvoice {name} thread ended unexpectedly"),
            };
            log::error(&msg, &[]);
            return Err(Error::Module(msg));
        }
        if ctx.stop().wait_timeout(TICK) {
            return Ok(());
        }
    }
}

/// The detail shown once the platform has given this install's key to another machine.
const REPLACED: &str = "replaced by a deployment on another machine; re-run the deploy command from the tenant's Agent tab to run it here";

/// Waits briefly for the workers; a worker inside a blocking call is left to finish it.
fn finish(workers: Vec<(&'static str, JoinHandle<()>)>) {
    let deadline = Instant::now() + WORKER_STOP_WAIT;
    while workers.iter().any(|(_, h)| !h.is_finished()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    for (name, h) in workers {
        if !h.is_finished() {
            log::warn(
                "einvoice worker still finishing an in-flight call; it ends on its own",
                &[("worker", V::S(name))],
            );
            continue;
        }
        if h.join().is_err() {
            log::error(
                "einvoice worker panicked while stopping",
                &[("worker", V::S(name))],
            );
        }
    }
    log::info("einvoice module stopped", &[]);
}

/// Answers `einvoice.status` and `einvoice.collect_now`.
fn answer(ctx: &ModuleContext, cmd: &Command, shared: Option<&Shared>, detail: &str) {
    let kind = cmd.kind.strip_prefix("einvoice.").unwrap_or(&cmd.kind);
    match (kind, shared) {
        ("status", _) => {
            let mut m = shared.map(|s| s.stats.to_map()).unwrap_or_default();
            m.insert("enrolled", shared.is_some());
            m.insert("detail", detail);
            ctx.complete(&cmd.id, true, &dl_json::to_string(&Value::Object(m)));
        }
        ("collect_now", Some(s)) => {
            s.collect_now.store(true, Ordering::Relaxed);
            ctx.complete(&cmd.id, true, "collect cycle queued");
        }
        ("collect_now", None) => ctx.complete(&cmd.id, false, detail),
        _ => ctx.complete(
            &cmd.id,
            false,
            &format!("unknown einvoice command {:?}", cmd.kind),
        ),
    }
}
