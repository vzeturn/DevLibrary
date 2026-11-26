//! The SqlClient behaviour the command units rely on, over a `dl_tds` connection:
//! requests that fail with the server's messages attached, explicit transactions, and
//! the `Convert.ToXxx(reader.GetValue(i), InvariantCulture)` reads.

use dl_decimal::{Decimal, RoundingMode};
use dl_tds::{Client, Param, ResultSet, Results, Row, Value};

use super::error::{CommandError, Result, check_ctx};
use crate::contracts::time::{DateTime, DateTimeOffset, TICKS_PER_SECOND, UNIX_EPOCH_TICKS};
use crate::util::ctx::Ctx;
use crate::util::store_text;

/// `SqlCommand.CommandTimeout` of the command units.
pub const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Runs `sql` with `params` and reads every result set.
pub fn query(c: &mut Client, sql: &str, params: &[Param], ctx: &Ctx) -> Result<Results> {
    check_ctx(ctx)?;
    c.take_messages();
    c.query(sql, params).map_err(|e| failed(c, e))
}

/// Runs `sql` and returns the rows affected (`ExecuteNonQuery`).
pub fn execute(c: &mut Client, sql: &str, params: &[Param], ctx: &Ctx) -> Result<u64> {
    Ok(query(c, sql, params, ctx)?.rows_affected())
}

/// Runs a parameterless statement (transaction control) as a plain batch.
pub fn batch(c: &mut Client, sql: &str) -> Result<()> {
    c.take_messages();
    c.batch(sql).map(|_| ()).map_err(|e| failed(c, e))
}

fn failed(c: &mut Client, e: dl_tds::Error) -> CommandError {
    CommandError::Sql(e, c.take_messages())
}

/// `BeginTransaction(IsolationLevel.ReadCommitted)`.
pub fn begin(c: &mut Client) -> Result<()> {
    batch(
        c,
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED; BEGIN TRANSACTION;",
    )
}

/// `Commit`.
pub fn commit(c: &mut Client) -> Result<()> {
    batch(c, "COMMIT TRANSACTION;")
}

/// Disposing an uncommitted `SqlTransaction`: rolls back whatever is still open.
pub fn rollback(c: &mut Client) -> Result<()> {
    batch(c, "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION;")
}

/// `ExecuteScalar`: the first column of the first row of the first result set.
pub fn scalar(r: &Results) -> Option<&Value> {
    r.first()
        .and_then(|s| s.rows().first())
        .and_then(|row| row.value(0))
        .filter(|v| !v.is_null())
}

/// The value of column `i`, `None` for NULL or a missing column.
pub fn value(row: &Row, i: usize) -> Option<&Value> {
    row.value(i).filter(|v| !v.is_null())
}

/// `Convert.ToInt64(value)`: integers as they are, decimals and floats rounded to even,
/// numeric text parsed.
pub fn to_i64(v: &Value) -> Result<i64> {
    let n = match v {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::I8(n) => Some(i64::from(*n)),
        Value::I16(n) => Some(i64::from(*n)),
        Value::I32(n) => Some(i64::from(*n)),
        Value::I64(n) => Some(*n),
        Value::Decimal(d) => d.round_dp(0, RoundingMode::HalfEven).to_i64(),
        Value::F64(f) => float_to_i64(*f),
        Value::F32(f) => float_to_i64(f64::from(*f)),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    };
    n.ok_or_else(|| {
        CommandError::invalid(format!(
            "The store returned {} where a number was expected.",
            v.kind()
        ))
    })
}

fn float_to_i64(f: f64) -> Option<i64> {
    let r = f.round_ties_even();
    // The bounds are exact powers of two, so the comparison is exact.
    if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&r) {
        Some(r as i64)
    } else {
        None
    }
}

/// `Convert.ToInt32(value)`.
pub fn to_i32(v: &Value) -> Result<i32> {
    i32::try_from(to_i64(v)?)
        .map_err(|_| CommandError::invalid("Value was either too large or too small for an Int32."))
}

/// `Convert.ToDecimal(value)`.
pub fn to_decimal(v: &Value) -> Result<Decimal> {
    let d = match v {
        Value::Bool(b) => Some(Decimal::from(i64::from(*b))),
        Value::I8(n) => Some(Decimal::from(i64::from(*n))),
        Value::I16(n) => Some(Decimal::from(i64::from(*n))),
        Value::I32(n) => Some(Decimal::from(i64::from(*n))),
        Value::I64(n) => Some(Decimal::from(*n)),
        Value::Decimal(d) => Some(*d),
        Value::F64(f) => format!("{f}").parse::<Decimal>().ok(),
        Value::F32(f) => format!("{f}").parse::<Decimal>().ok(),
        Value::String(s) => s.trim().parse::<Decimal>().ok(),
        _ => None,
    };
    d.ok_or_else(|| {
        CommandError::invalid(format!(
            "The store returned {} where a number was expected.",
            v.kind()
        ))
    })
}

