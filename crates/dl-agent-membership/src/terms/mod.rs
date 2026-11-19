//! `ExtraBedTermsSync` (`Sync/ExtraBedTermsSync.cs`, `.Catalog.cs`): SMILE's extra-bed terms
//! and its `SpecialService` catalogue reported to the hub (`POST api/agent/pms/extra-bed-terms`,
//! `POST api/agent/pms/special-services`), read-only towards SMILE. Read at most every
//! 300 s; posted when the hash changed and as a full snapshot at least every 24 h, so the
//! hub's `PMS_EXTRA_BED_TERMS_STALE` (26 h) stays quiet. A SMILE or hub failure is logged
//! and retried on the next due cycle; it never stops the caller.
//!
//! The hub's chosen special (`RefreshHubSpecialCodeAsync`) is not fetched here: the sync
//! worker already reads sync-config every 60 s and records the choice in
//! [`crate::writer::special_rows::HubSpecialCode`].

pub mod catalog;

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::hub::HubApi;
use crate::jobs::bed_terms::{ExtraBedTerms, wire_content_hash};
use crate::jobs::beds::{
    FOLD_FINGERPRINT_UNVERIFIED, FOLD_SPECIAL_BY_DATE_OFF, FOLD_SPECIAL_MISSING,
    FOLD_SPECIAL_UNUSABLE, MODE_FOLD_LEGACY, MODE_SMILE_SPECIAL,
};
use crate::jobs::plan::WriteError;
use crate::log::{self, V};
use crate::smile::TdsSmile;
use crate::stj::J;
use crate::writer::bed_terms_reader::{self, TermsRead};
use crate::writer::special_rows::HubSpecialCode;
use catalog::Catalog;

/// `ReadInterval`.
pub const READ_INTERVAL: Duration = Duration::from_secs(300);
/// `FullSnapshotInterval`.
pub const FULL_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// `AgentExtraBedTermsRequest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermsRequest {
    /// The terms read.
    pub terms: ExtraBedTerms,
    /// `ContentHash` (the wire hash).
    pub wire_hash: String,
    /// `ModeConfigured`.
    pub mode_configured: String,
    /// `ModeEffective`.
    pub mode_effective: &'static str,
    /// `ModeReason`.
    pub mode_reason: Option<&'static str>,
    /// `IsFullSnapshot`.
    pub is_full_snapshot: bool,
    /// `SpecialEarnCovered`.
    pub special_earn_covered: Option<bool>,
}

impl TermsRequest {
    /// The wire body.
    pub fn to_j(&self) -> J {
        let t = &self.terms;
        let sp = t.special.as_ref();
        J::Obj(vec![
            ("SmileHotelId", J::str(&t.smile_hotel_id)),
            ("SurchargeAmount", J::opt_dec(t.surcharge_amount)),
            ("TrnCode", J::opt_str(t.trn_code.as_deref())),
            ("TrnSub", J::opt_str(t.trn_sub.as_deref())),
            ("StockTotal", J::opt_int(t.stock_total.map(i64::from))),
            ("StructuralFingerprint", J::str(&t.structural_fingerprint)),
            ("ContentHash", J::str(&self.wire_hash)),
            ("ModeConfigured", J::str(&self.mode_configured)),
            ("ModeEffective", J::str(self.mode_effective)),
            ("ModeReason", J::opt_str(self.mode_reason)),
            ("IsFullSnapshot", J::Bool(self.is_full_snapshot)),
            ("SmileHotelDate", J::opt_date(t.smile_hotel_date)),
            ("SpecialCode", J::opt_str(t.special_code())),
            (
                "SpecialTrnCode",
                J::opt_str(sp.and_then(|s| s.trn_code.as_deref())),
            ),
            (
                "SpecialTrnSub",
                J::opt_str(sp.and_then(|s| s.trn_sub.as_deref())),
            ),
            ("SpecialByDate", J::opt_bool(t.special_by_date)),
            (
                "SpecialTaxLabel",
                J::opt_str(t.special_tax_label().as_deref()),
            ),
            (
                "SpecialMatchesRoomTax",
                J::opt_bool(t.special_matches_room_tax()),
            ),
            ("SpecialEarnCovered", J::opt_bool(self.special_earn_covered)),
        ])
    }
}

/// `AgentSpecialServiceCatalogRequest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRequest {
    /// The catalogue read.
    pub catalog: Catalog,
    /// `IsFullSnapshot`.
    pub is_full_snapshot: bool,
}

impl CatalogRequest {
    /// The wire body.
    pub fn to_j(&self) -> J {
        J::Obj(vec![
            ("ContentHash", J::str(&self.catalog.content_hash)),
            (
                "RateCodePlusPlus",
                J::opt_bool(self.catalog.rate_code_plus_plus),
            ),
            ("IsFullSnapshot", J::Bool(self.is_full_snapshot)),
            (
                "Items",
                J::Arr(
                    self.catalog
                        .items
                        .iter()
                        .map(catalog::SpecialItem::to_j)
                        .collect(),
                ),
            ),
        ])
    }
}

