//! Converts one JSON value of a `txn.snapshot` row to the value of its live column's SQL
//! type, or refuses it, before any write (`TxnValueConverter.cs`, ADR 0159): null
//! (nullable columns only), number for the integer, exact-numeric and float types (no
//! rounding, no overflow), string for the character types (within the column length),
//! boolean or 0/1 for `bit`, store-local `yyyy-MM-ddTHH:mm:ss[.f|.ff|.fff]` without offset
//! for the date/time types (`date` also takes `yyyy-MM-dd`), base64 for the binary types
//! and a GUID string for `uniqueidentifier`. Any other SQL type is refused.

use dl_decimal::{Decimal, RoundingMode};
use dl_json::{Number, Value as Json};
use dl_tds::{Param, ParamValue};
use dl_uuid::Uuid;

use super::error::{CommandError, Result};
use crate::contracts::utf16_len;
use crate::util::stj;

/// Longest input `dbo.Unicode2TCVN` takes (`@strInput NVARCHAR(4000)`).
pub const TCVN3_MAX_LENGTH: usize = 4000;

/// One live column of a vendor table as `sys.columns` describes it (`TxnLiveColumn`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxnLiveColumn {
    /// Column name.
    pub name: String,
    /// System type name (`numeric`, `varchar`, ...).
    pub sql_type: String,
    /// Byte length, -1 for `max`.
    pub max_length: i32,
    /// Precision.
    pub precision: u8,
    /// Scale.
    pub scale: u8,
    /// Nullable.
    pub nullable: bool,
    /// Has a default constraint.
    pub has_default: bool,
    /// Identity, computed or rowversion: never inserted.
    pub generated: bool,
}

impl TxnLiveColumn {
    /// A column with no default that is neither nullable nor generated (tests and fakes).
    pub fn new(name: &str, sql_type: &str, max_length: i32, precision: u8, scale: u8) -> Self {
        TxnLiveColumn {
            name: name.to_owned(),
            sql_type: sql_type.to_owned(),
            max_length,
            precision,
            scale,
            nullable: false,
            has_default: false,
            generated: false,
        }
    }
}

/// The converted value of one column (the CLR value of `TxnInsertValue.Value`).
#[derive(Debug, Clone, PartialEq)]
pub enum TxnValue {
    /// `bit`.
    Bool(bool),
    /// `tinyint`.
    U8(u8),
    /// `smallint`.
    I16(i16),
    /// `int`.
    I32(i32),
    /// `bigint`.
    I64(i64),
    /// `decimal` / `numeric` / `money` / `smallmoney`.
    Decimal(Decimal),
    /// `float`.
    F64(f64),
    /// `real`.
    F32(f32),
    /// The character types.
    Text(String),
    /// `datetime` / `datetime2` / `smalldatetime` / `date`.
    DateTime(dl_time::DateTime),
    /// The binary types.
    Bytes(Vec<u8>),
    /// `uniqueidentifier`.
    Uuid(Uuid),
}

/// One converted column value of a row to insert (`TxnInsertValue`); `value` `None` is SQL
/// NULL. `tcvn3` marks a character column of the catalog's TCVN3 transform, inserted
/// through `dbo.Unicode2TCVN(@p)`.
#[derive(Debug, Clone, PartialEq)]
pub struct TxnInsertValue {
    /// The live column.
    pub column: TxnLiveColumn,
    /// The value, `None` for NULL.
    pub value: Option<TxnValue>,
    /// Inserted through `dbo.Unicode2TCVN`.
    pub tcvn3: bool,
}

/// The rows of one allow-listed table that replace the aggregate's rows there
/// (`TxnTableWrite`; none: the rows are deleted).
#[derive(Debug, Clone, PartialEq)]
pub struct TxnTableWrite {
    /// Vendor table.
    pub table: String,
    /// Rows in payload order, values in live-column order.
    pub rows: Vec<Vec<TxnInsertValue>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bit,
    TinyInt,
    SmallInt,
    Int,
    BigInt,
    Decimal,
    Money,
    SmallMoney,
    Float,
    Real,
    Char,
    VarChar,
    NChar,
    NVarChar,
    Text,
    NText,
    DateTime,
    DateTime2,
    SmallDateTime,
    Date,
    Binary,
    VarBinary,
    Image,
    UniqueIdentifier,
}

