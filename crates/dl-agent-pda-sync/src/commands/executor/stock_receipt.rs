//! `txn.stock_receipt` (ADR 0169, RT-1113; `SiteCommandExecutor.StockReceipt.cs`): books a goods
//! receipt without a PO into SMILE_BO. The agent writes `tmpReceivingHeader` /
//! `tmpReceivingDetail` (`ReceivedNonePO` 1, VType from the site's `IVoucherType` receiving
//! row) and posts them with the vendor `RCVUpdateGL`, which writes ITrnCode 100 / ITrnSub 10,
//! PApp 'SR' into `INVTrn` and the `Inventory` balance. A receipt is one-shot: an aggregate
//! that was applied before answers `stale` to every later command (any version) and writes
//! nothing. A vendor post that posts nothing is an error, not a refusal: the command rolls
//! back and is retried.

use dl_decimal::Decimal;
use dl_time::Date;

use super::SiteCommandExecutor;
use super::stock_run::{distinct_items, open_period};
use crate::commands::error::Result;
use crate::commands::stock_unit::{
    StockError, StockReceipt, StockReceiptLine, StockReceiptPosted, StockResult, StockStoreMap,
    TxnStockUnit,
};
use crate::commands::txn::aggregate_hash;
use crate::commands::units::SiteCommandUnit;
use crate::contracts::WireJson;
use crate::contracts::wire::{
    SyncCommand, SyncCommandResult, TxnStockReceiptPayload, TxnStockReceiptResult,
    stock_receipt_hash_line, txn_outcomes, txn_stock_reasons as reasons, txn_stock_receipt_scope,
};
use crate::reporting::Value;
use crate::util::ctx::Ctx;

/// The header's `VDesc` / `VRef`: names the cloud receipt, and is how a repeat recognises its
/// own receipt.
fn receipt_reference(receipt_no: i64) -> String {
    format!("Cloud receipt {receipt_no}")
}

fn receipt_result(
    aggregate: &str,
    p: &TxnStockReceiptPayload,
    outcome: &str,
    applied_version: i64,
    posted: &StockReceiptPosted,
) -> TxnStockReceiptResult {
    let mut lines: Vec<_> = posted.lines.iter().collect();
    lines.sort_by(|a, b| a.item_code.cmp(&b.item_code));
    let hash = aggregate_hash(
        lines
            .iter()
            .map(|l| stock_receipt_hash_line(&l.item_code, p.store_code, l.qty, l.amount))
            .collect(),
    );
    TxnStockReceiptResult {
        aggregate: aggregate.to_owned(),
        version: p.version,
        outcome: outcome.to_owned(),
        applied_version,
        receipt_seq: posted.receipt_seq,
        voucher_no: posted.voucher_no.clone(),
        lines: i32::try_from(lines.len()).unwrap_or(i32::MAX),
        net_qty: lines.iter().fold(Decimal::ZERO, |a, l| a + l.qty),
        amount: lines.iter().fold(Decimal::ZERO, |a, l| a + l.amount),
        hash,
    }
}

impl SiteCommandExecutor {
    pub(super) fn txn_stock_receipt(
        &self,
        c: &SyncCommand,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        let Some(p) = TxnStockReceiptPayload::from_element(&c.payload) else {
            return Ok(self.invalid(c, "payload is not a txn.stock_receipt object"));
        };
        if let Some(problem) = p.problem() {
            return Ok(self.invalid(c, &problem));
        }
        if !c.aggregate_key.is_empty()
            && c.aggregate_key != txn_stock_receipt_scope::aggregate_key(p.receipt_no)
        {
            return Ok(self.invalid(c, "AggregateKey does not match the receipt"));
        }
        self.run_stock_once(
            c,
            ctx,
            |unit, t| self.apply_stock_receipt(c, &p, unit, t),
            |reason| {
                self.log.warn(
                    "Site command {Id} ({Type}) refused: receipt {Receipt} store {Store} {Reason}",
                    &[
                        Value::from(c.command_id),
                        Value::from(c.type_.as_str()),
                        Value::from(p.receipt_no),
                        Value::from(p.store_code),
                        Value::from(reason),
                    ],
                );
            },
        )
    }

