//! `txn.room_charge` / `txn.room_charge_void` (ADR 0162, RT-0908;
//! `SiteCommandExecutor.RoomCharge.cs`): posts the cloud bill's room charge to the in-house
//! folio of its room through the vendor `PostInterfaceTransactionToFolio`, exactly as POS WIN
//! does, in one transaction with the command log row. The bill is the folio reference
//! (`RefNumber`), so a newer version first reverses the rows it posted before and posts the
//! new state once; a version at or below the applied one is `stale` and writes nothing.
//! Refused (rolled back, no log row, a redrive runs again once fixed): Front Office database
//! unusable, no or many in-house folios, tender not mapped to a folio code.

use dl_decimal::Decimal;

use super::SiteCommandExecutor;
use super::txn::REFUSED;
use crate::commands::error::{CommandError, Result};
use crate::commands::txn::{TxnAppliedState, aggregate_hash};
use crate::commands::txn_room_charge::{FoActiveRow, FoPostLine, ROOM_CHARGE_CLERK};
use crate::commands::units::SiteCommandUnit;
use crate::contracts::wire::{
    SyncCommand, SyncCommandResult, TxnRoomChargePayload, TxnRoomChargeResult,
    TxnRoomChargeVoidPayload, room_charge_hash_line, txn_outcomes, txn_reasons,
    txn_room_charge_scope,
};
use crate::contracts::{DateTime, WireJson};
use crate::reporting::Value;
use crate::util::ctx::Ctx;

/// A store-state refusal (`TxnRefusedException`) or a real error.
enum Fail {
    Refused(&'static str),
    Error(CommandError),
}

impl From<CommandError> for Fail {
    fn from(e: CommandError) -> Self {
        Fail::Error(e)
    }
}

type Step<T> = std::result::Result<T, Fail>;

/// `RoomChargeHash`.
fn room_charge_hash(rows: &[FoActiveRow]) -> String {
    aggregate_hash(
        rows.iter()
            .map(|r| room_charge_hash_line(r.transaction_id, r.amount))
            .collect(),
    )
}

fn sum(rows: &[FoActiveRow]) -> Decimal {
    rows.iter().fold(Decimal::ZERO, |a, r| a + r.amount)
}

fn count(rows: &[FoActiveRow]) -> i32 {
    i32::try_from(rows.len()).unwrap_or(i32::MAX)
}

/// `ReverseActiveAsync`.
fn reverse_active(
    unit: &mut dyn SiteCommandUnit,
    database: &str,
    folio: i64,
    reference: &str,
    comment: &str,
    t: &Ctx,
) -> Result<()> {
    let fo = unit.room_charge();
    let rows = fo.lock_active(database, folio, reference, t)?;
    if !rows.is_empty() {
        let ids: Vec<i64> = rows.iter().map(|r| r.transaction_id).collect();
        fo.reverse(database, &ids, comment, t)?;
    }
    Ok(())
}

fn database(unit: &mut dyn SiteCommandUnit, rvc: i32, t: &Ctx) -> Step<String> {
    unit.room_charge()
        .resolve_database(rvc, t)?
        .ok_or(Fail::Refused(txn_reasons::FOLIO_DATABASE_INVALID))
}

impl SiteCommandExecutor {
    /// `txn.room_charge` (`TxnRoomChargeAsync`).
    pub(super) fn txn_room_charge(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let Some(p) = TxnRoomChargePayload::from_element(&c.payload) else {
            return Ok(self.invalid(c, "payload is not a txn.room_charge object"));
        };
        if let Some(problem) = p.problem() {
            return Ok(self.invalid(c, &problem));
        }
        self.run_room_charge(
            c,
            p.check_no,
            p.rvc,
            &mut |unit, t| self.post_room_charge(c, &p, unit, t),
            ctx,
        )
    }

    /// `txn.room_charge_void` (`TxnRoomChargeVoidAsync`).
    pub(super) fn txn_room_charge_void(
        &self,
        c: &SyncCommand,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        let Some(p) = TxnRoomChargeVoidPayload::from_element(&c.payload) else {
            return Ok(self.invalid(c, "payload is not a txn.room_charge_void object"));
        };
        if let Some(problem) = p.problem() {
            return Ok(self.invalid(c, &problem));
        }
        self.run_room_charge(
            c,
            p.check_no,
            p.rvc,
            &mut |unit, t| self.void_room_charge(c, &p, unit, t),
            ctx,
        )
    }

