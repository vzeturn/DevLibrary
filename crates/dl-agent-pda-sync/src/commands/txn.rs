//! `txn.snapshot` (ADR 0159, RT-0849; `SiteCommandExecutor.Txn.cs`, `ITxnCommandUnit.cs`,
//! `TxnAggregateHash.cs`): the cloud sends the full state of one bill at one version and
//! the agent replaces the bill's rows in the six allow-listed tables. Everything runs in
//! the command transaction: columns and values are checked against the live tables before
//! any write, the applied version is read update-locked (a version at or below it changes
//! nothing, outcome `stale`), the replace runs under a savepoint (a primary-key clash with
//! another bill rolls back to it and rejects `txn_key_conflict`, the log row still
//! commits), and the rows are re-read as capture reads them to return the aggregate hash.
//! Row values are never logged (personal data).
//! The executor side lives in `executor/txn.rs`.

use dl_encoding::hex;
use dl_hash::{Digest, Sha256};
use dl_json::{Map, Value as Json};
use dl_uuid::Uuid;

use super::error::{CommandError, Result};
use super::txn_value::{self, TxnInsertValue, TxnLiveColumn, TxnTableWrite};
use crate::capture::plan;
use crate::contracts::mapping::{CatalogRowMapper, VendorRow};
use crate::contracts::wire::txn_reasons;
use crate::util::ctx::Ctx;

/// `dbo.Pda_TxnApplied` of a keyed aggregate (`TxnAppliedState`, ADR 0160): the version and
/// the identity the store gave the aggregate's first row (`StoreKey`), `None` when not yet
/// known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxnAppliedState {
    /// Version recorded for the aggregate.
    pub version: i64,
    /// `Pda_TxnApplied.StoreKey`.
    pub store_key: Option<i64>,
}

/// The store reads and writes of `txn.snapshot` inside the transaction of
/// [`super::SiteCommandStore::run_once`] (`ITxnCommandUnit`). Table names come only from
/// [`txn_scopes`]; every value is a parameter.
pub trait TxnCommandUnit {
    /// The live columns of `dbo.<table>` in column order; an error when the table does
    /// not exist.
    fn columns(&mut self, table: &str, ctx: &Ctx) -> Result<Vec<TxnLiveColumn>>;

    /// The version recorded in `dbo.Pda_TxnApplied` for `aggregate`, read with
    /// `UPDLOCK, HOLDLOCK`; `None` when the aggregate was never written.
    fn lock_applied_version(&mut self, aggregate: &str, ctx: &Ctx) -> Result<Option<i64>>;

    /// Under a savepoint, deletes the rows of `scope_key` from each table and inserts the
    /// given rows. False (everything since the savepoint rolled back) on a primary-key or
    /// unique violation (SQL 2627/2601).
    fn replace(
        &mut self,
        scope_column: &str,
        scope_key: i64,
        tables: &[TxnTableWrite],
        ctx: &Ctx,
    ) -> Result<bool>;

    /// The rows of `scope_key` in `dbo.<table>` as capture reads them: only `columns`,
    /// those in `bytes_columns` as `CONVERT(varbinary(max), ...)`.
    fn read_rows(
        &mut self,
        table: &str,
        scope_column: &str,
        scope_key: i64,
        columns: &[String],
        bytes_columns: &[String],
        ctx: &Ctx,
    ) -> Result<Vec<VendorRow>>;

    /// Records `version`, the aggregate hash and the command id for `aggregate`.
    fn save_applied(
        &mut self,
        aggregate: &str,
        version: i64,
        hash: &str,
        command_id: Uuid,
        ctx: &Ctx,
    ) -> Result<()>;

    /// The applied state of a keyed aggregate (ADR 0160), read with `UPDLOCK, HOLDLOCK`;
    /// `None` when the aggregate was never written.
    fn lock_applied(&mut self, aggregate: &str, ctx: &Ctx) -> Result<Option<TxnAppliedState>>;

    /// Inserts one row into a shift table (`CashierShift`, `ShiftMoney`, `DailyCashLog`)
    /// and returns the identity the store gave it (`SCOPE_IDENTITY()`).
    fn insert(&mut self, table: &str, row: &[TxnInsertValue], ctx: &Ctx) -> Result<i64>;

    /// Updates the given columns of the shift-table row whose `key_column` (`ShiftNo` only)
    /// is `key`; an error when no row matches. No values: nothing runs.
    fn update(
        &mut self,
        table: &str,
        key_column: &str,
        key: i64,
        values: &[TxnInsertValue],
        ctx: &Ctx,
    ) -> Result<()>;

    /// [`TxnCommandUnit::save_applied`] plus the store key of a keyed aggregate.
    fn save_applied_keyed(
        &mut self,
        aggregate: &str,
        version: i64,
        hash: &str,
        command_id: Uuid,
        store_key: Option<i64>,
        ctx: &Ctx,
    ) -> Result<()>;
}

