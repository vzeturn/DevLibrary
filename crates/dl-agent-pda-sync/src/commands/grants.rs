//! Self-check of the order-verb grants (`OrderGrantsMonitor.cs`, `OrderGrantsRepair.cs`,
//! GR1/AG1): at service start, and after an order command was refused by SQL, it probes
//! `pda_sync`. Missing grants are first applied by the service itself, once per process
//! (works where the service's Windows identity may administer the SQL Server); when that
//! is not possible it sends one `SQL_GRANTS_MISSING` site event that tells the operator to
//! re-run the install command. One event per episode: it is sent again only after a probe
//! saw the grants in place.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::executor::{GrantsCheck, utc_now};
use crate::bootstrap::{OrderGrantsProbe, SqlBootstrapper};
use crate::config::time::SharedClock;
use crate::config::{AgentIdentity, codes};
use crate::contracts::time::DateTimeOffset;
use crate::contracts::wire::site_event_levels;
use crate::hosting::services::{LoopService, ServiceError};
use crate::reporting::{Logger, SiteEvents, Value};
use crate::security::{SharedSecretStore, names};
use crate::util::ctx::Ctx;

/// Category of the monitor's log lines.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Commands.OrderGrantsMonitor";
/// Category of the self-repair's log lines.
pub const REPAIR_CATEGORY: &str = "MM.PdaOrder.SyncAgent.Commands.BootstrapGrantsRepair";
/// A burst of refused commands shares one probe within this gap.
pub const MIN_PROBE_GAP: Duration = Duration::from_secs(60);
/// `OrderGrantsService.Attempts`.
pub const SERVICE_ATTEMPTS: usize = 6;
/// `OrderGrantsService.Gap`.
pub const SERVICE_GAP: Duration = Duration::from_secs(30);

/// The service's own attempt to apply missing grants (`IOrderGrantsRepair`).
pub trait GrantsRepair: Send + Sync {
    /// `None` when the bootstrap ran; otherwise why it could not (no secret in it).
    fn try_repair(&self, ctx: &Ctx) -> Option<String>;
}

/// The same idempotent bootstrap as `enroll` and `grants`, under the service's Windows
/// identity (`BootstrapGrantsRepair`).
pub struct BootstrapGrantsRepair {
    bootstrapper: Arc<SqlBootstrapper>,
    identity: AgentIdentity,
    log: Logger,
}

impl BootstrapGrantsRepair {
    /// A repair over `bootstrapper` for the enrolled target; `log` is the root logger.
    pub fn new(bootstrapper: Arc<SqlBootstrapper>, identity: AgentIdentity, log: &Logger) -> Self {
        BootstrapGrantsRepair {
            bootstrapper,
            identity,
            log: log.for_category(REPAIR_CATEGORY),
        }
    }
}

impl GrantsRepair for BootstrapGrantsRepair {
    fn try_repair(&self, ctx: &Ctx) -> Option<String> {
        let run = || -> Result<usize, crate::bootstrap::Error> {
            let preflight = self.bootstrapper.preflight(
                self.identity.sql_instance.as_deref(),
                self.identity.sql_database.as_deref(),
                ctx,
            )?;
            let report = self.bootstrapper.apply(&preflight, false, ctx)?;
            Ok(report.steps.iter().filter(|s| s.executed).count())
        };
        match run() {
            Ok(applied) => {
                self.log.info(
                    "Grants self-repair applied {Applied} SQL step(s)",
                    &[Value::from(applied)],
                );
                None
            }
            Err(e) => match (e.code(), e.sql()) {
                (Some(code), _) => {
                    let code = code.to_owned();
                    self.log.warn_ex(
                        e.to_exception(),
                        "Grants self-repair not possible under the service identity ({Code})",
                        &[Value::from(code.as_str())],
                    );
                    Some(code)
                }
                (None, Some(sql)) => {
                    let number = sql.number();
                    self.log.warn_ex(
                        e.to_exception(),
                        "Grants self-repair failed (SQL error {Number})",
                        &[Value::from(number)],
                    );
                    Some(format!("SQL error {number}"))
                }
                // C#: any other exception escapes the check; here it is the reason.
                (None, None) => Some(e.to_exception().type_name),
            },
        }
    }
}

#[derive(Default)]
struct MonitorState {
    reported: bool,
    repair_tried: bool,
    last_probe: Option<DateTimeOffset>,
}

/// The order grants monitor.
pub struct OrderGrantsMonitor {
    events: Arc<dyn SiteEvents>,
    probe: Arc<dyn OrderGrantsProbe>,
    identity: AgentIdentity,
    secrets: SharedSecretStore,
    clock: SharedClock,
    log: Logger,
    repair: Option<Arc<dyn GrantsRepair>>,
    state: Mutex<MonitorState>,
}

