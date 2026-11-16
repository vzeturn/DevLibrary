//! Query parameters and their RPC encoding.

use dl_decimal::Decimal;
use dl_time::{Date, DateTime, OffsetDateTime, Time};
use dl_uuid::Uuid;

use crate::types::ty;
use crate::wire::{put_utf16, to_utf16, utf16_len};
use crate::{Error, Value};

/// A parameter value with its SQL type. `None` in a typed variant is a typed NULL.
///
/// | Variant | Declared as |
/// |---|---|
/// | `Bool` | `bit` |
/// | `U8` / `I16` / `I32` / `I64` | `tinyint` / `smallint` / `int` / `bigint` |
/// | `F32` / `F64` | `real` / `float` |
/// | `Decimal` | `decimal(38, s)`, `s` = the value's scale (0 for NULL) |
/// | `String` | `nvarchar(4000)`, or `nvarchar(max)` above 4000 UTF-16 units |
/// | `Bytes` | `varbinary(8000)`, or `varbinary(max)` above 8000 bytes |
/// | `Uuid` | `uniqueidentifier` |
/// | `Date` / `Time` | `date` / `time(7)` |
/// | `DateTime` | `datetime2(7)` |
/// | `DateTimeOffset` | `datetimeoffset(7)` |
/// | `Null` | `nvarchar(4000)` NULL (converts implicitly to any type) |
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ParamValue {
    /// Untyped NULL.
    Null,
    /// `bit`.
    Bool(Option<bool>),
    /// `tinyint`.
    U8(Option<u8>),
    /// `smallint`.
    I16(Option<i16>),
    /// `int`.
    I32(Option<i32>),
    /// `bigint`.
    I64(Option<i64>),
    /// `real`.
    F32(Option<f32>),
    /// `float`.
    F64(Option<f64>),
    /// `decimal(38, s)`.
    Decimal(Option<Decimal>),
    /// `nvarchar`.
    String(Option<String>),
    /// `varbinary`.
    Bytes(Option<Vec<u8>>),
    /// `uniqueidentifier`.
    Uuid(Option<Uuid>),
    /// `date`.
    Date(Option<Date>),
    /// `time(7)`.
    Time(Option<Time>),
    /// `datetime2(7)`.
    DateTime(Option<DateTime>),
    /// `datetimeoffset(7)`.
    DateTimeOffset(Option<OffsetDateTime>),
}

/// Conversion into a [`ParamValue`]. Implemented for the Rust types in the
/// [`ParamValue`] table, their references where useful, `Option<T>` (typed NULL for
/// `None`) and [`Value`].
pub trait IntoParam {
    /// The parameter value.
    fn into_param(self) -> ParamValue;
    /// The typed NULL for this type (used for `Option::<Self>::None`).
    fn null() -> ParamValue
    where
        Self: Sized;
}

macro_rules! into_param {
    ($t:ty => $v:ident) => {
        impl IntoParam for $t {
            fn into_param(self) -> ParamValue {
                ParamValue::$v(Some(self.into()))
            }
            fn null() -> ParamValue {
                ParamValue::$v(None)
            }
        }
    };
}

into_param!(bool => Bool);
into_param!(u8 => U8);
into_param!(i16 => I16);
into_param!(i32 => I32);
into_param!(i64 => I64);
into_param!(f32 => F32);
into_param!(f64 => F64);
into_param!(Decimal => Decimal);
into_param!(String => String);
into_param!(&str => String);
into_param!(&String => String);
into_param!(Vec<u8> => Bytes);
into_param!(&[u8] => Bytes);
into_param!(Uuid => Uuid);
into_param!(Date => Date);
into_param!(Time => Time);
into_param!(DateTime => DateTime);
into_param!(OffsetDateTime => DateTimeOffset);

impl<T: IntoParam> IntoParam for Option<T> {
    fn into_param(self) -> ParamValue {
        match self {
            Some(v) => v.into_param(),
            None => T::null(),
        }
    }
    fn null() -> ParamValue {
        T::null()
    }
}

