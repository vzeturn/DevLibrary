//! The `speaker` module: the Go speaker-agent's alerts hosted by dl-agent.
//!
//! dl-agent owns the process, the Windows service and self-update. The module needs the
//! `pda-sync` enrolment on the same machine (see [`crate::auth`]): until
//! `dl-agent pda-enroll` has run it stays degraded and answers commands. Then it keeps the
//! order hub link ([`HubLink`]), refreshes the per-RVC sound modes every
//! `soundRefreshSeconds`, and plays through Windows SAPI ([`SystemOutput`]).
//!
//! It does **not** relay prints: `pda-sync` is the branch's print relay (ADR 0098) and
//! ADR 0026 allows one relay per branch.

pub mod commands;
pub mod settings;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dl_agent::Error;
use dl_agent::report::Severity;
use dl_agent::supervisor::{Factory, Module, ModuleContext};
use dl_json::Map;

use crate::auth::{EnrolledTokens, Enrolment, api_base, hub_url, pda_sync_dir};
use crate::hub::{HubLink, HubSettings, LinkState, TokenSource};
use crate::mode::Cache;
use crate::output::{Announcer, SystemOutput};
use settings::Settings;

/// The module name (the `modules.speaker` section, `<data>/modules/speaker`).
pub const NAME: &str = "speaker";

/// What the module says while `pda-sync` is not enrolled.
pub const NOT_ENROLLED: &str = "pda-sync is not enrolled on this machine: run dl-agent pda-enroll --code <code> --url <PDA cloud url> first";

const TICK: Duration = Duration::from_secs(1);
const RECHECK: Duration = Duration::from_secs(30);
const PUBLISH: Duration = Duration::from_secs(5);

/// The module.
#[derive(Debug, Default)]
pub struct SpeakerModule;

/// The factory to register with `App::module`.
pub fn speaker_module() -> Factory {
    Arc::new(|| Box::new(SpeakerModule) as Box<dyn Module>)
}

/// What the commands and the publisher read while the module runs.
#[derive(Debug)]
pub struct Runtime {
    /// The bound settings.
    pub settings: Settings,
    /// The per-RVC modes.
    pub cache: Arc<Cache>,
    /// The output queue (`None` before enrolment).
    pub announcer: Option<Arc<Announcer>>,
    /// The hub link (`None` before enrolment).
    pub link: Option<Arc<HubLink>>,
    /// The last sound-mode fetch failures.
    pub fetch_failures: Mutex<Vec<(i64, String)>>,
}

impl Module for SpeakerModule {
    fn name(&self) -> &str {
        NAME
    }