/// `DbType`: the kind of a supported system type name; `None` for an unsupported type.
fn kind(sql_type: &str) -> Option<Kind> {
    Some(match sql_type.to_ascii_lowercase().as_str() {
        "bit" => Kind::Bit,
        "tinyint" => Kind::TinyInt,
        "smallint" => Kind::SmallInt,
        "int" => Kind::Int,
        "bigint" => Kind::BigInt,
        "numeric" | "decimal" => Kind::Decimal,
        "money" => Kind::Money,
        "smallmoney" => Kind::SmallMoney,
        "float" => Kind::Float,
        "real" => Kind::Real,
        "char" => Kind::Char,
        "varchar" => Kind::VarChar,
        "nchar" => Kind::NChar,
        "nvarchar" => Kind::NVarChar,
        "text" => Kind::Text,
        "ntext" => Kind::NText,
        "datetime" => Kind::DateTime,
        "datetime2" => Kind::DateTime2,
        "smalldatetime" => Kind::SmallDateTime,
        "date" => Kind::Date,
        "binary" => Kind::Binary,
        "varbinary" => Kind::VarBinary,
        "image" => Kind::Image,
        "uniqueidentifier" => Kind::UniqueIdentifier,
        _ => return None,
    })
}

/// `MaxChars`: most characters (or bytes for binary) the column holds; -1 when unbounded.
pub fn max_chars(column: &TxnLiveColumn) -> i32 {
    if column.max_length < 0 {
        return -1;
    }
    match column.sql_type.to_ascii_lowercase().as_str() {
        "text" | "ntext" | "image" => -1,
        "nchar" | "nvarchar" => column.max_length / 2,
        _ => column.max_length,
    }
}

/// `TryConvert`: `Some(value)` when `json` fits `column` (`Some(None)` is SQL NULL),
/// `None` when the value is refused.
pub fn try_convert(column: &TxnLiveColumn, json: &Json, tcvn3: bool) -> Option<Option<TxnValue>> {
    if column.generated {
        return None;
    }
    if json.is_null() {
        return column.nullable.then_some(None);
    }
    let k = kind(&column.sql_type)?;
    if tcvn3 && !matches!(k, Kind::Char | Kind::VarChar | Kind::Text) {
        return None;
    }
    let v = match k {
        Kind::Bit => match json {
            Json::Bool(b) => TxnValue::Bool(*b),
            Json::Number(n) => match integer(n)? {
                0 => TxnValue::Bool(false),
                1 => TxnValue::Bool(true),
                _ => return None,
            },
            _ => return None,
        },
        Kind::TinyInt => TxnValue::U8(u8::try_from(integer(number(json)?)?).ok()?),
        Kind::SmallInt => TxnValue::I16(i16::try_from(integer(number(json)?)?).ok()?),
        Kind::Int => TxnValue::I32(i32::try_from(integer(number(json)?)?).ok()?),
        Kind::BigInt => TxnValue::I64(integer(number(json)?)?),
        Kind::Decimal => exact(
            json,
            u32::from(column.precision),
            u32::from(column.scale),
            None,
        )?,
        Kind::Money => exact(json, 19, 4, Some("922337203685477.5807"))?,
        Kind::SmallMoney => exact(json, 10, 4, Some("214748.3647"))?,
        Kind::Float => TxnValue::F64(double(json)?),
        Kind::Real => {
            let r = double(json)?;
            if r.abs() > f64::from(f32::MAX) {
                return None;
            }
            TxnValue::F32(r as f32)
        }
        Kind::Char | Kind::VarChar | Kind::NChar | Kind::NVarChar | Kind::Text | Kind::NText => {
            let s = json.as_str()?;
            let max = max_chars(column);
            let len = utf16_len(s);
            if max >= 0 && len > max as usize {
                return None;
            }
            if tcvn3 && len > TCVN3_MAX_LENGTH {
                return None;
            }
            TxnValue::Text(s.to_owned())
        }
        Kind::DateTime | Kind::DateTime2 | Kind::SmallDateTime | Kind::Date => {
            TxnValue::DateTime(moment(k, json.as_str()?)?)
        }
        Kind::Binary | Kind::VarBinary | Kind::Image => {
            let s = json.as_str()?;
            // Convert.FromBase64String skips white space.
            let compact: String = s
                .chars()
                .filter(|c| !matches!(c, ' ' | '\t' | '\r' | '\n'))
                .collect();
            let bytes = dl_encoding::base64::STANDARD.decode(compact).ok()?;
            let max = max_chars(column);
            if max >= 0 && bytes.len() > max as usize {
                return None;
            }
            TxnValue::Bytes(bytes)
        }
        Kind::UniqueIdentifier => TxnValue::Uuid(guid(json.as_str()?)?),
    };
    Some(Some(v))
}

