//! The `pda-sync` module: the PdaSyncAgent runtime (capture, upload, heartbeat, cloud
//! commands, staff sync, print relay, site events) hosted by dl-agent.
//!
//! dl-agent owns the process, the Windows service, self-update and the diagnostics
//! bundle; the module owns its data folder `<data>/modules/pda-sync` (the C#
//! `%ProgramData%\MM\SyncAgent` layout: `agent.json`, `secrets`, `outbox.db`,
//! `capture.db`, `blobs.db`, `status.json`, `logs`). Until `dl-agent pda-enroll` wrote
//! `agent.json` the module stays degraded and answers commands; then it runs the loops
//! of the C# host and mirrors `status.json` into its dl-agent state, metrics and reports.

pub mod commands;
pub mod dry_run;
pub mod selftest;
pub mod settings;
pub mod tool;

use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_agent::Error;
use dl_agent::report::Severity;
use dl_agent::supervisor::{Factory, Module, ModuleContext};
use dl_json::Map;

use crate::config::health::{DEGRADED, HEALTHY, REENROLL_REQUIRED};
use crate::config::time::{SystemClock, local_offset_minutes};
use crate::config::{AgentIdentity, AgentStatus};
use crate::contracts::time::TimeZone;
use crate::hosting::HostContext;
use crate::hosting::host::{AgentServices, build_host, hooks};
use crate::reporting::agent_logging::{create_service_logger, new_run_id};
use crate::reporting::{
    CloudLinkPublisher, LinkUpdate, ModuleReports, ReportFn, Sink, SiteEventLogBridge, TeeSink,
};
use commands::Runtime;

/// The module name (the `modules.pda-sync` section, `<data>/modules/pda-sync`).
pub const NAME: &str = "pda-sync";

/// What the module says while the machine is not enrolled.
pub const NOT_ENROLLED: &str =
    "not enrolled: run dl-agent pda-enroll --code <code> --url <PDA cloud url>";

const TICK: Duration = Duration::from_secs(1);
const RECHECK: Duration = Duration::from_secs(30);
const PUBLISH: Duration = Duration::from_secs(15);

/// The module.
#[derive(Debug, Default)]
pub struct PdaSyncModule;

/// The factory to register with `App::module`.
pub fn pda_sync_module() -> Factory {
    Arc::new(|| Box::new(PdaSyncModule) as Box<dyn Module>)
}

impl Module for PdaSyncModule {
    fn name(&self) -> &str {
        NAME
    }