/// `DecideMode`: the mode this agent can run in for these terms. The report cannot see
/// the hub's verified fingerprint, so an agent-priced special reports `FOLD_LEGACY` with
/// `FINGERPRINT_UNVERIFIED`; a PMS-priced one (ADR-0109) runs `SMILE_SPECIAL`.
pub fn decide_mode(
    configured: &str,
    terms: &ExtraBedTerms,
) -> (&'static str, Option<&'static str>) {
    if configured != MODE_SMILE_SPECIAL {
        return (MODE_FOLD_LEGACY, None);
    }
    if terms.special.is_none() {
        return (MODE_FOLD_LEGACY, Some(FOLD_SPECIAL_MISSING));
    }
    if !terms.special_usable() || !terms.special_priceable() {
        return (MODE_FOLD_LEGACY, Some(FOLD_SPECIAL_UNUSABLE));
    }
    if terms.special_by_date != Some(true) {
        return (MODE_FOLD_LEGACY, Some(FOLD_SPECIAL_BY_DATE_OFF));
    }
    if terms.pms_priced {
        (MODE_SMILE_SPECIAL, None)
    } else {
        (MODE_FOLD_LEGACY, Some(FOLD_FINGERPRINT_UNVERIFIED))
    }
}

/// `SpecialEarnCovered` (rule 6): a bed SMILE posts under the special earns points.
/// Override mode (`Earn:ChargeCodes` set): its `TrnCode` is one of the codes. Derived
/// mode: posted as its own line with a `TransactionCode` row.
pub fn special_earn_covered(terms: &ExtraBedTerms, earn_charge_codes: &[String]) -> Option<bool> {
    let special = terms.special.as_ref()?;
    Some(special.trn_code.as_deref().is_some_and(|code| {
        if earn_charge_codes.is_empty() {
            special.include_in_room_charge == Some(0) && terms.special_tax.is_some()
        } else {
            earn_charge_codes
                .iter()
                .any(|c| c.eq_ignore_ascii_case(code))
        }
    }))
}

/// What the sync reads its settings from.
#[derive(Debug, Clone)]
pub struct TermsOptions {
    /// `Smile:ExtraBedWriteMode`.
    pub mode: String,
    /// `Smile:ExtraBedSpecialCode`.
    pub configured_code: String,
    /// The hub's sync-config choice.
    pub hub_code: HubSpecialCode,
    /// `Smile:SmileHotelId`.
    pub smile_hotel_id: Option<String>,
    /// `Earn:ChargeCodes`.
    pub earn_charge_codes: Vec<String>,
}

/// Where the sync reads SMILE from: one session for the terms and the catalogue.
pub trait TermsSource: Send + Sync {
    /// The terms of `code`, and the catalogue (its own failure kept apart).
    fn read(&self, hotel_id: Option<&str>, code: &str) -> Result<Readings, WriteError>;
}

/// The terms read and the catalogue read (the catalogue may fail on its own).
pub type Readings = (TermsRead, Result<Option<Catalog>, WriteError>);

/// Both reads on one SMILE session.
pub fn read_both(
    s: &mut dyn crate::writer::session::Session,
    hotel_id: Option<&str>,
    code: &str,
) -> Result<Readings, WriteError> {
    let read = bed_terms_reader::read(s, hotel_id, code)?;
    let pp = read.terms.as_ref().and_then(|t| t.rate_code_plus_plus);
    let catalog = catalog::read(s, pp);
    Ok((read, catalog))
}

impl TermsSource for TdsSmile {
    fn read(&self, hotel_id: Option<&str>, code: &str) -> Result<Readings, WriteError> {
        self.session(None, |c| read_both(c, hotel_id, code))
    }
}

/// `ExtraBedTermsSync`: in-memory state; a restart posts once more (the hub upserts).
pub struct TermsSync {
    smile: Arc<dyn TermsSource>,
    hub: Arc<dyn HubApi>,
    opts: TermsOptions,
    last_read_at: Option<Instant>,
    last_posted_at: Option<Instant>,
    last_posted_hash: Option<String>,
    last_skip_reason: Option<String>,
    warned_no_terms_endpoint: bool,
    last_catalog_hash: Option<String>,
    last_catalog_at: Option<Instant>,
    warned_no_catalog_endpoint: bool,
}

impl std::fmt::Debug for TermsSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TermsSync").finish_non_exhaustive()
    }
}

fn full_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= FULL_SNAPSHOT_INTERVAL)
}

impl TermsSync {
    /// A sync reading `smile` and posting to `hub`.
    pub fn new(smile: Arc<dyn TermsSource>, hub: Arc<dyn HubApi>, opts: TermsOptions) -> TermsSync {
        TermsSync {
            smile,
            hub,
            opts,
            last_read_at: None,
            last_posted_at: None,
            last_posted_hash: None,
            last_skip_reason: None,
            warned_no_terms_endpoint: false,
            last_catalog_hash: None,
            last_catalog_at: None,
            warned_no_catalog_endpoint: false,
        }
    }

