//! `txn.stock_count` (ADR 0167, RT-1114; `SiteCommandExecutor.StockCount.cs`): books a
//! physical count of one cloud stock store into SMILE_BO. Counted minus system
//! (`Inventory.VEnd` of the open INV period at the mapped store, no row = 0) becomes
//! ITrnCode 200 / ITrnSub 60, PApp 'SI' lines of one voucher through the vendor
//! `INVUpdateGLAvg`, with the count session (`InvSession` / `InvSSDtl`, `InvStatus` 9) the exe
//! would have written. A count is one-shot: an aggregate that was applied before answers
//! `stale` to every later command (any version) and writes nothing.

use dl_decimal::Decimal;
use dl_time::Date;

use super::SiteCommandExecutor;
use super::stock_run::{Cache, distinct_items, open_period};
use crate::commands::error::Result;
use crate::commands::stock_unit::{
    StockCountLine, StockCountPosted, StockCountSession, StockError, StockResult, StockStoreMap,
    TxnStockUnit,
};
use crate::commands::txn::aggregate_hash;
use crate::commands::units::SiteCommandUnit;
use crate::contracts::WireJson;
use crate::contracts::wire::{
    SyncCommand, SyncCommandResult, TxnStockCountPayload, TxnStockCountResult,
    stock_count_hash_line, txn_outcomes, txn_stock_count_scope, txn_stock_reasons as reasons,
};
use crate::reporting::Value;
use crate::util::ctx::Ctx;

/// The session's `InvDesc` and the voucher's description: names the cloud count, and is how
/// a repeat recognises its own session.
fn count_reference(count_no: i64) -> String {
    format!("Cloud count {count_no}")
}

fn count_result(
    aggregate: &str,
    p: &TxnStockCountPayload,
    outcome: &str,
    applied_version: i64,
    posted: &StockCountPosted,
) -> TxnStockCountResult {
    let differences: Vec<_> = posted
        .lines
        .iter()
        .filter(|l| l.trn_qty() != Decimal::ZERO)
        .collect();
    let hash = aggregate_hash(
        differences
            .iter()
            .map(|d| stock_count_hash_line(&d.item_code, p.store_code, d.trn_qty()))
            .collect(),
    );
    TxnStockCountResult {
        aggregate: aggregate.to_owned(),
        version: p.version,
        outcome: outcome.to_owned(),
        applied_version,
        session_seq: posted.session_seq,
        voucher_no: posted.voucher_no.clone(),
        lines: i32::try_from(posted.lines.len()).unwrap_or(i32::MAX),
        differences: i32::try_from(differences.len()).unwrap_or(i32::MAX),
        net_qty: differences
            .iter()
            .fold(Decimal::ZERO, |a, d| a + d.trn_qty()),
        hash,
    }
}

impl SiteCommandExecutor {
    pub(super) fn txn_stock_count(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let Some(p) = TxnStockCountPayload::from_element(&c.payload) else {
            return Ok(self.invalid(c, "payload is not a txn.stock_count object"));
        };
        if let Some(problem) = p.problem() {
            return Ok(self.invalid(c, &problem));
        }
        if !c.aggregate_key.is_empty()
            && c.aggregate_key != txn_stock_count_scope::aggregate_key(p.count_no)
        {
            return Ok(self.invalid(c, "AggregateKey does not match the count"));
        }
        self.run_stock_once(
            c,
            ctx,
            |unit, t| self.apply_stock_count(c, &p, unit, t),
            |reason| {
                self.log.warn(
                    "Site command {Id} ({Type}) refused: count {Count} store {Store} {Reason}",
                    &[
                        Value::from(c.command_id),
                        Value::from(c.type_.as_str()),
                        Value::from(p.count_no),
                        Value::from(p.store_code),
                        Value::from(reason),
                    ],
                );
            },
        )
    }

