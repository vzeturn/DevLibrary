//! The dl-agent module: runs the PMS site agent (enroll, token, stream, the executor over
//! the SMILE databases and its op journal) under [`dl_agent::supervisor::Module`]. The
//! SMILE databases come from the core's `sql.json` first (auth mode, sealed password, TLS
//! rung), then from `databases.json` in the agent's state directory.
//!
//! The module is opt-in (`modules.pms-site.enabled = true`). Without an identity and
//! without an enrollment code it stays degraded ("not enrolled") and looks again every
//! 30 s; it does not fail and restart. The agent runs on a worker thread; the module
//! thread answers commands, publishes metrics and health, and on `stop` cancels the
//! worker, which gives in-flight ops up to 5 s, closes the stream with 1000 and ends.
//!
//! A terminal stop from the cloud (superseded, revoked, contract unsupported, refused
//! enrollment) parks the module degraded with the next step; it runs again once the
//! config file or `identity.json` changes, or on `pms.reconnect`.

pub mod commands;
pub mod prepare;
pub mod selftest;
pub mod settings;

use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dl_agent::Error;
use dl_agent::supervisor::{Factory, Module, ModuleContext};
use dl_json::Map;

use crate::agent::{self, AgentError, Deps};
use crate::config::Config;
use crate::conn::ConnError;
use crate::ctx::Ctx;
use crate::db;
use crate::exec::Executor;
use crate::log::{self, Logger};
use crate::telemetry::{self, Health, Level, Telemetry};
use crate::wire;
use commands::{Answer, CtxReporter, answer, publish_metrics};
use prepare::{Fingerprint, Prepared, fingerprint, prepare};
use settings::Settings;

/// The module name: config section, report module and command target.
pub const NAME: &str = "pms-site";
/// The stream counts as down once it has been reconnecting this long (one back-off cycle).
pub const DOWN_AFTER: Duration = wire::BACKOFF_MAX;

/// How often the module thread answers commands and publishes metrics and health.
const TICK: Duration = Duration::from_millis(250);
/// How often a waiting or parked module looks at the config again.
const RECHECK: Duration = Duration::from_secs(30);
/// How long a stop waits for the worker (in-flight grace 5 s + close handshake 2 s),
/// below the supervisor's 10 s.
const WORKER_STOP_WAIT: Duration = Duration::from_secs(8);

/// Parts a test or an embedding host can replace; `None` takes the production default.
#[derive(Clone, Default)]
pub struct Hooks {
    /// The op executor (default: SQL over the SMILE databases); it runs behind the journal.
    pub exec: Option<Arc<dyn Executor>>,
    /// The HTTP client for enroll, token and stream (default: OS TLS, env proxy).
    pub http: Option<Arc<dl_http::Client>>,
    /// The reconnect back-off (default: 1, 2, 4 … 30 s with jitter).
    pub backoff: Option<fn(u32) -> Duration>,
}

/// The SMILE PMS site module.
#[derive(Default)]
pub struct PmsSiteModule {
    hooks: Hooks,
}

/// The factory to register with `App::module`.
pub fn pms_site_module() -> Factory {
    pms_site_module_with(Hooks::default())
}

/// [`pms_site_module`] with replaced parts.
pub fn pms_site_module_with(hooks: Hooks) -> Factory {
    Arc::new(move || {
        Box::new(PmsSiteModule {
            hooks: hooks.clone(),
        }) as Box<dyn Module>
    })
}

/// The `hello` agent version: `dl-agent-<version>`, at most 32 characters.
pub fn hello_version() -> String {
    format!("dl-agent-{}", dl_agent::agent_version())
        .chars()
        .take(32)
        .collect()
}

/// How a cycle ended.
enum End {
    /// `ctx.stop()`.
    Stopped,
    /// `pms.reconnect`.
    Restart,
    /// A terminal stop: wait for a change.
    Park(String),
}

/// What an idle module watches for.
enum Watch {
    /// The prepare outcome (not enrolled, config invalid).
    Prepare,
    /// The config and identity files (after a terminal stop).
    Files(Box<Config>, Fingerprint),
}

impl Module for PmsSiteModule {
    fn name(&self) -> &str {
        NAME
    }