    fn enabled_by_default(&self) -> bool {
        false
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error> {
        let host = settings::host_context(ctx.config(), ctx.data_dir()).map_err(Error::Config)?;
        name_diagnostics(ctx, &host);
        let Some(identity) = wait_for_identity(ctx, &host)? else {
            return Ok(());
        };
        run_enrolled(ctx, host, identity)
    }
}

/// Names the module's files for the core diagnostics bundle, as the C# `diag` verb took
/// them (`DiagnosticBundle.Export`): the health record, the cloud's runtime settings and
/// the newest JSON-lines logs of `logs/` (service and tool runs; the core scrubs them
/// again). `agent.json`, `secrets/` and the SQLite queues stay out.
fn name_diagnostics(ctx: &ModuleContext, host: &HostContext) {
    ctx.add_diagnostic_file(host.paths.status_file());
    ctx.add_diagnostic_file(host.paths.runtime_config_file());
    ctx.add_diagnostic_folder(host.paths.logs_dir());
}

/// Loads `agent.json`, staying degraded until it exists; `None` when stopped.
fn wait_for_identity(
    ctx: &ModuleContext,
    host: &HostContext,
) -> Result<Option<AgentIdentity>, Error> {
    let rt = Runtime::waiting(host);
    let mut detail = String::new();
    let mut next_check = Instant::now();
    loop {
        if Instant::now() >= next_check {
            next_check = Instant::now() + RECHECK;
            let now = match AgentIdentity::try_load(&host.paths) {
                Ok(Some(id)) => return Ok(Some(id)),
                Ok(None) => NOT_ENROLLED.to_owned(),
                Err(e) => format!("agent.json unreadable: {e}; enroll again"),
            };
            if now != detail {
                ctx.set_degraded(&now);
                detail = now;
            }
        }
        while let Some(cmd) = ctx.next_command() {
            rt.answer(ctx, &cmd);
        }
        if ctx.stop().wait_timeout(TICK) {
            rt.finish(ctx);
            return Ok(None);
        }
    }
}

/// Builds the C# host over the module's folder and runs its loops until `ctx.stop()`.
fn run_enrolled(
    ctx: &ModuleContext,
    host: HostContext,
    identity: AgentIdentity,
) -> Result<(), Error> {
    // `TimeZoneInfo.Local` for a site without Agent:SiteTimeZone: the machine's offset.
    TimeZone::set_local(TimeZone::fixed("Local", i32::from(local_offset_minutes())));
    let clock = SystemClock::shared();
    let run_id = new_run_id("agent", clock.now());
    let bridge = Arc::new(SiteEventLogBridge::new(clock));
    // B5: every Error site event also goes to the hub as a problem report (throttled).
    let reports = {
        let ctx = ctx.clone();
        let report: ReportFn = Arc::new(move |code, message, context| {
            ctx.report(Severity::Error, code, message, context);
        });
        Arc::new(ModuleReports::new(report)) as Arc<dyn Sink>
    };
    let log = create_service_logger(
        &host.paths,
        &run_id,
        &identity,
        Some(Arc::new(TeeSink(vec![bridge.clone() as Arc<dyn Sink>, reports])) as Arc<dyn Sink>),
    );
    let secrets = crate::security::create(&host.paths);
    let services = AgentServices::build(&host, identity.clone(), run_id, log.clone(), secrets)
        .map_err(|e| Error::Module(format!("pda-sync services: {e}")))?;
    let agent_host = build_host(&services, &hooks(), Some(&bridge))
        .map_err(|e| Error::Module(format!("pda-sync host: {e}")))?;
    log.for_category("Agent").info(
        "PdaSyncAgent {Version} starting for agent {AgentId} (dl-agent module)",
        &[
            crate::reporting::Value::from(crate::hosting::agent_version()),
            crate::reporting::Value::from(identity.agent_id),
        ],
    );
    let stop = ctx.stop().clone();
    let worker = std::thread::Builder::new()
        .name("pda-sync host".into())
        .spawn(move || agent_host.run(&stop))
        .map_err(|e| Error::Io("spawn pda-sync host", e))?;
    let mut link = CloudLinkPublisher::new(&identity.cloud_url);
    let rt = Runtime::enrolled(&host, identity, &services);
    ctx.set_running("starting");
    let mut last = Published::default();
    let mut next_publish = Instant::now();
    loop {
        if Instant::now() >= next_publish {
            next_publish = Instant::now() + PUBLISH;
            last = publish(ctx, &rt, last);
            // B5: the watchdog watches the PDA cloud from the first accepted heartbeat.
            let reenroll = last.state == REENROLL_REQUIRED;
            match link.next(services.health.last_heartbeat_ok(), reenroll) {
                LinkUpdate::Keep => {}
                LinkUpdate::Set { url, ok_at } => ctx.set_cloud_link(&url, ok_at),
                LinkUpdate::Clear => ctx.clear_cloud_link(),
            }
        }
        while let Some(cmd) = ctx.next_command() {
            rt.answer(ctx, &cmd);
        }
        if ctx.stop().wait_timeout(TICK) {
            break;
        }
    }
    if link.declared() {
        ctx.clear_cloud_link();
    }
    rt.finish(ctx);
    match worker.join() {
        Ok(Ok(outcome)) if !outcome.abandoned.is_empty() => {
            let mut c = Map::new();
            c.insert("loops", outcome.abandoned.join(", "));
            ctx.report(
                Severity::Warning,
                "PDA_STOP_TIMEOUT",
                "pda-sync loops did not stop in time",
                c,
            );
            Ok(())
        }
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(Error::Module(format!("pda-sync host: {e}"))),
        Err(_) => Err(Error::Module("pda-sync host thread panicked".into())),
    }
}

/// What was last mirrored from `status.json`, so a report goes out once per change.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Published {
    state: String,
    code: Option<String>,
}

/// Mirrors `status.json` and the queue depths into the module state and metrics, and
/// reports a change into `degraded` or `reenroll_required` once.
fn publish(ctx: &ModuleContext, rt: &Runtime, last: Published) -> Published {
    for (k, v) in rt.metrics() {
        ctx.set_metric(&k, v);
    }
    let status = match AgentStatus::try_read(&rt.paths) {
        Ok(Some(s)) => s,
        Ok(None) => return last,
        Err(e) => {
            ctx.set_degraded(&format!("status.json unreadable: {e}"));
            return last;
        }
    };
    let now = Published {
        state: status.state.clone(),
        code: status.error_code.clone(),
    };
    let detail = describe(&status);
    if status.state == HEALTHY {
        ctx.set_running(&detail);
    } else {
        ctx.set_degraded(&detail);
    }
    if now != last && (status.state == DEGRADED || status.state == REENROLL_REQUIRED) {
        let severity = if status.state == REENROLL_REQUIRED {
            Severity::Critical
        } else {
            Severity::Warning
        };
        let mut c = Map::new();
        c.insert("state", status.state.as_str());
        if let Some(h) = &status.hint {
            c.insert("hint", h.as_str());
        }
        ctx.report(
            severity,
            status.error_code.as_deref().unwrap_or("PDA_DEGRADED"),
            &detail,
            c,
        );
    }
    now
}

/// `state [code]: hint`.
fn describe(s: &AgentStatus) -> String {
    let mut out = s.state.clone();
    if let Some(c) = &s.error_code {
        out.push_str(&format!(" [{c}]"));
    }
    if let Some(h) = &s.hint {
        out.push_str(": ");
        out.push_str(h);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_json::Value;

    #[test]
    fn the_diagnostics_bundle_gets_status_runtime_config_and_the_log_folder() {
        let dir = std::env::temp_dir().join(format!("pda-sync-diag-{}", dl_rand::u64()));
        let ctx = ModuleContext::standalone(NAME, Value::Null, dir.clone());
        let host = settings::host_context(ctx.config(), ctx.data_dir()).unwrap();
        name_diagnostics(&ctx, &host);
        let status = ctx.status();
        assert_eq!(
            status.diagnostic_files,
            [host.paths.status_file(), host.paths.runtime_config_file()]
        );
        assert_eq!(status.diagnostic_folders, [host.paths.logs_dir()]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
