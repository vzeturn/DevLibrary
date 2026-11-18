//! Column values and conversions to Rust types.

use std::fmt::Write as _;

use dl_decimal::Decimal;
use dl_time::{Date, DateTime, OffsetDateTime, Time};
use dl_uuid::Uuid;

use crate::Error;

/// One decoded column value, output parameter or SQL_VARIANT payload.
///
/// | SQL Server type | `Value` |
/// |---|---|
/// | `bit` | `Bool` |
/// | `tinyint` | `I8` (holds the unsigned 0–255 value) |
/// | `smallint` / `int` / `bigint` | `I16` / `I32` / `I64` |
/// | `real` declared `NOT NULL` (wire type FLT4) | `F32` |
/// | `real` nullable (FLTN, 4 bytes) | `F64`, widened like go-mssqldb |
/// | `float` | `F64` |
/// | `decimal`, `numeric`, `money`, `smallmoney` | `Decimal` (money at scale 4) |
/// | `char`, `varchar`, `text`, `nchar`, `nvarchar`, `ntext`, `xml` | `String` |
/// | `binary`, `varbinary`, `image`, `timestamp`, CLR UDTs | `Bytes` |
/// | `uniqueidentifier` | `Uuid` |
/// | `date` | `Date` |
/// | `time(n)` | `Time` |
/// | `datetime`, `smalldatetime`, `datetime2(n)` | `DateTime` |
/// | `datetimeoffset(n)` | `DateTimeOffset` (keeps the stored offset) |
/// | `sql_variant` | the value of its base type |
/// | any `NULL` | `Null` |
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// SQL NULL.
    Null,
    /// `bit`.
    Bool(bool),
    /// `tinyint`. SQL Server's tinyint is unsigned (0–255), hence the `u8`.
    I8(u8),
    /// `smallint`.
    I16(i16),
    /// `int`.
    I32(i32),
    /// `bigint`.
    I64(i64),
    /// `real` (non-nullable column).
    F32(f32),
    /// `float`, and nullable `real` widened to `f64`.
    F64(f64),
    /// `decimal`, `numeric`, `money`, `smallmoney`. Values with more than 28 decimal places
    /// (possible with `decimal(38, 29..38)`) are rounded half away from zero to 28.
    Decimal(Decimal),
    /// Character and XML data.
    String(String),
    /// Binary data and CLR UDT values.
    Bytes(Vec<u8>),
    /// `uniqueidentifier`, in its canonical (displayed) form.
    Uuid(Uuid),
    /// `date`.
    Date(Date),
    /// `time(n)`.
    Time(Time),
    /// `datetime`, `smalldatetime`, `datetime2(n)` (no time zone).
    DateTime(DateTime),
    /// `datetimeoffset(n)` with the offset stored in the column.
    DateTimeOffset(OffsetDateTime),
}

impl Value {
    /// `true` for [`Value::Null`].
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// The variant name, e.g. `"String"`.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "Null",
            Value::Bool(_) => "Bool",
            Value::I8(_) => "I8",
            Value::I16(_) => "I16",
            Value::I32(_) => "I32",
            Value::I64(_) => "I64",
            Value::F32(_) => "F32",
            Value::F64(_) => "F64",
            Value::Decimal(_) => "Decimal",
            Value::String(_) => "String",
            Value::Bytes(_) => "Bytes",
            Value::Uuid(_) => "Uuid",
            Value::Date(_) => "Date",
            Value::Time(_) => "Time",
            Value::DateTime(_) => "DateTime",
            Value::DateTimeOffset(_) => "DateTimeOffset",
        }
    }

    /// Converts to `T` with the same rules as [`Row::get`](crate::Row::get).
    ///
    /// # Errors
    /// [`Error::Null`] or [`Error::Conversion`] (with column 0).
    pub fn convert<T: FromValue>(&self) -> Result<T, Error> {
        if self.is_null() {
            return T::from_null().ok_or(Error::Null { column: 0 });
        }
        T::from_value(self)
    }

    fn as_int(&self) -> Option<i64> {
        match *self {
            Value::I8(v) => Some(v as i64),
            Value::I16(v) => Some(v as i64),
            Value::I32(v) => Some(v as i64),
            Value::I64(v) => Some(v),
            _ => None,
        }
    }

    /// The text database/sql produces when go-mssqldb's value for this column is scanned
    /// into a Go `string` (`None` for NULL or non-UTF-8 bytes).
    fn go_string(&self) -> Option<String> {
        Some(match self {
            Value::Null => return None,
            Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
            Value::I8(_) | Value::I16(_) | Value::I32(_) | Value::I64(_) => {
                self.as_int().unwrap_or(0).to_string()
            }
            Value::F32(v) => go_format_float(*v as f64, true),
            Value::F64(v) => go_format_float(*v, false),
            Value::Decimal(d) => d.to_string(),
            Value::String(s) => s.clone(),
            Value::Bytes(b) => return String::from_utf8(b.clone()).ok(),
            Value::Uuid(u) => u.to_string().to_ascii_uppercase(),
            Value::Date(d) => rfc3339_nano(d.midnight(), None),
            Value::Time(t) => rfc3339_nano(year_one().at(*t), None),
            Value::DateTime(dt) => rfc3339_nano(*dt, None),
            Value::DateTimeOffset(o) => {
                rfc3339_nano(o.datetime(), Some(o.offset().whole_seconds()))
            }
        })
    }
}

