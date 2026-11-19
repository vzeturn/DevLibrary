//! The `membership` module: waits for `config.dpapi`, then runs the sync thread (heartbeat,
//! sync-config, job count) and the scan thread (availability, rooms, folio observations)
//! and reports their health, metrics and problems to dl-agent.

pub mod scan;
pub mod selftest;
pub mod state;

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dl_agent::Error;
use dl_agent::command::Command;
use dl_agent::supervisor::{Factory, Module, ModuleContext};
use dl_json::{Map, Value};
use dl_winsvc::StopSignal;

use crate::checkout::CheckoutScanner;
use crate::config::{AgentConfig, ConfigStore, Dpapi, Sealer};
use crate::earn::pairs::{EarnSettings, PairSource};
use crate::earn::{EarnEnv, ModuleEnv};
use crate::hub::earn::EarnHub;
use crate::hub::gate::TokenGate;
use crate::hub::{HubApi, HubClient};
use crate::identity::{IdentityProbe, Snapshot, Tracker};
use crate::jobs::dispatch::{DispatchSettings, Dispatcher};
use crate::jobs::gate::WriteGate;
use crate::log::{self, V};
use crate::settings::Settings;
use crate::smile::{SmileError, SmileHealth, TdsSmile};
use crate::sync::{SyncSettings, SyncWorker};
use crate::writer::folio::{FolioWriter, WriteOptions};
use crate::writer::smile_writer::{EarnPath, SmileWriter};
use scan::{CheckoutPass, ScanDeps, ScanLoop};
use state::{RateLimit, Shared};

/// The module name (the `modules.membership` section).
pub const NAME: &str = "membership";
/// What the agent calls itself to the hub (heartbeat `AgentVersion`, enroll, User-Agent):
/// the released dl-agent version (`DL_AGENT_VERSION` at build), not this crate's own.
pub fn agent_version() -> &'static str {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| format!("dl-agent/{}", dl_agent::agent_version()))
}
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
        if let Some(problem) = &settings.write.extra_bed_mode_problem {
            log::warn(problem, &[]);
        }
        if !settings.read_only && !settings.pull_jobs {
            log::warn(
                "read_only is false but pull_jobs is false: no job is pulled, nothing is written",
                &[],
            );
        }
        let store = ConfigStore::new(settings.program_data_dir.clone(), self.sealer.clone());
        let Some(cfg) = wait_for_config(ctx, &store) else {
            return Ok(());
        };
        run_enrolled(ctx, cfg, &settings)
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

/// Health, identity probe, scan sources and the writer's connection (the last two absent
/// when the connection string is invalid).
type SmileHandles = (
    Arc<dyn SmileHealth>,
    Arc<dyn IdentityProbe>,
    Option<Arc<dyn scan::Sources>>,
    Option<Arc<TdsSmile>>,
);

