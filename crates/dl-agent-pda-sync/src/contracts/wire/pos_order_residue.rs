//! `pos.order_residue` contract (`Wire/PosOrderResidue.cs`, RT-1365, ADR 0182 follow-up): the
//! read-only reconcile of the order rows the agent's commands left in SMILE_POS.

use crate::contracts::json::{self, WireJson};
use crate::contracts::utf16_len;
use crate::util::stj::{Obj, ReadError, Writer};

/// Payload of `pos.order_residue` (`PosOrderResiduePayload`): `min_age_minutes` is how old an
/// empty open check must be to be listed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PosOrderResiduePayload {
    /// Minimum age of a listed empty open check, in minutes.
    pub min_age_minutes: i32,
    /// Why the cloud asks.
    pub reason: Option<String>,
}

impl PosOrderResiduePayload {
    /// `DefaultMinAgeMinutes`.
    pub const DEFAULT_MIN_AGE_MINUTES: i32 = 120;
    /// `MaxMinAgeMinutes` (one week).
    pub const MAX_MIN_AGE_MINUTES: i32 = 10080;
    /// Longest reason, in UTF-16 units.
    pub const MAX_REASON: usize = 300;
    /// Rows per list in the answer; a bigger residue answers the first ones, `truncated`.
    pub const MAX_ROWS: usize = 200;
    /// Aggregate key of this command: reads never order against a bill.
    pub const AGGREGATE: &str = "pos-residue";

    /// The validation problem, `None` when valid (`PosOrderResiduePayload.Problem`, same text).
    pub fn problem(&self) -> Option<&'static str> {
        if !(1..=Self::MAX_MIN_AGE_MINUTES).contains(&self.min_age_minutes) {
            return Some("min_age_minutes must be 1..10080");
        }
        match &self.reason {
            Some(r) if utf16_len(r) > Self::MAX_REASON => {
                Some("reason must be at most 300 characters")
            }
            _ => None,
        }
    }
}

impl WireJson for PosOrderResiduePayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("min_age_minutes").int(self.min_age_minutes.into());
        json::opt_str(w, "reason", &self.reason);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PosOrderResiduePayload {
            min_age_minutes: o.int("min_age_minutes")?,
            reason: o.opt_string("reason")?,
        })
    }
}

/// An open check of the result (`PosResidueCheck`): RVC, check number and the store's own
/// opening time (ISO, no zone).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PosResidueCheck {
    /// RVC.
    pub rvc_no: i32,
    /// Check number.
    pub check_no: i64,
    /// `yyyy-MM-ddTHH:mm:ss`, store local time.
    pub opened_local: String,
}

impl WireJson for PosResidueCheck {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("check_no").int(self.check_no);
        w.key("opened_local").string(&self.opened_local);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PosResidueCheck {
            rvc_no: o.int("rvc_no")?,
            check_no: o.int("check_no")?,
            opened_local: o.string("opened_local")?,
        })
    }
}

/// A `tmpTrnInfo` row whose `tmpTrn` line is gone (`PosResidueInfo`); `sent` rows carry the
/// kitchen void chit.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PosResidueInfo {
    /// Check number.
    pub check_no: i64,
    /// `TrnNo` (the gone line's `TrnSeq`).
    pub trn_no: i64,
    /// Already sent to the kitchen.
    pub sent: bool,
}

impl WireJson for PosResidueInfo {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("check_no").int(self.check_no);
        w.key("trn_no").int(self.trn_no);
        w.key("sent").bool(self.sent);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PosResidueInfo {
            check_no: o.int("check_no")?,
            trn_no: o.int("trn_no")?,
            sent: o.bool("sent")?,
        })
    }
}

/// Result of `pos.order_residue` (`PosOrderResidueResult`): only rows of checks this agent's
/// commands named, by kind, plus how many other rows were ignored. Check and line numbers
/// only: no item, price, table, cashier or guest.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PosOrderResidueResult {
    /// The age asked.
    pub min_age_minutes: i32,
    /// Empty open checks opened by the PDA workstation.
    pub empty_open_checks: Vec<PosResidueCheck>,
    /// `tmpTrnInfo` rows without a `tmpTrn` line.
    pub orphan_info_rows: Vec<PosResidueInfo>,
    /// `checkopenlog` rows still open for a check that no longer exists.
    pub stale_open_logs: Vec<PosResidueCheck>,
    /// `PDASendOrder` check numbers of a check that no longer exists.
    pub orphan_send_audits: Vec<i64>,
    /// Rows of checks POS WIN opened by itself.
    pub other_rows_ignored: i32,
    /// Orphan info rows already sent (kept for the void chit).
    pub sent_orphan_info_rows: i32,
    /// A list held more than [`PosOrderResiduePayload::MAX_ROWS`] rows.
    pub truncated: bool,
}

impl WireJson for PosOrderResidueResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("min_age_minutes").int(self.min_age_minutes.into());
        w.key("empty_open_checks");
        json::write_items(w, &self.empty_open_checks);
        w.key("orphan_info_rows");
        json::write_items(w, &self.orphan_info_rows);
        w.key("stale_open_logs");
        json::write_items(w, &self.stale_open_logs);
        w.key("orphan_send_audits");
        json::write_ints(w, &self.orphan_send_audits);
        w.key("other_rows_ignored")
            .int(self.other_rows_ignored.into());
        w.key("sent_orphan_info_rows")
            .int(self.sent_orphan_info_rows.into());
        w.key("truncated").bool(self.truncated);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PosOrderResidueResult {
            min_age_minutes: o.int("min_age_minutes")?,
            empty_open_checks: o.list("empty_open_checks", json::item)?,
            orphan_info_rows: o.list("orphan_info_rows", json::item)?,
            stale_open_logs: o.list("stale_open_logs", json::item)?,
            orphan_send_audits: o.list("orphan_send_audits", json::int_item)?,
            other_rows_ignored: o.int("other_rows_ignored")?,
            sent_orphan_info_rows: o.int("sent_orphan_info_rows")?,
            truncated: o.bool("truncated")?,
        })
    }
}
