//! The dl-agent module `hotellink`: wires the HL config, the SQL Server pool, the hub client
//! and the durable job queue into a [`Runner`], and turns its counters into the module
//! state, metrics and reports.
//!
//! The module is opt-in (`modules.hotellink.enabled = true`). Without a usable
//! `config.json` (or without a token) it stays degraded ("not configured: …") and looks for
//! the file again every 30 s. The hub long poll (25 s) and a SQL transaction cannot be
//! interrupted, so the runner has its own thread: on stop the module waits up to 5 s for the
//! job in flight and then returns, leaving that thread to end at its next check. A job it
//! did not finish stays in the durable queue and is applied or re-acked on the next start.

pub mod health;
pub mod selftest;
pub mod settings;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dl_agent::Error;
use dl_agent::command::Command;
use dl_agent::report::Severity;
use dl_agent::sql::{Pool, PoolOptions, TdsConnector};
use dl_agent::supervisor::{Factory, Module, ModuleContext};
use dl_json::{Map, Value};

use crate::config::{self, LocalConfig};
use crate::hubclient::{DEFAULT_HUB_FALLBACK, HubClient};
use crate::jobqueue::FileQueue;
use crate::log::{self, V};
use crate::pms::{FullRepo, SqlRepo};
use crate::run::{Parts, Runner, Timings};
use health::{Limiter, Signals, Verdict};
use settings::Settings;

/// The module name: config section, report module and command prefix.
pub const NAME: &str = "hotellink";
/// The queue file inside the module's state directory.
pub const QUEUE_FILE: &str = "queue.jsonl";
/// Written once the Go agent's `queue.jsonl` was imported.
pub const LEGACY_MARKER: &str = "legacy-queue-imported";

/// How often the module thread answers commands and publishes metrics.
const TICK: Duration = Duration::from_millis(500);
/// How often a module without a usable config looks again.
const RECHECK: Duration = Duration::from_secs(30);
/// How long a stop waits for the job in flight.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// One report per code per window.
const REPORT_WINDOW: Duration = Duration::from_secs(15 * 60);

/// The SMILE Hotel Link module.
#[derive(Debug, Default)]
pub struct HotellinkModule;

/// The factory to register with `App::module`.
pub fn hotellink_module() -> Factory {
    Arc::new(|| Box::new(HotellinkModule) as Box<dyn Module>)
}

impl Module for HotellinkModule {
    fn name(&self) -> &str {
        NAME
    }