impl IntoParam for ParamValue {
    fn into_param(self) -> ParamValue {
        self
    }
    fn null() -> ParamValue {
        ParamValue::Null
    }
}

impl IntoParam for Value {
    /// Maps each [`Value`] to the matching parameter type; `I8` (tinyint) becomes `U8`.
    fn into_param(self) -> ParamValue {
        match self {
            Value::Null => ParamValue::Null,
            Value::Bool(v) => ParamValue::Bool(Some(v)),
            Value::I8(v) => ParamValue::U8(Some(v)),
            Value::I16(v) => ParamValue::I16(Some(v)),
            Value::I32(v) => ParamValue::I32(Some(v)),
            Value::I64(v) => ParamValue::I64(Some(v)),
            Value::F32(v) => ParamValue::F32(Some(v)),
            Value::F64(v) => ParamValue::F64(Some(v)),
            Value::Decimal(v) => ParamValue::Decimal(Some(v)),
            Value::String(v) => ParamValue::String(Some(v)),
            Value::Bytes(v) => ParamValue::Bytes(Some(v)),
            Value::Uuid(v) => ParamValue::Uuid(Some(v)),
            Value::Date(v) => ParamValue::Date(Some(v)),
            Value::Time(v) => ParamValue::Time(Some(v)),
            Value::DateTime(v) => ParamValue::DateTime(Some(v)),
            Value::DateTimeOffset(v) => ParamValue::DateTimeOffset(Some(v)),
        }
    }
    fn null() -> ParamValue {
        ParamValue::Null
    }
}

impl<T: IntoParam + Clone> IntoParam for &T
where
    T: NotRef,
{
    fn into_param(self) -> ParamValue {
        self.clone().into_param()
    }
    fn null() -> ParamValue {
        T::null()
    }
}

/// Marker for owned parameter types, so `&T` works for them without conflicting with the
/// explicit `&str`, `&String` and `&[u8]` impls.
#[doc(hidden)]
pub trait NotRef {}
macro_rules! not_ref {
    ($($t:ty),*) => {$(impl NotRef for $t {})*};
}
not_ref!(
    bool,
    u8,
    i16,
    i32,
    i64,
    f32,
    f64,
    Decimal,
    Vec<u8>,
    Uuid,
    Date,
    Time,
    DateTime,
    OffsetDateTime,
    Value,
    ParamValue
);
impl<T: NotRef> NotRef for Option<T> {}

/// A named query parameter, referenced as `@name` in the SQL text.
///
/// ```
/// use dl_tds::Param;
/// let p = [
///     Param::new("Limit", 50i32),
///     Param::new("CollectFrom", Option::<dl_time::DateTime>::None),
///     Param::new("@ID", "123"), // a leading '@' is optional
/// ];
/// assert_eq!(p[2].name(), "ID");
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    name: String,
    value: ParamValue,
}

impl Param {
    /// A parameter named `name` (with or without the leading `@`).
    pub fn new(name: &str, value: impl IntoParam) -> Param {
        Param {
            name: name.strip_prefix('@').unwrap_or(name).to_string(),
            value: value.into_param(),
        }
    }

    /// The name without `@`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The value.
    pub fn value(&self) -> &ParamValue {
        &self.value
    }