impl OrderGrantsMonitor {
    /// A monitor; `log` is the root logger.
    pub fn new(
        events: Arc<dyn SiteEvents>,
        probe: Arc<dyn OrderGrantsProbe>,
        identity: AgentIdentity,
        secrets: SharedSecretStore,
        clock: SharedClock,
        log: &Logger,
        repair: Option<Arc<dyn GrantsRepair>>,
    ) -> OrderGrantsMonitor {
        OrderGrantsMonitor {
            events,
            probe,
            identity,
            secrets,
            clock,
            log: log.for_category(CATEGORY),
            repair,
            state: Mutex::new(MonitorState::default()),
        }
    }

    /// What the operator runs: the install command again (it applies the grants), or the
    /// verb alone.
    pub fn instruction(&self) -> String {
        format!(
            "Re-run the install command as administrator on the store server: irm {}/agent | iex (it applies the grants; or run: PdaSyncAgent grants)",
            self.identity.cloud_url.trim_end_matches('/')
        )
    }

    fn probe(
        &self,
        instance: &str,
        database: &str,
        password: &str,
        ctx: &Ctx,
        after_repair: bool,
    ) -> Option<Vec<String>> {
        match self.probe.missing(instance, database, password, ctx) {
            Ok(m) => Some(m),
            Err(e) => {
                let number = e.sql().map_or(0, |s| s.number());
                let template = if after_repair {
                    "The order grants could not be checked after the self-repair (SQL error {Number})"
                } else {
                    "The order grants could not be checked (SQL error {Number})"
                };
                self.log
                    .warn_ex(e.to_exception(), template, &[Value::from(number)]);
                None
            }
        }
    }

    /// True when the grants are missing, false when present, `None` when they could not
    /// be checked (logged) (`CheckAsync`).
    pub fn check_grants(&self, ctx: &Ctx) -> Option<bool> {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let now = utc_now(&self.clock);
        let gap = i64::try_from(MIN_PROBE_GAP.as_nanos() / 100).unwrap_or(i64::MAX);
        if st.reported
            && st
                .last_probe
                .is_some_and(|last| now.ticks_since(last) < gap)
        {
            return Some(true);
        }
        let password = self.secrets.load(names::SQL_PASSWORD).ok().flatten();
        let (Some(instance), Some(database), Some(password)) = (
            self.identity.sql_instance.as_deref(),
            self.identity.sql_database.as_deref(),
            password,
        ) else {
            self.log.warn(
                "The order grants cannot be checked: the SQL target or the pda_sync password is not recorded",
                &[],
            );
            return None;
        };
        let password = String::from_utf8_lossy(&password).into_owned();
        let mut missing = self.probe(instance, database, &password, ctx, false)?;
        let mut repair_failure: Option<String> = None;
        if !missing.is_empty()
            && let Some(repair) = &self.repair
            && !st.repair_tried
        {
            st.repair_tried = true;
            repair_failure = repair.try_repair(ctx);
            if repair_failure.is_none() {
                missing = self.probe(instance, database, &password, ctx, true)?;
                if missing.is_empty() {
                    self.events.report(
                        site_event_levels::INFO,
                        codes::SQL_GRANTS_REPAIRED,
                        "The agent applied the missing SQL grants itself; order commands from the cloud are accepted",
                        None,
                    );
                } else {
                    repair_failure =
                        Some("the grants are still missing after the setup ran".to_owned());
                }
            }
        }
        st.last_probe = Some(utc_now(&self.clock));
        if missing.is_empty() {
            st.reported = false;
            return Some(false);
        }
        if !st.reported {
            st.reported = true;
            let mut detail = dl_json::Map::new();
            detail.insert(
                "missing",
                dl_json::Value::Array(
                    missing
                        .iter()
                        .map(|m| dl_json::Value::String(m.clone()))
                        .collect(),
                ),
            );
            detail.insert(
                "self_repair",
                repair_failure.map_or(dl_json::Value::Null, dl_json::Value::String),
            );
            self.events.report(
                site_event_levels::ERROR,
                codes::SQL_GRANTS_MISSING,
                &format!(
                    "pda_sync lacks the order grants ({}); order commands are refused. {}",
                    missing.join(", "),
                    self.instruction()
                ),
                Some(detail),
            );
        }
        Some(true)
    }
}

impl GrantsCheck for OrderGrantsMonitor {
    fn check(&self, ctx: &Ctx) {
        let _ = self.check_grants(ctx);
    }
}

/// Runs the check at start, retrying while the POS database is unreachable
/// (`OrderGrantsService`).
pub struct OrderGrantsService {
    monitor: Arc<OrderGrantsMonitor>,
}

impl OrderGrantsService {
    /// The start-up check of `monitor`.
    pub fn new(monitor: Arc<OrderGrantsMonitor>) -> OrderGrantsService {
        OrderGrantsService { monitor }
    }
}

impl LoopService for OrderGrantsService {
    fn name(&self) -> &'static str {
        "OrderGrantsService"
    }

    fn run_loop(&mut self, ctx: &Ctx) -> Result<(), ServiceError> {
        for _ in 0..SERVICE_ATTEMPTS {
            if self.monitor.check_grants(ctx).is_some() || !ctx.sleep(SERVICE_GAP) {
                return Ok(());
            }
        }
        Ok(())
    }
}
