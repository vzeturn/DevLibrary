//! Self-check of the order-verb grants (`OrderGrantsMonitor.cs`, `OrderGrantsRepair.cs`,
//! GR1/AG1, RT-1309). The service probes `pda_sync` at start and then keeps watching:
//! missing grants are applied by the service itself, as each identity `pda-enroll` tries
//! ([`BootstrapGrantsRepair`]), and the repair is tried again every
//! [`RECHECK_INTERVAL`] while they stay missing. One `SQL_GRANTS_MISSING` site event per
//! episode is sent only when every identity failed (or no repair is wired); it is sent
//! again only after a probe saw the grants in place.
//!
//! An order command refused by SQL only probes, and never waits for or runs a repair, so
//! its answer reaches the cloud before the lease ends; it wakes the service's loop instead.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::Duration;

use super::executor::{GrantsCheck, utc_now};
use crate::bootstrap::OrderGrantsProbe;
use crate::config::time::SharedClock;
use crate::config::{AgentIdentity, codes};
use crate::contracts::time::DateTimeOffset;
use crate::contracts::wire::site_event_levels;
use crate::hosting::services::{LoopService, ServiceError};
use crate::reporting::{Logger, SiteEvents, Value};
use crate::security::{SharedSecretStore, names};
use crate::util::ctx::Ctx;

pub use super::grants_repair::{
    APPLIED, ApplyAs, BootstrapGrantsRepair, GrantsRepair, REPAIR_CATEGORY, RepairAttempt,
    RepairOutcome,
};

/// Category of the monitor's log lines.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Commands.OrderGrantsMonitor";
/// A burst of refused commands shares one probe within this gap.
pub const MIN_PROBE_GAP: Duration = Duration::from_secs(60);
/// `OrderGrantsService.Attempts`: quick retries while the POS database is unreachable.
pub const SERVICE_ATTEMPTS: usize = 6;
/// `OrderGrantsService.Gap`.
pub const SERVICE_GAP: Duration = Duration::from_secs(30);
/// How often the service probes and repairs again while the grants are missing or
/// cannot be checked.
pub const RECHECK_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// How often the service's wait looks for a wake-up from a refused command.
pub const WAKE_POLL: Duration = Duration::from_secs(2);

#[derive(Default)]
struct MonitorState {
    reported: bool,
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
    wake: AtomicBool,
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
            wake: AtomicBool::new(false),
        }
    }

    /// What the operator runs: the grants verb of dl-agent as a SQL administrator. (The
    /// C# hint re-ran the PDA cloud installer, which installs the old agent dl-agent
    /// replaces.)
    pub fn instruction(&self) -> String {
        "Run as administrator on the store server: dl-agent pda-grants (it applies the order grants of pda_sync)".to_owned()
    }

    /// True once a refused command asked the service's loop to check and repair now;
    /// reading it clears it.
    pub fn take_wake(&self) -> bool {
        self.wake.swap(false, Ordering::SeqCst)
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

    /// The service's check (`CheckAsync`): probes, runs the self-repair whenever the
    /// grants are missing, and reports. True when the grants are missing, false when
    /// present, `None` when they could not be checked (logged).
    pub fn check_grants(&self, ctx: &Ctx) -> Option<bool> {
        let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.check_locked(st, ctx, true)
    }

    /// The check after a refused command: never blocks on the service's check and never
    /// runs the repair (it wakes the service's loop for that).
    pub fn check_after_refusal(&self, ctx: &Ctx) -> Option<bool> {
        let st = match self.state.try_lock() {
            Ok(st) => st,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            // The service is probing or repairing right now; it reports the outcome.
            Err(TryLockError::WouldBlock) => return None,
        };
        self.check_locked(st, ctx, false)
    }

    fn check_locked(
        &self,
        mut st: MutexGuard<'_, MonitorState>,
        ctx: &Ctx,
        may_repair: bool,
    ) -> Option<bool> {
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
        let mut repair: Option<(RepairOutcome, String)> = None;
        if !missing.is_empty()
            && let Some(repairer) = &self.repair
        {
            if !may_repair {
                st.last_probe = Some(utc_now(&self.clock));
                if !st.reported {
                    // The service's loop repairs and reports; the command is answered now.
                    self.wake.store(true, Ordering::SeqCst);
                }
                return Some(true);
            }
            let outcome = repairer.try_repair(ctx);
            let mut note = outcome.summary();
            if let Some(identity) = outcome.applied_by() {
                let identity = identity.to_owned();
                missing = self.probe(instance, database, &password, ctx, true)?;
                if missing.is_empty() {
                    self.repaired(&identity, &outcome);
                } else {
                    note.push_str("; the grants are still missing after the setup ran");
                }
            }
            repair = Some((outcome, note));
        }
        st.last_probe = Some(utc_now(&self.clock));
        if missing.is_empty() {
            st.reported = false;
            return Some(false);
        }
        if !st.reported {
            st.reported = true;
            self.report_missing(&missing, repair.as_ref());
        }
        Some(true)
    }

    fn repaired(&self, identity: &str, outcome: &RepairOutcome) {
        let mut detail = dl_json::Map::new();
        detail.insert("identity", identity);
        detail.insert("identities", outcome.to_json());
        self.events.report(
            site_event_levels::INFO,
            codes::SQL_GRANTS_REPAIRED,
            &format!(
                "The agent applied the missing SQL grants itself as {identity}; order commands from the cloud are accepted"
            ),
            Some(detail),
        );
    }

    fn report_missing(&self, missing: &[String], repair: Option<&(RepairOutcome, String)>) {
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
            repair.map_or(dl_json::Value::Null, |(_, note)| {
                dl_json::Value::String(note.clone())
            }),
        );
        detail.insert(
            "identities",
            repair.map_or(dl_json::Value::Array(Vec::new()), |(o, _)| o.to_json()),
        );
        let tried = repair.map_or(String::new(), |(_, note)| {
            format!(
                " The agent could not apply them itself ({note}) and tries again every {} minutes.",
                RECHECK_INTERVAL.as_secs() / 60
            )
        });
        self.events.report(
            site_event_levels::ERROR,
            codes::SQL_GRANTS_MISSING,
            &format!(
                "pda_sync lacks the order grants ({}); order commands are refused.{tried} {}",
                missing.join(", "),
                self.instruction()
            ),
            Some(detail),
        );
    }
}