fn year_one() -> Date {
    // 0001-01-01 is inside dl-time's range; `Date::MIN` is never needed as a fallback.
    Date::new(1, 1, 1).unwrap_or(Date::MIN)
}

/// Formats like Go's `time.RFC3339Nano` ("2006-01-02T15:04:05.999999999Z07:00");
/// `offset` `None` means UTC.
fn rfc3339_nano(dt: DateTime, offset: Option<i32>) -> String {
    let mut s = String::with_capacity(35);
    let _ = write!(
        s,
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        dt.year(),
        dt.month().number(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    );
    let ns = dt.nanosecond();
    if ns != 0 {
        let frac = format!("{ns:09}");
        s.push('.');
        s.push_str(frac.trim_end_matches('0'));
    }
    match offset {
        None | Some(0) => s.push('Z'),
        Some(secs) => {
            let sign = if secs < 0 { '-' } else { '+' };
            let a = secs.unsigned_abs();
            let _ = write!(s, "{sign}{:02}:{:02}", a / 3600, (a / 60) % 60);
        }
    }
    s
}

/// Go's `strconv.FormatFloat(v, 'g', -1, 64 or 32)`: the shortest digits that round-trip,
/// in `e` notation when the decimal exponent is below -4 or at least 6 (Go uses 6 as the
/// precision for this decision when printing shortest), otherwise plain notation.
pub(crate) fn go_format_float(v: f64, bits32: bool) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "+Inf" } else { "-Inf" }.into();
    }
    let neg = v.is_sign_negative();
    // Shortest round-trip digits from Rust's formatter, e.g. "1.234567e6".
    let sci = if bits32 {
        format!("{:e}", (v as f32).abs())
    } else {
        format!("{:e}", v.abs())
    };
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp10: i32 = exp.parse().unwrap_or(0);
    let mut digits: Vec<u8> = mant.bytes().filter(|b| b.is_ascii_digit()).collect();
    while digits.len() > 1 && digits.last() == Some(&b'0') {
        digits.pop();
    }
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if digits == *b"0" {
        // Zero: Go prints "0" (and "-0").
        out.push('0');
        return out;
    }
    let nd = digits.len() as i32;
    let dp = exp10 + 1; // position of the decimal point relative to the digits
    let exp = dp - 1;
    // Go: eprec = 6 when shortest; %e if exp < -4 || exp >= eprec.
    if !(-4..6).contains(&exp) {
        // %e with nd-1 fractional digits.
        out.push(digits[0] as char);
        if nd > 1 {
            out.push('.');
            for &d in &digits[1..] {
                out.push(d as char);
            }
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        let a = exp.unsigned_abs();
        if a < 10 {
            out.push('0');
        }
        let _ = write!(out, "{a}");
        return out;
    }
    // %f with max(nd - dp, 0) fractional digits.
    if dp > 0 {
        for i in 0..dp {
            out.push(if i < nd {
                digits[i as usize] as char
            } else {
                '0'
            });
        }
    } else {
        out.push('0');
    }
    let frac = (nd - dp).max(0);
    if frac > 0 {
        out.push('.');
        for i in 0..frac {
            let j = dp + i;
            out.push(if (0..nd).contains(&j) {
                digits[j as usize] as char
            } else {
                '0'
            });
        }
    }
    out
}

