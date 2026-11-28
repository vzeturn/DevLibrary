//! `txn.shift_open` / `txn.shift_close` / `txn.cash_in` / `txn.cash_out` (ADR 0160, RT-0935;
//! `SiteCommandExecutor.TxnShift.cs`). Open and close carry the full state of a shift (header
//! and counted lines). `CashierShift.ShiftNo` is an identity the agent cannot set (no
//! `IDENTITY_INSERT` without ALTER), so the first command of the aggregate inserts the header,
//! reads the identity back and records it in `Pda_TxnApplied.StoreKey`; later commands update
//! the header's mutable columns by that key and replace the shift's `ShiftMoney` rows. A cash
//! movement appends one signed `DailyCashLog` row and sets the running totals, exactly once per
//! version; a shift the store does not have yet is refused `txn_shift_unknown`. Same safety as
//! `txn.snapshot`: columns and values are checked against the live tables before any write,
//! the applied version is read update-locked (at or below it: `stale`, nothing written), the
//! rows are re-read as capture reads them for the hash, and a store-state refusal rolls the
//! whole transaction back with no log row. Row values are never logged.

use dl_decimal::Decimal;
use dl_json::{Map, Number, Value as Json};

use super::txn::REFUSED;
use super::{SiteCommandExecutor, reasons};
use crate::commands::error::{CommandError, Result};
use crate::commands::txn::{
    TxnAppliedState, TxnCommandUnit, aggregate_hash, convert_rows_with, read_aggregate, read_plan,
};
use crate::commands::txn_value::{TxnInsertValue, TxnLiveColumn, TxnTableWrite, TxnValue};
use crate::commands::units::SiteCommandUnit;
use crate::contracts::WireJson;
use crate::contracts::mapping::CatalogRowMapper;
use crate::contracts::wire::{
    SyncCommand, SyncCommandResult, TxnCashPayload, TxnShiftMoneyLine, TxnShiftPayload,
    TxnShiftResult, txn_command_types, txn_outcomes, txn_reasons, txn_scopes,
};
use crate::reporting::Value;
use crate::util::ctx::Ctx;

const SHIFT_TABLE: &str = "CashierShift";
const MONEY_TABLE: &str = "ShiftMoney";
const SHIFT_NO: &str = txn_scopes::SHIFT_SCOPE_COLUMN;

/// The live columns and read plans of the tables one verb touches (`LiveColumnsAsync`).
struct Live {
    tables: Vec<&'static str>,
    columns: Vec<Vec<TxnLiveColumn>>,
    plans: Vec<(Vec<String>, Vec<String>)>,
}

impl Live {
    fn read(
        unit: &mut dyn TxnCommandUnit,
        mapper: &CatalogRowMapper,
        tables: &[&'static str],
        ctx: &Ctx,
    ) -> Result<Live> {
        let mut live = Live {
            tables: tables.to_vec(),
            columns: Vec::with_capacity(tables.len()),
            plans: Vec::with_capacity(tables.len()),
        };
        for table in tables {
            live.columns.push(unit.columns(table, ctx)?);
            live.plans.push(read_plan(mapper, table)?);
        }
        Ok(live)
    }

    fn index(&self, table: &str) -> Result<usize> {
        self.tables
            .iter()
            .position(|t| *t == table)
            .ok_or_else(|| CommandError::invalid(format!("Table {table} was not read.")))
    }
}

/// One run of a shift or cash verb: the refusal it ended with, if any.
struct ShiftRun<'a> {
    exec: &'a SiteCommandExecutor,
    c: &'a SyncCommand,
    mapper: &'a CatalogRowMapper,
    aggregate: String,
    refused: Option<&'static str>,
}

