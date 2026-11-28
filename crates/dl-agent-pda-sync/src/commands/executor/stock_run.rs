//! What the five stock verbs share (`SiteCommandExecutor.Stock*.cs`, ADR 0164, 0167, 0168,
//! 0169): the refusal wrapper around the command transaction, the per-run ledger caches and
//! the checks every verb makes on a SMILE_BO site before its first write.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

use super::SiteCommandExecutor;
use crate::commands::error::{CommandError, Result};
use crate::commands::stock_unit::{
    StockCodeCount, StockError, StockResult, StockSettings, TxnStockUnit,
};
use crate::commands::units::SiteCommandUnit;
use crate::contracts::wire::{SyncCommand, SyncCommandResult, txn_stock_reasons as reasons};
use crate::reporting::Value;
use crate::util::ctx::Ctx;

/// The message of the error that leaves `run_once` on a store-state refusal.
const REFUSED: &str = "stock verb refused by the store";

/// SMILE_BO databases (case-insensitive) already checked in this run: once per site before
/// its first voucher (the C# `ConcurrentDictionary` fields of the executor).
#[derive(Default)]
pub(super) struct StockCaches {
    ledgers: Mutex<HashSet<String>>,
    count_ledgers: Mutex<HashSet<String>>,
    issue_profiles: Mutex<HashSet<String>>,
    receipt_ledgers: Mutex<HashSet<String>>,
}

/// One of the caches of [`StockCaches`].
#[derive(Clone, Copy)]
pub(super) enum Cache {
    /// `_stockLedgers`: the ledger profile was read and is ready.
    Ledger,
    /// `_stockCountLedgers`: the count objects exist.
    Count,
    /// `_stockIssueProfiles`: the 200 / 50 profile was logged.
    IssueProfile,
    /// `_stockReceiptLedgers`: the receipt objects and voucher type were found.
    Receipt,
}

impl StockCaches {
    fn set(&self, cache: Cache) -> &Mutex<HashSet<String>> {
        match cache {
            Cache::Ledger => &self.ledgers,
            Cache::Count => &self.count_ledgers,
            Cache::IssueProfile => &self.issue_profiles,
            Cache::Receipt => &self.receipt_ledgers,
        }
    }

    pub(super) fn has(&self, cache: Cache, database: &str) -> bool {
        self.set(cache)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&database.to_lowercase())
    }

    pub(super) fn add(&self, cache: Cache, database: &str) {
        self.set(cache)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(database.to_lowercase());
    }
}

