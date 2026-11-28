//! [`TxnCommandUnit`] on the command transaction (`SqlTxnCommandUnit.cs`, ADR 0159).
//! Table names come only from the `txn_scopes` allow-list and are quoted; every value is
//! a parameter. Reads mirror the capture source (live column list, TCVN3 character
//! columns as `CONVERT(varbinary(max), ...)`) so the hashes equal the mirror's.

use dl_tds::{Param, ParamValue};
use dl_uuid::Uuid;

use super::error::{CommandError, Result};
use super::sql;
use super::sql_store::SqlSiteCommandUnit;
use super::txn::{TxnAppliedState, TxnCommandUnit};
use super::txn_value::{self, TxnInsertValue, TxnLiveColumn, TxnTableWrite};
use crate::capture::sql_builder;
use crate::contracts::mapping::{RawValue, VendorRow};
use crate::util::ctx::Ctx;

/// Savepoint of the replace, rolled back on a key conflict so the command log row still
/// commits (`Savepoint`).
pub const SAVEPOINT: &str = "pda_txn_snapshot";

/// `ColumnsSql`.
pub const COLUMNS_SQL: &str = "SELECT c.name, TYPE_NAME(c.system_type_id), c.max_length, c.precision, c.scale, c.is_nullable, \
CAST(CASE WHEN c.default_object_id <> 0 THEN 1 ELSE 0 END AS bit), \
CAST(CASE WHEN c.is_identity = 1 OR c.is_computed = 1 OR TYPE_NAME(c.system_type_id) = 'timestamp' THEN 1 ELSE 0 END AS bit) \
FROM sys.columns c WHERE c.object_id = OBJECT_ID(@Table, 'U') ORDER BY c.column_id;";

/// `LockSql`.
pub const LOCK_SQL: &str =
    "SELECT Version FROM dbo.Pda_TxnApplied WITH (UPDLOCK, HOLDLOCK) WHERE AggregateKey = @Key;";

/// `SaveSql`.
pub const SAVE_SQL: &str = "UPDATE dbo.Pda_TxnApplied SET Version = @Version, RowHash = @Hash, CommandId = @CommandId, AtUtc = SYSUTCDATETIME() \
WHERE AggregateKey = @Key; \
IF @@ROWCOUNT = 0 INSERT INTO dbo.Pda_TxnApplied (AggregateKey, Version, RowHash, CommandId) \
VALUES (@Key, @Version, @Hash, @CommandId);";

/// `LockStateSql` (ADR 0160): the version and the store key of a keyed aggregate.
pub const LOCK_STATE_SQL: &str = "SELECT Version, StoreKey FROM dbo.Pda_TxnApplied WITH (UPDLOCK, HOLDLOCK) WHERE AggregateKey = @Key;";

/// `SaveKeyedSql`.
pub const SAVE_KEYED_SQL: &str = "UPDATE dbo.Pda_TxnApplied SET Version = @Version, RowHash = @Hash, CommandId = @CommandId, StoreKey = @StoreKey, \
AtUtc = SYSUTCDATETIME() WHERE AggregateKey = @Key; \
IF @@ROWCOUNT = 0 INSERT INTO dbo.Pda_TxnApplied (AggregateKey, Version, RowHash, CommandId, StoreKey) \
VALUES (@Key, @Version, @Hash, @CommandId, @StoreKey);";

/// `ShiftWriteTables`: the only tables [`TxnCommandUnit::insert`] and
/// [`TxnCommandUnit::update`] write.
pub const SHIFT_WRITE_TABLES: [&str; 3] = ["CashierShift", "ShiftMoney", "DailyCashLog"];

/// `ShiftKeyColumns`: the only key column of [`TxnCommandUnit::update`].
pub const SHIFT_KEY_COLUMNS: [&str; 1] = ["ShiftNo"];

const PRIMARY_KEY_VIOLATION: i32 = 2627;
const UNIQUE_INDEX_VIOLATION: i32 = 2601;

fn quote(identifier: &str) -> Result<String> {
    sql_builder::quote(identifier).map_err(|e| CommandError::invalid(e.to_string()))
}

/// `DeleteSql`.
pub fn delete_sql(table: &str, scope_column: &str) -> Result<String> {
    Ok(format!(
        "DELETE FROM dbo.{} WHERE {} = @Scope;",
        quote(table)?,
        quote(scope_column)?
    ))
}

/// `SelectSql`.
pub fn select_sql(table: &str, scope_column: &str, list: &str) -> Result<String> {
    Ok(format!(
        "SELECT {list} FROM dbo.{} WHERE {} = @Scope;",
        quote(table)?,
        quote(scope_column)?
    ))
}