/// Runs the sync and scan threads until `ctx.stop()`.
fn run_enrolled(ctx: &ModuleContext, cfg: AgentConfig, settings: &Settings) -> Result<(), Error> {
    let gate = Arc::new(TokenGate::new(TOKEN_RETRY));
    let hub_client = Arc::new(
        HubClient::new(
            &cfg.hub_base_url,
            &cfg.agent_token,
            cfg.hotel_id,
            agent_version(),
            gate,
        )
        .map_err(|e| Error::Config(format!("membership hub address: {e}")))?,
    );
    let hub: Arc<dyn HubApi> = hub_client.clone();
    let shared = Arc::new(Shared::default());
    let (health, probe, scan_src, write_smile): SmileHandles = match (
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
            (
                a.clone(),
                a.clone(),
                Some(Arc::new(b) as Arc<dyn scan::Sources>),
                Some(a),
            )
        }
        (Err(e), _) | (_, Err(e)) => {
            let s = Arc::new(NoSmile(e));
            (s.clone(), s, None, None)
        }
    };
    let tracker = Arc::new(Tracker::new(probe));
    log::info(
        "membership module enrolled",
        &[
            ("hotel_id", V::S(&cfg.hotel_id.to_string())),
            ("hub", V::S(cfg.hub_base_url.trim())),
            ("pull_jobs", V::B(settings.pull_jobs)),
            ("read_only", V::B(settings.read_only)),
            ("folio_scan", V::B(settings.folio.enabled)),
            ("checkout_scan", V::B(settings.checkout.enabled)),
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
        agent_version(),
    );
    let env: Arc<dyn EarnEnv> = Arc::new(ModuleEnv {
        tracker: tracker.clone(),
        shared: shared.clone(),
    });
    let earn_hub: Arc<dyn EarnHub> = hub_client.clone();
    if let Some(d) = dispatcher(
        settings,
        &cfg,
        write_smile,
        &hub,
        &tracker,
        (&earn_hub, &env),
    ) {
        sync = sync.with_dispatcher(d);
    }
    let s = stop.clone();
    let mut workers = vec![("sync", spawn("membership-sync", move || sync.run(&s))?)];
    if let Some(src) = scan_src {
        let deps = ScanDeps {
            src,
            hub,
            tracker: tracker.clone(),
            shared: shared.clone(),
        };
        let mut scans = ScanLoop::new(deps, settings, &cfg.hotel_id.to_string(), agent_version());
        if let Some(pass) = checkout_pass(settings, &cfg, &shared, &earn_hub, &env) {
            scans = scans.with_checkout(pass);
        }
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

/// The job dispatcher when writes are on (`read_only: false`) and SMILE is reachable by
/// connection string; `None` keeps the M1 behaviour (jobs counted, never acked).
fn dispatcher(
    settings: &Settings,
    cfg: &AgentConfig,
    smile: Option<Arc<TdsSmile>>,
    hub: &Arc<dyn HubApi>,
    tracker: &Arc<Tracker>,
    (earn_hub, env): (&Arc<dyn EarnHub>, &Arc<dyn EarnEnv>),
) -> Option<Arc<Dispatcher>> {
    if settings.read_only {
        return None;
    }
    let Some(smile) = smile else {
        log::error(
            "read_only is false but the SMILE connection string is invalid: jobs are not run",
            &[],
        );
        return None;
    };
    let w = &settings.write;
    let folio = FolioWriter::new(WriteOptions {
        write_lock_timeout_ms: w.write_lock_timeout_ms,
        default_payment_code: w.default_payment_code.clone(),
        default_breakfast_code: w.default_breakfast_code.clone(),
        extra_bed_mode: w.extra_bed_mode.clone(),
        extra_bed_special_code: settings.smile.extra_bed_special_code.clone(),
    });
    let gate = Arc::new(WriteGate::new(hub.clone(), tracker.clone()));
    log::warn(
        "SMILE writes are ON: hub jobs are executed and acked (stop the .NET agent service)",
        &[("extra_bed_mode", V::S(&w.extra_bed_mode))],
    );
    Some(Arc::new(Dispatcher::new(
        Arc::new(SmileWriter::new(smile, folio).with_earn(EarnPath {
            pairs: PairSource::new(earn_settings(settings)),
            hub: earn_hub.clone(),
            env: env.clone(),
        })),
        Some(gate),
        DispatchSettings {
            origin_code: cfg.origin_code.clone(),
            member_discount_code: settings.smile.member_discount_code.clone(),
            extra_bed_mode: w.extra_bed_mode.clone(),
        },
    )))
}

/// The checkout scan when `checkout_scan_enabled` (default: writes on), on a SMILE
/// session of its own.
fn checkout_pass(
    settings: &Settings,
    cfg: &AgentConfig,
    shared: &Arc<Shared>,
    hub: &Arc<dyn EarnHub>,
    env: &Arc<dyn EarnEnv>,
) -> Option<CheckoutPass> {
    if !settings.checkout.enabled {
        return None;
    }
    let smile = TdsSmile::new(&cfg.smile_connection_string).ok()?;
    Some(CheckoutPass {
        scanner: CheckoutScanner::new(
            settings.checkout.clone(),
            &settings.smile.guest_phone_column,
            PairSource::new(earn_settings(settings)),
            shared.walk_in.clone(),
            agent_version(),
        ),
        db: Arc::new(smile),
        hub: hub.clone(),
        env: env.clone(),
    })
}

/// `EarnOptions` plus the configured extra-bed code, for every earn path.
pub(crate) fn earn_settings(settings: &Settings) -> EarnSettings {
    EarnSettings {
        charge_codes: settings.smile.earn_charge_codes.clone(),
        member_discount_code: settings.smile.member_discount_code.clone(),
        extra_bed_special_code: settings.smile.extra_bed_special_code.clone(),
    }
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
