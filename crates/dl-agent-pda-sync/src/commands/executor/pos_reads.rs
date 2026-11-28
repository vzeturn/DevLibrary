//! The read-only SMILE_POS verbs (RT-1427, ported from the PDAOrder agent):
//! `pos.revenue_summary` (ADR 0149, RT-0843), `pos.bill_list` (RT-0842) and
//! `pos.order_residue` (RT-1365, ADR 0183). Each answers
//! from SELECTs on its source: no write transaction and no `Pda_SyncCommandLog` row, so a
//! redelivery simply reads again. An invalid payload is the `invalid_payload` rejection
//! that never reaches the source; an error (cancellation, SQL) propagates unchanged, since
//! the command wrote nothing. Without its source a verb is an unknown type.

use std::sync::Arc;

use super::{SiteCommandExecutor, reasons};
use crate::commands::error::Result;
use crate::commands::order_residue::{OrderResidueSource, ResidueCheck, run, sortable};
use crate::commands::pos_bills::PosBillSource;
use crate::commands::pos_revenue::PosRevenueSource;
use crate::contracts::WireJson;
use crate::contracts::wire::{
    PosBillListPayload, PosBillListResult, PosOrderResiduePayload, PosOrderResidueResult,
    PosResidueCheck, PosResidueInfo, PosRevenueSummaryPayload, PosRevenueSummaryResult,
    SyncCommand, SyncCommandResult,
};
use crate::reporting::Value;
use crate::util::ctx::Ctx;

/// The sources of the read-only POS verbs.
#[derive(Default)]
pub(super) struct PosReads {
    revenue: Option<Arc<dyn PosRevenueSource>>,
    bills: Option<Arc<dyn PosBillSource>>,
    residue: Option<Arc<dyn OrderResidueSource>>,
}

impl SiteCommandExecutor {
    /// Runs `pos.revenue_summary` from `source` (without one the type is unknown).
    pub fn with_revenue(mut self, source: Arc<dyn PosRevenueSource>) -> Self {
        self.reads.revenue = Some(source);
        self
    }

    /// Runs `pos.bill_list` from `source` (without one the type is unknown).
    pub fn with_bills(mut self, source: Arc<dyn PosBillSource>) -> Self {
        self.reads.bills = Some(source);
        self
    }

    /// Runs `pos.order_residue` reading from `source` (without one the type is unknown).
    pub fn with_residue(mut self, source: Arc<dyn OrderResidueSource>) -> Self {
        self.reads.residue = Some(source);
        self
    }

    /// Runs `pos.revenue_summary`: Applied with a [`PosRevenueSummaryResult`].
    pub(super) fn pos_revenue(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(
            c,
            "a pos.revenue_summary",
            PosRevenueSummaryPayload::problem,
        ) {
            Ok(p) => p,
            Err(r) => return Ok(*r),
        };
        let Some(source) = self.reads.revenue.clone() else {
            return Ok(self.reject(c, reasons::UNKNOWN_TYPE));
        };
        let days = source.daily(p.rvc_no, &p.date_from, &p.date_to, ctx)?;
        self.log.info(
            "pos.revenue_summary {Id}: RVC {Rvc} {From}..{To} -> {Days} day(s); reason: {Reason}",
            &[
                Value::from(c.command_id),
                Value::from(p.rvc_no),
                Value::from(p.date_from.as_str()),
                Value::from(p.date_to.as_str()),
                Value::from(days.len()),
                p.reason.as_deref().map_or(Value::Null, Value::from),
            ],
        );
        let result = PosRevenueSummaryResult {
            rvc_no: p.rvc_no,
            date_from: p.date_from,
            date_to: p.date_to,
            days,
        };
        Ok(self.applied(c, Some(result.to_element())))
    }