    /// The SQL type declaration used in the `sp_executesql` parameter list.
    pub fn declaration(&self) -> String {
        match &self.value {
            ParamValue::Null => "nvarchar(4000)".into(),
            ParamValue::Bool(_) => "bit".into(),
            ParamValue::U8(_) => "tinyint".into(),
            ParamValue::I16(_) => "smallint".into(),
            ParamValue::I32(_) => "int".into(),
            ParamValue::I64(_) => "bigint".into(),
            ParamValue::F32(_) => "real".into(),
            ParamValue::F64(_) => "float".into(),
            ParamValue::Decimal(d) => format!("decimal(38,{})", d.map_or(0, |d| d.scale())),
            ParamValue::String(s) => {
                if s.as_deref().map_or(0, utf16_len) > 4000 {
                    "nvarchar(max)".into()
                } else {
                    "nvarchar(4000)".into()
                }
            }
            ParamValue::Bytes(b) => {
                if b.as_ref().map_or(0, Vec::len) > 8000 {
                    "varbinary(max)".into()
                } else {
                    "varbinary(8000)".into()
                }
            }
            ParamValue::Uuid(_) => "uniqueidentifier".into(),
            ParamValue::Date(_) => "date".into(),
            ParamValue::Time(_) => "time(7)".into(),
            ParamValue::DateTime(_) => "datetime2(7)".into(),
            ParamValue::DateTimeOffset(_) => "datetimeoffset(7)".into(),
        }
    }
}

/// `sp_executesql` procedure id.
const SP_EXECUTESQL: u16 = 10;

