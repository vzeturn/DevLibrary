//! `SqlMasterCommandUnit`: the reads and writes of `master.upsert` on the command's
//! connection and transaction. The row is read the way capture reads it so the hash equals
//! the mirror's; only the constant statements of [`MasterEntity`] run, values are
//! parameters.

use dl_decimal::RoundingMode;
use dl_tds::{Client, Param, ParamValue};

use super::MasterEntity;
use super::column::ColumnKind;
use super::key::MasterKey;
use crate::capture::sql_builder;
use crate::commands::error::{CommandError, Result};
use crate::commands::sql;
use crate::contracts::mapping::{RawValue, VendorRow};
use crate::util::ctx::Ctx;

/// `SqlMasterCommandUnit.ColumnsSql`.
pub const COLUMNS_SQL: &str = "SELECT c.name FROM sys.columns c WHERE c.object_id = OBJECT_ID(@Table, 'U') ORDER BY c.column_id;";

/// `SqlMasterCommandUnit.CreateLockSql`: serialises the agent's creates on one vendor table
/// (ADR 0152). The outlet tables are heaps with no index, where `UPDLOCK, HOLDLOCK` cannot
/// take a key-range lock: two creates would each hold the table lock of the absent check
/// and deadlock on their inserts. The application lock is owned by the transaction and
/// released with it.
pub const CREATE_LOCK_SQL: &str = "DECLARE @rc int; EXEC @rc = sp_getapplock @Resource = @Resource, @LockMode = 'Exclusive', @LockOwner = 'Transaction', @LockTimeout = 15000; SELECT @rc;";

/// `SqlMasterCommandUnit.CreateLockResource`.
pub fn create_lock_resource(entity: &MasterEntity) -> String {
    format!("pda-master-create:dbo.{}", entity.table)
}

/// Reads the row of `entity` for `key` the way the capture source does: live column list,
/// never-sync columns left out, the TCVN3 character columns as `CONVERT(varbinary(max), ...)`.
/// Update-locked until commit; for a create, first the per-table create lock, then the
/// range held (`HOLDLOCK`).
pub(in crate::commands) fn read_row(
    c: &mut Client,
    entity: &MasterEntity,
    key: &MasterKey,
    for_create: bool,
    exclude: &[String],
    bytes_columns: &[String],
    ctx: &Ctx,
) -> Result<Option<VendorRow>> {
    if for_create {
        let r = sql::query(
            c,
            CREATE_LOCK_SQL,
            &[Param::new("Resource", create_lock_resource(entity))],
            ctx,
        )?;
        let rc = sql::scalar(&r)
            .map(sql::to_i32)
            .transpose()?
            .ok_or_else(|| {
                CommandError::invalid(format!(
                    "{}: the create lock on dbo.{} returned no result.",
                    entity.wire, entity.table
                ))
            })?;
        if rc < 0 {
            return Err(CommandError::invalid(format!(
                "{}: the create lock on dbo.{} was not granted ({rc}).",
                entity.wire, entity.table
            )));
        }
    }
    let meta = sql::query(
        c,
        COLUMNS_SQL,
        &[Param::new("Table", format!("dbo.{}", entity.table))],
        ctx,
    )?;
    let mut columns: Vec<String> = Vec::new();
    if let Some(set) = meta.first() {
        for row in set.rows() {
            if let Some(name) = sql::text(row, 0) {
                columns.push(name);
            }
        }
    }
    if columns.is_empty() {
        return Err(CommandError::invalid(format!(
            "Table dbo.{} does not exist or has no columns.",
            entity.table
        )));
    }
    let contains = |list: &[String], name: &str| list.iter().any(|x| x.eq_ignore_ascii_case(name));
    let mut list: Vec<String> = Vec::new();
    for name in columns.iter().filter(|n| !contains(exclude, n)) {
        let quoted = sql_builder::quote(name).map_err(|e| CommandError::invalid(e.to_string()))?;
        list.push(if contains(bytes_columns, name) {
            format!("CONVERT(varbinary(max), {quoted}) AS {quoted}")
        } else {
            quoted
        });
    }
    let r = sql::query(
        c,
        &entity.select_sql(&list.join(", "), for_create),
        &key_params(entity, key)?,
        ctx,
    )?;
    let Some(set) = r.first() else {
        return Ok(None);
    };
    let Some(row) = set.rows().first() else {
        return Ok(None);
    };
    // A logical key without a vendor primary key may match several rows: the row hash
    // would not identify one, so nothing is written.
    if set.rows().len() > 1 {
        return Err(CommandError::invalid(format!(
            "{} {key} matches more than one row of dbo.{}.",
            entity.wire, entity.table
        )));
    }
    let mut out = VendorRow::new();
    for (col, v) in set.columns().iter().zip(row.values()) {
        out.insert(col.name().to_owned(), RawValue::from_tds(v));
    }
    Ok(Some(out))
}

