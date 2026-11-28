//! `txn.stock_deplete` and `txn.stock_restore` (ADR 0164, RT-1022;
//! `SiteCommandExecutor.Stock.cs`): the cloud brings what bill `CheckNo` holds out of SMILE_BO
//! stock to the payload's lines. The agent keeps the held lines per bill
//! (`dbo.Pda_TxnStockHeld`), so a newer version writes only the difference, as one stock issue
//! voucher (ITrnCode 200 / ITrnSub 70, PApp 'SX') per business day and BO store, posted by the
//! vendor `INVUpdateGLAvg`. A version at or below the applied one is `stale` and writes
//! nothing. A refusal rolls the transaction back (no log row, a redrive runs again once fixed).

use dl_decimal::Decimal;
use dl_time::Date;

use super::SiteCommandExecutor;
use super::stock_run::{distinct_items, period_closed};
use crate::commands::error::Result;
use crate::commands::stock_unit::{
    StockError, StockHeldLine, StockResult, StockSettings, StockStoreMap, StockVoucher,
    StockVoucherLine, TxnStockUnit,
};
use crate::commands::txn::aggregate_hash;
use crate::commands::units::SiteCommandUnit;
use crate::config::time::local_offset_minutes;
use crate::contracts::WireJson;
use crate::contracts::wire::{
    SyncCommand, SyncCommandResult, TxnStockDepletePayload, TxnStockRestorePayload, TxnStockResult,
    stock_hash_line, txn_outcomes, txn_stock_reasons as reasons, txn_stock_scope,
};
use crate::reporting::Value;
use crate::util::ctx::Ctx;

/// A cloud store resolved to its BO map and database.
struct Mapped {
    code: i32,
    map: StockStoreMap,
    database: String,
}

fn same_line(a: &StockHeldLine, b: &StockHeldLine) -> bool {
    a.store_code == b.store_code && a.item_code == b.item_code
}

/// Signed ledger moves (negative takes stock out) from `held` to `target`, zero moves
/// dropped (`StockDeltas`).
fn stock_deltas(held: &[StockHeldLine], target: &[StockHeldLine], day: Date) -> Vec<StockHeldLine> {
    let mut moves = Vec::new();
    for t in target {
        let before = held
            .iter()
            .find(|h| same_line(h, t))
            .map_or(Decimal::ZERO, |h| h.qty);
        if t.qty != before {
            moves.push(StockHeldLine {
                qty: before - t.qty,
                ..t.clone()
            });
        }
    }
    for h in held
        .iter()
        .filter(|h| !target.iter().any(|t| same_line(h, t)))
    {
        moves.push(StockHeldLine { day, ..h.clone() });
    }
    moves
}

fn sum(values: impl Iterator<Item = Decimal>) -> Decimal {
    values.fold(Decimal::ZERO, |a, b| a + b)
}

fn stock_result(
    aggregate: &str,
    version: i64,
    outcome: &str,
    applied_version: i64,
    held: &[StockHeldLine],
) -> TxnStockResult {
    TxnStockResult {
        aggregate: aggregate.to_owned(),
        version,
        outcome: outcome.to_owned(),
        applied_version,
        lines: i32::try_from(held.len()).unwrap_or(i32::MAX),
        net_qty: sum(held.iter().map(|h| h.qty)),
        hash: aggregate_hash(
            held.iter()
                .map(|h| stock_hash_line(&h.item_code, h.store_code, h.qty))
                .collect(),
        ),
    }
}

/// One voucher group: the moves of one business day and BO store (and its department,
/// voucher type and counter location).
struct Group {
    day: Date,
    database_upper: String,
    map: StockStoreMap,
    database: String,
    moves: Vec<StockHeldLine>,
}

