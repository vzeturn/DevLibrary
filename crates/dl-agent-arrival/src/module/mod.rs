//! The dl-agent module `arrival`: runs the Check-In site link under
//! [`dl_agent::supervisor::Module`].
//!
//! The module is opt-in (`modules.arrival.enabled = true`) and reads the existing .NET
//! install in place (see [`crate::agent::layout`]). While the site is not enrolled, or the
//! settings cannot be used, it stays degraded with the reason and looks again every
//! [`RECHECK`]; it does not fail and restart. An install without its tenant key does not
//! start either (the agent never makes one at run time): it reports `tenant_key_missing` /
//! `tenant_key_unreadable` and waits for the key. Once the link runs, the module turns running
//! after the first answered poll and degraded while the cloud or the PMS keeps failing.
//!
//! Config section:
//!
//! ```json
//! "arrival": { "enabled": true, "install_dir": "C:\\Program Files\\MM-Arrival Agent", "utc_offset_minutes": 420 }
//! ```
//!
//! `install_dir` defaults to the folder Install.ps1 uses; `utc_offset_minutes` is the hotel's
//! offset for the `{today}` of the watches (default 420, Vietnam), the
//! `HotelUtcOffsetMinutes` of the .NET agent.

pub mod commands;
pub mod picture;
pub mod reporter;
pub mod selftest;
pub mod sweep;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_agent::report::Severity;
use dl_agent::supervisor::{Factory, Module, ModuleContext};
use dl_json::{Map, Value};
use dl_time::Offset;

use crate::agent::clock::SystemClock;
use crate::agent::core_sql;
use crate::agent::executor::Executor;
use crate::agent::health::{self, Health};
use crate::agent::installer;
use crate::agent::layout::Layout;
use crate::agent::problems::ProblemJournal;
use crate::agent::runtime::{Installer, Parts, Wiring};
use crate::agent::secrets::{self, Identity, TenantKeyStore};
use crate::agent::settings::Settings;
use crate::agent::tds::{Target, TdsExecutor, TdsOptions};
use crate::agent::{eventlog, inventory};
use crate::error::Error;
use reporter::CtxSink;

/// The module name: config section and report module.
pub const NAME: &str = "arrival";
/// How often a waiting module looks at the install again.
pub const RECHECK: Duration = Duration::from_secs(60);
/// The hotel offset when the config names none (UTC+7).
pub const DEFAULT_UTC_OFFSET_MINUTES: i32 = 420;
/// The state detail while no identity exists.
pub const NOT_ENROLLED: &str = "not enrolled: run `dl-agent arrival-enroll` (or Install.ps1)";

/// How often an idle module answers commands.
const IDLE_TICK: Duration = Duration::from_millis(250);

/// The module's config section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleConfig {
    /// Where the .NET agent is installed (default: the platform default).
    pub install_dir: Option<PathBuf>,
    /// The hotel's UTC offset in minutes.
    pub utc_offset_minutes: i32,
}

impl ModuleConfig {
    /// Reads the section (`Null` gives the defaults).
    ///
    /// # Errors
    /// [`Error::Config`] for a value of the wrong type or an offset beyond ±18 h.
    pub fn from_value(v: &Value) -> Result<ModuleConfig, Error> {
        let install_dir = match v.get("install_dir") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.trim().is_empty() => None,
            Some(Value::String(s)) => Some(PathBuf::from(s.trim())),
            Some(_) => return Err(Error::Config("install_dir must be a string".into())),
        };
        let utc_offset_minutes = match v.get("utc_offset_minutes") {
            None | Some(Value::Null) => DEFAULT_UTC_OFFSET_MINUTES,
            Some(n) => n
                .as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .filter(|n| n.abs() <= 18 * 60)
                .ok_or_else(|| {
                    Error::Config("utc_offset_minutes must be a whole number within ±1080".into())
                })?,
        };
        Ok(ModuleConfig {
            install_dir,
            utc_offset_minutes,
        })
    }

    /// The offset as a [`dl_time::Offset`].
    ///
    /// # Errors
    /// [`Error::Config`] when out of range (checked by [`ModuleConfig::from_value`]).
    pub fn offset(&self) -> Result<Offset, Error> {
        Offset::from_seconds(self.utc_offset_minutes * 60)
            .map_err(|_| Error::Config("utc_offset_minutes is out of range".into()))
    }
}

impl Default for ModuleConfig {
    fn default() -> ModuleConfig {
        ModuleConfig {
            install_dir: None,
            utc_offset_minutes: DEFAULT_UTC_OFFSET_MINUTES,
        }
    }
}