impl GrantsCheck for OrderGrantsMonitor {
    fn check(&self, ctx: &Ctx) {
        // The outcome is logged and reported inside; the refused command is answered
        // whatever it is.
        let _ = self.check_after_refusal(ctx);
    }
}

/// Runs the check at start and keeps watching (`OrderGrantsService`): quick retries
/// while the POS database is unreachable, then a probe and repair every
/// [`RECHECK_INTERVAL`] while the grants are missing or unchecked, and at once when a
/// refused command wakes it.
pub struct OrderGrantsService {
    monitor: Arc<OrderGrantsMonitor>,
}

impl OrderGrantsService {
    /// The watch of `monitor`.
    pub fn new(monitor: Arc<OrderGrantsMonitor>) -> OrderGrantsService {
        OrderGrantsService { monitor }
    }

    /// Waits up to `limit` (`None`: until woken); false when `ctx` ended.
    fn wait(&self, ctx: &Ctx, limit: Option<Duration>) -> bool {
        let mut waited = Duration::ZERO;
        loop {
            if self.monitor.take_wake() || limit.is_some_and(|l| waited >= l) {
                return true;
            }
            if !ctx.sleep(WAKE_POLL) {
                return false;
            }
            waited += WAKE_POLL;
        }
    }
}

/// How long the service waits after a check: `None` until a refused command wakes it.
pub fn next_wait(checked: Option<bool>, unchecked_in_a_row: usize) -> Option<Duration> {
    match checked {
        Some(false) => None,
        Some(true) => Some(RECHECK_INTERVAL),
        None if unchecked_in_a_row < SERVICE_ATTEMPTS => Some(SERVICE_GAP),
        None => Some(RECHECK_INTERVAL),
    }
}

impl LoopService for OrderGrantsService {
    fn name(&self) -> &'static str {
        "OrderGrantsService"
    }

    fn run_loop(&mut self, ctx: &Ctx) -> Result<(), ServiceError> {
        let mut unchecked = 0;
        loop {
            let checked = self.monitor.check_grants(ctx);
            unchecked = if checked.is_none() { unchecked + 1 } else { 0 };
            if !self.wait(ctx, next_wait(checked, unchecked)) {
                return Ok(());
            }
        }
    }
}