impl ShiftRun<'_> {
    /// Records the refusal and returns the error that rolls the transaction back.
    fn refuse(&mut self, reason: &'static str) -> CommandError {
        self.refused = Some(reason);
        CommandError::invalid(REFUSED)
    }

    /// `ConvertOrRefuse`: converts rows against the live table or refuses the command
    /// (`txn_column_unknown` / `txn_value_invalid`), logging no values.
    fn convert(
        &mut self,
        live: &Live,
        table: &'static str,
        rows: &[Option<Map>],
        require_all: bool,
    ) -> Result<Vec<Vec<TxnInsertValue>>> {
        let i = live.index(table)?;
        match convert_rows_with(&live.columns[i], rows, &live.plans[i].1, require_all) {
            Ok(converted) => Ok(converted),
            Err(reason) => {
                self.exec.log.warn(
                    "Site command {Id} ({Type}) refused: {Aggregate} table {Table} {Reason}",
                    &[
                        Value::from(self.c.command_id),
                        Value::from(self.c.type_.as_str()),
                        Value::from(self.aggregate.as_str()),
                        Value::from(table),
                        Value::from(reason),
                    ],
                );
                Err(self.refuse(reason))
            }
        }
    }

    /// The first converted row (each call converts exactly one).
    fn convert_one(
        &mut self,
        live: &Live,
        table: &'static str,
        row: Map,
        require_all: bool,
    ) -> Result<Vec<TxnInsertValue>> {
        self.convert(live, table, &[Some(row)], require_all)?
            .pop()
            .ok_or_else(|| CommandError::invalid(format!("{table} gave no converted row.")))
    }

    /// `ReadAggregateAsync` over `tables` of `live`.
    fn read(
        &self,
        unit: &mut dyn TxnCommandUnit,
        live: &Live,
        scope_column: &str,
        key: i64,
        tables: &[&str],
        ctx: &Ctx,
    ) -> Result<(String, Vec<(String, i32)>)> {
        let mut columns = Vec::with_capacity(tables.len());
        let mut plans = Vec::with_capacity(tables.len());
        for table in tables {
            let i = live.index(table)?;
            columns.push(live.columns[i].clone());
            plans.push(live.plans[i].clone());
        }
        read_aggregate(
            unit,
            self.mapper,
            scope_column,
            key,
            tables,
            &columns,
            &plans,
            ctx,
        )
    }

    fn applied(&self, result: &TxnShiftResult) -> SyncCommandResult {
        self.exec.applied(self.c, Some(result.to_element()))
    }

    /// `ShiftStaleAsync`: a version at or below the applied one changes nothing; the
    /// answer is the shift's current hash.
    fn stale(
        &self,
        unit: &mut dyn TxnCommandUnit,
        live: &Live,
        version: i64,
        applied: TxnAppliedState,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        let (hash, row_counts) = match applied.store_key {
            Some(key) => self.read(unit, live, SHIFT_NO, key, &txn_scopes::SHIFT_TABLES, ctx)?,
            None => (
                aggregate_hash(Vec::new()),
                txn_scopes::SHIFT_TABLES
                    .iter()
                    .map(|t| ((*t).to_owned(), 0))
                    .collect(),
            ),
        };
        Ok(self.applied(&TxnShiftResult {
            aggregate: self.aggregate.clone(),
            version,
            outcome: txn_outcomes::STALE.to_owned(),
            applied_version: applied.version,
            hash,
            row_counts,
            shift_no: applied.store_key.unwrap_or(0),
            trn_seq: None,
            cash_log_hash: None,
        }))
    }

    /// `RunShiftAsync`.
    fn shift(
        &mut self,
        p: &TxnShiftPayload,
        unit: &mut dyn TxnCommandUnit,
        t: &Ctx,
    ) -> Result<SyncCommandResult> {
        let live = Live::read(unit, self.mapper, &txn_scopes::SHIFT_TABLES, t)?;
        let insert_row = self.convert_one(&live, SHIFT_TABLE, shift_header_row(p, true)?, true)?;
        let update_row =
            self.convert_one(&live, SHIFT_TABLE, shift_header_row(p, false)?, false)?;
        // ShiftNo is not known before the header is written: money rows are checked with a
        // placeholder and bound afterwards.
        let lines: Vec<Option<Map>> = p
            .money_lines()
            .iter()
            .map(|l| Some(money_row(p, l)))
            .collect();
        let money = self.convert(&live, MONEY_TABLE, &lines, true)?;

        let applied = unit.lock_applied(&self.aggregate, t)?;
        if let Some(stored) = applied
            && stored.version >= p.version
        {
            return self.stale(unit, &live, p.version, stored, t);
        }

        let shift_no = match applied.and_then(|a| a.store_key) {
            Some(known) => {
                unit.update(SHIFT_TABLE, SHIFT_NO, known, &update_row, t)?;
                known
            }
            None => unit.insert(SHIFT_TABLE, &insert_row, t)?,
        };

        let rows = money
            .into_iter()
            .map(|r| bind_shift_no(r, shift_no))
            .collect::<Result<Vec<_>>>()?;
        let write = TxnTableWrite {
            table: MONEY_TABLE.to_owned(),
            rows,
        };
        if !unit.replace(SHIFT_NO, shift_no, &[write], t)? {
            self.exec.log.warn(
                "Site command {Id} ({Type}) refused: {Aggregate} v{Version} collides with another aggregate's key",
                &[
                    Value::from(self.c.command_id),
                    Value::from(self.c.type_.as_str()),
                    Value::from(self.aggregate.as_str()),
                    Value::from(p.version),
                ],
            );
            return Err(self.refuse(txn_reasons::KEY_CONFLICT));
        }

        let (hash, row_counts) = self.read(
            unit,
            &live,
            SHIFT_NO,
            shift_no,
            &txn_scopes::SHIFT_TABLES,
            t,
        )?;
        unit.save_applied_keyed(
            &self.aggregate,
            p.version,
            &hash,
            self.c.command_id,
            Some(shift_no),
            t,
        )?;
        Ok(self.applied(&TxnShiftResult {
            aggregate: self.aggregate.clone(),
            version: p.version,
            outcome: txn_outcomes::APPLIED.to_owned(),
            applied_version: p.version,
            hash,
            row_counts,
            shift_no,
            trn_seq: None,
            cash_log_hash: None,
        }))
    }

    /// `RunCashAsync`.
    fn cash(
        &mut self,
        p: &TxnCashPayload,
        unit: &mut dyn TxnCommandUnit,
        t: &Ctx,
    ) -> Result<SyncCommandResult> {
        let [shift, money] = txn_scopes::SHIFT_TABLES;
        let live = Live::read(
            unit,
            self.mapper,
            &[shift, money, txn_scopes::CASH_LOG_TABLE],
            t,
        )?;
        let log_row = self.convert_one(
            &live,
            txn_scopes::CASH_LOG_TABLE,
            cash_log_row(p, &self.c.type_)?,
            true,
        )?;
        let totals = self.convert_one(&live, SHIFT_TABLE, totals_row(p)?, false)?;

        let applied = unit.lock_applied(&self.aggregate, t)?;
        if let Some(stored) = applied
            && stored.version >= p.version
        {
            return self.stale(unit, &live, p.version, stored, t);
        }
        let Some(shift_no) = applied.and_then(|a| a.store_key) else {
            self.exec.log.warn(
                "Site command {Id} ({Type}) refused: {Aggregate} movement {Movement} has no shift in the store yet",
                &[
                    Value::from(self.c.command_id),
                    Value::from(self.c.type_.as_str()),
                    Value::from(self.aggregate.as_str()),
                    Value::from(p.movement_key),
                ],
            );
            return Err(self.refuse(txn_reasons::SHIFT_UNKNOWN));
        };

        unit.update(SHIFT_TABLE, SHIFT_NO, shift_no, &totals, t)?;
        let trn_seq = unit.insert(txn_scopes::CASH_LOG_TABLE, &log_row, t)?;

        let (hash, row_counts) = self.read(
            unit,
            &live,
            SHIFT_NO,
            shift_no,
            &txn_scopes::SHIFT_TABLES,
            t,
        )?;
        let (log_hash, _) = self.read(
            unit,
            &live,
            txn_scopes::CASH_LOG_KEY_COLUMN,
            trn_seq,
            &[txn_scopes::CASH_LOG_TABLE],
            t,
        )?;
        unit.save_applied_keyed(
            &self.aggregate,
            p.version,
            &hash,
            self.c.command_id,
            Some(shift_no),
            t,
        )?;
        Ok(self.applied(&TxnShiftResult {
            aggregate: self.aggregate.clone(),
            version: p.version,
            outcome: txn_outcomes::APPLIED.to_owned(),
            applied_version: p.version,
            hash,
            row_counts,
            shift_no,
            trn_seq: Some(trn_seq),
            cash_log_hash: Some(log_hash),
        }))
    }
}

