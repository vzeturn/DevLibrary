//! The read-only store verbs (RT-1427, ported from the PDAOrder agent):
//! `pos.revenue_summary` (ADR 0149, RT-0843), `pos.bill_list` (RT-0842) and the SMILE_BO
//! `pos.stock_balance` (RT-1024, ADR 0165). Each answers
//! from SELECTs on its source: no write transaction and no `Pda_SyncCommandLog` row, so a
//! redelivery simply reads again. An invalid payload is the `invalid_payload` rejection
//! that never reaches the source; an error (cancellation, SQL) propagates unchanged, since
//! the command wrote nothing. Without its source a verb is an unknown type.

use std::sync::Arc;

use super::{SiteCommandExecutor, reasons};
use crate::commands::error::Result;
use crate::commands::pos_bills::PosBillSource;
use crate::commands::pos_revenue::PosRevenueSource;
use crate::commands::pos_stock::PosStockSource;
use crate::contracts::WireJson;
use crate::contracts::wire::{
    PosBillListPayload, PosBillListResult, PosRevenueSummaryPayload, PosRevenueSummaryResult,
    PosStockBalancePayload, PosStockBalanceResult, SyncCommand, SyncCommandResult,
};
use crate::reporting::Value;
use crate::util::ctx::Ctx;

/// The sources of the read-only POS verbs.
#[derive(Default)]
pub(super) struct PosReads {
    revenue: Option<Arc<dyn PosRevenueSource>>,
    bills: Option<Arc<dyn PosBillSource>>,
    stock: Option<Arc<dyn PosStockSource>>,
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

    /// Runs `pos.stock_balance` reading from `source` (without one the type is unknown).
    pub fn with_stock_balance(mut self, source: Arc<dyn PosStockSource>) -> Self {
        self.reads.stock = Some(source);
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

    /// Runs `pos.stock_balance`: Applied with a [`PosStockBalanceResult`], or a rejection.
    pub(super) fn pos_stock_balance(
        &self,
        c: &SyncCommand,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        let Some(p) = PosStockBalancePayload::from_element(&c.payload) else {
            return Ok(self.invalid(c, "payload is not a pos.stock_balance object"));
        };
        if let Some(problem) = p.problem() {
            return Ok(self.invalid(c, &problem));
        }
        let Some(source) = self.reads.stock.clone() else {
            return Ok(self.reject(c, reasons::UNKNOWN_TYPE));
        };
        let codes: Vec<String> = p.item_codes.iter().map(|c| c.trim().to_owned()).collect();
        let read = source.read(p.store_code, &codes, ctx)?;
        if let Some(refusal) = read.refusal {
            self.log.warn(
                "pos.stock_balance {Id}: store {Store} refused: {Reason}",
                &[
                    Value::from(c.command_id),
                    Value::from(p.store_code),
                    Value::from(refusal),
                ],
            );
            return Ok(self.reject(c, refusal));
        }
        self.log.info(
            "pos.stock_balance {Id}: store {Store} -> SMILE_BO {Database} store {Rvc} reason {Reason}: {Asked} item(s) asked, {Found} with ledger lines",
            &[
                Value::from(c.command_id),
                Value::from(p.store_code),
                Value::from(read.database.as_str()),
                Value::from(read.store_rvc),
                p.reason.as_deref().map_or(Value::Null, Value::from),
                Value::from(p.item_codes.len()),
                Value::from(read.items.len()),
            ],
        );
        let result = PosStockBalanceResult {
            store_code: p.store_code,
            database: read.database,
            store_rvc: read.store_rvc,
            items: read.items,
        };
        Ok(self.applied(c, Some(result.to_element())))
    }
}
