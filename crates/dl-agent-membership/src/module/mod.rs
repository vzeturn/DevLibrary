//! The `membership` module: waits for `config.dpapi`, then runs the sync thread (heartbeat,
//! sync-config, job count) and the scan thread (availability, rooms, folio observations)
//! and reports their health, metrics and problems to dl-agent.

pub mod scan;
pub mod state;

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dl_agent::Error;
use dl_agent::command::Command;
use dl_agent::supervisor::{Factory, Module, ModuleContext};
use dl_json::{Map, Value};
use dl_winsvc::StopSignal;

use crate::config::{AgentConfig, ConfigStore, Dpapi, Sealer};
use crate::hub::gate::TokenGate;
use crate::hub::{HubApi, HubClient};
use crate::identity::{IdentityProbe, Snapshot, Tracker};
use crate::log::{self, V};
use crate::settings::Settings;
use crate::smile::{SmileError, SmileHealth, TdsSmile};
use crate::sync::{SyncSettings, SyncWorker};
use scan::{ScanDeps, ScanLoop};
use state::{RateLimit, Shared};

/// The module name (the `modules.membership` section).
pub const NAME: &str = "membership";
/// What the agent calls itself to the hub (heartbeat `AgentVersion`, enroll, User-Agent).
pub const AGENT_VERSION: &str = concat!("dl-agent/", env!("CARGO_PKG_VERSION"));
/// `TokenRejectedRetryMinutes`.
pub const TOKEN_RETRY: Duration = Duration::from_secs(15 * 60);

const TICK: Duration = Duration::from_secs(1);
const RECHECK: Duration = Duration::from_secs(30);
const WORKER_STOP_WAIT: Duration = Duration::from_secs(5);

/// The module. Holds the sealer of `config.dpapi` (DPAPI; tests pass another).
pub struct MembershipModule {
    sealer: Arc<dyn Sealer>,
}

impl std::fmt::Debug for MembershipModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MembershipModule").finish_non_exhaustive()
    }
}

impl Default for MembershipModule {
    fn default() -> MembershipModule {
        MembershipModule::with_sealer(Arc::new(Dpapi))
    }
}

impl MembershipModule {
    /// A module that opens `config.dpapi` with `sealer`.
    pub fn with_sealer(sealer: Arc<dyn Sealer>) -> MembershipModule {
        MembershipModule { sealer }
    }
}

/// The factory to register with `App::module`.
pub fn membership_module() -> Factory {
    Arc::new(|| Box::new(MembershipModule::default()) as Box<dyn Module>)
}

impl Module for MembershipModule {
    fn name(&self) -> &str {
        NAME
    }