    fn apply_stock_receipt(
        &self,
        c: &SyncCommand,
        p: &TxnStockReceiptPayload,
        unit: &mut dyn SiteCommandUnit,
        t: &Ctx,
    ) -> StockResult<SyncCommandResult> {
        let aggregate = txn_stock_receipt_scope::aggregate(p.receipt_no);
        let bo = unit.stock();
        let applied = bo.lock_applied(&aggregate, t)?;
        let map = bo
            .map(p.store_code, t)?
            .ok_or(StockError::Refused(reasons::STORE_UNMAPPED))?;
        let database = bo
            .resolve_database(map.database.as_deref(), t)?
            .ok_or(StockError::Refused(reasons::DATABASE_INVALID))?;
        let reference = receipt_reference(p.receipt_no);

        if let Some(stored) = applied {
            // Answered from the receipt the first command wrote, so a repeat carries the same
            // facts and hash as that write did. The key is only a header number: it is trusted
            // only when that receipt is this one's (store, vendor and reference).
            let written = match stored.store_key {
                Some(key) => bo.read_receipt(&database, key, t)?,
                None => None,
            };
            let Some(written) = written else {
                return Err(StockError::Refused(reasons::RECEIPT_MISSING));
            };
            if written.store_rvc != Some(map.store_rvc)
                || written.vendor_code != Some(p.vendor_code)
                || written.reference.as_deref() != Some(reference.as_str())
            {
                return Err(StockError::Refused(reasons::RECEIPT_MISMATCH));
            }
            let result =
                receipt_result(&aggregate, p, txn_outcomes::STALE, stored.version, &written);
            return Ok(self.applied(c, Some(result.to_element())));
        }

        let Ok(day) = Date::parse_iso(&p.business_date) else {
            return Ok(self.invalid(c, "BusinessDate is not a date"));
        };
        let period = self.check_stock_receipt(bo, &map, &database, p, day, t)?;
        let receipt = StockReceipt {
            database: database.clone(),
            day,
            period,
            store_rvc: map.store_rvc,
            vendor_code: p.vendor_code,
            reference,
            user: p.actor.trim().to_owned(),
            lines: p
                .lines
                .iter()
                .map(|l| StockReceiptLine {
                    item_code: l.item_code.trim().to_owned(),
                    qty: l.qty,
                    unit_price: l.unit_price,
                })
                .collect(),
        };
        let posted = bo.post_receipt(&receipt, t)?;

        let result = receipt_result(&aggregate, p, txn_outcomes::APPLIED, p.version, &posted);
        bo.save_applied(
            &aggregate,
            p.version,
            &result.hash,
            c.command_id,
            Some(posted.receipt_seq),
            t,
        )?;
        self.log.info(
            "Site command {Id} ({Type}) receipt {Receipt}: voucher {Voucher} (header {Seq}), {Lines} line(s), amount {Amount}",
            &[
                Value::from(c.command_id),
                Value::from(c.type_.as_str()),
                Value::from(p.receipt_no),
                Value::from(posted.voucher_no.as_str()),
                Value::from(posted.receipt_seq),
                Value::from(result.lines),
                Value::from(result.amount.to_string()),
            ],
        );
        Ok(self.applied(c, Some(result.to_element())))
    }

    /// Every refusal a receipt can meet before anything is written; returns the open INV
    /// period the receipt belongs to (`CheckStockReceiptAsync`).
    fn check_stock_receipt(
        &self,
        bo: &mut dyn TxnStockUnit,
        map: &StockStoreMap,
        database: &str,
        p: &TxnStockReceiptPayload,
        day: Date,
        t: &Ctx,
    ) -> StockResult<i32> {
        self.check_stock_site(bo, database, t)?;
        self.check_receipt_site(bo, database, t)?;
        if !bo.stores_exist(database, &[map.store_rvc], t)? {
            return Err(StockError::Refused(reasons::STORE_UNMAPPED));
        }
        let settings = bo.settings(database, t)?;
        // FIFO needs InvFIFOLog layers the agent does not write (the receipt would cost
        // nothing in a FIFO ledger): the site is refused.
        let period = day.year() * 100 + i32::from(day.month().number());
        let inv = open_period(&settings, period)?;
        if period != inv {
            return Err(StockError::Refused(reasons::RECEIPT_PERIOD_NOT_OPEN));
        }
        let items = distinct_items(p.lines.iter().map(|l| l.item_code.as_str()));
        if !bo.unknown_items(database, &items, t)?.is_empty() {
            return Err(StockError::Refused(reasons::ITEM_UNKNOWN));
        }
        if !bo.receipt_target_valid(database, map.store_rvc, p.vendor_code, &items, t)? {
            return Err(StockError::Refused(reasons::RECEIPT_TARGET_INVALID));
        }
        Ok(inv)
    }
}