/// `TxnAggregateHash.Line`: `{table}|{rowHash}\n`.
pub fn aggregate_line(table: &str, row_hash: &str) -> String {
    format!("{table}|{row_hash}\n")
}

/// `TxnAggregateHash.Of`: SHA-256 (lower hex) over the lines sorted ordinally. The cloud
/// computes the same value from its mirror rows, so the line format must never change.
pub fn aggregate_hash(mut lines: Vec<String>) -> String {
    lines.sort();
    hex::encode(Sha256::digest(lines.concat().as_bytes()))
}

/// `ReadPlan`: never-sync columns capture does not read, and the TCVN3 character columns
/// it reads as bytes.
pub(super) fn read_plan(
    mapper: &CatalogRowMapper,
    table: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    let Some(t) = mapper.catalog().find(table) else {
        return Err(CommandError::invalid(format!(
            "Table {table} is not in the sync catalog."
        )));
    };
    let bytes = t
        .transforms
        .as_ref()
        .and_then(|x| x.tcvn3_decode.clone())
        .unwrap_or_default();
    Ok((plan::exclude_of(mapper.catalog(), t), bytes))
}

pub(super) fn contains_ci(list: &[String], name: &str) -> bool {
    list.iter().any(|x| x.eq_ignore_ascii_case(name))
}

/// `ConvertRows`: the rows of one table as insert values in live-column order, or the
/// rejection reason (`txn_column_unknown` / `txn_value_invalid`). A NOT NULL column
/// without a default must be present.
pub fn convert_rows(
    columns: &[TxnLiveColumn],
    rows: &[Option<Map>],
    tcvn3: &[String],
) -> std::result::Result<Vec<Vec<TxnInsertValue>>, &'static str> {
    convert_rows_with(columns, rows, tcvn3, true)
}

/// [`convert_rows`] where `require_all` false accepts a partial row (an UPDATE, ADR 0160):
/// a missing NOT NULL column is then not a refusal.
pub fn convert_rows_with(
    columns: &[TxnLiveColumn],
    rows: &[Option<Map>],
    tcvn3: &[String],
    require_all: bool,
) -> std::result::Result<Vec<Vec<TxnInsertValue>>, &'static str> {
    let empty = Map::new();
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let row = row.as_ref().unwrap_or(&empty);
        let mut given: Vec<(&str, &Json)> = Vec::with_capacity(row.len());
        for (key, value) in row.iter() {
            let known = columns.iter().any(|c| c.name.eq_ignore_ascii_case(key));
            if !known || given.iter().any(|(k, _)| k.eq_ignore_ascii_case(key)) {
                return Err(txn_reasons::COLUMN_UNKNOWN);
            }
            given.push((key.as_str(), value));
        }
        let mut values = Vec::with_capacity(given.len());
        for column in columns {
            let Some((_, json)) = given
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&column.name))
            else {
                if require_all && !column.nullable && !column.has_default && !column.generated {
                    return Err(txn_reasons::VALUE_INVALID);
                }
                continue;
            };
            let is_tcvn3 = contains_ci(tcvn3, &column.name);
            let Some(converted) = txn_value::try_convert(column, json, is_tcvn3) else {
                return Err(txn_reasons::VALUE_INVALID);
            };
            let tcvn3 = is_tcvn3 && converted.is_some();
            values.push(TxnInsertValue {
                column: column.clone(),
                value: converted,
                tcvn3,
            });
        }
        result.push(values);
    }
    Ok(result)
}

/// `ReadAggregateAsync`: re-reads the aggregate the way capture does and returns its
/// aggregate hash and the row count per table (scope order).
#[allow(clippy::too_many_arguments)]
pub(super) fn read_aggregate(
    unit: &mut dyn TxnCommandUnit,
    mapper: &CatalogRowMapper,
    scope_column: &str,
    scope_key: i64,
    tables: &[&str],
    live: &[Vec<TxnLiveColumn>],
    plans: &[(Vec<String>, Vec<String>)],
    ctx: &Ctx,
) -> Result<(String, Vec<(String, i32)>)> {
    let mut lines = Vec::new();
    let mut counts = Vec::with_capacity(tables.len());
    for ((table, columns), (exclude, bytes)) in tables.iter().zip(live).zip(plans) {
        let read: Vec<String> = columns
            .iter()
            .map(|c| c.name.clone())
            .filter(|n| !contains_ci(exclude, n))
            .collect();
        let rows = unit.read_rows(table, scope_column, scope_key, &read, bytes, ctx)?;
        counts.push((
            (*table).to_owned(),
            i32::try_from(rows.len()).unwrap_or(i32::MAX),
        ));
        for row in &rows {
            let canon = mapper.canonicalize(table, row).map_err(|e| {
                CommandError::invalid(format!("Row of {table} cannot be hashed: {e}"))
            })?;
            lines.push(aggregate_line(table, &canon.hash));
        }
    }
    Ok((aggregate_hash(lines), counts))
}
