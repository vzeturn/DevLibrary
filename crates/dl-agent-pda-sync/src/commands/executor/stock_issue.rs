//! `txn.stock_issue` (ADR 0168, RT-1112; `SiteCommandExecutor.StockIssue.cs`): books a cloud
//! stock issue voucher into SMILE_BO as one voucher (sApp 'SX') of ITrnCode 200 / ITrnSub 50
//! lines (PApp 'SX', `TrnQty` negative, RVC = the mapped BO source store, WithRVC = the
//! destination) through the vendor `INVUpdateGLAvg`, which adds the 100 / 50 mirror line. The
//! destination is the consumption location of the store's map (no or the same
//! `WithStoreCode`) or the mapped BO store of another cloud store. An issue is one-shot: an
//! applied `IssueNo` answers `stale` to every later version and writes nothing; the same
//! version again with other lines or another destination is refused. The quantity on hand is
//! not checked: the vendor post lets a store go negative.

use std::collections::HashMap;

use dl_decimal::Decimal;
use dl_time::Date;

use super::SiteCommandExecutor;
use super::stock_run::{distinct_items, open_period};
use crate::commands::error::Result;
use crate::commands::stock_unit::{
    StockError, StockIssuePosted, StockResult, StockStoreMap, StockVoucher, StockVoucherLine,
    TxnStockUnit,
};
use crate::commands::txn::aggregate_hash;
use crate::commands::units::SiteCommandUnit;
use crate::contracts::WireJson;
use crate::contracts::wire::{
    SyncCommand, SyncCommandResult, TxnStockIssuePayload, TxnStockIssueResult,
    stock_issue_hash_line, txn_outcomes, txn_stock_issue_scope, txn_stock_reasons as reasons,
};
use crate::reporting::Value;
use crate::util::ctx::Ctx;

/// The voucher's `VRef` and the start of its `VDesc`: names the cloud issue, and is how a
/// repeat recognises its own voucher.
fn issue_reference(issue_no: i64) -> String {
    format!("Cloud issue {issue_no}")
}

/// True when the voucher holds exactly the payload's items and quantities (out, so negative)
/// and its destination.
fn same_issue_lines(written: &StockIssuePosted, p: &TxnStockIssuePayload, with_rvc: i32) -> bool {
    if written.lines.len() != p.lines.len() || written.lines.iter().any(|l| l.with_rvc != with_rvc)
    {
        return false;
    }
    let expected: HashMap<&str, Decimal> = p
        .lines
        .iter()
        .map(|l| (l.item_code.trim(), -l.qty))
        .collect();
    written.lines.iter().all(|l| {
        expected
            .get(l.item_code.as_str())
            .is_some_and(|q| *q == l.trn_qty)
    })
}

fn issue_result(
    aggregate: &str,
    p: &TxnStockIssuePayload,
    outcome: &str,
    applied_version: i64,
    posted: &StockIssuePosted,
) -> TxnStockIssueResult {
    let hash = aggregate_hash(
        posted
            .lines
            .iter()
            .map(|l| {
                stock_issue_hash_line(&l.item_code, p.store_code, p.with_store_code, -l.trn_qty)
            })
            .collect(),
    );
    TxnStockIssueResult {
        aggregate: aggregate.to_owned(),
        version: p.version,
        outcome: outcome.to_owned(),
        applied_version,
        voucher_no: posted.voucher_no.clone(),
        lines: i32::try_from(posted.lines.len()).unwrap_or(i32::MAX),
        net_qty: posted
            .lines
            .iter()
            .fold(Decimal::ZERO, |a, l| a + l.trn_qty),
        hash,
    }
}