    fn enabled_by_default(&self) -> bool {
        false
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error> {
        let settings = Settings::from_config(ctx.config(), ctx.data_dir());
        if !settings.read_only {
            let why =
                "read_only is false, but this build has no writers yet (M2): set read_only to true";
            log::error(why, &[]);
            ctx.set_degraded(why);
            idle_until_stopped(ctx, why);
            return Ok(());
        }
        let store = ConfigStore::new(settings.program_data_dir.clone(), self.sealer.clone());
        let Some(cfg) = wait_for_config(ctx, &store) else {
            return Ok(());
        };
        run_enrolled(ctx, cfg, &settings)
    }
}

/// Answers commands until stopped.
fn idle_until_stopped(ctx: &ModuleContext, detail: &str) {
    loop {
        while let Some(cmd) = ctx.next_command() {
            answer(ctx, &cmd, None, detail);
        }
        if ctx.stop().wait_timeout(TICK) {
            return;
        }
    }
}

/// Loads `config.dpapi`, staying degraded until it exists and opens; `None` when stopped.
fn wait_for_config(ctx: &ModuleContext, store: &ConfigStore) -> Option<AgentConfig> {
    let mut detail = String::new();
    let mut next_check = Instant::now();
    loop {
        if Instant::now() >= next_check {
            next_check = Instant::now() + RECHECK;
            let now = match store.load() {
                Ok(Some(cfg)) => return Some(cfg),
                Ok(None) => format!(
                    "not enrolled: no {} in {}; run dl-agent membership-enroll",
                    crate::config::FILE_NAME,
                    store.dir().display()
                ),
                Err(e) => format!("{} unreadable: {e}", crate::config::FILE_NAME),
            };
            if now != detail {
                log::warn(
                    "membership module waiting for config",
                    &[("detail", V::S(&now))],
                );
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

/// Stands in for SMILE when the connection string is unusable: the heartbeat still goes
/// out (with `smileOk = false`) and the scan thread is not started.
struct NoSmile(SmileError);

impl SmileHealth for NoSmile {
    fn ping(&self) -> Result<(), SmileError> {
        Err(self.0.clone())
    }
}

impl IdentityProbe for NoSmile {
    fn read_identity(&self) -> Result<Snapshot, SmileError> {
        Err(self.0.clone())
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, Error> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map_err(|e| Error::Io("spawn membership worker", e))
}

/// Runs the sync and scan threads until `ctx.stop()`.
fn run_enrolled(ctx: &ModuleContext, cfg: AgentConfig, settings: &Settings) -> Result<(), Error> {
    let gate = Arc::new(TokenGate::new(TOKEN_RETRY));
    let hub: Arc<dyn HubApi> = Arc::new(
        HubClient::new(
            &cfg.hub_base_url,
            &cfg.agent_token,
            cfg.hotel_id,
            AGENT_VERSION,
            gate,
        )
        .map_err(|e| Error::Config(format!("membership hub address: {e}")))?,
    );
    let shared = Arc::new(Shared::default());
    let (health, probe, scan_src): (Arc<dyn SmileHealth>, Arc<dyn IdentityProbe>, _) = match (
        TdsSmile::new(&cfg.smile_connection_string),
        TdsSmile::new(&cfg.smile_connection_string),
    ) {
        (Ok(a), Ok(b)) => {
            let t = a.target();
            log::info(
                "SMILE target",
                &[
                    ("server", V::S(&t.data_source)),
                    ("database", V::S(&t.database)),
                ],
            );
            let a = Arc::new(a);
            (a.clone(), a, Some(Arc::new(b) as Arc<dyn scan::Sources>))
        }
        (Err(e), _) | (_, Err(e)) => {
            let s = Arc::new(NoSmile(e));
            (s.clone(), s, None)
        }
    };
    let tracker = Arc::new(Tracker::new(probe));
    log::info(
        "membership module enrolled",
        &[
            ("hotel_id", V::S(&cfg.hotel_id.to_string())),
            ("hub", V::S(cfg.hub_base_url.trim())),
            ("pull_jobs", V::B(settings.pull_jobs)),
            ("folio_scan", V::B(settings.folio.enabled)),
        ],
    );

    let stop = StopSignal::new();
    let mut sync = SyncWorker::new(
        hub.clone(),
        health,
        tracker.clone(),
        shared.clone(),
        SyncSettings {
            pull_jobs: settings.pull_jobs,
            max_jobs_per_pull: settings.max_jobs_per_pull,
            heartbeat: settings.heartbeat,
            capabilities: settings.capabilities.clone(),
        },
        AGENT_VERSION,
    );
    let s = stop.clone();
    let mut workers = vec![("sync", spawn("membership-sync", move || sync.run(&s))?)];
    if let Some(src) = scan_src {
        let deps = ScanDeps {
            src,
            hub,
            tracker: tracker.clone(),
            shared: shared.clone(),
        };
        let mut scans = ScanLoop::new(deps, settings, &cfg.hotel_id.to_string(), AGENT_VERSION);
        let s = stop.clone();
        match spawn("membership-scan", move || scans.run(&s)) {
            Ok(h) => workers.push(("scan", h)),
            Err(e) => {
                stop.stop();
                finish(workers);
                return Err(e);
            }
        }
    }
    let result = supervise(ctx, &shared, &tracker, &stop, &mut workers, settings, &cfg);
    stop.stop();
    finish(workers);
    result
}

/// The module thread's loop: commands, reports, metrics, health and worker crashes.
fn supervise(
    ctx: &ModuleContext,
    shared: &Shared,
    tracker: &Tracker,
    stop: &StopSignal,
    workers: &mut Vec<(&'static str, JoinHandle<()>)>,
    settings: &Settings,
    cfg: &AgentConfig,
) -> Result<(), Error> {
    let mut limit = RateLimit::new(settings.report_interval);
    let hotel = cfg.hotel_id.to_string();
    loop {
        shared.set_pinned_identity(tracker.pinned());
        let h = shared.health();
        let detail = match h.problem() {
            Some(p) => {
                ctx.set_degraded(&p);
                p
            }
            None if h.heartbeat_ok => {
                let d = format!("hotel {hotel}: heartbeat ok; scanning read-only");
                ctx.set_running(&d);
                d
            }
            None => "waiting for the first heartbeat".to_owned(),
        };
        let now = Instant::now();
        for r in shared.take_reports() {
            if limit.allow(r.code, now) {
                let mut m = Map::new();
                m.insert("hotel_id", hotel.as_str());
                for (k, v) in r.context {
                    m.insert(k, v);
                }
                ctx.report(r.severity, r.code, &r.message, m);
            }
        }
        for (k, v) in shared.metrics().iter() {
            ctx.set_metric(k, v.clone());
        }
        while let Some(cmd) = ctx.next_command() {
            answer(ctx, &cmd, Some(shared), &detail);
        }
        if !stop.is_stopped()
            && let Some(i) = workers.iter().position(|(_, h)| h.is_finished())
        {
            let (name, h) = workers.remove(i);
            let msg = match h.join() {
                Err(_) => format!("membership {name} thread panicked"),
                Ok(()) => format!("membership {name} thread ended unexpectedly"),
            };
            log::error(&msg, &[]);
            return Err(Error::Module(msg));
        }
        if ctx.stop().wait_timeout(TICK) {
            return Ok(());
        }
    }
}

/// Waits briefly for the workers; one inside a blocking call is left to finish it.
fn finish(workers: Vec<(&'static str, JoinHandle<()>)>) {
    let deadline = Instant::now() + WORKER_STOP_WAIT;
    while workers.iter().any(|(_, h)| !h.is_finished()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    for (name, h) in workers {
        if !h.is_finished() {
            log::warn(
                "membership worker still finishing an in-flight call; it ends on its own",
                &[("worker", V::S(name))],
            );
            continue;
        }
        if h.join().is_err() {
            log::error(
                "membership worker panicked while stopping",
                &[("worker", V::S(name))],
            );
        }
    }
    log::info("membership module stopped", &[]);
}

/// Answers `membership.status`.
fn answer(ctx: &ModuleContext, cmd: &Command, shared: Option<&Shared>, detail: &str) {
    let kind = cmd.kind.strip_prefix("membership.").unwrap_or(&cmd.kind);
    if kind == "status" {
        let mut m = shared.map(Shared::metrics).unwrap_or_default();
        m.insert("enrolled", shared.is_some());
        m.insert("detail", detail);
        ctx.complete(&cmd.id, true, &dl_json::to_string(&Value::Object(m)));
    } else {
        ctx.complete(
            &cmd.id,
            false,
            &format!("unknown membership command {:?}", cmd.kind),
        );
    }
}