/// Parts a test can replace; `None` takes the production default.
#[derive(Clone, Default)]
pub struct Hooks {
    /// The PMS executor (default: SQL Server through dl-tds). With a replaced executor the
    /// procedure bundle is not installed.
    pub executor: Option<Arc<dyn Executor>>,
    /// See [`RECHECK`].
    pub recheck: Option<Duration>,
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks")
            .field("executor", &self.executor.is_some())
            .field("recheck", &self.recheck)
            .finish()
    }
}

/// The Check-In site module.
#[derive(Debug, Default)]
pub struct ArrivalModule {
    hooks: Hooks,
}

/// The factory to register with `App::module`.
pub fn arrival_module() -> Factory {
    arrival_module_with(Hooks::default())
}

/// [`arrival_module`] with replaced parts.
pub fn arrival_module_with(hooks: Hooks) -> Factory {
    Arc::new(move || {
        Box::new(ArrivalModule {
            hooks: hooks.clone(),
        }) as Box<dyn Module>
    })
}

impl Module for ArrivalModule {
    fn name(&self) -> &str {
        NAME
    }

    fn enabled_by_default(&self) -> bool {
        false
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), dl_agent::Error> {
        let config = match ModuleConfig::from_value(ctx.config()) {
            Ok(c) => c,
            Err(e) => {
                // A config change restarts the module; until then there is nothing to try.
                ctx.set_degraded(&format!("config invalid: {e}"));
                idle(ctx, None);
                return Ok(());
            }
        };
        let sink = Arc::new(CtxSink::new(ctx.clone()));
        let health = Arc::new(Health::new(sink.clone(), health::REPORT_INTERVAL));
        commands::publish_metrics(ctx);
        loop {
            match self.prepare(&config, ctx, &health, &sink) {
                Ok((parts, journal)) => {
                    let on_tick = || answer_commands(ctx);
                    let result = parts.run(ctx.stop(), &on_tick).map_err(Into::into);
                    if result.is_ok() {
                        // Stopped on purpose: the supervisor has no link to watch any more.
                        sink.unlink();
                    }
                    journal.mark_stopped();
                    return result;
                }
                Err(detail) => {
                    ctx.set_degraded(&detail);
                    if idle(ctx, Some(self.hooks.recheck.unwrap_or(RECHECK))) {
                        return Ok(());
                    }
                }
            }
        }
    }
}

impl ArrivalModule {
    /// Opens the install; `Err` is the state detail to wait with.
    fn prepare(
        &self,
        config: &ModuleConfig,
        ctx: &ModuleContext,
        health: &Arc<Health>,
        sink: &Arc<CtxSink>,
    ) -> Result<(Parts, Arc<ProblemJournal>), String> {
        let cannot = |e: Error| {
            dl_log::error!("the arrival link cannot start"; error = %e);
            format!("cannot start: {e}")
        };
        let layout = Layout::detect(config.install_dir.as_deref(), ctx.data_dir());
        let mut settings = Settings::load(layout.install_dir()).map_err(cannot)?;
        let identity_path = layout.resolve(&settings.identity_file);
        let journal = Arc::new(ProblemJournal::next_to(&identity_path));
        // The diagnostics bundle takes them even before enrolment (the record says why not).
        ctx.add_diagnostic_file(journal.path().to_path_buf());
        ctx.add_diagnostic_file(
            layout
                .install_dir()
                .join(crate::tools::files::ENROLL_RECORD),
        );
        let identity = Identity::load(&identity_path)
            .map_err(cannot)?
            .ok_or_else(|| NOT_ENROLLED.to_string())?;
        if let Err((code, message)) = key_ready(&settings, &layout) {
            dl_log::error!("the arrival link has no usable tenant key"; code = %code, error = %message);
            let mut context = Map::new();
            let path = |p: &str| crate::tools::files::path_text(&layout.resolve(p));
            context.insert("tenant_key_file", path(&settings.tenant_key_file));
            context.insert("vault_directory", path(&settings.vault_directory));
            journal.record("Error", "TenantKey", &message, Some(code));
            ctx.report(Severity::Error, code, &message, context);
            return Err(format!("cannot start: {message}"));
        }
        pms_from_core(&mut settings, ctx);
        let (executor, installer): (Arc<dyn Executor>, Option<Installer>) =
            match &self.hooks.executor {
                Some(e) => (Arc::clone(e), None),
                None => {
                    let tds =
                        TdsExecutor::new(&settings, tds_options(&settings)).map_err(cannot)?;
                    let installer = settings.auto_install_procedures.then(|| {
                        let (connection, source) =
                            (settings.pms_connection_string.clone(), settings.pms_source);
                        Box::new(move || {
                            installer::ensure_installed(Target::new(&connection, source), false)
                        }) as Installer
                    });
                    (Arc::new(tds), installer)
                }
            };
        let wiring = Wiring {
            executor,
            clock: Arc::new(SystemClock),
            health: Arc::clone(health),
            utc_offset: config.offset().map_err(cannot)?,
            installer,
            problems: Some(Arc::clone(&journal)),
            installations: None,
        };
        let mut wiring = wiring;
        wiring.installations = start_journal(&journal, sink);
        let parts = Parts::assemble(&settings, &layout, &identity, wiring).map_err(cannot)?;
        sink.link_to(&settings.cloud_base_url);
        Ok((parts, journal))
    }
}