/// Go's `strconv.ParseBool`.
fn go_parse_bool(s: &str) -> Option<bool> {
    match s {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

fn conv(v: &Value, to: &'static str, reason: impl Into<String>) -> Error {
    Error::Conversion {
        column: 0,
        from: v.kind(),
        to,
        reason: reason.into(),
    }
}

/// Conversion from a column [`Value`] to a Rust type, used by [`Row::get`](crate::Row::get).
///
/// The rules reproduce what Go's `database/sql` `Scan` does with the values go-mssqldb
/// returns, so code ported from Go reads the same data the same way:
///
/// | Target | Accepted values |
/// |---|---|
/// | `String` | every non-NULL value: integers in decimal, `bool` as `true`/`false`, floats as Go `strconv.FormatFloat(v, 'g', -1, …)` (e.g. `1.5`, `1e+06`), decimal/money with all stored decimals (`12.3400`), date/time as RFC 3339 with nanoseconds (`2024-05-01T10:20:30.5Z`, offset kept for datetimeoffset), `uniqueidentifier` as upper-case hyphenated text, binary as UTF-8 (error if invalid) |
/// | `i64`, `i32`, `i16`, `u8`, `u16`, `u32`, `u64` | integer columns (range-checked); strings, decimal/money and floats when their text form above is an integer literal (so `decimal(10,0)` and `3.0` work, `12.50`, money and `true` do not) |
/// | `f64`, `f32` | integers, floats, decimal/money, numeric strings |
/// | `bool` | `bit`; integers 0/1; strings and decimals `1`/`0`/`t`/`f`/`true`/`false` (Go `ParseBool`) |
/// | `Decimal` | decimal/money, integers, numeric strings, finite floats |
/// | `DateTime` | `datetime`/`datetime2`/`smalldatetime`; `date` (midnight); `time` (on 0001-01-01); `datetimeoffset` (its local wall-clock time) |
/// | `OffsetDateTime` | `datetimeoffset`; other date/time types taken as UTC (go-mssqldb's default location) |
/// | `Date`, `Time` | the matching part of any date/time value |
/// | `Uuid` | `uniqueidentifier`, 16-byte binary (SQL Server byte order), UUID strings |
/// | `Vec<u8>` | binary as is, `uniqueidentifier` in SQL Server byte order, anything else as the bytes of its `String` form |
/// | `Value` | anything (a clone) |
/// | `Option<T>` | `None` for NULL, otherwise as `T` |
///
/// NULL into any non-`Option` type is [`Error::Null`], as in Go.
pub trait FromValue: Sized {
    /// Converts a non-NULL value.
    ///
    /// # Errors
    /// [`Error::Conversion`] when the value cannot be represented as `Self`.
    fn from_value(v: &Value) -> Result<Self, Error>;

    /// The result for NULL, or `None` if NULL is not allowed (the default).
    fn from_null() -> Option<Self> {
        None
    }
}

impl FromValue for Value {
    fn from_value(v: &Value) -> Result<Self, Error> {
        Ok(v.clone())
    }
    fn from_null() -> Option<Self> {
        Some(Value::Null)
    }
}

impl<T: FromValue> FromValue for Option<T> {
    fn from_value(v: &Value) -> Result<Self, Error> {
        T::from_value(v).map(Some)
    }
    fn from_null() -> Option<Self> {
        Some(None)
    }
}

impl FromValue for String {
    fn from_value(v: &Value) -> Result<Self, Error> {
        if let Value::String(s) = v {
            return Ok(s.clone());
        }
        v.go_string()
            .ok_or_else(|| conv(v, "String", "binary data is not valid UTF-8"))
    }
}

macro_rules! from_int {
    ($($t:ty),*) => {$(
        impl FromValue for $t {
            fn from_value(v: &Value) -> Result<Self, Error> {
                if let Some(i) = v.as_int() {
                    return <$t>::try_from(i).map_err(|_| conv(v, stringify!($t), format!("{i} is out of range")));
                }
                match v {
                    Value::Uuid(_) | Value::Date(_) | Value::Time(_) | Value::DateTime(_)
                    | Value::DateTimeOffset(_) => return Err(conv(v, stringify!($t), "not a number")),
                    _ => {}
                }
                let s = v.go_string().unwrap_or_default();
                s.parse::<$t>().map_err(|e| conv(v, stringify!($t), format!("{s:?}: {e}")))
            }
        }
    )*};
}

from_int!(i64, i32, i16, u8, u16, u32, u64);

impl FromValue for f64 {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::F64(f) => Ok(*f),
            // Go formats the float32 with 32-bit shortest digits, then parses as 64-bit.
            Value::F32(f) => Ok(format!("{f}").parse::<f64>().unwrap_or(*f as f64)),
            Value::Decimal(d) => Ok(d.to_f64()),
            Value::Bool(_)
            | Value::Uuid(_)
            | Value::Date(_)
            | Value::Time(_)
            | Value::DateTime(_)
            | Value::DateTimeOffset(_) => Err(conv(v, "f64", "not a number")),
            _ => match v.as_int() {
                Some(i) => Ok(i as f64),
                None => {
                    let s = v.go_string().unwrap_or_default();
                    s.parse::<f64>()
                        .map_err(|e| conv(v, "f64", format!("{s:?}: {e}")))
                }
            },
        }
    }
}

