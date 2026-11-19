//! The earn routes of `HubClient`: `earn`, `earn/walk-in`, `bookings/pending-checkout`,
//! `bookings/pms-cancelled`, `folio-revenue` and `checkout-scan-summary`.
//!
//! A 202 on `earn` / `earn/walk-in` is `HeldAsync`: the hub keeps the report because the
//! source SMILE database is not confirmed yet. It counts as accepted and is never resent
//! (a held walk-in reads as `NoPoints`).

use dl_uuid::Uuid;

use super::{HubClient, HubError, ensure_ok, ensure_success};
use crate::earn::wire::{
    self, EarnReport, PendingBooking, RevenueLine, ScanSummary, WalkInEarnReport, WalkInOutcome,
    WalkInResult,
};
use crate::log::{self, V};
use crate::wire::parse::{self as p, str_prop};

/// What the earn paths need from the hub. [`HubClient`] is the real one.
pub trait EarnHub: Send + Sync {
    /// `ReportEarnAsync` (204; 202 = held).
    fn report_earn(&self, report: &EarnReport) -> Result<(), HubError>;
    /// `ReportWalkInEarnAsync` (200 + result; 202 = held, read as `NoPoints`).
    fn report_walk_in_earn(&self, report: &WalkInEarnReport) -> Result<WalkInResult, HubError>;
    /// `GetPendingCheckoutAsync`.
    fn pending_checkout(&self, max: u32, grace_days: u32) -> Result<Vec<PendingBooking>, HubError>;
    /// `ReportPmsCancelledAsync`: `None` when the hub is older than the route
    /// (401/404/405).
    fn report_pms_cancelled(
        &self,
        booking_id: Uuid,
        confirmation_code: &str,
    ) -> Result<Option<bool>, HubError>;
    /// `ReportFolioRevenueAsync` (200/204).
    fn report_folio_revenue(&self, folio_ref: &str, lines: &[RevenueLine]) -> Result<(), HubError>;
    /// `ReportCheckoutScanSummaryAsync` (200/204).
    fn report_scan_summary(&self, summary: &ScanSummary) -> Result<(), HubError>;
}

/// `HeldAsync`: logs the held report (outcome and identity id from the body when it reads).
fn held(op: &str, body: &[u8]) {
    let parsed = p::json(body).ok();
    let outcome = parsed
        .as_ref()
        .and_then(|v| str_prop(v, "Outcome"))
        .unwrap_or_else(|| "?".into());
    let identity = parsed
        .as_ref()
        .and_then(|v| str_prop(v, "IdentityId"))
        .unwrap_or_default();
    log::info(
        "hub held the report: the source SMILE database is not confirmed yet; not resending",
        &[
            ("op", V::S(op)),
            ("outcome", V::S(&outcome)),
            ("identity_id", V::S(&identity)),
        ],
    );
}

impl EarnHub for HubClient {
    fn report_earn(&self, report: &EarnReport) -> Result<(), HubError> {
        let resp = self.send("earn", "api/agent/earn", Some(report.to_j().to_json()))?;
        if resp.status.as_u16() == 202 {
            held("earn", &resp.body);
            return Ok(());
        }
        ensure_ok(&resp, "earn")
    }

    fn report_walk_in_earn(&self, report: &WalkInEarnReport) -> Result<WalkInResult, HubError> {
        let resp = self.send(
            "earn/walk-in",
            "api/agent/earn/walk-in",
            Some(report.to_j().to_json()),
        )?;
        if resp.status.as_u16() == 202 {
            held("earn/walk-in", &resp.body);
            return Ok(WalkInResult {
                outcome: WalkInOutcome::NoPoints,
                points_awarded: 0,
            });
        }
        ensure_success(&resp, "earn/walk-in")?;
        Ok(wire::walk_in_result(&resp.body)?)
    }

    fn pending_checkout(&self, max: u32, grace_days: u32) -> Result<Vec<PendingBooking>, HubError> {
        let resp = self.send(
            "bookings/pending-checkout",
            &format!("api/agent/bookings/pending-checkout?max={max}&graceDays={grace_days}"),
            None,
        )?;
        ensure_success(&resp, "bookings/pending-checkout")?;
        Ok(wire::pending_bookings(&resp.body)?)
    }

    fn report_pms_cancelled(
        &self,
        booking_id: Uuid,
        confirmation_code: &str,
    ) -> Result<Option<bool>, HubError> {
        let resp = self.send(
            "bookings/pms-cancelled",
            "api/agent/bookings/pms-cancelled",
            Some(wire::pms_cancel_j(booking_id, confirmation_code).to_json()),
        )?;
        if matches!(resp.status.as_u16(), 401 | 404 | 405) {
            return Ok(None);
        }
        ensure_success(&resp, "bookings/pms-cancelled")?;
        Ok(Some(wire::pms_cancel_result(&resp.body)?))
    }

    fn report_folio_revenue(&self, folio_ref: &str, lines: &[RevenueLine]) -> Result<(), HubError> {
        let resp = self.send(
            "folio-revenue",
            "api/agent/folio-revenue",
            Some(wire::folio_revenue_j(folio_ref, lines).to_json()),
        )?;
        ensure_ok(&resp, "folio-revenue")
    }

    fn report_scan_summary(&self, summary: &ScanSummary) -> Result<(), HubError> {
        let resp = self.send(
            "checkout-scan-summary",
            "api/agent/checkout-scan-summary",
            Some(summary.to_j().to_json()),
        )?;
        ensure_ok(&resp, "checkout-scan-summary")
    }
}