    /// Runs `pos.bill_list`: Applied with a [`PosBillListResult`] of at most
    /// [`PosBillListPayload::MAX_BILLS`] bills, `truncated` when the day has more.
    pub(super) fn pos_bills(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "a pos.bill_list", PosBillListPayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(*r),
        };
        let Some(source) = self.reads.bills.clone() else {
            return Ok(self.reject(c, reasons::UNKNOWN_TYPE));
        };
        let max = PosBillListPayload::MAX_BILLS;
        let mut bills = source.bills(p.rvc_no, &p.business_date, max + 1, ctx)?;
        let truncated = bills.len() > max;
        bills.truncate(max);
        self.log.info(
            "pos.bill_list {Id}: RVC {Rvc} business date {Day} -> {Bills} bill(s), truncated {Truncated}; reason: {Reason}",
            &[
                Value::from(c.command_id),
                Value::from(p.rvc_no),
                Value::from(p.business_date.as_str()),
                Value::from(bills.len()),
                Value::from(truncated),
                p.reason.as_deref().map_or(Value::Null, Value::from),
            ],
        );
        let result = PosBillListResult {
            rvc_no: p.rvc_no,
            business_date: p.business_date,
            bills,
            truncated,
        };
        Ok(self.applied(c, Some(result.to_element())))
    }

    /// Runs `pos.order_residue` (`PosOrderResidueCommandHandler`): the read half of
    /// `order-residue`, never the cleanup; no write transaction and no `Pda_SyncCommandLog`
    /// row. The log carries counts only, never a check number. An error (cancellation, SQL)
    /// propagates: the command wrote nothing, so a redelivery reads again.
    pub(super) fn pos_order_residue(
        &self,
        c: &SyncCommand,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        let Some(source) = self.reads.residue.clone() else {
            return Ok(self.reject(c, reasons::UNKNOWN_TYPE));
        };
        let p = match self.payload(c, "a pos.order_residue", PosOrderResiduePayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(*r),
        };
        let report = run(
            &*source,
            false,
            "pos.order_residue",
            p.min_age_minutes,
            &mut |_| Ok(()),
            ctx,
        )?;
        let o = &report.ours;
        let max = PosOrderResiduePayload::MAX_ROWS;
        let truncated = o.empty_open_checks.len() > max
            || o.orphan_info_rows.len() > max
            || o.stale_open_logs.len() > max
            || o.orphan_send_audits.len() > max;
        let check = |c: &ResidueCheck| PosResidueCheck {
            rvc_no: c.rvc_no,
            check_no: c.check_no,
            opened_local: sortable(c.opened_local),
        };
        let result = PosOrderResidueResult {
            min_age_minutes: p.min_age_minutes,
            empty_open_checks: o.empty_open_checks.iter().take(max).map(check).collect(),
            orphan_info_rows: o
                .orphan_info_rows
                .iter()
                .take(max)
                .map(|r| PosResidueInfo {
                    check_no: r.check_no,
                    trn_no: r.trn_no,
                    sent: r.sent,
                })
                .collect(),
            stale_open_logs: o.stale_open_logs.iter().take(max).map(check).collect(),
            orphan_send_audits: o.orphan_send_audits.iter().take(max).copied().collect(),
            other_rows_ignored: report.other_rows_ignored,
            sent_orphan_info_rows: report.sent_orphan_info_rows,
            truncated,
        };
        self.log.info(
            "pos.order_residue {Id}: reason {Reason}: {Empty} empty check(s), {Info} orphan info row(s), {Logs} stale open-log row(s), {Audits} send audit(s), {Ignored} ignored",
            &[
                Value::from(c.command_id),
                p.reason.as_deref().map_or(Value::Null, Value::from),
                Value::from(o.empty_open_checks.len()),
                Value::from(o.orphan_info_rows.len()),
                Value::from(o.stale_open_logs.len()),
                Value::from(o.orphan_send_audits.len()),
                Value::from(report.other_rows_ignored),
            ],
        );
        Ok(self.applied(c, Some(result.to_element())))
    }
}