/// ALL_HEADERS with the transaction descriptor header (required since TDS 7.2).
pub(crate) fn all_headers(out: &mut Vec<u8>, transaction: u64) {
    out.extend_from_slice(&22u32.to_le_bytes());
    out.extend_from_slice(&18u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&transaction.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
}

/// SQLBatch payload.
pub(crate) fn sql_batch(sql: &str, transaction: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(22 + sql.len() * 2);
    all_headers(&mut out, transaction);
    put_utf16(&mut out, sql);
    out
}

/// RPC payload calling `sp_executesql @stmt, @params, @p1, ...`, as go-mssqldb sends
/// named parameters.
pub(crate) fn sp_executesql(
    sql: &str,
    params: &[Param],
    transaction: u64,
) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(64 + sql.len() * 2);
    all_headers(&mut out, transaction);
    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    out.extend_from_slice(&SP_EXECUTESQL.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // option flags
    let decls: Vec<String> = params
        .iter()
        .map(|p| format!("@{} {}", p.name, p.declaration()))
        .collect();
    write_string_param(&mut out, "", sql);
    write_string_param(&mut out, "", &decls.join(","));
    for p in params {
        if p.name.is_empty() || utf16_len(&p.name) > 127 {
            return Err(Error::Param {
                name: p.name.clone(),
                message: "name must be 1 to 127 characters".into(),
            });
        }
        write_param(&mut out, p)?;
    }
    Ok(out)
}

fn write_name(out: &mut Vec<u8>, name: &str) {
    if name.is_empty() {
        out.push(0);
    } else {
        let full = format!("@{name}");
        out.push(utf16_len(&full) as u8);
        put_utf16(out, &full);
    }
}

/// An nvarchar parameter as go-mssqldb writes @stmt/@params: nvarchar(N) when it fits in
/// 4000 characters, otherwise nvarchar(max).
fn write_string_param(out: &mut Vec<u8>, name: &str, s: &str) {
    write_name(out, name);
    out.push(0); // status
    write_nvarchar(out, Some(s));
}

/// Parameters carry an all-zero collation, as go-mssqldb sends; the server ignores it for
/// nvarchar.
const DEFAULT_COLLATION: [u8; 5] = [0; 5];

fn write_nvarchar(out: &mut Vec<u8>, s: Option<&str>) {
    let bytes = s.map(to_utf16);
    let max = bytes.as_ref().is_some_and(|b| b.len() > 8000);
    out.push(ty::NVARCHAR);
    if max {
        out.extend_from_slice(&0xFFFFu16.to_le_bytes());
        out.extend_from_slice(&DEFAULT_COLLATION);
        write_plp(out, bytes.as_deref());
    } else {
        out.extend_from_slice(&8000u16.to_le_bytes());
        out.extend_from_slice(&DEFAULT_COLLATION);
        match bytes {
            None => out.extend_from_slice(&0xFFFFu16.to_le_bytes()),
            Some(b) => {
                out.extend_from_slice(&(b.len() as u16).to_le_bytes());
                out.extend_from_slice(&b);
            }
        }
    }
}

fn write_plp(out: &mut Vec<u8>, data: Option<&[u8]>) {
    match data {
        None => out.extend_from_slice(&u64::MAX.to_le_bytes()),
        Some(d) => {
            out.extend_from_slice(&(d.len() as u64).to_le_bytes());
            for chunk in d.chunks(0x7FFF_FFFF) {
                out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
                out.extend_from_slice(chunk);
            }
            out.extend_from_slice(&0u32.to_le_bytes());
        }
    }
}

/// Days since 0001-01-01.
fn days_0001(d: Date) -> u32 {
    (d.to_unix_days() as i64 + 719_162) as u32
}

/// TIME(7) value: 100 ns units since midnight, 5 bytes.
fn time7(t: Time) -> [u8; 5] {
    let v = t.nanos_of_day() / 100;
    let b = v.to_le_bytes();
    [b[0], b[1], b[2], b[3], b[4]]
}

fn put_date3(out: &mut Vec<u8>, d: Date) {
    out.extend_from_slice(&days_0001(d).to_le_bytes()[..3]);
}

fn write_param(out: &mut Vec<u8>, p: &Param) -> Result<(), Error> {
    write_name(out, &p.name);
    out.push(0); // status: input
    // Writes a byte-length type: type id, max size, then the value (len 0 = NULL).
    fn bytelen(out: &mut Vec<u8>, id: u8, size: u8, v: Option<&[u8]>) {
        out.push(id);
        out.push(size);
        match v {
            None => out.push(0),
            Some(b) => {
                out.push(b.len() as u8);
                out.extend_from_slice(b);
            }
        }
    }
    match &p.value {
        ParamValue::Null => write_nvarchar(out, None),
        ParamValue::Bool(v) => bytelen(
            out,
            ty::BITN,
            1,
            v.map(|b| [b as u8]).as_ref().map(|a| &a[..]),
        ),
        ParamValue::U8(v) => bytelen(out, ty::INTN, 1, v.map(|x| [x]).as_ref().map(|a| &a[..])),
        ParamValue::I16(v) => bytelen(
            out,
            ty::INTN,
            2,
            v.map(i16::to_le_bytes).as_ref().map(|a| &a[..]),
        ),
        ParamValue::I32(v) => bytelen(
            out,
            ty::INTN,
            4,
            v.map(i32::to_le_bytes).as_ref().map(|a| &a[..]),
        ),
        ParamValue::I64(v) => bytelen(
            out,
            ty::INTN,
            8,
            v.map(i64::to_le_bytes).as_ref().map(|a| &a[..]),
        ),
        ParamValue::F32(v) => bytelen(
            out,
            ty::FLTN,
            4,
            v.map(f32::to_le_bytes).as_ref().map(|a| &a[..]),
        ),
        ParamValue::F64(v) => bytelen(
            out,
            ty::FLTN,
            8,
            v.map(f64::to_le_bytes).as_ref().map(|a| &a[..]),
        ),
        ParamValue::Decimal(v) => {
            out.push(ty::DECIMALN);
            out.push(17);
            out.push(38);
            out.push(v.map_or(0, |d| d.scale()) as u8);
            match v {
                None => out.push(0),
                Some(d) => {
                    out.push(17);
                    out.push(if d.is_negative() { 0 } else { 1 });
                    out.extend_from_slice(&d.mantissa().unsigned_abs().to_le_bytes());
                }
            }
        }
        ParamValue::String(s) => write_nvarchar(out, s.as_deref()),
        ParamValue::Bytes(b) => {
            out.push(ty::BIGVARBIN);
            match b {
                Some(b) if b.len() > 8000 => {
                    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
                    write_plp(out, Some(b));
                }
                _ => {
                    out.extend_from_slice(&8000u16.to_le_bytes());
                    match b {
                        None => out.extend_from_slice(&0xFFFFu16.to_le_bytes()),
                        Some(b) => {
                            out.extend_from_slice(&(b.len() as u16).to_le_bytes());
                            out.extend_from_slice(b);
                        }
                    }
                }
            }
        }
        ParamValue::Uuid(u) => bytelen(
            out,
            ty::GUID,
            16,
            u.map(|u| u.to_bytes_le()).as_ref().map(|a| &a[..]),
        ),
        ParamValue::Date(d) => {
            out.push(ty::DATEN);
            match d {
                None => out.push(0),
                Some(d) => {
                    out.push(3);
                    put_date3(out, *d);
                }
            }
        }
        ParamValue::Time(t) => {
            out.push(ty::TIMEN);
            out.push(7);
            match t {
                None => out.push(0),
                Some(t) => {
                    out.push(5);
                    out.extend_from_slice(&time7(*t));
                }
            }
        }
        ParamValue::DateTime(dt) => {
            out.push(ty::DATETIME2N);
            out.push(7);
            match dt {
                None => out.push(0),
                Some(dt) => {
                    check_year(p, dt.year())?;
                    out.push(8);
                    out.extend_from_slice(&time7(dt.time()));
                    put_date3(out, dt.date());
                }
            }
        }
        ParamValue::DateTimeOffset(o) => {
            out.push(ty::DATETIMEOFFSETN);
            out.push(7);
            match o {
                None => out.push(0),
                Some(o) => {
                    let utc = o
                        .checked_to_offset(dl_time::Offset::UTC)
                        .ok_or_else(|| Error::Param {
                            name: p.name.clone(),
                            message: "datetimeoffset out of range".into(),
                        })?
                        .datetime();
                    check_year(p, utc.year())?;
                    let minutes = (o.offset().whole_seconds() / 60) as i16;
                    out.push(10);
                    out.extend_from_slice(&time7(utc.time()));
                    put_date3(out, utc.date());
                    out.extend_from_slice(&minutes.to_le_bytes());
                }
            }
        }
    }
    if let ParamValue::Date(Some(d)) = &p.value {
        check_year(p, d.year())?;
    }
    Ok(())
}

fn check_year(p: &Param, year: i32) -> Result<(), Error> {
    if (1..=9999).contains(&year) {
        Ok(())
    } else {
        Err(Error::Param {
            name: p.name.clone(),
            message: format!("year {year} is outside 0001-9999"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn encoded(p: Param) -> Vec<u8> {
        let mut out = Vec::new();
        write_param(&mut out, &p).unwrap();
        out
    }

    #[test]
    fn declarations() {
        assert_eq!(Param::new("a", 1i32).declaration(), "int");
        assert_eq!(Param::new("a", 1i64).declaration(), "bigint");
        assert_eq!(Param::new("a", true).declaration(), "bit");
        assert_eq!(Param::new("a", 1.5f64).declaration(), "float");
        assert_eq!(Param::new("a", "x").declaration(), "nvarchar(4000)");
        assert_eq!(
            Param::new("a", "x".repeat(4001)).declaration(),
            "nvarchar(max)"
        );
        assert_eq!(
            Param::new("a", Decimal::new(125, 2).unwrap()).declaration(),
            "decimal(38,2)"
        );
        assert_eq!(
            Param::new("a", Option::<DateTime>::None).declaration(),
            "datetime2(7)"
        );
        assert_eq!(
            Param::new("a", Option::<OffsetDateTime>::None).declaration(),
            "datetimeoffset(7)"
        );
        assert_eq!(Param::new("a", &b"xy"[..]).declaration(), "varbinary(8000)");
        assert_eq!(
            Param::new("a", vec![0u8; 8001]).declaration(),
            "varbinary(max)"
        );
        assert_eq!(
            Param::new("a", ParamValue::Null).declaration(),
            "nvarchar(4000)"
        );
        let s = String::from("s");
        assert_eq!(Param::new("@a", &s).name(), "a");
        // The `&T` impl is what is being tested here.
        #[allow(clippy::needless_borrows_for_generic_args)]
        let by_ref = Param::new("a", &5i32);
        assert_eq!(by_ref.value(), &ParamValue::I32(Some(5)));
        assert_eq!(Param::new("a", Some(5u8)).value(), &ParamValue::U8(Some(5)));
        assert_eq!(
            Param::new("a", Value::I8(5)).value(),
            &ParamValue::U8(Some(5))
        );
    }

    /// The RPC go-mssqldb sent for `ExecContext(..., Named("Limit", int32(50)),
    /// Named("CollectFrom", sql.NullTime{}))` (testdata/go_mssqldb_capture.txt). dl-tds
    /// writes the same ALL_HEADERS, procedure id, @stmt and @Limit bytes; the declaration
    /// list differs only in the time type (go: datetimeoffset(7), dl-tds: datetime2(7)).
    #[test]
    fn rpc_matches_go_mssqldb_where_types_agree() {
        let capture = include_str!("../testdata/go_mssqldb_capture.txt");
        let line = capture.lines().find(|l| l.starts_with("3 ")).unwrap();
        let go = hex(&line[2..]);
        let go = &go[8..]; // packet header
        let sql = "EXEC dbo.sp_X @Limit = @Limit, @CollectFrom = @CollectFrom";
        let ours = sp_executesql(sql, &[Param::new("Limit", 50i32)], 0).unwrap();
        // ALL_HEADERS + proc id + flags + @stmt as go-mssqldb: type, max len, collation, value.
        let stmt_end = 22 + 6 + 2 + 1 + 2 + 5 + 2 + sql.len() * 2;
        assert_eq!(ours[..22 + 6], go[..22 + 6]);
        // go declares @stmt as nvarchar(len); dl-tds always as nvarchar(4000).
        assert_eq!(go[28..31], [0, 0, ty::NVARCHAR]);
        assert_eq!(ours[28..31], [0, 0, ty::NVARCHAR]);
        assert_eq!(ours[33..stmt_end], go[33..stmt_end]);
        // The @Limit int parameter is identical.
        let limit = encoded(Param::new("Limit", 50i32));
        let pos = go.windows(limit.len()).position(|w| w == limit.as_slice());
        assert!(pos.is_some(), "go-mssqldb sent the same bytes for @Limit");
        // @Ok bit from the second capture.
        let line = capture
            .lines()
            .filter(|l| l.starts_with("3 "))
            .nth(1)
            .unwrap();
        let go2 = hex(&line[2..]);
        let ok = encoded(Param::new("Ok", true));
        assert!(go2.windows(ok.len()).any(|w| w == ok.as_slice()));
        // SQLBatch for "SET NOCOUNT ON".
        let line = capture.lines().find(|l| l.starts_with("1 ")).unwrap();
        let go3 = hex(&line[2..]);
        assert_eq!(sql_batch("SET NOCOUNT ON", 0), go3[8..]);
    }

    #[test]
    fn value_encodings() {
        // int NULL: INTN, size 4, len 0.
        assert_eq!(
            encoded(Param::new("a", Option::<i32>::None)),
            [2, b'@', 0, b'a', 0, 0, ty::INTN, 4, 0]
        );
        assert_eq!(
            encoded(Param::new("a", -2i64))[6..],
            [
                ty::INTN,
                8,
                8,
                0xFE,
                0xFF,
                0xFF,
                0xFF,
                0xFF,
                0xFF,
                0xFF,
                0xFF
            ]
        );
        assert_eq!(encoded(Param::new("a", 1.5f64))[6..9], [ty::FLTN, 8, 8]);
        // decimal(38,2) -12.50
        let d = encoded(Param::new("a", Decimal::new(-1250, 2).unwrap()));
        assert_eq!(d[6..12], [ty::DECIMALN, 17, 38, 2, 17, 0]);
        assert_eq!(u128::from_le_bytes(d[12..28].try_into().unwrap()), 1250);
        // nvarchar
        let s = encoded(Param::new("a", "hé"));
        assert_eq!(
            s[6..],
            [
                ty::NVARCHAR,
                0x40,
                0x1F,
                0,
                0,
                0,
                0,
                0,
                4,
                0,
                b'h',
                0,
                0xE9,
                0
            ]
        );
        let s = encoded(Param::new("a", Option::<String>::None));
        assert_eq!(
            s[6..],
            [ty::NVARCHAR, 0x40, 0x1F, 0, 0, 0, 0, 0, 0xFF, 0xFF]
        );
        // nvarchar(max): PLP with known length and one chunk.
        let long = "x".repeat(5000);
        let s = encoded(Param::new("a", long.as_str()));
        assert_eq!(s[6..9], [ty::NVARCHAR, 0xFF, 0xFF]);
        assert_eq!(u64::from_le_bytes(s[14..22].try_into().unwrap()), 10_000);
        assert_eq!(u32::from_le_bytes(s[22..26].try_into().unwrap()), 10_000);
        assert_eq!(s[s.len() - 4..], [0, 0, 0, 0]);
        // varbinary NULL / value
        assert_eq!(
            encoded(Param::new("a", Option::<Vec<u8>>::None))[6..],
            [ty::BIGVARBIN, 0x40, 0x1F, 0xFF, 0xFF]
        );
        assert_eq!(
            encoded(Param::new("a", vec![7u8]))[6..],
            [ty::BIGVARBIN, 0x40, 0x1F, 1, 0, 7]
        );
        // uniqueidentifier in SQL Server byte order
        let u: Uuid = "6f9619ff-8b86-d011-b42d-00c04fc964ff".parse().unwrap();
        assert_eq!(
            encoded(Param::new("a", u))[6..12],
            [ty::GUID, 16, 16, 0xFF, 0x19, 0x96]
        );
        // datetime2(7) 2024-05-01 10:20:30.5
        let dt: DateTime = "2024-05-01T10:20:30.5".parse().unwrap();
        let e = encoded(Param::new("a", dt));
        assert_eq!(e[6..9], [ty::DATETIME2N, 7, 8]);
        let mut t = [0u8; 8];
        t[..5].copy_from_slice(&e[9..14]);
        assert_eq!(u64::from_le_bytes(t), 372_305_000_000);
        assert_eq!(u32::from_le_bytes([e[14], e[15], e[16], 0]), 739_006);
        // datetimeoffset(7) stores UTC + offset minutes
        let o: OffsetDateTime = "2024-05-01T10:20:30+07:00".parse().unwrap();
        let e = encoded(Param::new("a", o));
        assert_eq!(e[6..9], [ty::DATETIMEOFFSETN, 7, 10]);
        let mut t = [0u8; 8];
        t[..5].copy_from_slice(&e[9..14]);
        assert_eq!(
            u64::from_le_bytes(t),
            (3 * 3600 + 20 * 60 + 30) * 10_000_000
        );
        assert_eq!(i16::from_le_bytes([e[17], e[18]]), 420);
        // date / time
        let d: Date = "0001-01-01".parse().unwrap();
        assert_eq!(encoded(Param::new("a", d))[6..], [ty::DATEN, 3, 0, 0, 0]);
        let tm: Time = "00:00:01".parse().unwrap();
        assert_eq!(
            encoded(Param::new("a", tm))[6..],
            [ty::TIMEN, 7, 5, 0x80, 0x96, 0x98, 0, 0]
        );
        // Out-of-range year.
        let early: DateTime = "0000-12-31T00:00:00".parse().unwrap();
        let mut out = Vec::new();
        assert!(write_param(&mut out, &Param::new("a", early)).is_err());
        assert!(sp_executesql("x", &[Param::new("", 1i32)], 0).is_err());
    }
}