fn number(json: &Json) -> Option<&Number> {
    match json {
        Json::Number(n) => Some(n),
        _ => None,
    }
}

/// The source text of a JSON number.
fn number_text(n: &Number) -> String {
    n.as_raw_str().map_or_else(|| n.to_string(), str::to_owned)
}

/// `TryGetInt64`: the integer lexeme only.
fn integer(n: &Number) -> Option<i64> {
    match n.as_raw_str() {
        Some(raw) => raw.parse::<i64>().ok(),
        None => n.as_i64(),
    }
}

/// `TryGetDouble` and finite.
fn double(json: &Json) -> Option<f64> {
    let d = number_text(number(json)?).parse::<f64>().ok()?;
    d.is_finite().then_some(d)
}

/// `Exact`: no silent rounding, no overflow of the integer digits.
fn exact(json: &Json, precision: u32, scale: u32, abs_max: Option<&str>) -> Option<TxnValue> {
    let v = stj::parse_dotnet_decimal(&number_text(number(json)?))?;
    if v.round_dp(scale, RoundingMode::HalfEven) != v {
        return None;
    }
    let integer_digits = precision.saturating_sub(scale) as usize;
    let whole = v.abs().trunc(0).to_string();
    if whole.trim_start_matches('0').len() > integer_digits {
        return None;
    }
    if let Some(max) = abs_max
        && v.abs() > max.parse::<Decimal>().ok()?
    {
        return None;
    }
    Some(TxnValue::Decimal(v))
}

/// `Guid.TryParse` over the shapes `dl_uuid` reads (D, N, B) plus P; surrounding white
/// space is trimmed as .NET does.
fn guid(s: &str) -> Option<Uuid> {
    let s = s.trim();
    if s.len() >= 4 && s[..4].eq_ignore_ascii_case("urn:") {
        return None;
    }
    if s.len() == 38 && s.starts_with('(') && s.ends_with(')') {
        return Uuid::parse(&s[1..37]).ok();
    }
    Uuid::parse(s).ok()
}