/// One voucher per business day and warehouse: two cloud stores booked into the same BO store
/// share it. Ordered by day, database (ordinal), BO store.
fn group_vouchers(
    moves: &[StockHeldLine],
    maps: &[Mapped],
    reference: &str,
    user: &str,
) -> Vec<StockVoucher> {
    let mut groups: Vec<Group> = Vec::new();
    for m in moves {
        let Some(mapped) = maps.iter().find(|x| x.code == m.store_code) else {
            continue;
        };
        let upper = mapped.database.to_uppercase();
        let found = groups.iter_mut().find(|g| {
            g.day == m.day
                && g.database_upper == upper
                && g.map.store_rvc == mapped.map.store_rvc
                && g.map.with_rvc == mapped.map.with_rvc
                && g.map.dept_code == mapped.map.dept_code
                && g.map.j_type == mapped.map.j_type
        });
        match found {
            Some(g) => g.moves.push(m.clone()),
            None => groups.push(Group {
                day: m.day,
                database_upper: upper,
                map: mapped.map.clone(),
                database: mapped.database.clone(),
                moves: vec![m.clone()],
            }),
        }
    }
    groups.sort_by(|a, b| {
        (a.day, &a.database_upper, a.map.store_rvc).cmp(&(
            b.day,
            &b.database_upper,
            b.map.store_rvc,
        ))
    });
    groups
        .into_iter()
        .map(|g| {
            let mut lines: Vec<StockVoucherLine> = Vec::new();
            for m in &g.moves {
                match lines.iter_mut().find(|l| l.item_code == m.item_code) {
                    Some(l) => l.trn_qty += m.qty,
                    None => lines.push(StockVoucherLine {
                        item_code: m.item_code.clone(),
                        trn_qty: m.qty,
                    }),
                }
            }
            lines.retain(|l| l.trn_qty != Decimal::ZERO);
            StockVoucher {
                database: g.database,
                day: g.day,
                store_rvc: g.map.store_rvc,
                with_rvc: g.map.with_rvc,
                dept_code: g.map.dept_code,
                j_type: g.map.j_type,
                reference: reference.to_owned(),
                user: user.to_owned(),
                lines,
                trn_sub: StockVoucher::DEPLETION_SUB,
                description: None,
            }
        })
        .filter(|v| !v.lines.is_empty())
        .collect()
}

fn year_month(day: Date) -> i32 {
    day.year() * 100 + i32::from(day.month().number())
}

impl SiteCommandExecutor {
    pub(super) fn txn_stock_deplete(
        &self,
        c: &SyncCommand,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        let Some(p) = TxnStockDepletePayload::from_element(&c.payload) else {
            return Ok(self.invalid(c, "payload is not a txn.stock_deplete object"));
        };
        if let Some(problem) = p.problem() {
            return Ok(self.invalid(c, &problem));
        }
        let Ok(day) = Date::parse_iso(&p.business_date) else {
            return Ok(self.invalid(c, "BusinessDate is not a date"));
        };
        let target: Vec<StockHeldLine> = p
            .lines
            .iter()
            .map(|l| StockHeldLine {
                item_code: l.item_code.trim().to_owned(),
                store_code: l.store_code,
                qty: l.qty,
                day,
            })
            .collect();
        self.run_stock_bill(
            c,
            p.check_no,
            p.rvc,
            p.version,
            &target,
            day,
            p.actor.trim(),
            ctx,
        )
    }