/// `InsertSql`: `INSERT INTO dbo.[T] ([c0], ...) VALUES (@p0, dbo.Unicode2TCVN(@p1), ...);`.
pub fn insert_sql(table: &str, row: &[TxnInsertValue]) -> Result<String> {
    let mut columns = Vec::with_capacity(row.len());
    let mut values = Vec::with_capacity(row.len());
    for (i, v) in row.iter().enumerate() {
        columns.push(quote(&v.column.name)?);
        values.push(if v.tcvn3 {
            format!("dbo.Unicode2TCVN(@p{i})")
        } else {
            format!("@p{i}")
        });
    }
    Ok(format!(
        "INSERT INTO dbo.{} ({}) VALUES ({});",
        quote(table)?,
        columns.join(", "),
        values.join(", ")
    ))
}

/// `InsertIdentitySql`: [`insert_sql`] then `SELECT CAST(SCOPE_IDENTITY() AS bigint);`.
pub fn insert_identity_sql(table: &str, row: &[TxnInsertValue]) -> Result<String> {
    Ok(insert_sql(table, row)? + " SELECT CAST(SCOPE_IDENTITY() AS bigint);")
}

/// `UpdateSql`: `UPDATE dbo.[T] SET [c0] = @p0, [c1] = dbo.Unicode2TCVN(@p1) WHERE [Key] = @Key;`.
pub fn update_sql(table: &str, key_column: &str, values: &[TxnInsertValue]) -> Result<String> {
    let mut set = Vec::with_capacity(values.len());
    for (i, v) in values.iter().enumerate() {
        let p = format!("@p{i}");
        let value = if v.tcvn3 {
            format!("dbo.Unicode2TCVN({p})")
        } else {
            p
        };
        set.push(format!("{} = {value}", quote(&v.column.name)?));
    }
    Ok(format!(
        "UPDATE dbo.{} SET {} WHERE {} = @Key;",
        quote(table)?,
        set.join(", "),
        quote(key_column)?
    ))
}

/// `RequireShiftTable`.
fn require_shift_table(table: &str) -> Result<()> {
    if SHIFT_WRITE_TABLES.contains(&table) {
        Ok(())
    } else {
        Err(CommandError::Argument("Table is not allowed.".into()))
    }
}

fn value_params(values: &[TxnInsertValue]) -> Result<Vec<Param>> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| txn_value::param(&format!("p{i}"), v))
        .collect()
}

fn flag(row: &dl_tds::Row, i: usize) -> Result<bool> {
    Ok(sql::value(row, i).map_or(Ok(0), sql::to_i64)? != 0)
}

fn small<T: TryFrom<i64>>(row: &dl_tds::Row, i: usize) -> Result<T> {
    let n = sql::value(row, i).map_or(Ok(0), sql::to_i64)?;
    T::try_from(n).map_err(|_| CommandError::invalid("sys.columns returned a value out of range."))
}

impl SqlSiteCommandUnit<'_> {
    fn replace_all(
        &mut self,
        scope_column: &str,
        scope_key: i64,
        tables: &[TxnTableWrite],
        ctx: &Ctx,
    ) -> Result<()> {
        for table in tables {
            sql::execute(
                self.c,
                &delete_sql(&table.table, scope_column)?,
                &[Param::new("Scope", scope_key)],
                ctx,
            )?;
            for row in &table.rows {
                let params = value_params(row)?;
                sql::execute(self.c, &insert_sql(&table.table, row)?, &params, ctx)?;
            }
        }
        Ok(())
    }
}

