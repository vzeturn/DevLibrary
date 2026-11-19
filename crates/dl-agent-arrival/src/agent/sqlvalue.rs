//! SQL values to and from the wire.
//!
//! Columns become the JSON `System.Text.Json` writes for the CLR value `SqlDataReader`
//! returns (`PmsExecutor.Project`): `real` as `float`, `decimal`/`money` with their scale,
//! binary as base64, GUIDs lower case, `date` as a `DateTime` at midnight, `time` as a
//! `TimeSpan`. Parameters are typed the way `PmsExecutor` declares them (`DbType` per catalog
//! type); `DateTime` goes as `datetime2(7)` and `Xml` as `nvarchar`, both converted by the
//! server on the procedure's declared type.

use dl_json::{Number, Value as Json};
use dl_tds::{ParamValue as SqlParam, Value as Sql};
use dl_time::{DateTime, Time};

use crate::contracts::ParamType;
use crate::contracts::validator::ParamValue;
use crate::contracts::values::to_base64;
use crate::netdec::NetDecimal;
use crate::stj::num;

fn number(text: &str) -> Json {
    Number::from_raw(text).map_or_else(|| Json::String(text.to_string()), Json::Number)
}

/// A `decimal` as .NET's `decimal.ToString()` writes it: the scale kept.
pub fn decimal_text(d: &dl_decimal::Decimal) -> String {
    let m = d.mantissa();
    NetDecimal {
        mantissa: m.unsigned_abs(),
        scale: u8::try_from(d.scale()).unwrap_or(u8::MAX),
        negative: m < 0,
    }
    .to_string()
}

/// One column value as JSON; `type_name` is the column's SQL type (`Column::type_name`).
///
/// # Errors
/// A value kind this build does not know (a newer `dl-tds`).
pub fn to_json(value: &Sql, type_name: &str) -> Result<Json, String> {
    Ok(match value {
        Sql::Null => Json::Null,
        Sql::Bool(b) => Json::Bool(*b),
        Sql::I8(n) => Json::from(*n),
        Sql::I16(n) => Json::from(*n),
        Sql::I32(n) => Json::from(*n),
        Sql::I64(n) => Json::from(*n),
        Sql::F32(f) => number(&num::format_f32(*f)),
        #[allow(clippy::cast_possible_truncation)]
        Sql::F64(f) if type_name == "REAL" => number(&num::format_f32(*f as f32)),
        Sql::F64(f) => number(&num::format_f64(*f)),
        Sql::Decimal(d) => number(&decimal_text(d)),
        Sql::String(s) => Json::String(s.clone()),
        Sql::Bytes(b) => Json::String(to_base64(b)),
        Sql::Uuid(u) => Json::String(u.to_string().to_ascii_lowercase()),
        Sql::Date(d) => Json::String(num::format_datetime(&DateTime::new(*d, Time::MIDNIGHT))),
        Sql::Time(t) => Json::String(num::format_time(t)),
        Sql::DateTime(dt) => Json::String(num::format_datetime(dt)),
        Sql::DateTimeOffset(o) => Json::String(num::format_dto(o)),
        other => return Err(format!("unsupported column value {}", other.kind())),
    })
}

/// A typed NULL for a catalog parameter type.
pub fn null_of(ty: ParamType) -> SqlParam {
    match ty {
        ParamType::Int => SqlParam::I32(None),
        ParamType::Long => SqlParam::I64(None),
        ParamType::Decimal => SqlParam::Decimal(None),
        ParamType::Date => SqlParam::Date(None),
        ParamType::DateTime => SqlParam::DateTime(None),
        ParamType::Bool => SqlParam::Bool(None),
        ParamType::Binary => SqlParam::Bytes(None),
        ParamType::String | ParamType::Xml => SqlParam::String(None),
    }
}

/// A converted value as a SQL parameter (`None` for a decimal past 38 digits, which a
/// validated value never is).
pub fn to_param(value: ParamValue) -> Option<SqlParam> {
    Some(match value {
        ParamValue::Int(n) => SqlParam::I32(Some(n)),
        ParamValue::Long(n) => SqlParam::I64(Some(n)),
        ParamValue::Decimal(d) => SqlParam::Decimal(Some(d.to_sql_decimal()?)),
        ParamValue::Date(d) => SqlParam::Date(Some(d)),
        ParamValue::DateTime(dt) => SqlParam::DateTime(Some(dt)),
        ParamValue::Bool(b) => SqlParam::Bool(Some(b)),
        ParamValue::Binary(b) => SqlParam::Bytes(Some(b)),
        ParamValue::Text(s) => SqlParam::String(Some(s)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_columns_print_as_float_and_money_keeps_its_scale() {
        assert_eq!(
            to_json(&Sql::F64(f64::from(0.1f32)), "REAL"),
            Ok(number("0.1"))
        );
        assert_eq!(to_json(&Sql::F64(0.1), "FLOAT"), Ok(number("0.1")));
        let money = dl_decimal::Decimal::from_parts(125_000, true, 4).unwrap();
        assert_eq!(decimal_text(&money), "-12.5000");
        assert_eq!(
            to_json(&Sql::Bytes(vec![0, 1]), "VARBINARY"),
            Ok(Json::String("AAE=".into()))
        );
        assert_eq!(null_of(ParamType::Xml), SqlParam::String(None));
    }
}