    /// `RunIfDueAsync`: one cycle when due; `true` when the terms were posted. Never fails:
    /// a SMILE or hub error is logged and the next due cycle reads again.
    pub fn run_if_due(&mut self, now: Instant) -> bool {
        if self
            .last_read_at
            .is_some_and(|t| now.saturating_duration_since(t) < READ_INTERVAL)
        {
            return false;
        }
        self.last_read_at = Some(now);
        match self.run(now) {
            Ok(posted) => posted,
            Err(e) => {
                log::warn(
                    "reading or reporting the SMILE extra-bed terms failed; retried in 300 s",
                    &[("error", V::S(&e.to_string()))],
                );
                false
            }
        }
    }

    fn run(&mut self, now: Instant) -> Result<bool, WriteError> {
        let hub_code = self.opts.hub_code.get();
        let code = hub_code
            .clone()
            .unwrap_or_else(|| self.opts.configured_code.clone());
        let hotel = self.opts.smile_hotel_id.clone();
        let (mut read, catalog) = self.smile.read(hotel.as_deref(), &code)?;
        self.report_catalog(catalog, now);
        if hub_code.is_some() {
            read.terms = read.terms.map(|t| t.as_pms_priced());
        }
        let Some(terms) = read.terms else {
            if read.skip_reason != self.last_skip_reason {
                log::warn(
                    "SMILE extra-bed terms not reported",
                    &[("reason", V::S(read.skip_reason.as_deref().unwrap_or("")))],
                );
            }
            self.last_skip_reason = read.skip_reason;
            return Ok(false);
        };
        self.last_skip_reason = None;
        let (effective, reason) = decide_mode(&self.opts.mode, &terms);
        let earn = special_earn_covered(&terms, &self.opts.earn_charge_codes);
        let wire_hash = wire_content_hash(
            &terms.content_hash,
            &self.opts.mode,
            effective,
            reason,
            earn,
        );
        let full = full_due(self.last_posted_at, now);
        if !full && self.last_posted_hash.as_deref() == Some(wire_hash.as_str()) {
            return Ok(false);
        }
        for w in &terms.warnings {
            log::warn("SMILE extra-bed terms", &[("warning", V::S(w))]);
        }
        let req = TermsRequest {
            terms,
            wire_hash: wire_hash.clone(),
            mode_configured: self.opts.mode.clone(),
            mode_effective: effective,
            mode_reason: reason,
            is_full_snapshot: full,
            special_earn_covered: earn,
        };
        if !self.hub.post_extra_bed_terms(&req)? {
            if !self.warned_no_terms_endpoint {
                log::info(
                    "the hub has no api/agent/pms/extra-bed-terms yet (older hub); retried later",
                    &[],
                );
            }
            self.warned_no_terms_endpoint = true;
            return Ok(false);
        }
        self.warned_no_terms_endpoint = false;
        self.last_posted_at = Some(now);
        self.last_posted_hash = Some(wire_hash);
        let t = &req.terms;
        log::info(
            "SMILE extra-bed terms reported",
            &[
                ("hotel_id", V::S(&t.smile_hotel_id)),
                ("special", V::S(t.special_code().unwrap_or("-"))),
                ("special_by_date", V::S(&format!("{:?}", t.special_by_date))),
                ("stock", V::S(&format!("{:?}", t.stock_total))),
                ("mode_configured", V::S(&self.opts.mode)),
                ("mode_effective", V::S(effective)),
                ("mode_reason", V::S(reason.unwrap_or("-"))),
                ("full", V::B(full)),
            ],
        );
        Ok(true)
    }

    /// `ReportCatalogAsync`: best effort, never stops the terms report.
    fn report_catalog(&mut self, read: Result<Option<Catalog>, WriteError>, now: Instant) {
        let catalog = match read {
            Ok(Some(c)) => c,
            Ok(None) => return,
            Err(e) => {
                log::warn(
                    "reading the SMILE SpecialService catalogue failed; retried in 300 s",
                    &[("error", V::S(&e.to_string()))],
                );
                return;
            }
        };
        let full = full_due(self.last_catalog_at, now);
        if !full && self.last_catalog_hash.as_deref() == Some(catalog.content_hash.as_str()) {
            return;
        }
        let count = catalog.items.len();
        let hash = catalog.content_hash.clone();
        let req = CatalogRequest {
            catalog,
            is_full_snapshot: full,
        };
        match self.hub.post_special_services(&req) {
            Ok(true) => {
                self.warned_no_catalog_endpoint = false;
                self.last_catalog_at = Some(now);
                self.last_catalog_hash = Some(hash);
                log::info(
                    "SMILE SpecialService catalogue reported",
                    &[("count", V::I(count as i64)), ("full", V::B(full))],
                );
            }
            Ok(false) => {
                if !self.warned_no_catalog_endpoint {
                    log::info(
                        "the hub has no api/agent/pms/special-services yet (older hub); retried later",
                        &[],
                    );
                }
                self.warned_no_catalog_endpoint = true;
            }
            Err(e) => log::warn(
                "reporting the SMILE SpecialService catalogue failed; retried in 300 s",
                &[("error", V::S(&e.to_string()))],
            ),
        }
    }
}

#[cfg(test)]
#[path = "terms_tests.rs"]
mod tests;