    fn enabled_by_default(&self) -> bool {
        false
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error> {
        let settings = settings::parse(ctx.config()).map_err(Error::Config)?;
        let Some((identity_url, tokens)) = wait_for_enrolment(ctx, &settings)? else {
            return Ok(());
        };
        run_enrolled(ctx, settings, &identity_url, Arc::from(tokens))
    }
}

/// Waits for the `pda-sync` enrolment, staying degraded; `None` when stopped.
fn wait_for_enrolment(
    ctx: &ModuleContext,
    settings: &Settings,
) -> Result<Option<(String, Box<EnrolledTokens>)>, Error> {
    let rt = Runtime {
        settings: settings.clone(),
        cache: Arc::new(Cache::new()),
        announcer: None,
        link: None,
        fetch_failures: Mutex::new(Vec::new()),
    };
    let pda_dir = pda_sync_dir(ctx.data_dir());
    let mut detail = String::new();
    let mut next_check = Instant::now();
    loop {
        if Instant::now() >= next_check {
            next_check = Instant::now() + RECHECK;
            let now = match EnrolledTokens::load(&pda_dir) {
                Ok(Enrolment::Ready(identity, tokens)) => {
                    return Ok(Some((identity.cloud_url, tokens)));
                }
                Ok(Enrolment::NotEnrolled) => NOT_ENROLLED.to_owned(),
                Err(e) => e,
            };
            if now != detail {
                ctx.set_degraded(&now);
                detail = now;
            }
        }
        commands::answer_all(ctx, &rt);
        if ctx.stop().wait_timeout(TICK) {
            return Ok(None);
        }
    }
}

fn run_enrolled(
    ctx: &ModuleContext,
    settings: Settings,
    cloud_url: &str,
    tokens: Arc<EnrolledTokens>,
) -> Result<(), Error> {
    let cache = Arc::new(Cache::new());
    let output = Arc::new(SystemOutput::new(ctx.data_dir().join("tones")));
    let announcer = Announcer::start(cache.clone(), output)
        .map_err(|e| Error::Io("spawn speaker output", e))?;
    let link = Arc::new(HubLink::new(
        HubSettings::new(hub_url(cloud_url), settings.rvcs.clone()),
        tokens.clone() as Arc<dyn TokenSource>,
        announcer.clone(),
    ));
    let rt = Arc::new(Runtime {
        settings,
        cache,
        announcer: Some(announcer.clone()),
        link: Some(link.clone()),
        fetch_failures: Mutex::new(Vec::new()),
    });
    let refresher = {
        let (rt, stop, base) = (rt.clone(), ctx.stop().clone(), api_base(cloud_url));
        std::thread::Builder::new()
            .name("speaker-modes".into())
            .spawn(move || {
                let http = dl_http::Client::with_options(crate::hub::http_options());
                loop {
                    let failed =
                        crate::fetch::refresh(&http, &base, &*tokens, &rt.settings.rvcs, &rt.cache);
                    *rt.fetch_failures
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner) = failed;
                    if stop.wait_timeout(rt.settings.sound_refresh) {
                        break;
                    }
                }
            })
            .map_err(|e| Error::Io("spawn speaker sound modes", e))?
    };
    let hub = {
        let (link, stop) = (link.clone(), ctx.stop().clone());
        std::thread::Builder::new()
            .name("speaker-hub".into())
            .spawn(move || link.run(&stop))
            .map_err(|e| Error::Io("spawn speaker hub", e))?
    };
    ctx.set_running("connecting to the order hub");
    let mut last = LinkState::default();
    let mut reported_refusal = false;
    let mut next_publish = Instant::now();
    loop {
        if Instant::now() >= next_publish {
            next_publish = Instant::now() + PUBLISH;
            let now = link.state();
            publish(ctx, &rt, &now, &last, &mut reported_refusal);
            last = now;
        }
        commands::answer_all(ctx, &rt);
        if ctx.stop().wait_timeout(TICK) {
            break;
        }
    }
    let joined = [hub.join(), refresher.join()];
    announcer.stop();
    if joined.iter().any(Result::is_err) {
        return Err(Error::Module("a speaker thread panicked".into()));
    }
    Ok(())
}

/// Mirrors the link into the module state and metrics; reports a refused join once.
fn publish(
    ctx: &ModuleContext,
    rt: &Runtime,
    now: &LinkState,
    last: &LinkState,
    reported_refusal: &mut bool,
) {
    if let Some(a) = &rt.announcer {
        for (k, v) in a.stats().metrics() {
            ctx.set_metric(k, v);
        }
    }
    ctx.set_metric("rvcs_joined", now.joined.len() as u64);
    ctx.set_metric("connects", now.connects);
    let refused: Vec<i64> = now.refused.iter().map(|r| r.0).collect();
    if now.connected && refused.is_empty() {
        ctx.set_running(&format!("joined RVCs {:?}", now.joined));
    } else if now.connected {
        ctx.set_degraded(&format!(
            "joined RVCs {:?}; the cloud refused {refused:?}",
            now.joined
        ));
    } else if !refused.is_empty() {
        ctx.set_degraded(&format!(
            "the PDA cloud refused JoinRvc for RVCs {refused:?} ({}); the cloud must accept the enrolled agent for JoinRvc (dl-agent-speaker README, cloud change needed)",
            now.refused.first().map(|r| r.1.as_str()).unwrap_or("")
        ));
    } else if let Some(e) = &now.last_error {
        ctx.set_degraded(&format!("order hub not connected: {e}"));
    }
    if !refused.is_empty() && !*reported_refusal {
        *reported_refusal = true;
        let mut c = Map::new();
        c.insert(
            "rvcs",
            dl_json::Value::Array(refused.iter().map(|r| dl_json::Value::from(*r)).collect()),
        );
        if let Some((_, m)) = now.refused.first() {
            c.insert("error", m.as_str());
        }
        ctx.report(
            Severity::Warning,
            "SPEAKER_JOIN_REFUSED",
            "the PDA cloud refused JoinRvc for the enrolled agent",
            c,
        );
    }
    if refused.is_empty() && now != last {
        *reported_refusal = false;
    }
}
