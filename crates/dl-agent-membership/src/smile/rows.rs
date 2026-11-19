//! Column readers that accept the shapes SMILE installs actually use (a status may be
//! `tinyint` on one site and `int` on another, a folio number `numeric` or `decimal`), the
//! way Dapper's conversions do. A missing column or an unconvertible value is
//! [`SmileError::Decode`] with the column name only.

use dl_decimal::Decimal;
use dl_tds::{Row, Value};
use dl_time::{Date, DateTime};

use super::SmileError;

fn value<'a>(row: &'a Row, col: &'static str) -> Result<&'a Value, SmileError> {
    row.value(col).ok_or(SmileError::Decode(col))
}

/// A nullable string, as stored.
pub(crate) fn opt_str(row: &Row, col: &'static str) -> Result<Option<String>, SmileError> {
    match value(row, col)? {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        Value::Decimal(d) => Ok(Some(d.to_string())),
        Value::I8(n) => Ok(Some(n.to_string())),
        Value::I16(n) => Ok(Some(n.to_string())),
        Value::I32(n) => Ok(Some(n.to_string())),
        Value::I64(n) => Ok(Some(n.to_string())),
        _ => Err(SmileError::Decode(col)),
    }
}

/// A string, trimmed; `NULL` and blanks are `None` (`string.IsNullOrWhiteSpace ? null : Trim()`).
pub(crate) fn trimmed(row: &Row, col: &'static str) -> Result<Option<String>, SmileError> {
    Ok(opt_str(row, col)?.and_then(|s| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_owned())
    }))
}

/// A string that must be present; `NULL` reads as empty (Dapper's `= ""` default).
pub(crate) fn text(row: &Row, col: &'static str) -> Result<String, SmileError> {
    Ok(opt_str(row, col)?.unwrap_or_default())
}

/// A nullable integer of any SQL width (decimals are truncated, like a .NET cast).
pub(crate) fn opt_int(row: &Row, col: &'static str) -> Result<Option<i64>, SmileError> {
    match value(row, col)? {
        Value::Null => Ok(None),
        Value::Bool(b) => Ok(Some(i64::from(*b))),
        Value::I8(n) => Ok(Some(i64::from(*n))),
        Value::I16(n) => Ok(Some(i64::from(*n))),
        Value::I32(n) => Ok(Some(i64::from(*n))),
        Value::I64(n) => Ok(Some(*n)),
        Value::Decimal(d) => d.to_i64().map(Some).ok_or(SmileError::Decode(col)),
        Value::String(s) => s
            .trim()
            .parse::<i64>()
            .map(Some)
            .map_err(|_| SmileError::Decode(col)),
        _ => Err(SmileError::Decode(col)),
    }
}

/// An integer; `NULL` reads as 0.
pub(crate) fn int(row: &Row, col: &'static str) -> Result<i64, SmileError> {
    Ok(opt_int(row, col)?.unwrap_or(0))
}

/// A nullable bit (also from 0/1 integers).
pub(crate) fn opt_bool(row: &Row, col: &'static str) -> Result<Option<bool>, SmileError> {
    match value(row, col)? {
        Value::Bool(b) => Ok(Some(*b)),
        Value::Null => Ok(None),
        _ => opt_int(row, col).map(|n| n.map(|n| n != 0)),
    }
}

/// A bit; `NULL` reads as `false`.
pub(crate) fn boolean(row: &Row, col: &'static str) -> Result<bool, SmileError> {
    Ok(opt_bool(row, col)?.unwrap_or(false))
}

/// A nullable decimal (integers widen).
pub(crate) fn opt_dec(row: &Row, col: &'static str) -> Result<Option<Decimal>, SmileError> {
    match value(row, col)? {
        Value::Null => Ok(None),
        Value::Decimal(d) => Ok(Some(*d)),
        Value::I8(n) => Ok(Some(Decimal::from(i64::from(*n)))),
        Value::I16(n) => Ok(Some(Decimal::from(i64::from(*n)))),
        Value::I32(n) => Ok(Some(Decimal::from(i64::from(*n)))),
        Value::I64(n) => Ok(Some(Decimal::from(*n))),
        Value::String(s) => s
            .trim()
            .parse::<Decimal>()
            .map(Some)
            .map_err(|_| SmileError::Decode(col)),
        _ => Err(SmileError::Decode(col)),
    }
}

/// A decimal; `NULL` reads as 0.
pub(crate) fn dec(row: &Row, col: &'static str) -> Result<Decimal, SmileError> {
    Ok(opt_dec(row, col)?.unwrap_or(Decimal::ZERO))
}

/// A nullable `datetime` / `datetime2` / `date`.
pub(crate) fn opt_datetime(row: &Row, col: &'static str) -> Result<Option<DateTime>, SmileError> {
    match value(row, col)? {
        Value::Null => Ok(None),
        Value::DateTime(d) => Ok(Some(*d)),
        Value::Date(d) => Ok(Some(d.midnight())),
        Value::DateTimeOffset(d) => Ok(Some(d.datetime())),
        _ => Err(SmileError::Decode(col)),
    }
}

/// A date-time that must be present.
pub(crate) fn datetime(row: &Row, col: &'static str) -> Result<DateTime, SmileError> {
    opt_datetime(row, col)?.ok_or(SmileError::Decode(col))
}

/// The date part of a date-time that must be present (`DateOnly.FromDateTime`).
pub(crate) fn date(row: &Row, col: &'static str) -> Result<Date, SmileError> {
    datetime(row, col).map(DateTime::date)
}

/// A nullable GUID.
pub(crate) fn opt_uuid(row: &Row, col: &'static str) -> Result<Option<dl_uuid::Uuid>, SmileError> {
    match value(row, col)? {
        Value::Null => Ok(None),
        Value::Uuid(u) => Ok(Some(*u)),
        _ => Err(SmileError::Decode(col)),
    }
}