/// `SiteCommandExecutor` of an in-run list of code counts: `200/70=3, 100/10=1` or `none`.
fn codes_text(codes: &[StockCodeCount]) -> String {
    if codes.is_empty() {
        return "none".to_owned();
    }
    codes
        .iter()
        .map(|c| format!("{}/{}={}", c.trn_code, c.trn_sub, c.lines))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The open INV period of `settings` when `period` (yyyyMM) may be written into it:
/// weighted-average cost only, and a period neither before the INV nor the GL period.
pub(super) fn open_period(settings: &StockSettings, period: i32) -> StockResult<i32> {
    if settings.cost_method != Some(0) {
        return Err(StockError::Refused(reasons::COST_METHOD_UNSUPPORTED));
    }
    match settings.inv_period {
        Some(inv) if period >= inv && settings.gl_period.is_none_or(|gl| period >= gl) => Ok(inv),
        _ => Err(StockError::Refused(reasons::PERIOD_CLOSED)),
    }
}

/// `period < inv || (gl is set && period < gl)` for one yyyyMM, without the cost method.
pub(super) fn period_closed(settings: &StockSettings, period: i32) -> bool {
    match settings.inv_period {
        Some(inv) => period < inv || settings.gl_period.is_some_and(|gl| period < gl),
        None => true,
    }
}

impl SiteCommandExecutor {
    /// Runs `apply` in the command transaction. A refusal of the store rolls the
    /// transaction back with no log row (a redrive of the same command id runs again once
    /// the store is fixed), is logged by `log_refusal` and answers a rejection.
    pub(super) fn run_stock_once(
        &self,
        c: &SyncCommand,
        ctx: &Ctx,
        mut apply: impl FnMut(&mut dyn SiteCommandUnit, &Ctx) -> StockResult<SyncCommandResult>,
        log_refusal: impl FnOnce(&str),
    ) -> Result<SyncCommandResult> {
        let mut refused: Option<&'static str> = None;
        let r = self.store.run_once(
            c.command_id,
            &mut |unit: &mut dyn SiteCommandUnit, t: &Ctx| match apply(unit, t) {
                Ok(r) => Ok(r),
                Err(StockError::Command(e)) => Err(e),
                Err(StockError::Refused(reason)) => {
                    refused = Some(reason);
                    Err(CommandError::invalid(REFUSED))
                }
            },
            ctx,
        );
        match refused {
            Some(reason) => {
                log_refusal(reason);
                Ok(self.reject(c, reason))
            }
            None => r,
        }
    }

    /// `CheckStockSiteAsync`: before the first voucher of a SMILE_BO database in this run its
    /// ledger objects must be there; the profile is logged for the operator.
    pub(super) fn check_stock_site(
        &self,
        bo: &mut dyn TxnStockUnit,
        database: &str,
        t: &Ctx,
    ) -> StockResult<()> {
        if self.stock_caches.has(Cache::Ledger, database) {
            return Ok(());
        }
        let profile = bo.ledger_profile(database, t)?;
        if !profile.ready {
            return Err(StockError::Refused(reasons::LEDGER_MISSING));
        }
        self.log.info(
            "SMILE_BO {Database}: {VoucherTypes} IVoucherType row(s); posted SX lines by ITrnCode/ITrnSub: {Codes}",
            &[
                Value::from(database),
                Value::from(profile.voucher_types),
                Value::from(codes_text(&profile.sx_codes).as_str()),
            ],
        );
        if !profile.sx_codes.is_empty()
            && !profile
                .sx_codes
                .iter()
                .any(|c| c.trn_code == 200 && c.trn_sub == 70)
        {
            self.log.warn(
                "SMILE_BO {Database} holds no 200/70 SX line yet; cloud depletions are written as 200/70 (docs/ops/smile-bo-itrncode-map.md)",
                &[Value::from(database)],
            );
        }
        self.stock_caches.add(Cache::Ledger, database);
        Ok(())
    }

    /// `CheckReceiptSiteAsync`: the receipt objects and receiving voucher type, once per run.
    pub(super) fn check_receipt_site(
        &self,
        bo: &mut dyn TxnStockUnit,
        database: &str,
        t: &Ctx,
    ) -> StockResult<()> {
        if self.stock_caches.has(Cache::Receipt, database) {
            return Ok(());
        }
        let ledger = bo.receipt_ledger(database, t)?;
        if !ledger.ready {
            return Err(StockError::Refused(reasons::LEDGER_MISSING));
        }
        let Some(voucher_type) = ledger.voucher_type else {
            return Err(StockError::Refused(reasons::VOUCHER_TYPE_UNKNOWN));
        };
        self.log.info(
            "SMILE_BO {Database}: receiving voucher type {VoucherType}; posted SR lines by ITrnCode/ITrnSub: {Codes}",
            &[
                Value::from(database),
                Value::from(voucher_type.as_str()),
                Value::from(codes_text(&ledger.sr_codes).as_str()),
            ],
        );
        if !ledger.sr_codes.is_empty()
            && !ledger
                .sr_codes
                .iter()
                .any(|c| c.trn_code == 100 && c.trn_sub == 10)
        {
            self.log.warn(
                "SMILE_BO {Database} holds no 100/10 SR line yet; cloud receipts are written as 100/10 (docs/ops/smile-bo-itrncode-map.md)",
                &[Value::from(database)],
            );
        }
        self.stock_caches.add(Cache::Receipt, database);
        Ok(())
    }

    /// `LogStockIssueProfileAsync`: the 200 / 50 profile, logged once per run.
    pub(super) fn log_stock_issue_profile(
        &self,
        bo: &mut dyn TxnStockUnit,
        database: &str,
        t: &Ctx,
    ) -> StockResult<()> {
        if self.stock_caches.has(Cache::IssueProfile, database) {
            return Ok(());
        }
        let profile = bo.ledger_profile(database, t)?;
        if !profile.sx_codes.is_empty()
            && !profile
                .sx_codes
                .iter()
                .any(|c| c.trn_code == 200 && c.trn_sub == 50)
        {
            self.log.warn(
                "SMILE_BO {Database} holds no 200/50 SX line yet; cloud issue vouchers are written as 200/50 (docs/ops/smile-bo-itrncode-map.md)",
                &[Value::from(database)],
            );
        }
        self.stock_caches.add(Cache::IssueProfile, database);
        Ok(())
    }
}

/// The trimmed item codes of the lines, distinct in first-appearance order (ordinal).
pub(super) fn distinct_items<'a>(codes: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    for code in codes {
        let code = code.trim();
        if !items.iter().any(|i| i == code) {
            items.push(code.to_owned());
        }
    }
    items
}