/// One typed parameter per key part.
fn key_params(entity: &MasterEntity, key: &MasterKey) -> Result<Vec<Param>> {
    if key.values.len() != entity.key.len() {
        return Err(CommandError::invalid(format!(
            "{} {key} does not have the {} key parts of the entity.",
            entity.wire,
            entity.key.len()
        )));
    }
    Ok(entity
        .key
        .iter()
        .zip(&key.values)
        .map(|(part, value)| part.param_of(value))
        .collect())
}

/// Runs the entity's update; whether a row changed. More than one matching row is an error
/// (rolled back).
pub(in crate::commands) fn update_row(
    c: &mut Client,
    entity: &MasterEntity,
    key: &MasterKey,
    values: &[(&str, &str)],
    ctx: &Ctx,
) -> Result<bool> {
    let changed = sql::execute(
        c,
        entity.update_sql,
        &statement_params(entity, key, values)?,
        ctx,
    )?;
    if changed > 1 {
        return Err(CommandError::invalid(format!(
            "{} {key} matched {changed} rows of dbo.{}; the transaction is rolled back.",
            entity.wire, entity.table
        )));
    }
    Ok(changed > 0)
}

/// Runs the entity's insert (the caller checked the key is absent under the create lock);
/// false when it inserted nothing (no template row). More than one row is an error.
pub(in crate::commands) fn insert_row(
    c: &mut Client,
    entity: &MasterEntity,
    key: &MasterKey,
    values: &[(&str, &str)],
    ctx: &Ctx,
) -> Result<bool> {
    let Some(insert_sql) = entity.insert_sql else {
        return Err(CommandError::invalid(format!(
            "{} has no create statement.",
            entity.wire
        )));
    };
    let inserted = sql::execute(c, insert_sql, &statement_params(entity, key, values)?, ctx)?;
    if inserted > 1 {
        return Err(CommandError::invalid(format!(
            "{} {key} inserted {inserted} rows into dbo.{}; the transaction is rolled back.",
            entity.wire, entity.table
        )));
    }
    Ok(inserted == 1)
}

/// `SqlMasterCommandUnit.Statement`: the key parameters plus one typed parameter per
/// writable column. A column without a value is a typed NULL for a `keeps_missing` entity
/// (`COALESCE(@Col, Col)` keeps the store value) and an error otherwise.
fn statement_params(
    entity: &MasterEntity,
    key: &MasterKey,
    values: &[(&str, &str)],
) -> Result<Vec<Param>> {
    let mut params = key_params(entity, key)?;
    for column in entity.writable {
        let name = column.name;
        // Fail closed: a TCVN3 column written without dbo.Unicode2TCVN (or a Unicode one
        // through it) would store other bytes.
        if let Some(wrong) = column.sql_problem(entity.update_sql) {
            return Err(CommandError::invalid(format!("{}: {wrong}.", entity.wire)));
        }
        let Some(value) = values.iter().find(|(n, _)| *n == name).map(|(_, v)| *v) else {
            if !entity.keeps_missing {
                return Err(CommandError::invalid(format!(
                    "{}: {name} has no value.",
                    entity.wire
                )));
            }
            let null = match column.kind {
                ColumnKind::Numeric { .. } => ParamValue::Decimal(None),
                ColumnKind::Bool => ParamValue::Bool(None),
                ColumnKind::Int { .. } => ParamValue::I32(None),
                ColumnKind::Tcvn3Text | ColumnKind::Unicode => ParamValue::String(None),
                ColumnKind::Text => ParamValue::VarChar(None),
            };
            params.push(Param::new(name, null));
            continue;
        };
        match column.kind {
            ColumnKind::Bool => {
                let flag = column.bool_value(value).ok_or_else(|| {
                    CommandError::invalid(format!("{name} is not 0, 1, true or false."))
                })?;
                params.push(Param::new(name, flag));
            }
            ColumnKind::Int { .. } => {
                let number = column.int_value(value).ok_or_else(|| {
                    CommandError::invalid(format!("{name} is not an integer in range."))
                })?;
                params.push(Param::new(name, number));
            }
            ColumnKind::Numeric { scale, .. } => {
                let d = column.decimal(value).ok_or_else(|| {
                    CommandError::invalid(format!("{name} is not a valid decimal."))
                })?;
                let d = d.rescale(scale, RoundingMode::HalfEven).map_err(|_| {
                    CommandError::invalid(format!("{name} does not fit the column scale."))
                })?;
                params.push(Param::new(name, d));
            }
            _ => params.push(sql::text_param(name, Some(value))),
        }
    }
    Ok(params)
}