    pub(super) fn txn_stock_restore(
        &self,
        c: &SyncCommand,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        let Some(p) = TxnStockRestorePayload::from_element(&c.payload) else {
            return Ok(self.invalid(c, "payload is not a txn.stock_restore object"));
        };
        if let Some(problem) = p.problem() {
            return Ok(self.invalid(c, &problem));
        }
        // A reversal goes into the open period, never back into a month SMILE_BO may have
        // closed since: the store's current day.
        let (y, m, d, ..) = self.clock.now().to_offset(local_offset_minutes()).parts();
        let today = i32::try_from(y)
            .ok()
            .and_then(|y| Date::new(y, u8::try_from(m).ok()?, u8::try_from(d).ok()?).ok());
        let Some(today) = today else {
            return Ok(self.invalid(c, "the store day is not a date"));
        };
        self.run_stock_bill(
            c,
            p.check_no,
            p.rvc,
            p.version,
            &[],
            today,
            p.actor.trim(),
            ctx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn run_stock_bill(
        &self,
        c: &SyncCommand,
        check_no: i64,
        rvc: i32,
        version: i64,
        target: &[StockHeldLine],
        day: Date,
        user: &str,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        if !c.aggregate_key.is_empty()
            && c.aggregate_key != txn_stock_scope::aggregate_key(check_no)
        {
            return Ok(self.invalid(c, "AggregateKey does not match the bill"));
        }
        self.run_stock_once(
            c,
            ctx,
            |unit, t| self.apply_stock_bill(c, check_no, version, target, day, user, unit, t),
            |reason| {
                self.log.warn(
                    "Site command {Id} ({Type}) refused: bill {Check} RVC {Rvc} {Reason}",
                    &[
                        Value::from(c.command_id),
                        Value::from(c.type_.as_str()),
                        Value::from(check_no),
                        Value::from(rvc),
                        Value::from(reason),
                    ],
                );
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_stock_bill(
        &self,
        c: &SyncCommand,
        check_no: i64,
        version: i64,
        target: &[StockHeldLine],
        day: Date,
        user: &str,
        unit: &mut dyn SiteCommandUnit,
        t: &Ctx,
    ) -> StockResult<SyncCommandResult> {
        let bo = unit.stock();
        let aggregate = txn_stock_scope::aggregate(check_no);
        let applied = bo.lock_applied(&aggregate, t)?;
        let held = bo.held(&aggregate, t)?;
        if let Some(stored) = applied
            && stored.version >= version
        {
            let result = stock_result(
                &aggregate,
                version,
                txn_outcomes::STALE,
                stored.version,
                &held,
            );
            return Ok(self.applied(c, Some(result.to_element())));
        }

        // The delta per item and cloud store, every move dated the command's day (a
        // depletion's business day, a restore's store day).
        let deltas = stock_deltas(&held, target, day);
        let vouchers = self.stock_vouchers(bo, &deltas, check_no, user, t)?;
        let mut last_seq = None;
        for voucher in &vouchers {
            last_seq = Some(bo.post_voucher(voucher, t)?);
        }

        let next: Vec<StockHeldLine> = target
            .iter()
            .map(|l| match held.iter().find(|h| same_line(h, l)) {
                Some(h) if h.qty == l.qty => h.clone(),
                _ => l.clone(),
            })
            .collect();
        bo.save_held(&aggregate, &next, t)?;
        let result = stock_result(&aggregate, version, txn_outcomes::APPLIED, version, &next);
        bo.save_applied(&aggregate, version, &result.hash, c.command_id, last_seq, t)?;
        self.log.info(
            "Site command {Id} ({Type}) bill {Check}: {Vouchers} SMILE_BO stock voucher(s), {Lines} line(s) held",
            &[
                Value::from(c.command_id),
                Value::from(c.type_.as_str()),
                Value::from(check_no),
                Value::from(vouchers.len()),
                Value::from(next.len()),
            ],
        );
        Ok(self.applied(c, Some(result.to_element())))
    }

    /// Maps every cloud store to its SMILE_BO store, checks each SMILE_BO site, then groups
    /// the moves by day and BO store (`StockVouchersAsync`).
    fn stock_vouchers(
        &self,
        bo: &mut dyn TxnStockUnit,
        moves: &[StockHeldLine],
        check_no: i64,
        user: &str,
        t: &Ctx,
    ) -> StockResult<Vec<StockVoucher>> {
        if moves.is_empty() {
            return Ok(Vec::new());
        }
        let mut maps: Vec<Mapped> = Vec::new();
        for m in moves {
            if maps.iter().any(|x| x.code == m.store_code) {
                continue;
            }
            let map = bo
                .map(m.store_code, t)?
                .ok_or(StockError::Refused(reasons::STORE_UNMAPPED))?;
            let database = bo
                .resolve_database(map.database.as_deref(), t)?
                .ok_or(StockError::Refused(reasons::DATABASE_INVALID))?;
            maps.push(Mapped {
                code: m.store_code,
                map,
                database,
            });
        }

        // The sites in first-appearance order, one per database (case-insensitive).
        let mut sites: Vec<(String, Vec<&Mapped>)> = Vec::new();
        for m in &maps {
            let key = m.database.to_lowercase();
            match sites.iter_mut().find(|(k, _)| *k == key) {
                Some((_, members)) => members.push(m),
                None => sites.push((key, vec![m])),
            }
        }
        for (_, members) in &sites {
            let database = members[0].database.as_str();
            self.check_stock_site(bo, database, t)?;
            let mut rvcs: Vec<i32> = Vec::new();
            for m in members {
                for rvc in [m.map.store_rvc, m.map.with_rvc] {
                    if !rvcs.contains(&rvc) {
                        rvcs.push(rvc);
                    }
                }
            }
            if !bo.stores_exist(database, &rvcs, t)? {
                return Err(StockError::Refused(reasons::STORE_UNMAPPED));
            }
            let settings = bo.settings(database, t)?;
            self.check_site_moves(bo, database, &settings, members, moves, t)?;
        }

        let reference = format!("Cloud bill {check_no}");
        Ok(group_vouchers(moves, &maps, &reference, user))
    }

    /// The cost method, the periods and the items of the moves booked into one site.
    fn check_site_moves(
        &self,
        bo: &mut dyn TxnStockUnit,
        database: &str,
        settings: &StockSettings,
        members: &[&Mapped],
        moves: &[StockHeldLine],
        t: &Ctx,
    ) -> StockResult<()> {
        if settings.cost_method != Some(0) {
            return Err(StockError::Refused(reasons::COST_METHOD_UNSUPPORTED));
        }
        let of_site: Vec<&StockHeldLine> = moves
            .iter()
            .filter(|m| members.iter().any(|x| x.code == m.store_code))
            .collect();
        for m in &of_site {
            if period_closed(settings, year_month(m.day)) {
                return Err(StockError::Refused(reasons::PERIOD_CLOSED));
            }
        }
        let items = distinct_items(of_site.iter().map(|m| m.item_code.as_str()));
        if !bo.unknown_items(database, &items, t)?.is_empty() {
            return Err(StockError::Refused(reasons::ITEM_UNKNOWN));
        }
        Ok(())
    }
}