impl TxnCommandUnit for SqlSiteCommandUnit<'_> {
    fn columns(&mut self, table: &str, ctx: &Ctx) -> Result<Vec<TxnLiveColumn>> {
        let r = sql::query(
            self.c,
            COLUMNS_SQL,
            &[Param::new("Table", format!("dbo.{table}"))],
            ctx,
        )?;
        let mut columns = Vec::new();
        if let Some(set) = r.first() {
            for row in set.rows() {
                columns.push(TxnLiveColumn {
                    name: sql::text(row, 0).unwrap_or_default(),
                    sql_type: sql::text(row, 1).unwrap_or_default(),
                    max_length: small(row, 2)?,
                    precision: small(row, 3)?,
                    scale: small(row, 4)?,
                    nullable: flag(row, 5)?,
                    has_default: flag(row, 6)?,
                    generated: flag(row, 7)?,
                });
            }
        }
        if columns.is_empty() {
            return Err(CommandError::invalid(format!(
                "Table dbo.{table} does not exist or has no columns."
            )));
        }
        Ok(columns)
    }

    fn lock_applied_version(&mut self, aggregate: &str, ctx: &Ctx) -> Result<Option<i64>> {
        let r = sql::query(
            self.c,
            LOCK_SQL,
            &[Param::new("Key", aggregate.to_owned())],
            ctx,
        )?;
        sql::scalar(&r).map(sql::to_i64).transpose()
    }

    fn replace(
        &mut self,
        scope_column: &str,
        scope_key: i64,
        tables: &[TxnTableWrite],
        ctx: &Ctx,
    ) -> Result<bool> {
        sql::batch(self.c, &format!("SAVE TRANSACTION {SAVEPOINT};"))?;
        match self.replace_all(scope_column, scope_key, tables, ctx) {
            Ok(()) => Ok(true),
            Err(e)
                if matches!(
                    e.sql_number(),
                    Some(PRIMARY_KEY_VIOLATION | UNIQUE_INDEX_VIOLATION)
                ) =>
            {
                // The statement failed but the transaction lives on (XACT_ABORT is off):
                // undo this command's deletes and inserts only, keep the command log row,
                // and let the executor reject with txn_key_conflict.
                sql::batch(self.c, &format!("ROLLBACK TRANSACTION {SAVEPOINT};"))?;
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    fn read_rows(
        &mut self,
        table: &str,
        scope_column: &str,
        scope_key: i64,
        columns: &[String],
        bytes_columns: &[String],
        ctx: &Ctx,
    ) -> Result<Vec<VendorRow>> {
        let mut list = Vec::with_capacity(columns.len());
        for name in columns {
            let quoted = quote(name)?;
            list.push(
                if bytes_columns.iter().any(|b| b.eq_ignore_ascii_case(name)) {
                    format!("CONVERT(varbinary(max), {quoted}) AS {quoted}")
                } else {
                    quoted
                },
            );
        }
        let r = sql::query(
            self.c,
            &select_sql(table, scope_column, &list.join(", "))?,
            &[Param::new("Scope", scope_key)],
            ctx,
        )?;
        let mut rows = Vec::new();
        if let Some(set) = r.first() {
            for row in set.rows() {
                let mut out = VendorRow::new();
                for (col, v) in set.columns().iter().zip(row.values()) {
                    out.insert(col.name().to_owned(), RawValue::from_tds(v));
                }
                rows.push(out);
            }
        }
        Ok(rows)
    }

    fn save_applied(
        &mut self,
        aggregate: &str,
        version: i64,
        hash: &str,
        command_id: Uuid,
        ctx: &Ctx,
    ) -> Result<()> {
        sql::execute(
            self.c,
            SAVE_SQL,
            &[
                Param::new("Key", aggregate.to_owned()),
                Param::new("Version", version),
                Param::new("Hash", ParamValue::VarChar(Some(hash.to_owned()))),
                Param::new("CommandId", command_id),
            ],
            ctx,
        )?;
        Ok(())
    }
    fn lock_applied(&mut self, aggregate: &str, ctx: &Ctx) -> Result<Option<TxnAppliedState>> {
        let r = sql::query(
            self.c,
            LOCK_STATE_SQL,
            &[Param::new("Key", aggregate.to_owned())],
            ctx,
        )?;
        let Some(row) = r.first().and_then(|s| s.rows().first()) else {
            return Ok(None);
        };
        let version = sql::value(row, 0)
            .map(sql::to_i64)
            .transpose()?
            .ok_or_else(|| CommandError::invalid("Pda_TxnApplied.Version is NULL."))?;
        let store_key = sql::value(row, 1).map(sql::to_i64).transpose()?;
        Ok(Some(TxnAppliedState { version, store_key }))
    }

    fn insert(&mut self, table: &str, row: &[TxnInsertValue], ctx: &Ctx) -> Result<i64> {
        require_shift_table(table)?;
        let r = sql::query(
            self.c,
            &insert_identity_sql(table, row)?,
            &value_params(row)?,
            ctx,
        )?;
        match sql::scalar(&r) {
            Some(identity) => sql::to_i64(identity),
            None => Err(CommandError::invalid(format!(
                "dbo.{table} gave no identity for the inserted row."
            ))),
        }
    }

    fn update(
        &mut self,
        table: &str,
        key_column: &str,
        key: i64,
        values: &[TxnInsertValue],
        ctx: &Ctx,
    ) -> Result<()> {
        require_shift_table(table)?;
        if !SHIFT_KEY_COLUMNS.contains(&key_column) {
            return Err(CommandError::Argument("Key column is not allowed.".into()));
        }
        if values.is_empty() {
            return Ok(());
        }
        let mut params = value_params(values)?;
        params.push(Param::new("Key", key));
        let changed = sql::execute(
            self.c,
            &update_sql(table, key_column, values)?,
            &params,
            ctx,
        )?;
        if changed < 1 {
            return Err(CommandError::invalid(format!(
                "dbo.{table} has no row for the shift key {key}."
            )));
        }
        Ok(())
    }

    fn save_applied_keyed(
        &mut self,
        aggregate: &str,
        version: i64,
        hash: &str,
        command_id: Uuid,
        store_key: Option<i64>,
        ctx: &Ctx,
    ) -> Result<()> {
        sql::execute(
            self.c,
            SAVE_KEYED_SQL,
            &[
                Param::new("Key", aggregate.to_owned()),
                Param::new("Version", version),
                Param::new("Hash", ParamValue::VarChar(Some(hash.to_owned()))),
                Param::new("CommandId", command_id),
                Param::new("StoreKey", ParamValue::I64(store_key)),
            ],
            ctx,
        )?;
        Ok(())
    }
}