/// Takes the core's `sql.json` SMILE_FO connection when it declares one; an unusable entry is
/// reported and the appsettings connection stays.
fn pms_from_core(settings: &mut Settings, ctx: &ModuleContext) {
    match core_sql::apply(settings, ctx.data_dir()) {
        Ok(source) => {
            let pin = source.pin().map_or("auto", |r| r.name());
            dl_log::info!("PMS connection chosen"; source = source.name(), transport = pin);
        }
        Err(e) => {
            dl_log::warn!("the core SQL settings cannot serve the PMS; using PmsConnectionString"; error = %e);
            ctx.report(Severity::Warning, core_sql::UNUSABLE, &e, Map::new());
        }
    }
}

/// The tenant key the settings reach, read without ever making one; `Err` is the problem
/// code and the message (paths only, never the key).
fn key_ready(settings: &Settings, layout: &Layout) -> Result<(), (&'static str, String)> {
    let file = layout.resolve(&settings.tenant_key_file);
    match TenantKeyStore::new(&settings.tenant_key, file.clone()).load_existing() {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(("tenant_key_missing", secrets::missing(&file))),
        Err(e) => Err((
            "tenant_key_unreadable",
            format!("the tenant key cannot be read ({e}); restore it and enrol again"),
        )),
    }
}

/// Opens the run in the journal, attaches it to the sink, lists the agent copies on this
/// machine and reads Windows' event logs in the background (the hello waits for nothing).
fn start_journal(journal: &Arc<ProblemJournal>, sink: &Arc<CtxSink>) -> Option<String> {
    journal.mark_started(
        &dl_agent::agent_version().to_string(),
        &dl_agent::hub::hostname(),
    );
    sink.attach(Arc::clone(journal));
    let (installations, old_left) = inventory::describe();
    if old_left {
        journal.record(
            "Warning",
            "Inventory",
            &format!(
                "The old Check-In agent ({}) is still installed or running here: remove it with the bench (remove_legacy).",
                inventory::OLD_BINARY
            ),
            Some("inventory|old-agent"),
        );
    }
    let j = Arc::clone(journal);
    let spawned = std::thread::Builder::new()
        .name("arrival-eventlog".into())
        .spawn(move || {
            if let Err(e) = eventlog::import(&j) {
                dl_log::warn!("Windows event logs not read"; error = %e);
            }
        });
    if let Err(e) = spawned {
        dl_log::warn!("event-log reader not started"; error = %e);
    }
    installations
}

/// Pool sizing: the request lane, one session per watch and one for the hello.
fn tds_options(settings: &Settings) -> TdsOptions {
    let secs = |n: i32| Duration::from_secs(u64::try_from(n).unwrap_or(0).max(1));
    let lanes = usize::try_from(settings.max_concurrency)
        .unwrap_or(1)
        .max(1);
    TdsOptions {
        max_connections: lanes + settings.watches.len() + 1,
        command_timeout: secs(settings.command_timeout_seconds),
        lock_timeout: secs(settings.lock_timeout_seconds),
    }
}

/// Every command gets one answer (see [`commands`]).
fn answer_commands(ctx: &ModuleContext) {
    commands::answer_all(ctx);
}

/// Waits `limit` (forever when `None`) answering commands; `true` when stopped.
fn idle(ctx: &ModuleContext, limit: Option<Duration>) -> bool {
    let end = limit.map(|d| Instant::now() + d);
    loop {
        answer_commands(ctx);
        if ctx.stop().wait_timeout(IDLE_TICK) {
            return true;
        }
        if end.is_some_and(|end| Instant::now() >= end) {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_validation() {
        assert_eq!(
            ModuleConfig::from_value(&Value::Null).expect("null"),
            ModuleConfig::default()
        );
        let v = dl_json::parse(
            r#"{"enabled":true,"install_dir":" D:\\Agent ","utc_offset_minutes":-300}"#,
        )
        .expect("json");
        let c = ModuleConfig::from_value(&v).expect("valid");
        assert_eq!(c.install_dir, Some(PathBuf::from("D:\\Agent")));
        assert_eq!(c.offset().expect("offset").whole_seconds(), -18_000);
        for bad in [
            r#"{"utc_offset_minutes":2000}"#,
            r#"{"utc_offset_minutes":"420"}"#,
            r#"{"install_dir":5}"#,
        ] {
            assert!(
                ModuleConfig::from_value(&dl_json::parse(bad).expect("json")).is_err(),
                "{bad}"
            );
        }
    }
}