impl FromValue for f32 {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::F32(f) => Ok(*f),
            Value::Bool(_)
            | Value::Uuid(_)
            | Value::Date(_)
            | Value::Time(_)
            | Value::DateTime(_)
            | Value::DateTimeOffset(_) => Err(conv(v, "f32", "not a number")),
            _ => {
                let s = v.go_string().unwrap_or_default();
                s.parse::<f32>()
                    .map_err(|e| conv(v, "f32", format!("{s:?}: {e}")))
            }
        }
    }
}

impl FromValue for bool {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::Bool(b) => Ok(*b),
            Value::String(_) | Value::Bytes(_) | Value::Decimal(_) => {
                let s = v.go_string().unwrap_or_default();
                go_parse_bool(&s).ok_or_else(|| conv(v, "bool", format!("{s:?} is not a boolean")))
            }
            _ => match v.as_int() {
                Some(0) => Ok(false),
                Some(1) => Ok(true),
                Some(i) => Err(conv(v, "bool", format!("{i} is not 0 or 1"))),
                None => Err(conv(v, "bool", "not a boolean")),
            },
        }
    }
}

impl FromValue for Decimal {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::Decimal(d) => Ok(*d),
            Value::F32(_) | Value::F64(_) => {
                let f = f64::from_value(v)?;
                if !f.is_finite() {
                    return Err(conv(v, "Decimal", "not finite"));
                }
                format!("{f}")
                    .parse::<Decimal>()
                    .map_err(|e| conv(v, "Decimal", e.to_string()))
            }
            Value::String(s) => s
                .trim()
                .parse::<Decimal>()
                .map_err(|e| conv(v, "Decimal", e.to_string())),
            _ => match v.as_int() {
                Some(i) => Ok(Decimal::from(i)),
                None => Err(conv(v, "Decimal", "not a number")),
            },
        }
    }
}

impl FromValue for DateTime {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::DateTime(d) => Ok(*d),
            Value::Date(d) => Ok(d.midnight()),
            Value::Time(t) => Ok(year_one().at(*t)),
            Value::DateTimeOffset(o) => Ok(o.datetime()),
            _ => Err(conv(v, "DateTime", "not a date/time value")),
        }
    }
}

impl FromValue for OffsetDateTime {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::DateTimeOffset(o) => Ok(*o),
            _ => DateTime::from_value(v)
                .map(DateTime::assume_utc)
                .map_err(|_| conv(v, "OffsetDateTime", "not a date/time value")),
        }
    }
}

impl FromValue for Date {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::Date(d) => Ok(*d),
            Value::DateTime(d) => Ok(d.date()),
            Value::DateTimeOffset(o) => Ok(o.date()),
            _ => Err(conv(v, "Date", "not a date value")),
        }
    }
}

impl FromValue for Time {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::Time(t) => Ok(*t),
            Value::DateTime(d) => Ok(d.time()),
            Value::DateTimeOffset(o) => Ok(o.time()),
            _ => Err(conv(v, "Time", "not a time value")),
        }
    }
}

impl FromValue for Uuid {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::Uuid(u) => Ok(*u),
            Value::Bytes(b) if b.len() == 16 => {
                let mut a = [0u8; 16];
                a.copy_from_slice(b);
                Ok(Uuid::from_bytes_le(a))
            }
            Value::String(s) => s
                .trim()
                .parse::<Uuid>()
                .map_err(|e| conv(v, "Uuid", e.to_string())),
            _ => Err(conv(v, "Uuid", "not a uniqueidentifier")),
        }
    }
}