/// `Moment`: strict `yyyy-MM-ddTHH:mm:ss[.f|.ff|.fff]` (`date` also `yyyy-MM-dd`) with
/// the SQL range of the type.
fn moment(k: Kind, s: &str) -> Option<dl_time::DateTime> {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| -> Option<u32> {
        let part = b.get(r)?;
        part.iter()
            .all(u8::is_ascii_digit)
            .then(|| part.iter().fold(0u32, |a, d| a * 10 + u32::from(d - b'0')))
    };
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let (y, mo, d) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    let (mut h, mut mi, mut sec, mut nanos) = (0, 0, 0, 0);
    if !(k == Kind::Date && b.len() == 10) {
        if !(19..=23).contains(&b.len()) || b.len() == 20 {
            return None;
        }
        if b[10] != b'T' || b[13] != b':' || b[16] != b':' {
            return None;
        }
        (h, mi, sec) = (digits(11..13)?, digits(14..16)?, digits(17..19)?);
        if b.len() > 19 {
            if b[19] != b'.' {
                return None;
            }
            let frac = &b[20..];
            let f = digits(20..b.len())?;
            nanos = f * 10u32.pow(9 - frac.len() as u32);
        }
    }
    if y < 1 || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    let date = dl_time::Date::new(
        i32::try_from(y).ok()?,
        u8::try_from(mo).ok()?,
        u8::try_from(d).ok()?,
    )
    .ok()?;
    let time = dl_time::Time::new(h as u8, mi as u8, sec as u8, nanos).ok()?;
    let at = (y, mo, d, h, mi, sec, nanos);
    match k {
        Kind::DateTime if at < (1753, 1, 1, 0, 0, 0, 0) => return None,
        Kind::SmallDateTime
            if !((1900, 1, 1, 0, 0, 0, 0)..=(2079, 6, 6, 23, 59, 0, 0)).contains(&at) =>
        {
            return None;
        }
        Kind::Date if (h, mi, sec, nanos) != (0, 0, 0, 0) => return None,
        _ => {}
    }
    Some(dl_time::DateTime::new(date, time))
}

/// The SQL parameter of one insert value (`SqlTxnCommandUnit.Parameter`): the TCVN3
/// columns as `nvarchar` for `dbo.Unicode2TCVN`, every other value as its column's type
/// (typed NULL for a NULL).
pub fn param(name: &str, value: &TxnInsertValue) -> Result<Param> {
    let column = &value.column;
    let text = |v: &Option<TxnValue>| match v {
        Some(TxnValue::Text(s)) => Ok(Some(s.clone())),
        None => Ok(None),
        Some(other) => Err(mismatch(column, other)),
    };
    if value.tcvn3 {
        return Ok(Param::new(name, ParamValue::String(text(&value.value)?)));
    }
    let k = kind(&column.sql_type).ok_or_else(|| {
        CommandError::invalid(format!(
            "Unsupported SQL type {} of {}.",
            column.sql_type, column.name
        ))
    })?;
    macro_rules! typed {
        ($variant:ident) => {
            match &value.value {
                None => ParamValue::$variant(None),
                Some(TxnValue::$variant(x)) => ParamValue::$variant(Some(x.clone())),
                Some(other) => return Err(mismatch(column, other)),
            }
        };
    }
    let p = match k {
        Kind::Bit => typed!(Bool),
        Kind::TinyInt => typed!(U8),
        Kind::SmallInt => typed!(I16),
        Kind::Int => typed!(I32),
        Kind::BigInt => typed!(I64),
        Kind::Decimal | Kind::Money | Kind::SmallMoney => typed!(Decimal),
        Kind::Float => typed!(F64),
        Kind::Real => typed!(F32),
        Kind::Char | Kind::VarChar | Kind::Text => ParamValue::VarChar(text(&value.value)?),
        Kind::NChar | Kind::NVarChar | Kind::NText => ParamValue::String(text(&value.value)?),
        Kind::Date => match &value.value {
            None => ParamValue::Date(None),
            Some(TxnValue::DateTime(t)) => ParamValue::Date(Some(t.date())),
            Some(other) => return Err(mismatch(column, other)),
        },
        Kind::DateTime | Kind::DateTime2 | Kind::SmallDateTime => typed!(DateTime),
        Kind::Binary | Kind::VarBinary | Kind::Image => typed!(Bytes),
        Kind::UniqueIdentifier => typed!(Uuid),
    };
    Ok(Param::new(name, p))
}

/// Never names the value (row values may be personal data).
fn mismatch(column: &TxnLiveColumn, _: &TxnValue) -> CommandError {
    CommandError::invalid(format!(
        "A converted value does not match SQL type {} of {}.",
        column.sql_type, column.name
    ))
}
