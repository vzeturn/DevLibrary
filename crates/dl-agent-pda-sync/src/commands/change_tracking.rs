//! The Change Tracking check of the running service (RT-1318, ADR 0011 D2). Behind the flag
//! `capture.change_tracking` (default off) the grants service probes the captured tables as
//! `pda_sync` after its grants check and, while Change Tracking is not on for all of them,
//! runs a round as the bootstrap identities of ADR 0010
//! ([`run_round`](super::change_tracking_round::run_round)), again every
//! [`RECHECK_INTERVAL`] while work is left. With the flag off nothing is probed and no
//! statement is issued.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::change_tracking_round::{classify, report, run_round};
use super::grants::RECHECK_INTERVAL;
use crate::bootstrap::change_tracking::plan;
use crate::bootstrap::change_tracking_sql::{
    ChangeTrackingProbe, CtExecutor, SqlChangeTrackingProbe, SqlCtExecutor,
};
use crate::config::AgentIdentity;
use crate::hosting::enroll_attempts::{Attempt, DefaultLogin, default_attempts};
use crate::reporting::{Logger, SiteEvents, Value};
use crate::security::{SharedSecretStore, names};
use crate::util::ctx::Ctx;

/// Category of the check's log lines.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Commands.ChangeTrackingMonitor";

/// Where Change Tracking stands after a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtState {
    /// The flag is off: nothing was probed or run.
    Off,
    /// Every captured table with a primary key is tracked and readable.
    Ready,
    /// Work is left (a refusal, a lock timeout, a failure): tried again later.
    Pending,
    /// The state could not be read (no SQL target or password, a probe failure).
    Unchecked,
}

impl CtState {
    /// How long until the next check: [`RECHECK_INTERVAL`] while work is left or the
    /// state is unknown, `None` (no more checks) when off or ready.
    pub fn recheck(self) -> Option<Duration> {
        match self {
            CtState::Pending | CtState::Unchecked => Some(RECHECK_INTERVAL),
            CtState::Off | CtState::Ready => None,
        }
    }
}

/// What the check runs on.
pub struct CtParts {
    /// The flag `capture.change_tracking`.
    pub enabled: bool,
    /// The captured tables ([`captured_tables`](crate::capture::plan::captured_tables)).
    pub tables: Vec<String>,
    /// The read-only probe as `pda_sync`.
    pub probe: Arc<dyn ChangeTrackingProbe>,
    /// The administrator sessions.
    pub exec: Arc<dyn CtExecutor>,
    /// The identities, in order ([`default_attempts`]).
    pub attempts: Vec<Attempt>,
    /// Where the outcome is reported.
    pub events: Arc<dyn SiteEvents>,
    /// The enrolled SQL target.
    pub identity: AgentIdentity,
    /// The `pda_sync` password.
    pub secrets: SharedSecretStore,
}

/// The service's Change Tracking check.
pub struct ChangeTrackingMonitor {
    parts: CtParts,
    root: Logger,
    log: Logger,
    reported: Mutex<Option<String>>,
}

impl ChangeTrackingMonitor {
    /// A check over `parts`; `log` is the root logger.
    pub fn new(parts: CtParts, log: &Logger) -> ChangeTrackingMonitor {
        ChangeTrackingMonitor {
            parts,
            root: log.clone(),
            log: log.for_category(CATEGORY),
            reported: Mutex::new(None),
        }
    }

    /// The real check: the SQL probe and sessions, and the identities of `pda-enroll`
    /// ([`DefaultLogin::from_process`]). With `enabled` false no identity is loaded.
    pub fn real(
        enabled: bool,
        tables: Vec<String>,
        events: Arc<dyn SiteEvents>,
        identity: AgentIdentity,
        secrets: SharedSecretStore,
        log: &Logger,
    ) -> ChangeTrackingMonitor {
        let attempts = if enabled {
            default_attempts(&DefaultLogin::from_process())
        } else {
            Vec::new()
        };
        ChangeTrackingMonitor::new(
            CtParts {
                enabled,
                tables,
                probe: Arc::new(SqlChangeTrackingProbe),
                exec: Arc::new(SqlCtExecutor),
                attempts,
                events,
                identity,
                secrets,
            },
            log,
        )
    }

    /// Probes and, while Change Tracking is not on for every captured table that can
    /// have it, runs a round and reports it (a repeated outcome is reported once).
    pub fn check(&self, ctx: &Ctx) -> CtState {
        let p = &self.parts;
        if !p.enabled {
            return CtState::Off;
        }
        let password = p.secrets.load(names::SQL_PASSWORD).ok().flatten();
        let (Some(instance), Some(database), Some(password)) = (
            p.identity.sql_instance.as_deref(),
            p.identity.sql_database.as_deref(),
            password,
        ) else {
            self.log.warn(
                "Change Tracking cannot be checked: the SQL target or the pda_sync password is not recorded",
                &[],
            );
            return CtState::Unchecked;
        };
        let password = String::from_utf8_lossy(&password).into_owned();
        let probe = match p.probe.probe(instance, database, &password, &p.tables, ctx) {
            Ok(probe) => probe,
            Err(e) => {
                let (failure, _) = classify(&e);
                self.log.warn_ex(
                    e.to_exception(),
                    "Change Tracking could not be probed as pda_sync ({Failure})",
                    &[Value::from(failure.as_str())],
                );
                return CtState::Unchecked;
            }
        };
        let plan = plan(&probe, true);
        if plan.is_done() {
            self.log.info(
                "Change Tracking: {State}",
                &[Value::from(probe.describe().as_str())],
            );
            return CtState::Ready;
        }
        let outcome = run_round(
            &plan,
            &p.attempts,
            &*p.exec,
            instance,
            database,
            ctx,
            &self.root,
        );
        let r = report(&outcome, database);
        let signature = format!("{}|{}", r.code, r.message);
        let mut last = self.reported.lock().unwrap_or_else(PoisonError::into_inner);
        if last.as_deref() != Some(signature.as_str()) {
            p.events.report(r.level, r.code, &r.message, Some(r.detail));
            *last = Some(signature);
        }
        if outcome.pending {
            CtState::Pending
        } else {
            CtState::Ready
        }
    }
}

#[cfg(test)]
#[path = "change_tracking_tests.rs"]
mod tests;
