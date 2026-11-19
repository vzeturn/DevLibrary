//! `EarnPairSource.ResolveAsync` for the earn paths (job, checkout, walk-in): the earn
//! pairs of a booking or walk-in scope, or `None` when SMILE does not say which code is
//! the room charge (`PMS_EARN_ROOM_CODE_AMBIGUOUS`, logged; the caller earns nothing).
//!
//! The 24 h drift check (`CheckDriftAsync`) is not ported: it only logs a warning and
//! never changes the set (as in the folio-observation scanner).

use std::sync::Mutex;

use super::sql::{EarnDb, FolioScope, room_charge_trn};
use crate::folio::earn::{self, EarnPair};
use crate::log::{self, V};
use crate::smile::SmileError;

/// `EarnOptions` and the extra-bed code the catalogue read needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EarnSettings {
    /// `Earn:ChargeCodes` (empty: derived from SMILE).
    pub charge_codes: Vec<String>,
    /// `Earn:MemberDiscountCode` (empty: disabled).
    pub member_discount_code: String,
    /// `Smile:ExtraBedSpecialCode`, the fallback of the hub's choice.
    pub extra_bed_special_code: String,
}

/// `EarnPairSource`: one per earn path; remembers the last set it logged.
#[derive(Debug, Default)]
pub struct PairSource {
    settings: EarnSettings,
    last_logged: Mutex<Option<String>>,
}

impl PairSource {
    /// A source for `settings`.
    pub fn new(settings: EarnSettings) -> PairSource {
        PairSource {
            settings,
            last_logged: Mutex::new(None),
        }
    }

    /// The settings.
    pub fn settings(&self) -> &EarnSettings {
        &self.settings
    }

    /// `ResolveAsync`. `hub_extra_bed_code` is the hub's sync-config choice
    /// (`EffectiveExtraBedSpecialCode`).
    ///
    /// # Errors
    /// A SMILE read failed.
    pub fn resolve(
        &self,
        db: &mut dyn EarnDb,
        scope: &FolioScope,
        hub_extra_bed_code: Option<&str>,
    ) -> Result<Option<Vec<EarnPair>>, SmileError> {
        let set = match earn::from_override(&self.settings.charge_codes) {
            Some(set) => {
                log::warn(
                    "PMS_EARN_CHARGE_CODE_OVERRIDE: Earn:ChargeCodes replaces the earn codes derived from SMILE; every sub-code of these main codes earns",
                    &[("codes", V::S(&self.settings.charge_codes.join(",")))],
                );
                set
            }
            None => {
                let rm = room_charge_trn(db, scope)?;
                let code = hub_extra_bed_code
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .unwrap_or(&self.settings.extra_bed_special_code)
                    .to_owned();
                let catalog = db.earn_catalog(&code)?;
                let set = earn::resolve(&catalog, &rm);
                if set.is_ambiguous() {
                    log::error(
                        "PMS_EARN_ROOM_CODE_AMBIGUOUS: SMILE must have exactly one TransactionCode row with AutoPost = 1; no earn this cycle, the folios stay pending",
                        &[("auto_post_rows", V::I(set.auto_post_count as i64))],
                    );
                    return Ok(None);
                }
                self.log_on_change(&set);
                set
            }
        };
        Ok(Some(earn::with_member_discount(
            set.pairs,
            &self.settings.member_discount_code,
        )))
    }

    fn log_on_change(&self, set: &earn::EarnPairSet) {
        let description = format!("{} | {}", set.describe(), set.dropped.join(","));
        {
            let mut last = self
                .last_logged
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if last.as_deref() == Some(description.as_str()) {
                return;
            }
            *last = Some(description);
        }
        let dropped = if set.dropped.is_empty() {
            "-".to_owned()
        } else {
            set.dropped.join(",")
        };
        log::info(
            "earn codes derived from the SMILE configuration",
            &[
                ("pairs", V::S(&set.describe())),
                ("dropped", V::S(&dropped)),
            ],
        );
    }
}