    fn apply_stock_count(
        &self,
        c: &SyncCommand,
        p: &TxnStockCountPayload,
        unit: &mut dyn SiteCommandUnit,
        t: &Ctx,
    ) -> StockResult<SyncCommandResult> {
        let aggregate = txn_stock_count_scope::aggregate(p.count_no);
        let bo = unit.stock();
        let applied = bo.lock_applied(&aggregate, t)?;
        let map = bo
            .map(p.store_code, t)?
            .ok_or(StockError::Refused(reasons::STORE_UNMAPPED))?;
        let database = bo
            .resolve_database(map.database.as_deref(), t)?
            .ok_or(StockError::Refused(reasons::DATABASE_INVALID))?;

        if let Some(stored) = applied {
            // Answered from the session the first command wrote, so a repeat carries the same
            // facts and hash as that write did. The key is only a session number: it is
            // trusted only when that session is this count's (store and reference), so a map
            // that moved since the first write can never answer with another session's numbers.
            let written = match stored.store_key {
                Some(key) => bo.read_count(&database, key, t)?,
                None => None,
            };
            let Some(written) = written else {
                return Err(StockError::Refused(reasons::COUNT_SESSION_MISSING));
            };
            if written.store_rvc != Some(map.store_rvc)
                || written.reference.as_deref() != Some(count_reference(p.count_no).as_str())
            {
                return Err(StockError::Refused(reasons::COUNT_SESSION_MISMATCH));
            }
            let result = count_result(&aggregate, p, txn_outcomes::STALE, stored.version, &written);
            return Ok(self.applied(c, Some(result.to_element())));
        }

        let Ok(day) = Date::parse_iso(&p.business_date) else {
            return Ok(self.invalid(c, "BusinessDate is not a date"));
        };
        let period = self.check_stock_count(bo, &map, &database, p, day, t)?;
        let session = StockCountSession {
            database: database.clone(),
            day,
            period,
            store_rvc: map.store_rvc,
            with_rvc: map.with_rvc,
            dept_code: map.dept_code.clone().unwrap_or_default(),
            j_type: map.j_type.clone(),
            reference: count_reference(p.count_no),
            user: p.actor.trim().to_owned(),
            lines: p
                .lines
                .iter()
                .map(|l| StockCountLine {
                    item_code: l.item_code.trim().to_owned(),
                    counted_qty: l.counted_qty,
                })
                .collect(),
        };
        let posted = bo.post_count(&session, t)?;

        let result = count_result(&aggregate, p, txn_outcomes::APPLIED, p.version, &posted);
        bo.save_applied(
            &aggregate,
            p.version,
            &result.hash,
            c.command_id,
            Some(posted.session_seq),
            t,
        )?;
        self.log.info(
            "Site command {Id} ({Type}) count {Count}: session {Session}, voucher {Voucher}, {Lines} counted line(s), {Differences} difference(s)",
            &[
                Value::from(c.command_id),
                Value::from(c.type_.as_str()),
                Value::from(p.count_no),
                Value::from(posted.session_seq),
                Value::from(posted.voucher_no.as_deref().unwrap_or("none")),
                Value::from(result.lines),
                Value::from(result.differences),
            ],
        );
        Ok(self.applied(c, Some(result.to_element())))
    }

    /// Every refusal a count can meet before anything is written; returns the open INV period
    /// the session belongs to (`CheckStockCountAsync`).
    fn check_stock_count(
        &self,
        bo: &mut dyn TxnStockUnit,
        map: &StockStoreMap,
        database: &str,
        p: &TxnStockCountPayload,
        day: Date,
        t: &Ctx,
    ) -> StockResult<i32> {
        self.check_stock_site(bo, database, t)?;
        if !self.stock_caches.has(Cache::Count, database) {
            if !bo.count_ledger_ready(database, t)? {
                return Err(StockError::Refused(reasons::LEDGER_MISSING));
            }
            self.stock_caches.add(Cache::Count, database);
        }
        if !bo.stores_exist(database, &[map.store_rvc, map.with_rvc], t)? {
            return Err(StockError::Refused(reasons::STORE_UNMAPPED));
        }
        // A session needs a department (the exe refuses without one): the map must name it,
        // none is made up.
        let dept = map.dept_code.as_deref().unwrap_or("");
        if dept.trim().is_empty()
            || !bo.count_target_valid(database, map.store_rvc, map.with_rvc, dept, t)?
        {
            return Err(StockError::Refused(reasons::STORE_UNMAPPED));
        }
        let settings = bo.settings(database, t)?;
        let period = day.year() * 100 + i32::from(day.month().number());
        let inv = open_period(&settings, period)?;
        if period != inv {
            return Err(StockError::Refused(reasons::COUNT_PERIOD_NOT_OPEN));
        }
        let items = distinct_items(p.lines.iter().map(|l| l.item_code.as_str()));
        if !bo.unknown_items(database, &items, t)?.is_empty() {
            return Err(StockError::Refused(reasons::ITEM_UNKNOWN));
        }
        Ok(inv)
    }
}