    fn enabled_by_default(&self) -> bool {
        false
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error> {
        let settings = Settings::from_config(ctx.config());
        let reporter = Arc::new(CtxReporter::new(ctx.clone()));
        let tel = Arc::new(Telemetry::new(Some(reporter)));
        loop {
            let (detail, watch) = match prepare(&settings) {
                Prepared::Ready(cfg) => match self.cycle(ctx, &cfg, &tel)? {
                    End::Stopped => return Ok(()),
                    End::Restart => continue,
                    End::Park(detail) => {
                        let fp = fingerprint(&cfg);
                        (detail, Watch::Files(cfg, fp))
                    }
                },
                Prepared::Waiting { detail, invalid } => {
                    if invalid {
                        let mut m = Map::new();
                        m.insert("module", NAME);
                        tel.report(Level::Error, telemetry::CODE_CONFIG_INVALID, &detail, m);
                    }
                    (detail, Watch::Prepare)
                }
            };
            if !idle(ctx, &settings, &tel, &detail, &watch) {
                return Ok(());
            }
        }
    }
}

impl PmsSiteModule {
    /// Runs the agent on a worker thread until stop, `pms.reconnect` or its end.
    fn cycle(&self, ctx: &ModuleContext, cfg: &Config, tel: &Arc<Telemetry>) -> Result<End, Error> {
        let log = Logger::agent(log::parse_level(&cfg.log_level));
        let life = Ctx::background();
        let deps = Deps {
            log: Some(log.clone()),
            http: self.hooks.http.clone(),
            exec: self.hooks.exec.clone(),
            sql: db::shared_pools(ctx),
            backoff: self.hooks.backoff,
            telemetry: Some(tel.clone()),
            version: hello_version(),
            ..Deps::default()
        };
        let (wctx, wcfg) = (life.clone(), cfg.clone());
        let worker = thread::Builder::new()
            .name("pms-site-agent".into())
            .spawn(move || agent::run(&wctx, &wcfg, deps))
            .map_err(|e| Error::Io("spawn pms-site worker", e))?;
        let connecting = format!("connecting to {}", cfg.cloud_url);
        let running = format!("connected to {}", cfg.cloud_url);
        let end = loop {
            let detail = match tel.health(DOWN_AFTER) {
                Health::Up => {
                    ctx.set_running(&running);
                    running.clone()
                }
                Health::Down { for_secs, reason } => {
                    let d = format!("reconnecting for {for_secs} s: {reason}");
                    ctx.set_degraded(&d);
                    d
                }
                Health::Pending => connecting.clone(),
            };
            publish_metrics(ctx, tel);
            let mut restart = false;
            while let Some(cmd) = ctx.next_command() {
                restart |= answer(ctx, &cmd, tel, &detail) == Answer::Reconnect;
            }
            if restart {
                break Some(End::Restart);
            }
            if worker.is_finished() {
                break None;
            }
            if ctx.stop().wait_timeout(TICK) {
                break Some(End::Stopped);
            }
        };
        life.cancel();
        let Some(end) = end else {
            return outcome(tel, worker.join());
        };
        finish(worker, &log);
        publish_metrics(ctx, tel);
        Ok(end)
    }
}

/// Waits briefly for the worker after a cancel; one still inside a blocking call is left to
/// finish it.
fn finish(worker: JoinHandle<Result<(), AgentError>>, log: &Logger) {
    let deadline = Instant::now() + WORKER_STOP_WAIT;
    while !worker.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    if !worker.is_finished() {
        log.warn("agent.module.worker_still_finishing", &[]);
        return;
    }
    match worker.join() {
        Err(_) => log.error("agent.module.worker_panicked", &[]),
        Ok(Err(e)) => log.warn(
            "agent.module.worker_ended",
            crate::attrs!["reason" => e.to_string()],
        ),
        Ok(Ok(())) => log.info("agent.module.stopped", &[]),
    }
}

/// What a worker that ended on its own means for the module.
fn outcome(tel: &Telemetry, res: thread::Result<Result<(), AgentError>>) -> Result<End, Error> {
    let err = match res {
        Err(_) => return Err(Error::Module("pms-site agent thread panicked".into())),
        Ok(Ok(())) => return Err(Error::Module("pms-site agent ended unexpectedly".into())),
        Ok(Err(e)) => e,
    };
    let (code, reason, next) = match &err {
        AgentError::Stopped(c) => {
            let reason = agent::stop_reason(&err).unwrap_or("stopped");
            let code = if *c == ConnError::Revoked {
                telemetry::CODE_AUTH_FAILED
            } else {
                telemetry::CODE_STREAM_FAILED
            };
            (code, reason, agent::next_step(reason))
        }
        AgentError::EnrollRefused => (
            telemetry::CODE_AUTH_FAILED,
            "enroll_refused",
            "put a new code in enrollment_code or run dl-agent pms-enroll --force with it",
        ),
        AgentError::NotEnrolled => {
            return Ok(End::Park(format!("not enrolled: {err}")));
        }
        AgentError::Other(m) => return Err(Error::Module(m.clone())),
    };
    let detail = format!("{err}; {next}");
    let mut m = Map::new();
    m.insert("stop_reason", reason);
    tel.report(Level::Critical, code, &detail, m);
    Ok(End::Park(detail))
}

/// Stays degraded with `detail`, answering commands, until stop (`false`), a reconnect
/// command or a change worth a new attempt (`true`).
fn idle(
    ctx: &ModuleContext,
    settings: &Settings,
    tel: &Telemetry,
    detail: &str,
    watch: &Watch,
) -> bool {
    ctx.set_degraded(detail);
    let lg = Logger::agent(log::Level::Info);
    lg.warn("agent.module.waiting", crate::attrs!["detail" => detail]);
    let mut next = Instant::now() + RECHECK;
    loop {
        publish_metrics(ctx, tel);
        while let Some(cmd) = ctx.next_command() {
            if answer(ctx, &cmd, tel, detail) == Answer::Reconnect {
                return true;
            }
        }
        if Instant::now() >= next {
            next = Instant::now() + RECHECK;
            let changed = match watch {
                Watch::Prepare => prepare(settings).detail() != Some(detail),
                Watch::Files(cfg, fp) => fingerprint(cfg) != *fp,
            };
            if changed {
                return true;
            }
        }
        if ctx.stop().wait_timeout(TICK) {
            return false;
        }
    }
}