    /// `RunRoomChargeAsync`: the aggregate-key check, then the work in `run_once`; a
    /// refusal rolls back (no log row) and answers rejected.
    fn run_room_charge(
        &self,
        c: &SyncCommand,
        check_no: i64,
        rvc: i32,
        work: &mut dyn FnMut(&mut dyn SiteCommandUnit, &Ctx) -> Step<SyncCommandResult>,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        if !c.aggregate_key.is_empty()
            && c.aggregate_key != txn_room_charge_scope::aggregate_key(check_no)
        {
            return Ok(self.invalid(c, "AggregateKey does not match the bill"));
        }
        let mut refused: Option<&'static str> = None;
        let r = self.store.run_once(
            c.command_id,
            &mut |unit: &mut dyn SiteCommandUnit, t: &Ctx| {
                refused = None;
                match work(unit, t) {
                    Ok(result) => Ok(result),
                    Err(Fail::Refused(reason)) => {
                        refused = Some(reason);
                        Err(CommandError::invalid(REFUSED))
                    }
                    Err(Fail::Error(e)) => Err(e),
                }
            },
            ctx,
        );
        match refused {
            Some(reason) => {
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
                Ok(self.reject(c, reason))
            }
            None => r,
        }
    }

    /// `PostRoomChargeAsync`.
    fn post_room_charge(
        &self,
        c: &SyncCommand,
        p: &TxnRoomChargePayload,
        unit: &mut dyn SiteCommandUnit,
        t: &Ctx,
    ) -> Step<SyncCommandResult> {
        let aggregate = txn_room_charge_scope::aggregate(p.check_no);
        let applied = unit.txn().lock_applied(&aggregate, t)?;
        if let Some(stored) = applied.as_ref().filter(|s| s.version >= p.version) {
            return Ok(self.room_charge_stale(c, p.rvc, &aggregate, p.version, stored, unit, t)?);
        }
        let database = database(unit, p.rvc, t)?;
        let link = unit
            .room_charge()
            .link_code(p.rvc, p.trn_code, p.trn_sub, t)?;
        let Some(link) = link.filter(|l| *l >= 100) else {
            return Err(Fail::Refused(txn_reasons::FOLIO_LINK_UNKNOWN));
        };

        let room = p.room_no.trim();
        let mut folios = unit
            .room_charge()
            .in_house_folios(&database, room, p.folio_num, t)?;
        if p.folio_num > 0 {
            folios.retain(|f| f.room_code.to_uppercase() == room.to_uppercase());
        }
        let folio = match folios.as_slice() {
            [] => return Err(Fail::Refused(txn_reasons::FOLIO_UNAVAILABLE)),
            [one] => one.folio_num,
            _ => return Err(Fail::Refused(txn_reasons::FOLIO_AMBIGUOUS)),
        };

        let reference = p.check_no.to_string();
        let comment = format!("Cloud bill {reference} re-posted");
        // A newer version replaces the earlier posting: reverse what it left on the folio it
        // used (the guest may have moved room).
        if let Some(earlier) = applied
            .as_ref()
            .and_then(|a| a.store_key)
            .filter(|e| *e != folio)
        {
            reverse_active(unit, &database, earlier, &reference, &comment, t)?;
        }
        reverse_active(unit, &database, folio, &reference, &comment, t)?;

        let at = DateTime::try_parse_invariant(&p.at)
            .ok_or_else(|| CommandError::invalid("At must be a local date-time."))?;
        let line_comment = match p.comment.as_deref().map(str::trim) {
            Some(text) if !text.is_empty() => text.to_owned(),
            _ => format!("Cloud bill {reference}"),
        };
        unit.room_charge().post(
            &database,
            &FoPostLine {
                folio_num: folio,
                transaction_code: link / 100,
                transaction_sub_code: link % 100,
                ref_number: reference.clone(),
                amount: p.amount,
                meal_period: p.meal_period,
                comment: line_comment,
                clerk_id: ROOM_CHARGE_CLERK.to_owned(),
                transaction_time: at,
                sts_code8: p.rvc.to_string(),
            },
            t,
        )?;

        let active = unit
            .room_charge()
            .lock_active(&database, folio, &reference, t)?;
        let hash = room_charge_hash(&active);
        unit.txn().save_applied_keyed(
            &aggregate,
            p.version,
            &hash,
            c.command_id,
            Some(folio),
            t,
        )?;
        self.log.info(
            "Site command {Id} (txn.room_charge) bill {Check} RVC {Rvc} posted to folio {Folio} ({Rows} active rows)",
            &[
                Value::from(c.command_id),
                Value::from(p.check_no),
                Value::from(p.rvc),
                Value::from(folio),
                Value::from(active.len()),
            ],
        );
        let result = TxnRoomChargeResult {
            aggregate,
            version: p.version,
            outcome: txn_outcomes::APPLIED.to_owned(),
            applied_version: p.version,
            folio_num: folio,
            active_amount: sum(&active),
            active_rows: count(&active),
            hash,
        };
        Ok(self.applied(c, Some(result.to_element())))
    }