impl SiteCommandExecutor {
    pub(super) fn txn_stock_issue(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let Some(p) = TxnStockIssuePayload::from_element(&c.payload) else {
            return Ok(self.invalid(c, "payload is not a txn.stock_issue object"));
        };
        if let Some(problem) = p.problem() {
            return Ok(self.invalid(c, &problem));
        }
        if !c.aggregate_key.is_empty()
            && c.aggregate_key != txn_stock_issue_scope::aggregate_key(p.issue_no)
        {
            return Ok(self.invalid(c, "AggregateKey does not match the issue voucher"));
        }
        self.run_stock_once(
            c,
            ctx,
            |unit, t| self.apply_stock_issue(c, &p, unit, t),
            |reason| {
                self.log.warn(
                    "Site command {Id} ({Type}) refused: issue {Issue} store {Store} {Reason}",
                    &[
                        Value::from(c.command_id),
                        Value::from(c.type_.as_str()),
                        Value::from(p.issue_no),
                        Value::from(p.store_code),
                        Value::from(reason),
                    ],
                );
            },
        )
    }

    fn apply_stock_issue(
        &self,
        c: &SyncCommand,
        p: &TxnStockIssuePayload,
        unit: &mut dyn SiteCommandUnit,
        t: &Ctx,
    ) -> StockResult<SyncCommandResult> {
        let aggregate = txn_stock_issue_scope::aggregate(p.issue_no);
        let bo = unit.stock();
        let applied = bo.lock_applied(&aggregate, t)?;
        let map = bo
            .map(p.store_code, t)?
            .ok_or(StockError::Refused(reasons::STORE_UNMAPPED))?;
        let database = bo
            .resolve_database(map.database.as_deref(), t)?
            .ok_or(StockError::Refused(reasons::DATABASE_INVALID))?;
        let with_rvc = issue_destination(bo, &map, &database, p, t)?;

        if let Some(stored) = applied {
            // Answered from the voucher the first command wrote, so a repeat carries the same
            // facts and hash as that write did. The key is only a VSeq of one database: it is
            // trusted only when that voucher is this issue's (reference, source store), and a
            // repeat of the applied version must also carry the applied lines and destination
            // (the key is bound to the first payload).
            let written = match stored.store_key {
                Some(key) => bo.read_issue(&database, key, t)?,
                None => None,
            };
            let Some(written) = written else {
                return Err(StockError::Refused(reasons::ISSUE_VOUCHER_MISSING));
            };
            if written.reference.as_deref() != Some(issue_reference(p.issue_no).as_str())
                || written.lines.is_empty()
                || written.lines.iter().any(|l| l.rvc != map.store_rvc)
            {
                return Err(StockError::Refused(reasons::ISSUE_VOUCHER_MISMATCH));
            }
            if stored.version == p.version && !same_issue_lines(&written, p, with_rvc) {
                return Err(StockError::Refused(reasons::ISSUE_PAYLOAD_MISMATCH));
            }
            let result = issue_result(&aggregate, p, txn_outcomes::STALE, stored.version, &written);
            return Ok(self.applied(c, Some(result.to_element())));
        }

        let Ok(day) = Date::parse_iso(&p.business_date) else {
            return Ok(self.invalid(c, "BusinessDate is not a date"));
        };
        self.check_stock_issue(bo, &map, &database, p, with_rvc, day, t)?;
        let reference = issue_reference(p.issue_no);
        let note = p.reason.as_deref().map(str::trim).unwrap_or_default();
        let voucher = StockVoucher {
            database: database.clone(),
            day,
            store_rvc: map.store_rvc,
            with_rvc,
            dept_code: map.dept_code.clone(),
            j_type: map.j_type.clone(),
            reference: reference.clone(),
            user: p.actor.trim().to_owned(),
            lines: p
                .lines
                .iter()
                .map(|l| StockVoucherLine {
                    item_code: l.item_code.trim().to_owned(),
                    trn_qty: -l.qty,
                })
                .collect(),
            trn_sub: 50,
            description: Some(if note.is_empty() {
                reference
            } else {
                format!("{reference} - {note}")
            }),
        };
        let vseq = bo.post_voucher(&voucher, t)?;

        // The result is what SMILE_BO holds after the post, the same read a stale repeat
        // uses; a voucher that is not there is a failure (rolled back).
        let posted = match bo.read_issue(&database, vseq, t)? {
            Some(posted) if posted.lines.len() == p.lines.len() => posted,
            _ => {
                return Err(crate::commands::error::CommandError::invalid(format!(
                    "Issue voucher {vseq} in {database} was not found with its {} 200/50 line(s) after INVUpdateGLAvg; the command is rolled back.",
                    p.lines.len()
                ))
                .into());
            }
        };
        let result = issue_result(&aggregate, p, txn_outcomes::APPLIED, p.version, &posted);
        bo.save_applied(
            &aggregate,
            p.version,
            &result.hash,
            c.command_id,
            Some(vseq),
            t,
        )?;
        self.log.info(
            "Site command {Id} ({Type}) issue {Issue}: voucher {Voucher} (VSeq {VSeq}), {Lines} line(s), RVC {Rvc} -> {With}",
            &[
                Value::from(c.command_id),
                Value::from(c.type_.as_str()),
                Value::from(p.issue_no),
                Value::from(posted.voucher_no.as_str()),
                Value::from(vseq),
                Value::from(result.lines),
                Value::from(map.store_rvc),
                Value::from(with_rvc),
            ],
        );
        Ok(self.applied(c, Some(result.to_element())))
    }