/// Which verb body a run executes.
enum Verb<'p> {
    Shift(&'p TxnShiftPayload),
    Cash(&'p TxnCashPayload),
}

impl SiteCommandExecutor {
    /// `txn.shift_open` / `txn.shift_close` (`TxnShiftAsync`).
    pub(super) fn txn_shift(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let Some(p) = TxnShiftPayload::from_element(&c.payload) else {
            return Ok(self.invalid(c, "payload is not a txn shift object"));
        };
        if let Some(problem) = p.problem(Some(&c.type_)) {
            return Ok(self.invalid(c, &problem));
        }
        self.run_txn_verb(c, p.shift_key, p.aggregate(), Verb::Shift(&p), ctx)
    }

    /// `txn.cash_in` / `txn.cash_out` (`TxnCashAsync`).
    pub(super) fn txn_cash(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let Some(p) = TxnCashPayload::from_element(&c.payload) else {
            return Ok(self.invalid(c, "payload is not a txn cash object"));
        };
        if let Some(problem) = p.problem(Some(&c.type_)) {
            return Ok(self.invalid(c, &problem));
        }
        self.run_txn_verb(c, p.shift_key, p.aggregate(), Verb::Cash(&p), ctx)
    }

    /// `RunTxnVerbAsync`: the aggregate-key and hasher checks, then the verb in
    /// `run_once`; a store-state refusal leaves it with an error (rolled back, no log row)
    /// and answers rejected.
    fn run_txn_verb(
        &self,
        c: &SyncCommand,
        shift_key: i64,
        aggregate: String,
        verb: Verb<'_>,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        if !c.aggregate_key.is_empty()
            && c.aggregate_key != txn_scopes::aggregate_key(txn_scopes::SHIFT, shift_key)
        {
            return Ok(self.invalid(c, "AggregateKey does not match the shift"));
        }
        let Some(mapper) = self.mapper.clone() else {
            return Ok(self.reject(c, reasons::UNKNOWN_TYPE));
        };
        let mut run = ShiftRun {
            exec: self,
            c,
            mapper: &mapper,
            aggregate,
            refused: None,
        };
        let r = self.store.run_once(
            c.command_id,
            &mut |unit: &mut dyn SiteCommandUnit, t: &Ctx| {
                run.refused = None;
                match &verb {
                    Verb::Shift(p) => run.shift(p, unit.txn(), t),
                    Verb::Cash(p) => run.cash(p, unit.txn(), t),
                }
            },
            ctx,
        );
        match run.refused {
            Some(reason) => Ok(self.reject(c, reason)),
            None => r,
        }
    }
}

/// A decimal as the JSON number System.Text.Json writes (scale kept).
fn number(d: &Decimal) -> Result<Json> {
    Number::from_raw(&d.to_string())
        .map(Json::Number)
        .ok_or_else(|| CommandError::invalid(format!("{d} is not a JSON number.")))
}

/// `ShiftHeaderRow`: all header columns on the first write, only the mutable ones
/// afterwards (the key columns never change).
fn shift_header_row(p: &TxnShiftPayload, insert: bool) -> Result<Map> {
    let mut row = Map::new();
    if insert {
        row.insert("RVCNo", p.rvc);
        row.insert("Cashier", p.cashier.as_str());
        row.insert("StartTime", p.start_time.as_str());
        row.insert("OpenDate", p.open_date.as_str());
    }
    row.insert("ShiftStatus", p.shift_status);
    row.insert("CashOpen", number(&p.cash_open)?);
    row.insert("CashIn", number(&p.cash_in)?);
    row.insert("CashOut", number(&p.cash_out)?);
    if let Some(end) = &p.cash_end {
        row.insert("CashEnd", number(end)?);
    }
    if let Some(end_time) = &p.end_time {
        row.insert("EndTime", end_time.as_str());
    }
    if let Some(close_date) = &p.close_date {
        row.insert("CloseDate", close_date.as_str());
    }
    if let Some(notes) = &p.notes {
        row.insert("ShiftNotes", notes.as_str());
    }
    Ok(row)
}

/// `MoneyRow`: one counted line, `ShiftNo` a placeholder bound after the header write.
fn money_row(p: &TxnShiftPayload, line: &TxnShiftMoneyLine) -> Map {
    let mut row = Map::new();
    row.insert("CrCode", line.cr_code);
    row.insert("RVCNo", p.rvc);
    row.insert("POSDate", p.open_date.as_str());
    row.insert(SHIFT_NO, 0);
    row.insert("Cashier", p.cashier.as_str());
    row.insert("NoteName", line.note_name.as_str());
    row.insert("QTy", line.qty);
    row.insert("Amount", line.amount);
    row.insert("TrnCode", line.trn_code);
    row.insert("TrnTime", line.trn_time.as_str());
    row
}

/// `TotalsRow`: the shift's running cash totals.
fn totals_row(p: &TxnCashPayload) -> Result<Map> {
    let mut row = Map::new();
    row.insert("CashIn", number(&p.shift_cash_in)?);
    row.insert("CashOut", number(&p.shift_cash_out)?);
    Ok(row)
}

/// `CashLogRow`: cash out is stored negative (ADR 0160 §3), the business date in
/// `TrnDate`, the movement time in `PostTime`.
fn cash_log_row(p: &TxnCashPayload, type_: &str) -> Result<Map> {
    let amount = if type_ == txn_command_types::CASH_OUT {
        -p.amount
    } else {
        p.amount
    };
    let mut row = Map::new();
    row.insert("TrnDate", p.business_date.as_str());
    row.insert("TrnDesc", p.reason.as_str());
    row.insert("TrnAmount", number(&amount)?);
    row.insert("PostdBy", p.cashier.as_str());
    row.insert("PostTime", p.at.as_str());
    Ok(row)
}

/// `BindShiftNo`: replaces the placeholder `ShiftNo` of a checked money row with the
/// shift's store key, in the column's own value type (`Convert.ChangeType`).
fn bind_shift_no(row: Vec<TxnInsertValue>, shift_no: i64) -> Result<Vec<TxnInsertValue>> {
    row.into_iter()
        .map(|mut v| {
            if v.column.name.eq_ignore_ascii_case(SHIFT_NO)
                && let Some(old) = &v.value
            {
                v.value = Some(change_type(old, shift_no)?);
            }
            Ok(v)
        })
        .collect()
}

fn change_type(like: &TxnValue, n: i64) -> Result<TxnValue> {
    let overflow = |t: &str| {
        CommandError::invalid(format!("Value was either too large or too small for {t}."))
    };
    Ok(match like {
        TxnValue::I64(_) => TxnValue::I64(n),
        TxnValue::I32(_) => TxnValue::I32(i32::try_from(n).map_err(|_| overflow("an Int32"))?),
        TxnValue::I16(_) => TxnValue::I16(i16::try_from(n).map_err(|_| overflow("an Int16"))?),
        TxnValue::U8(_) => TxnValue::U8(u8::try_from(n).map_err(|_| overflow("an unsigned byte"))?),
        TxnValue::Decimal(_) => TxnValue::Decimal(Decimal::from(n)),
        #[allow(clippy::cast_precision_loss)]
        TxnValue::F64(_) => TxnValue::F64(n as f64),
        #[allow(clippy::cast_precision_loss)]
        TxnValue::F32(_) => TxnValue::F32(n as f32),
        TxnValue::Text(_) => TxnValue::Text(n.to_string()),
        TxnValue::Bool(_) => TxnValue::Bool(n != 0),
        TxnValue::DateTime(_) | TxnValue::Bytes(_) | TxnValue::Uuid(_) => {
            return Err(CommandError::invalid(format!(
                "{SHIFT_NO} cannot be bound to a {like:?} column."
            )));
        }
    })
}