impl FromValue for Vec<u8> {
    fn from_value(v: &Value) -> Result<Self, Error> {
        match v {
            Value::Bytes(b) => Ok(b.clone()),
            Value::Uuid(u) => Ok(u.to_bytes_le().to_vec()),
            _ => String::from_value(v).map(String::into_bytes),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_float_formatting() {
        // Expected strings from Go: strconv.FormatFloat(v, 'g', -1, 64).
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (-2.5, "-2.5"),
            (0.1, "0.1"),
            (123456.0, "123456"),
            (1234567.0, "1.234567e+06"),
            (1e6, "1e+06"),
            (1e21, "1e+21"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-10, "1.5e-10"),
            (std::f64::consts::PI, "3.141592653589793"),
            (100.25, "100.25"),
            (999999.0, "999999"),
            (1e100, "1e+100"),
            (f64::MAX, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
        ];
        for (v, s) in cases {
            assert_eq!(go_format_float(*v, false), *s, "{v}");
        }
        assert_eq!(go_format_float(0.1f32 as f64, true), "0.1");
        assert_eq!(go_format_float(0.1f32 as f64, false), "0.10000000149011612");
        assert_eq!(go_format_float(16777216.0, true), "1.6777216e+07");
        assert_eq!(go_format_float(f64::NAN, false), "NaN");
        assert_eq!(go_format_float(f64::NEG_INFINITY, false), "-Inf");
    }

    fn dt(s: &str) -> DateTime {
        s.parse().unwrap()
    }

    #[test]
    fn string_conversions_match_go_scan() {
        let s = |v: Value| String::from_value(&v).unwrap();
        assert_eq!(s(Value::I32(-5)), "-5");
        assert_eq!(s(Value::I8(255)), "255");
        assert_eq!(s(Value::Bool(true)), "true");
        assert_eq!(s(Value::F64(2.0)), "2");
        assert_eq!(
            s(Value::Decimal(Decimal::new(123400, 4).unwrap())),
            "12.3400"
        );
        assert_eq!(s(Value::Decimal(Decimal::new(-5, 2).unwrap())), "-0.05");
        assert_eq!(
            s(Value::DateTime(dt("2024-05-01T10:20:30.5"))),
            "2024-05-01T10:20:30.5Z"
        );
        assert_eq!(
            s(Value::DateTime(dt("2024-05-01T10:20:30"))),
            "2024-05-01T10:20:30Z"
        );
        assert_eq!(
            s(Value::Date("2024-05-01".parse().unwrap())),
            "2024-05-01T00:00:00Z"
        );
        assert_eq!(
            s(Value::Time("01:02:03.1234567".parse().unwrap())),
            "0001-01-01T01:02:03.1234567Z"
        );
        let o: OffsetDateTime = "2024-05-01T10:20:30+07:00".parse().unwrap();
        assert_eq!(s(Value::DateTimeOffset(o)), "2024-05-01T10:20:30+07:00");
        let o: OffsetDateTime = "2024-05-01T10:20:30-03:30".parse().unwrap();
        assert_eq!(s(Value::DateTimeOffset(o)), "2024-05-01T10:20:30-03:30");
        let u: Uuid = "6f9619ff-8b86-d011-b42d-00c04fc964ff".parse().unwrap();
        assert_eq!(s(Value::Uuid(u)), "6F9619FF-8B86-D011-B42D-00C04FC964FF");
        assert_eq!(s(Value::Bytes(b"abc".to_vec())), "abc");
        assert!(String::from_value(&Value::Bytes(vec![0xFF])).is_err());
        assert!(matches!(
            Value::Null.convert::<String>(),
            Err(Error::Null { .. })
        ));
        assert_eq!(Value::Null.convert::<Option<String>>().unwrap(), None);
    }

    #[test]
    fn integer_conversions_match_go_scan() {
        assert_eq!(i64::from_value(&Value::I32(7)).unwrap(), 7);
        assert_eq!(i32::from_value(&Value::I64(7)).unwrap(), 7);
        assert!(i32::from_value(&Value::I64(1 << 40)).is_err());
        assert!(u8::from_value(&Value::I16(-1)).is_err());
        assert_eq!(i64::from_value(&Value::String("42".into())).unwrap(), 42);
        assert_eq!(i64::from_value(&Value::String("+42".into())).unwrap(), 42);
        assert!(i64::from_value(&Value::String(" 42".into())).is_err());
        assert_eq!(i64::from_value(&Value::F64(3.0)).unwrap(), 3);
        assert!(i64::from_value(&Value::F64(2.5)).is_err());
        assert!(i64::from_value(&Value::F64(1e21)).is_err());
        assert_eq!(
            i64::from_value(&Value::Decimal(Decimal::from(12))).unwrap(),
            12
        );
        assert!(i64::from_value(&Value::Decimal(Decimal::new(1200, 2).unwrap())).is_err());
        assert!(i64::from_value(&Value::Bool(true)).is_err());
        assert!(i64::from_value(&Value::DateTime(dt("2024-01-01T00:00:00"))).is_err());
    }

    #[test]
    fn float_bool_decimal_conversions() {
        assert_eq!(
            f64::from_value(&Value::I64(1 << 53)).unwrap(),
            9007199254740992.0
        );
        assert_eq!(f64::from_value(&Value::F32(0.1)).unwrap(), 0.1);
        assert_eq!(
            f64::from_value(&Value::Decimal(Decimal::new(123400, 4).unwrap())).unwrap(),
            12.34
        );
        assert_eq!(
            f64::from_value(&Value::String("1e3".into())).unwrap(),
            1000.0
        );
        assert!(f64::from_value(&Value::Bool(true)).is_err());
        assert_eq!(f32::from_value(&Value::F64(0.1)).unwrap(), 0.1f32);

        assert!(bool::from_value(&Value::Bool(true)).unwrap());
        assert!(bool::from_value(&Value::I32(1)).unwrap());
        assert!(!bool::from_value(&Value::I8(0)).unwrap());
        assert!(bool::from_value(&Value::I32(2)).is_err());
        assert!(bool::from_value(&Value::String("True".into())).unwrap());
        assert!(!bool::from_value(&Value::String("f".into())).unwrap());
        assert!(bool::from_value(&Value::String("yes".into())).is_err());
        assert!(bool::from_value(&Value::Decimal(Decimal::from(1))).unwrap());
        assert!(bool::from_value(&Value::Decimal(Decimal::new(100, 2).unwrap())).is_err());
        assert!(bool::from_value(&Value::F64(1.0)).is_err());

        assert_eq!(
            Decimal::from_value(&Value::I64(5)).unwrap(),
            Decimal::from(5)
        );
        assert_eq!(
            Decimal::from_value(&Value::String("1.25".into())).unwrap(),
            Decimal::new(125, 2).unwrap()
        );
        assert_eq!(
            Decimal::from_value(&Value::F64(0.5)).unwrap(),
            Decimal::new(5, 1).unwrap()
        );
    }

    #[test]
    fn date_time_conversions() {
        let d: Date = "2024-02-29".parse().unwrap();
        assert_eq!(DateTime::from_value(&Value::Date(d)).unwrap(), d.midnight());
        let o: OffsetDateTime = "2024-05-01T10:20:30+07:00".parse().unwrap();
        assert_eq!(
            DateTime::from_value(&Value::DateTimeOffset(o)).unwrap(),
            dt("2024-05-01T10:20:30")
        );
        let utc = OffsetDateTime::from_value(&Value::DateTime(dt("2024-05-01T10:20:30"))).unwrap();
        assert_eq!(utc.to_rfc3339(), "2024-05-01T10:20:30Z");
        assert_eq!(
            Date::from_value(&Value::DateTime(dt("2024-05-01T10:20:30")))
                .unwrap()
                .day(),
            1
        );
        assert_eq!(
            Time::from_value(&Value::DateTime(dt("2024-05-01T10:20:30")))
                .unwrap()
                .hour(),
            10
        );
        assert!(DateTime::from_value(&Value::String("2024-05-01".into())).is_err());
        let u: Uuid = "6f9619ff-8b86-d011-b42d-00c04fc964ff".parse().unwrap();
        let raw = Vec::<u8>::from_value(&Value::Uuid(u)).unwrap();
        assert_eq!(raw[..4], [0xFF, 0x19, 0x96, 0x6F]);
        assert_eq!(Uuid::from_value(&Value::Bytes(raw)).unwrap(), u);
        assert_eq!(Vec::<u8>::from_value(&Value::I32(12)).unwrap(), b"12");
    }
}