    /// Every refusal an issue can meet before anything is written.
    #[allow(clippy::too_many_arguments)]
    fn check_stock_issue(
        &self,
        bo: &mut dyn TxnStockUnit,
        map: &StockStoreMap,
        database: &str,
        p: &TxnStockIssuePayload,
        with_rvc: i32,
        day: Date,
        t: &Ctx,
    ) -> StockResult<()> {
        self.check_stock_site(bo, database, t)?;
        self.log_stock_issue_profile(bo, database, t)?;
        if !bo.stores_exist(database, &[map.store_rvc, with_rvc], t)? {
            return Err(StockError::Refused(reasons::STORE_UNMAPPED));
        }
        // The source is a stock store (RVCType 0); the destination a consumption location (1)
        // for an issue to the room, a stock store (0) for a transfer.
        let wanted = if p.to_consumption() { 1 } else { 0 };
        if bo.store_type(database, map.store_rvc, t)? != Some(0)
            || bo.store_type(database, with_rvc, t)? != Some(wanted)
        {
            return Err(StockError::Refused(reasons::STORE_UNMAPPED));
        }
        let settings = bo.settings(database, t)?;
        open_period(
            &settings,
            day.year() * 100 + i32::from(day.month().number()),
        )?;
        let items = distinct_items(p.lines.iter().map(|l| l.item_code.as_str()));
        if !bo.unknown_items(database, &items, t)?.is_empty() {
            return Err(StockError::Refused(reasons::ITEM_UNKNOWN));
        }
        Ok(())
    }
}

/// The BO destination (`WithRVC`) of the issue: the map's consumption location for no or the
/// same `WithStoreCode`, else the BO store of that cloud store's map, which must sit in the
/// same SMILE_BO database and be another BO store than the source (a transfer to itself is no
/// movement). Maps only; the types are checked by `check_stock_issue`.
fn issue_destination(
    bo: &mut dyn TxnStockUnit,
    map: &StockStoreMap,
    database: &str,
    p: &TxnStockIssuePayload,
    t: &Ctx,
) -> StockResult<i32> {
    let Some(with_store_code) = p.with_store_code.filter(|_| !p.to_consumption()) else {
        return Ok(map.with_rvc);
    };
    let target = bo
        .map(with_store_code, t)?
        .ok_or(StockError::Refused(reasons::STORE_UNMAPPED))?;
    let target_database = bo
        .resolve_database(target.database.as_deref(), t)?
        .ok_or(StockError::Refused(reasons::DATABASE_INVALID))?;
    if !target_database.eq_ignore_ascii_case(database) || target.store_rvc == map.store_rvc {
        return Err(StockError::Refused(reasons::STORE_UNMAPPED));
    }
    Ok(target.store_rvc)
}