    /// `VoidRoomChargeAsync`.
    fn void_room_charge(
        &self,
        c: &SyncCommand,
        p: &TxnRoomChargeVoidPayload,
        unit: &mut dyn SiteCommandUnit,
        t: &Ctx,
    ) -> Step<SyncCommandResult> {
        let aggregate = txn_room_charge_scope::aggregate(p.check_no);
        let applied = unit.txn().lock_applied(&aggregate, t)?;
        if let Some(stored) = applied.as_ref().filter(|s| s.version >= p.version) {
            return Ok(self.room_charge_stale(c, p.rvc, &aggregate, p.version, stored, unit, t)?);
        }
        let folio = applied.as_ref().and_then(|a| a.store_key).unwrap_or(0);
        // Nothing was ever posted: the void is recorded (so an older post can no longer land)
        // and changes no folio.
        if folio > 0 {
            let database = database(unit, p.rvc, t)?;
            let check = p.check_no.to_string();
            let comment = format!("Void cloud bill {check}: {}", p.reason.trim());
            reverse_active(unit, &database, folio, &check, &comment, t)?;
        }
        let hash = room_charge_hash(&[]);
        unit.txn().save_applied_keyed(
            &aggregate,
            p.version,
            &hash,
            c.command_id,
            (folio > 0).then_some(folio),
            t,
        )?;
        self.log.info(
            "Site command {Id} (txn.room_charge_void) bill {Check} RVC {Rvc} voided on folio {Folio}",
            &[
                Value::from(c.command_id),
                Value::from(p.check_no),
                Value::from(p.rvc),
                Value::from(folio),
            ],
        );
        let result = TxnRoomChargeResult {
            aggregate,
            version: p.version,
            outcome: txn_outcomes::APPLIED.to_owned(),
            applied_version: p.version,
            folio_num: folio,
            active_amount: Decimal::ZERO,
            active_rows: 0,
            hash,
        };
        Ok(self.applied(c, Some(result.to_element())))
    }

    /// `RoomChargeStaleAsync`: the recorded state, with the folio's current active rows.
    #[allow(clippy::too_many_arguments)]
    fn room_charge_stale(
        &self,
        c: &SyncCommand,
        rvc: i32,
        aggregate: &str,
        version: i64,
        applied: &TxnAppliedState,
        unit: &mut dyn SiteCommandUnit,
        t: &Ctx,
    ) -> Result<SyncCommandResult> {
        let reference = aggregate.rsplit(':').next().unwrap_or(aggregate);
        let mut rows = Vec::new();
        if let Some(folio) = applied.store_key.filter(|f| *f > 0) {
            let fo = unit.room_charge();
            if let Some(database) = fo.resolve_database(rvc, t)? {
                rows = fo.lock_active(&database, folio, reference, t)?;
            }
        }
        let result = TxnRoomChargeResult {
            aggregate: aggregate.to_owned(),
            version,
            outcome: txn_outcomes::STALE.to_owned(),
            applied_version: applied.version,
            folio_num: applied.store_key.unwrap_or(0),
            active_amount: sum(&rows),
            active_rows: count(&rows),
            hash: room_charge_hash(&rows),
        };
        Ok(self.applied(c, Some(result.to_element())))
    }
}