/// `r.IsDBNull(i) ? null : Convert.ToString(r.GetValue(i), InvariantCulture)`, cleaned by
/// [`value_text`].
pub fn text(row: &Row, i: usize) -> Option<String> {
    value(row, i).map(value_text)
}

/// `Convert.ToString(value, InvariantCulture)` of a non-NULL store value, without NUL
/// characters and trailing padding ([`store_text::clean`], RT-1309), so it can go into a
/// command result the cloud stores in PostgreSQL.
pub fn value_text(v: &Value) -> String {
    store_text::clean(match v {
        Value::String(s) => s.clone(),
        Value::Decimal(d) => d.to_string(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        other => other.convert::<String>().unwrap_or_default(),
    })
}

/// `r.IsDBNull(i) ? 0m : Convert.ToDecimal(r.GetValue(i))`.
pub fn number(row: &Row, i: usize) -> Result<Decimal> {
    value(row, i).map_or(Ok(Decimal::ZERO), to_decimal)
}

/// `Convert.ToInt64(r.GetValue(i))` of a column that must not be NULL.
pub fn int64(row: &Row, i: usize) -> Result<i64> {
    value(row, i).map_or_else(
        || {
            Err(CommandError::invalid(
                "Object cannot be cast from DBNull to other types.",
            ))
        },
        to_i64,
    )
}

/// `Convert.ToInt32(r.GetValue(i))` of a column that must not be NULL.
pub fn int32(row: &Row, i: usize) -> Result<i32> {
    value(row, i).map_or_else(
        || {
            Err(CommandError::invalid(
                "Object cannot be cast from DBNull to other types.",
            ))
        },
        to_i32,
    )
}

/// `(int)decimal`: truncates toward zero, saturating like the C# cast never does (an
/// out-of-range value is a store error).
pub fn decimal_to_i32(d: Decimal) -> Result<i32> {
    d.trunc(0)
        .to_i64()
        .and_then(|n| i32::try_from(n).ok())
        .ok_or_else(|| {
            CommandError::invalid("Value was either too large or too small for an Int32.")
        })
}

/// The bytes of a VARBINARY column, `None` for NULL.
pub fn bytes(row: &Row, i: usize) -> Option<&[u8]> {
    match value(row, i)? {
        Value::Bytes(b) => Some(b),
        _ => None,
    }
}

/// The name of the first column of `set` (`SqlDataReader.GetName(0)`).
pub fn first_column_is(set: &ResultSet, name: &str, min_columns: usize) -> bool {
    set.columns().len() >= min_columns
        && set
            .columns()
            .first()
            .is_some_and(|c| c.name().eq_ignore_ascii_case(name))
}

/// A `DateTimeOffset` as the `datetime2` UTC value SqlClient sends for
/// `result.AtUtc.UtcDateTime`.
pub fn utc_datetime(at: DateTimeOffset) -> Result<dl_time::DateTime> {
    dl_time::OffsetDateTime::from_unix_nanos(at.unix_nanos())
        .map(|t| t.datetime())
        .map_err(|e| CommandError::invalid(e.to_string()))
}

/// A `datetime2` read back as UTC (`DateTime.SpecifyKind(.., Utc)`).
pub fn from_utc_datetime(t: dl_time::DateTime) -> Result<DateTimeOffset> {
    DateTimeOffset::from_unix_nanos(t.assume_utc().unix_nanos(), 0)
        .ok_or_else(|| CommandError::invalid("The command log holds a time out of range."))
}

/// A C# `DateTime` (any kind) as the wall-clock value SqlClient sends.
pub fn wall_datetime(t: DateTime) -> Result<dl_time::DateTime> {
    let ticks = i128::from(t.ticks()) - i128::from(UNIX_EPOCH_TICKS);
    let nanos = ticks * (1_000_000_000 / i128::from(TICKS_PER_SECOND));
    dl_time::OffsetDateTime::from_unix_nanos(nanos)
        .map(|t| t.datetime())
        .map_err(|e| CommandError::invalid(e.to_string()))
}

/// `SqlDbType.NVarChar` text parameter (NULL for `None`).
pub fn text_param(name: &str, value: Option<&str>) -> Param {
    Param::new(name, value.map(str::to_owned))
}

/// The legacy `DECIMAL(19,4)` money parameter: the value at scale 4.
pub fn money_param(name: &str, value: Decimal) -> Param {
    let v = value.rescale(4, RoundingMode::HalfUp).unwrap_or(value);
    Param::new(name, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_text_drops_nul_characters_and_char_padding() {
        // RT-1309: a name with NULs and a padded CHAR(n) code, as SMILE stores them.
        assert_eq!(value_text(&Value::String("C\0\0\0".into())), "C");
        assert_eq!(value_text(&Value::String("100001      ".into())), "100001");
        assert_eq!(value_text(&Value::String(" T1 ".into())), " T1");
        assert_eq!(value_text(&Value::Bool(true)), "True");
    }
}