    fn enabled_by_default(&self) -> bool {
        false
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error> {
        let settings = Settings::from_config(ctx.config());
        let Some((cfg, repo)) = wait_for_config(ctx, &settings.config_path) else {
            return Ok(());
        };
        let wiring = Wiring {
            repo,
            timings: Timings::default(),
            grace: STOP_GRACE,
        };
        run_configured(ctx, &cfg, &settings.config_path, wiring)
    }
}

/// What `run_configured` runs on; tests swap the repository and the waits.
pub(crate) struct Wiring {
    pub repo: Arc<dyn FullRepo>,
    pub timings: Timings,
    pub grace: Duration,
}

/// Why the config cannot run, or the config and its SQL repository.
fn usable(path: &Path) -> Result<(LocalConfig, Arc<dyn FullRepo>), String> {
    let cfg = match config::load(path) {
        Ok(Some(c)) => c,
        Ok(None) => return Err(format!("not configured: no config at {}", path.display())),
        Err(e) => return Err(format!("not configured: {e}")),
    };
    if let Some(p) = cfg.problem() {
        return Err(format!("not configured: {p}"));
    }
    let repo = sql_repo(&cfg).map_err(|e| format!("not configured: {e}"))?;
    Ok((cfg, repo))
}

/// The SQL Server repository on dl-agent's TDS stack (old-TLS / unencrypted ladder included).
fn sql_repo(cfg: &LocalConfig) -> Result<Arc<dyn FullRepo>, Error> {
    let connector = TdsConnector::parse(&cfg.sql.conn_string())?;
    let pool = Pool::new(NAME, connector, PoolOptions::default());
    Ok(Arc::new(SqlRepo::new(Arc::new(pool))))
}

/// Stays degraded until the config is usable; `None` when stopped.
fn wait_for_config(ctx: &ModuleContext, path: &Path) -> Option<(LocalConfig, Arc<dyn FullRepo>)> {
    let mut detail = String::new();
    let mut next_check = Instant::now();
    loop {
        if Instant::now() >= next_check {
            next_check = Instant::now() + RECHECK;
            match usable(path) {
                Ok(found) => return Some(found),
                Err(now) => {
                    if now != detail {
                        log::warn(
                            "hotellink module waiting for config",
                            &[("detail", V::S(&now))],
                        );
                        ctx.set_degraded(&now);
                        detail = now;
                    }
                }
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

/// `X-Agent-Version`.
pub fn agent_version_header() -> String {
    format!("dl-agent-{}", dl_agent::agent_version())
}

fn hub_client(cfg: &LocalConfig) -> HubClient {
    let hub = HubClient::new(
        &cfg.hub_url,
        &cfg.hotel_id,
        &cfg.token,
        &agent_version_header(),
        &cfg.instance_id,
    );
    let mut fallbacks = cfg.hub_url_fallbacks.clone();
    fallbacks.push(DEFAULT_HUB_FALLBACK.to_owned());
    hub.set_fallbacks(&fallbacks);
    hub
}

/// Opens the module's queue and imports the Go agent's unfinished jobs once.
fn open_queue(state_dir: &Path, legacy_dir: Option<&Path>) -> Result<FileQueue, Error> {
    std::fs::create_dir_all(state_dir).map_err(|e| Error::Io("create hotellink state dir", e))?;
    let queue = FileQueue::open(&state_dir.join(QUEUE_FILE))
        .map_err(|e| Error::Module(format!("open hotellink queue: {}", e.message())))?;
    if let Some(dir) = legacy_dir {
        let legacy = dir.join(QUEUE_FILE);
        let n = queue
            .import_legacy(&legacy, &state_dir.join(LEGACY_MARKER))
            .map_err(|e| Error::Module(format!("import the Go agent's queue: {}", e.message())))?;
        if n > 0 {
            log::info(
                "imported the Go agent's unfinished jobs",
                &[("entries", V::I(n as i64))],
            );
        }
    }
    Ok(queue)
}

fn spawn(r: &Arc<Runner>) -> Result<JoinHandle<()>, Error> {
    let me = r.clone();
    std::thread::Builder::new()
        .name("hl-runner".into())
        .spawn(move || me.run())
        .map_err(|e| Error::Io("spawn hotellink runner", e))
}

/// Runs the loops until `ctx.stop()`.
pub(crate) fn run_configured(
    ctx: &ModuleContext,
    cfg: &LocalConfig,
    config_path: &Path,
    w: Wiring,
) -> Result<(), Error> {
    let state_dir = ctx.data_dir().join(NAME);
    let legacy_dir = config_path.parent().filter(|d| !d.as_os_str().is_empty());
    let queue = open_queue(&state_dir, legacy_dir)?;
    let parts = Parts {
        hub: hub_client(cfg),
        repo: w.repo,
        queue,
        state_dir,
        legacy_policy_dir: legacy_dir.map(Path::to_path_buf),
    };
    let runner = Arc::new(Runner::with(
        parts,
        w.timings,
        crate::reconcile::system_clock(),
    ));
    log::info(
        "hotellink module started",
        &[
            ("hotelId", V::S(&cfg.hotel_id)),
            ("hubUrl", V::S(&cfg.hub_url)),
        ],
    );
    let handle = spawn(&runner)?;
    let result = supervise(ctx, &runner, &handle);
    runner.stop();
    finish(handle, w.grace);
    result
}

/// The module thread's loop: commands, metrics, state and reports.
fn supervise(
    ctx: &ModuleContext,
    runner: &Arc<Runner>,
    handle: &JoinHandle<()>,
) -> Result<(), Error> {
    let started = Instant::now();
    let mut limiter = Limiter::new(REPORT_WINDOW);
    let mut superseded_reported = false;
    let mut detail = "starting".to_owned();
    loop {
        for (k, v) in health::metrics(runner).iter() {
            ctx.set_metric(k, v.clone());
        }
        for ev in runner.stats().take_events() {
            if let Some(held) = limiter.admit(ev.code, Instant::now()) {
                ctx.report(
                    health::severity(ev.code),
                    ev.code,
                    &ev.message,
                    health::report_context(held),
                );
            }
        }
        let signals = Signals::read(runner);
        match health::verdict(&signals, started.elapsed()) {
            Verdict::Starting => {}
            Verdict::Running(d) => {
                ctx.set_running(&d);
                detail = d;
            }
            Verdict::Degraded(d) => {
                ctx.set_degraded(&d);
                detail = d;
            }
        }
        if signals.superseded && !superseded_reported {
            superseded_reported = true;
            log::error("hotellink stopped: another install took over", &[]);
            ctx.report(
                Severity::Critical,
                "hl_auth_failed",
                health::SUPERSEDED,
                Map::new(),
            );
        }
        while let Some(cmd) = ctx.next_command() {
            answer(ctx, &cmd, Some(runner), &detail);
        }
        // The runner ends by itself only when superseded (it then waits for the stop here);
        // anything else is a crash the supervisor restarts.
        if handle.is_finished()
            && !signals.superseded
            && !runner
                .stats()
                .superseded
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            let msg = "hotellink runner thread ended unexpectedly".to_owned();
            log::error(&msg, &[]);
            return Err(Error::Module(msg));
        }
        if ctx.stop().wait_timeout(TICK) {
            return Ok(());
        }
    }
}

/// Waits up to `grace` for the runner; one still inside a call is left to end on its own.
fn finish(handle: JoinHandle<()>, grace: Duration) {
    let deadline = Instant::now() + grace;
    while !handle.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if !handle.is_finished() {
        log::warn(
            "hotellink runner still inside a hub or SQL call; it ends on its own, unfinished jobs stay queued",
            &[],
        );
        return;
    }
    if handle.join().is_err() {
        log::error("hotellink runner panicked while stopping", &[]);
    }
    log::info("hotellink module stopped", &[]);
}

/// Answers `hl.status` and `hl.sync-mapping` (also with the `hotellink.` prefix).
fn answer(ctx: &ModuleContext, cmd: &Command, runner: Option<&Arc<Runner>>, detail: &str) {
    let kind = cmd
        .kind
        .strip_prefix("hl.")
        .or_else(|| cmd.kind.strip_prefix("hotellink."))
        .unwrap_or(&cmd.kind);
    match (kind, runner) {
        ("status", _) => {
            let mut m = runner.map(|r| health::metrics(r)).unwrap_or_default();
            m.insert("configured", runner.is_some());
            m.insert("detail", detail);
            ctx.complete(&cmd.id, true, &dl_json::to_string(&Value::Object(m)));
        }
        ("sync-mapping", Some(r)) => {
            let (r, c, id) = (r.clone(), ctx.clone(), cmd.id.clone());
            let spawned = std::thread::Builder::new()
                .name("hl-sync-mapping".into())
                .spawn(move || match r.sync_mapping() {
                    Ok(()) => c.complete(&id, true, "mapping pushed"),
                    Err(e) => {
                        c.complete(&id, false, &format!("mapping push failed: {}", e.message()))
                    }
                });
            if let Err(e) = spawned {
                ctx.complete(&cmd.id, false, &format!("spawn: {e}"));
            }
        }
        ("sync-mapping", None) => ctx.complete(&cmd.id, false, detail),
        _ => ctx.complete(
            &cmd.id,
            false,
            &format!("unknown hotellink command {:?}", cmd.kind),
        ),
    }
}
