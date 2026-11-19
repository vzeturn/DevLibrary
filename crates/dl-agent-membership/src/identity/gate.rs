//! `PmsWriteGate.Decide` and `GatedPush`: whether the availability and room-inventory
//! pushes may go to the hub, given the hub's sync-config (baseline identity, write gate)
//! and the current identity reading.
//!
//! The same decision guards SMILE writes ([`crate::jobs::gate::WriteGate`]).

use std::sync::Mutex;

use super::{Snapshot, same_ignoring_missing_guid};
use crate::identity::ForeignState;
use crate::wire::parse::SyncConfig;

/// `PMS_IDENTITY_MISMATCH`.
pub const IDENTITY_MISMATCH: &str = "PMS_IDENTITY_MISMATCH";
/// `PMS_STALE`.
pub const STALE: &str = "PMS_STALE";

/// `PmsWriteGateDecision`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// Whether the push (or write) may go ahead.
    pub allowed: bool,
    /// The closing code, when closed.
    pub code: Option<&'static str>,
    /// A log-safe reason (fingerprints and ages, no guest data).
    pub reason: String,
    /// Allowed, but worth a warning.
    pub warn: bool,
}

impl Decision {
    fn open() -> Decision {
        Decision {
            allowed: true,
            code: None,
            reason: "write gate open".into(),
            warn: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Off,
    AlertOnly,
    Block,
}

fn mode(m: Option<&str>) -> Mode {
    match m.map(|s| s.trim().to_uppercase()).as_deref() {
        Some("BLOCK") => Mode::Block,
        Some("ALERTONLY" | "ALERT_ONLY") => Mode::AlertOnly,
        _ => Mode::Off,
    }
}

/// `PmsWriteGate.Decide`.
pub fn decide(
    config: Option<&SyncConfig>,
    current: Option<&Snapshot>,
    foreign_age_minutes: Option<i64>,
) -> Decision {
    let Some(config) = config else {
        return Decision::open();
    };
    let mut warning = None;
    if let Some(baseline) = &config.baseline_identity {
        match current {
            None => {
                warning = Some(Decision {
                    allowed: true,
                    code: None,
                    reason: "SMILE identity not read yet; not compared with the baseline".into(),
                    warn: true,
                });
            }
            Some(cur) => {
                let expected = super::fingerprint(baseline);
                let seen = cur.fingerprint();
                if expected != seen && !same_ignoring_missing_guid(&expected, &seen) {
                    return Decision {
                        allowed: false,
                        code: Some(IDENTITY_MISMATCH),
                        reason: format!(
                            "SMILE database {seen} is not the hotel's baseline {expected}"
                        ),
                        warn: false,
                    };
                }
            }
        }
    }
    if let (Some(gate), Some(cur)) = (&config.write_gate, current) {
        let m = mode(gate.mode.as_deref());
        if m != Mode::Off {
            let threshold = gate.threshold_hours.max(1) * 60;
            let stale = match cur.foreign_state {
                ForeignState::NoneInWindow => true,
                ForeignState::Known => foreign_age_minutes.is_some_and(|a| a > threshold),
                ForeignState::Unreadable => false,
            };
            if stale {
                let age = foreign_age_minutes
                    .map_or_else(|| "over 30 days".to_owned(), |a| format!("{} h", a / 60));
                let reason = format!(
                    "no SMILE activity outside the agent for {age} (threshold {} h)",
                    gate.threshold_hours
                );
                return if m == Mode::Block {
                    Decision {
                        allowed: false,
                        code: Some(STALE),
                        reason,
                        warn: false,
                    }
                } else {
                    Decision {
                        allowed: true,
                        code: None,
                        reason: reason + "; alert only",
                        warn: true,
                    }
                };
            }
        }
    }
    warning.unwrap_or_else(Decision::open)
}

/// `GatedPush`: one push's view of the gate, logging only when it opens or closes.
#[derive(Debug)]
pub struct GatedPush {
    push: &'static str,
    closed: Mutex<Option<&'static str>>,
}

impl GatedPush {
    /// A gate for `push` (`availability`, `room-inventory`).
    pub fn new(push: &'static str) -> GatedPush {
        GatedPush {
            push,
            closed: Mutex::new(None),
        }
    }

    /// Whether the push may go now.
    pub fn allows(
        &self,
        config: &SyncConfig,
        current: Option<&Snapshot>,
        foreign_age_minutes: Option<i64>,
    ) -> bool {
        let d = decide(Some(config), current, foreign_age_minutes);
        let mut closed = self
            .closed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !d.allowed {
            if *closed != d.code {
                crate::log::info(
                    "push to the hub skipped: write gate closed",
                    &[
                        ("push", crate::log::V::S(self.push)),
                        ("code", crate::log::V::S(d.code.unwrap_or(""))),
                        ("reason", crate::log::V::S(&d.reason)),
                    ],
                );
            }
            *closed = d.code;
            return false;
        }
        if closed.take().is_some() {
            crate::log::info(
                "write gate open again",
                &[("push", crate::log::V::S(self.push))],
            );
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use dl_time::Date;

    use super::*;
    use crate::wire::DbIdentity;
    use crate::wire::parse::WriteGate;

    fn snap(db: &str, state: ForeignState) -> Snapshot {
        let d = Date::new(2019, 5, 6).unwrap().midnight();
        Snapshot {
            identity: DbIdentity {
                server: "srv".into(),
                database: db.into(),
                create_date: d,
                database_guid: None,
                family_guid: None,
                service_broker_guid: None,
            },
            pms_now: d,
            last_foreign_change: None,
            foreign_state: state,
        }
    }

    #[test]
    fn baseline_mismatch_closes_and_stale_follows_the_mode() {
        let cur = snap("db", ForeignState::Known);
        let mut cfg = SyncConfig {
            baseline_identity: Some(snap("other", ForeignState::Known).identity),
            ..SyncConfig::default()
        };
        assert_eq!(
            decide(Some(&cfg), Some(&cur), None).code,
            Some(IDENTITY_MISMATCH)
        );
        assert!(
            decide(Some(&cfg), None, None).warn,
            "no reading: allowed with a warning"
        );
        cfg.baseline_identity = Some(cur.identity.clone());
        assert!(decide(Some(&cfg), Some(&cur), None).allowed);
        cfg.write_gate = Some(WriteGate {
            mode: Some("block".into()),
            threshold_hours: 2,
        });
        assert!(
            decide(Some(&cfg), Some(&cur), Some(120)).allowed,
            "at threshold"
        );
        assert_eq!(decide(Some(&cfg), Some(&cur), Some(121)).code, Some(STALE));
        cfg.write_gate.as_mut().unwrap().mode = Some("ALERT_ONLY".into());
        let d = decide(Some(&cfg), Some(&cur), Some(121));
        assert!(d.allowed && d.warn);
        let unreadable = snap("db", ForeignState::Unreadable);
        assert!(decide(Some(&cfg), Some(&unreadable), None).allowed);
        let push = GatedPush::new("availability");
        cfg.write_gate.as_mut().unwrap().mode = Some("BLOCK".into());
        assert!(!push.allows(&cfg, Some(&cur), Some(500)));
        assert!(push.allows(&cfg, Some(&cur), Some(5)));
    }
}
