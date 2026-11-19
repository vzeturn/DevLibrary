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
mod commands;
pub mod selftest;
pub mod settings;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use dl_agent::Error;
use dl_agent::report::Severity;
use dl_agent::supervisor::{Factory, Module, ModuleContext};
use dl_json::Map;
use dl_winsvc::StopSignal;

use crate::agent_config::AgentConfig;
use crate::log::{self, V};
use crate::outbox::Outbox;
use crate::platform::Platform;
use crate::relay::transport::HttpTransport;
use crate::relay::{Exit, Worker};
use crate::runner::Runner;
use collector::{Collector, Effective, RELAY_BUSY, Shared, exclusive};
use commands::Env;
use settings::Settings;

/// The module name: config section, report module and command prefix.
pub const NAME: &str = "einvoice";
/// The outbox file inside the module's data directory.
pub const OUTBOX_FILE: &str = "outbox.json";

/// How often the module thread answers commands and publishes metrics.
const TICK: Duration = Duration::from_secs(1);
/// How often a module without a usable `config.enc` looks again.
const RECHECK: Duration = Duration::from_secs(30);
/// How often a running module looks for a re-sealed `config.enc` (re-enroll, new key).
const CONFIG_WATCH: Duration = Duration::from_secs(5);
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
        let path = settings.config_dir.join(crate::agent_config::CONFIG_FILE);
        loop {
            let Some(cfg) = wait_for_config(ctx, &settings) else {
                return Ok(());
            };
            let watch = ConfigWatch::new(path.clone(), Instant::now());
            match run_enrolled(ctx, cfg, &settings, watch)? {
                Outcome::Stopped => return Ok(()),
                Outcome::Reload => log::info(
                    "einvoice module reloading config.enc",
                    &[("reason", V::S("config.enc changed on disk"))],
                ),
            }
        }
    }
}

/// Why `run_enrolled` returned without an error.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// `ctx.stop()` fired.
    Stopped,
    /// `config.enc` was re-sealed (or removed): load it again with fresh workers.
    Reload,
}

/// Size and modification time of `config.enc`; `None` when it is missing.
type Fingerprint = Option<(u64, Option<SystemTime>)>;

fn fingerprint(path: &Path) -> Fingerprint {
    std::fs::metadata(path)
        .ok()
        .map(|m| (m.len(), m.modified().ok()))
}

/// Notices that `config.enc` changed under a running module. A change counts once it has
/// held still for one look, so a file that is still being written is not loaded half-way.
#[derive(Debug)]
struct ConfigWatch {
    path: PathBuf,
    loaded: Fingerprint,
    seen: Fingerprint,
    next: Instant,
}

impl ConfigWatch {
    fn new(path: PathBuf, now: Instant) -> Self {
        let loaded = fingerprint(&path);
        Self {
            path,
            seen: loaded,
            loaded,
            next: now + CONFIG_WATCH,
        }
    }

    /// True when the file differs from the loaded one and did not change since the last look.
    fn changed(&mut self, now: Instant) -> bool {
        if now < self.next {
            return false;
        }
        self.next = now + CONFIG_WATCH;
        let cur = fingerprint(&self.path);
        let stable = cur == self.seen;
        self.seen = cur;
        stable && cur != self.loaded
    }
}

/// Loads `config.enc`, staying degraded until it exists and opens; `None` when stopped.
fn wait_for_config(ctx: &ModuleContext, settings: &Settings) -> Option<AgentConfig> {
    let dir = settings.config_dir.as_path();
    let env = Env {
        settings,
        shared: None,
    };
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
            commands::answer(ctx, &cmd, &env, &detail);
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
    mut watch: ConfigWatch,
) -> Result<Outcome, Error> {
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
    let sql = settings.open_sql(ctx, &cfg).map_err(|e| {
        let mut m = Map::new();
        m.insert("config_dir", settings.config_dir.display().to_string());
        ctx.report(Severity::Error, "einvoice_sql_source", &e, m);
        ctx.set_degraded(&e);
        Error::Module(e)
    })?;
    let shared = Arc::new(Shared::default());
    shared.set_effective(Effective {
        cfg: cfg.clone(),
        pos_item_name: sql.pos_item_name(),
    });
    *shared.sql.lock().unwrap_or_else(PoisonError::into_inner) = sql.describe();
    let runner = Runner::new(sql, cli, outbox, cfg);

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

    let env = Env {
        settings,
        shared: Some(&shared),
    };
    let result = supervise(ctx, &env, &stop, &mut workers, &running, &mut watch);
    stop.stop();
    finish(workers);
    result
}

/// The module thread's loop: commands, metrics, health and worker crashes.
fn supervise(
    ctx: &ModuleContext,
    env: &Env<'_>,
    stop: &StopSignal,
    workers: &mut Vec<(&'static str, JoinHandle<()>)>,
    running: &str,
    watch: &mut ConfigWatch,
) -> Result<Outcome, Error> {
    let Some(shared) = env.shared else {
        return Err(Error::Module("einvoice collector state missing".into()));
    };
    let mut replaced = false;
    loop {
        let detail = if replaced {
            REPLACED.to_owned()
        } else {
            shared.problem().unwrap_or_else(|| running.to_owned())
        };
        while let Some(cmd) = ctx.next_command() {
            commands::answer(ctx, &cmd, env, &detail);
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
        // A re-enroll (`einvoice-enroll --force`) seals a new key: the old one is revoked, so
        // the workers must start again on the new file, and a replaced device recovers.
        if watch.changed(Instant::now()) {
            return Ok(Outcome::Reload);
        }
        if ctx.stop().wait_timeout(TICK) {
            return Ok(Outcome::Stopped);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "dl-einvoice-watch-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("config.enc")
    }

    /// Looks `n` watch periods after `start`.
    fn look(w: &mut ConfigWatch, start: Instant, n: u32) -> bool {
        w.changed(start + CONFIG_WATCH * n)
    }

    #[test]
    fn unchanged_file_never_reloads() {
        let path = scratch("same");
        std::fs::write(&path, b"sealed-1").expect("write");
        let start = Instant::now();
        let mut w = ConfigWatch::new(path, start);
        assert!(!w.changed(start), "too early to look");
        assert!(!look(&mut w, start, 1));
        assert!(!look(&mut w, start, 2));
    }

    #[test]
    fn resealed_file_reloads_once_it_holds_still() {
        let path = scratch("reseal");
        std::fs::write(&path, b"sealed-1").expect("write");
        let start = Instant::now();
        let mut w = ConfigWatch::new(path.clone(), start);
        std::fs::write(&path, b"sealed-two-longer").expect("rewrite");
        assert!(!look(&mut w, start, 1), "first sight of the change waits");
        assert!(look(&mut w, start, 2), "stable change reloads");
    }

    #[test]
    fn removed_file_reloads_into_not_enrolled() {
        let path = scratch("gone");
        std::fs::write(&path, b"sealed-1").expect("write");
        let start = Instant::now();
        let mut w = ConfigWatch::new(path.clone(), start);
        std::fs::remove_file(&path).expect("remove");
        assert!(!look(&mut w, start, 1));
        assert!(look(&mut w, start, 2));
    }
}
