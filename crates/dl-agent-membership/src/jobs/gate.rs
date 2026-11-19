//! The write side of `PmsWriteGate`: `EvaluateAsync`, the 60 s sync-config cache, the
//! log-on-change and `Denied`. The decision itself is [`identity::gate::decide`], shared
//! with the push gate.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::JobAction;
use super::result::{Ack, class_of_code};
use crate::hub::{HubApi, HubError};
use crate::identity::Tracker;
use crate::identity::gate::{Decision, STALE, decide};
use crate::log::{self, V};
use crate::wire::parse::SyncConfig;

/// `ConfigTtl`.
pub const CONFIG_TTL: Duration = Duration::from_secs(60);

/// `GatedActions` (`PmsDbIdentityPolicy.WriteActions`): earn and availability only read.
pub fn is_gated(action: JobAction) -> bool {
    matches!(
        action,
        JobAction::Create
            | JobAction::Modify
            | JobAction::Cancel
            | JobAction::LinkFolio
            | JobAction::RedeemVoucher
            | JobAction::GrantReward
    )
}

/// `PmsWriteGate.Denied`: the ack for a gated job the gate refused.
pub fn denied(job_id: &str, d: &Decision) -> Ack {
    let code = d.code.unwrap_or(STALE);
    let (code, class) = class_of_code(code);
    Ack::fail(
        job_id,
        &format!("{code}: {}", d.reason),
        Some(code),
        Some(class),
    )
}

#[derive(Debug, Default)]
struct State {
    config: Option<SyncConfig>,
    at: Option<Instant>,
    last_logged: Option<String>,
}

/// The gate a dispatcher asks before a SMILE write.
pub struct WriteGate {
    hub: Arc<dyn HubApi>,
    tracker: Arc<Tracker>,
    state: Mutex<State>,
}

impl std::fmt::Debug for WriteGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteGate").finish_non_exhaustive()
    }
}

impl WriteGate {
    /// A gate reading sync-config from `hub` and the identity from `tracker`.
    pub fn new(hub: Arc<dyn HubApi>, tracker: Arc<Tracker>) -> WriteGate {
        WriteGate {
            hub,
            tracker,
            state: Mutex::new(State::default()),
        }
    }

    /// `EvaluateAsync`.
    ///
    /// # Errors
    /// The sync-config read failed and no config was ever read.
    pub fn evaluate(&self, now: Instant) -> Result<Decision, HubError> {
        if self.tracker.current().is_none() {
            // One bounded read-only identity probe, never the write connection.
            if let crate::identity::Refresh::Failed(e) = self.tracker.refresh_if_due(false) {
                log::warn(
                    "SMILE identity read failed before a write; the gate decides without it",
                    &[("error", V::S(&e))],
                );
            }
        }
        let config = self.config(now)?;
        let current = self.tracker.current();
        let d = decide(
            config.as_ref(),
            current.as_ref(),
            self.tracker.foreign_age_minutes(),
        );
        self.log_on_change(&d);
        Ok(d)
    }

    fn config(&self, now: Instant) -> Result<Option<SyncConfig>, HubError> {
        let cached = {
            let s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if let (Some(c), Some(at)) = (&s.config, s.at)
                && now.saturating_duration_since(at) < CONFIG_TTL
            {
                return Ok(Some(c.clone()));
            }
            s.config.clone()
        };
        match self.hub.sync_config() {
            Ok(fresh) => {
                let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                s.config = Some(fresh.clone());
                s.at = Some(now);
                Ok(Some(fresh))
            }
            Err(e) if cached.is_some() => {
                log::warn(
                    "sync-config read failed for the write gate; using the last one read",
                    &[("error", V::S(&e.to_string()))],
                );
                Ok(cached)
            }
            Err(e) => Err(e),
        }
    }

    fn log_on_change(&self, d: &Decision) {
        let key = format!("{}|{:?}|{}|{}", d.allowed, d.code, d.warn, d.reason);
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if s.last_logged.as_deref() == Some(key.as_str()) {
            return;
        }
        let was_logged = s.last_logged.is_some();
        s.last_logged = Some(key);
        drop(s);
        if !d.allowed {
            log::warn(
                "SMILE write gate closed",
                &[
                    ("code", V::S(d.code.unwrap_or(""))),
                    ("reason", V::S(&d.reason)),
                ],
            );
        } else if d.warn {
            log::warn(
                "SMILE write gate open with a warning",
                &[("reason", V::S(&d.reason))],
            );
        } else if was_logged {
            log::info("SMILE write gate open again", &[]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_writing_actions_are_gated() {
        let gated: Vec<_> = JobAction::ALL.iter().filter(|a| is_gated(**a)).collect();
        assert_eq!(gated.len(), 6);
        assert!(!is_gated(JobAction::EarnPoints) && !is_gated(JobAction::Availability));
    }

    #[test]
    fn denied_ack_uses_the_code_and_the_environment_class() {
        let d = Decision {
            allowed: false,
            code: Some("PMS_IDENTITY_MISMATCH"),
            reason: "db x".into(),
            warn: false,
        };
        let a = denied("j", &d);
        assert_eq!(a.error.as_deref(), Some("PMS_IDENTITY_MISMATCH: db x"));
        assert_eq!(a.error_class, Some("PMS_ENVIRONMENT"));
        let stale = Decision { code: None, ..d };
        assert_eq!(denied("j", &stale).error_code.as_deref(), Some("PMS_STALE"));
    }
}
