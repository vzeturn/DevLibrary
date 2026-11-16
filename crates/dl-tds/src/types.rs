//! TYPE_INFO parsing and value decoding for every TDS 7.4 data type.

use dl_decimal::Decimal;
use dl_time::{Date, DateTime, Offset, OffsetDateTime, Time};
use dl_uuid::Uuid;

use crate::collation::Collation;
use crate::wire::{Cursor, utf16le};
use crate::{Error, Value};

/// TDS data type ids.
#[allow(dead_code)]
pub(crate) mod ty {
    pub const NULL: u8 = 0x1F;
    pub const INT1: u8 = 0x30;
    pub const BIT: u8 = 0x32;
    pub const INT2: u8 = 0x34;
    pub const INT4: u8 = 0x38;
    pub const DATETIM4: u8 = 0x3A;
    pub const FLT4: u8 = 0x3B;
    pub const MONEY: u8 = 0x3C;
    pub const DATETIME: u8 = 0x3D;
    pub const FLT8: u8 = 0x3E;
    pub const MONEY4: u8 = 0x7A;
    pub const INT8: u8 = 0x7F;
    pub const GUID: u8 = 0x24;
    pub const INTN: u8 = 0x26;
    pub const DECIMAL: u8 = 0x37;
    pub const NUMERIC: u8 = 0x3F;
    pub const BITN: u8 = 0x68;
    pub const DECIMALN: u8 = 0x6A;
    pub const NUMERICN: u8 = 0x6C;
    pub const FLTN: u8 = 0x6D;
    pub const MONEYN: u8 = 0x6E;
    pub const DATETIMN: u8 = 0x6F;
    pub const DATEN: u8 = 0x28;
    pub const TIMEN: u8 = 0x29;
    pub const DATETIME2N: u8 = 0x2A;
    pub const DATETIMEOFFSETN: u8 = 0x2B;
    pub const CHAR: u8 = 0x2F;
    pub const VARCHAR: u8 = 0x27;
    pub const BINARY: u8 = 0x2D;
    pub const VARBINARY: u8 = 0x25;
    pub const BIGVARBIN: u8 = 0xA5;
    pub const BIGVARCHR: u8 = 0xA7;
    pub const BIGBINARY: u8 = 0xAD;
    pub const BIGCHAR: u8 = 0xAF;
    pub const NVARCHAR: u8 = 0xE7;
    pub const NCHAR: u8 = 0xEF;
    pub const XML: u8 = 0xF1;
    pub const UDT: u8 = 0xF0;
    pub const TEXT: u8 = 0x23;
    pub const IMAGE: u8 = 0x22;
    pub const NTEXT: u8 = 0x63;
    pub const SSVARIANT: u8 = 0x62;
}

/// How a value of a type is length-prefixed in a ROW.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Len {
    /// Always this many bytes (NULLTYPE is `Fixed(0)`).
    Fixed(usize),
    /// One length byte; 0 = NULL.
    Byte,
    /// Two length bytes; 0xFFFF = NULL.
    UShort,
    /// PARTLENTYPE (the `(max)` types, XML, UDT).
    Plp,
    /// TEXT/NTEXT/IMAGE: text pointer, timestamp, 4-byte length.
    Text,
    /// SQL_VARIANT: 4-byte length then base type and properties.
    Variant,
}

/// Parsed TYPE_INFO of a column, parameter or return value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeInfo {
    pub(crate) id: u8,
    pub(crate) len: Len,
    /// Declared maximum length in bytes (0xFFFF for `(max)`), or the fixed size.
    pub(crate) size: u32,
    pub(crate) precision: u8,
    pub(crate) scale: u8,
    pub(crate) collation: Option<Collation>,
    /// UDT type name (for display only).
    pub(crate) udt_name: String,
}

impl TypeInfo {
    fn new(id: u8, len: Len, size: u32) -> TypeInfo {
        TypeInfo {
            id,
            len,
            size,
            precision: 0,
            scale: 0,
            collation: None,
            udt_name: String::new(),
        }
    }

    /// Reads TYPE_INFO starting at the type id. `colmetadata` adds the TableName parts
    /// that follow TEXT/NTEXT/IMAGE in COLMETADATA.
    pub(crate) fn read(cur: &mut Cursor<'_>, colmetadata: bool) -> Result<TypeInfo, Error> {
        let id = cur.u8()?;
        let fixed = |n: usize| Ok(TypeInfo::new(id, Len::Fixed(n), n as u32));
        match id {
            ty::NULL => fixed(0),
            ty::INT1 | ty::BIT => fixed(1),
            ty::INT2 => fixed(2),
            ty::INT4 | ty::DATETIM4 | ty::FLT4 | ty::MONEY4 => fixed(4),
            ty::MONEY | ty::DATETIME | ty::FLT8 | ty::INT8 => fixed(8),
            ty::DATEN => Ok(TypeInfo::new(id, Len::Byte, 3)),
            ty::TIMEN | ty::DATETIME2N | ty::DATETIMEOFFSETN => {
                let scale = cur.u8()?;
                if scale > 7 {
                    return Err(cur.err("time scale above 7"));
                }
                let mut t = TypeInfo::new(id, Len::Byte, 0);
                t.size = time_len(scale) as u32
                    + match id {
                        ty::DATETIME2N => 3,
                        ty::DATETIMEOFFSETN => 5,
                        _ => 0,
                    };
                t.scale = scale;
                Ok(t)
            }
            ty::GUID
            | ty::INTN
            | ty::BITN
            | ty::FLTN
            | ty::MONEYN
            | ty::DATETIMN
            | ty::CHAR
            | ty::VARCHAR
            | ty::BINARY
            | ty::VARBINARY => {
                let size = cur.u8()?;
                Ok(TypeInfo::new(id, Len::Byte, size as u32))
            }
            ty::DECIMAL | ty::NUMERIC | ty::DECIMALN | ty::NUMERICN => {
                let size = cur.u8()?;
                let mut t = TypeInfo::new(id, Len::Byte, size as u32);
                t.precision = cur.u8()?;
                t.scale = cur.u8()?;
                if t.scale > 38 {
                    return Err(cur.err("decimal scale above 38"));
                }
                Ok(t)
            }
            ty::BIGVARBIN
            | ty::BIGBINARY
            | ty::BIGVARCHR
            | ty::BIGCHAR
            | ty::NVARCHAR
            | ty::NCHAR => {
                let size = cur.u16()?;
                let len = if size == 0xFFFF {
                    Len::Plp
                } else {
                    Len::UShort
                };
                let mut t = TypeInfo::new(id, len, size as u32);
                if matches!(id, ty::BIGVARCHR | ty::BIGCHAR | ty::NVARCHAR | ty::NCHAR) {
                    t.collation = Some(Collation::from_bytes(cur.array()?));
                }
                Ok(t)
            }
            ty::XML => {
                if cur.u8()? != 0 {
                    cur.b_varchar()?; // database
                    cur.b_varchar()?; // owning schema
                    cur.us_varchar()?; // schema collection
                }
                Ok(TypeInfo::new(id, Len::Plp, 0xFFFF))
            }
            ty::UDT => {
                let size = cur.u16()?;
                cur.b_varchar()?; // database
                cur.b_varchar()?; // schema
                let mut t = TypeInfo::new(id, Len::Plp, size as u32);
                t.udt_name = cur.b_varchar()?;
                cur.us_varchar()?; // assembly-qualified name
                Ok(t)
            }
            ty::TEXT | ty::NTEXT | ty::IMAGE => {
                let size = cur.u32()?;
                let mut t = TypeInfo::new(id, Len::Text, size);
                if id != ty::IMAGE {
                    t.collation = Some(Collation::from_bytes(cur.array()?));
                }
                if colmetadata {
                    let parts = cur.u8()?;
                    for _ in 0..parts {
                        cur.us_varchar()?;
                    }
                }
                Ok(t)
            }
            ty::SSVARIANT => {
                let size = cur.u32()?;
                Ok(TypeInfo::new(id, Len::Variant, size))
            }
            _ => Err(cur.err("unknown data type")),
        }
    }

    /// The SQL Server type name in upper case, like go-mssqldb's `DatabaseTypeName`.
    pub(crate) fn type_name(&self) -> &'static str {
        match self.id {
            ty::NULL => "NULL",
            ty::INT1 => "TINYINT",
            ty::BIT | ty::BITN => "BIT",
            ty::INT2 => "SMALLINT",
            ty::INT4 => "INT",
            ty::INT8 => "BIGINT",
            ty::INTN => match self.size {
                1 => "TINYINT",
                2 => "SMALLINT",
                4 => "INT",
                _ => "BIGINT",
            },
            ty::DATETIM4 => "SMALLDATETIME",
            ty::DATETIME => "DATETIME",
            ty::DATETIMN => {
                if self.size == 4 {
                    "SMALLDATETIME"
                } else {
                    "DATETIME"
                }
            }
            ty::FLT4 => "REAL",
            ty::FLT8 => "FLOAT",
            ty::FLTN => {
                if self.size == 4 {
                    "REAL"
                } else {
                    "FLOAT"
                }
            }
            ty::MONEY4 => "SMALLMONEY",
            ty::MONEY => "MONEY",
            ty::MONEYN => {
                if self.size == 4 {
                    "SMALLMONEY"
                } else {
                    "MONEY"
                }
            }
            ty::DECIMAL | ty::DECIMALN => "DECIMAL",
            ty::NUMERIC | ty::NUMERICN => "NUMERIC",
            ty::GUID => "UNIQUEIDENTIFIER",
            ty::DATEN => "DATE",
            ty::TIMEN => "TIME",
            ty::DATETIME2N => "DATETIME2",
            ty::DATETIMEOFFSETN => "DATETIMEOFFSET",
            ty::CHAR | ty::BIGCHAR => "CHAR",
            ty::VARCHAR | ty::BIGVARCHR => "VARCHAR",
            ty::BINARY | ty::BIGBINARY => "BINARY",
            ty::VARBINARY | ty::BIGVARBIN => "VARBINARY",
            ty::NVARCHAR => "NVARCHAR",
            ty::NCHAR => "NCHAR",
            ty::XML => "XML",
            ty::UDT => "UDT",
            ty::TEXT => "TEXT",
            ty::NTEXT => "NTEXT",
            ty::IMAGE => "IMAGE",
            ty::SSVARIANT => "SQL_VARIANT",
            _ => "UNKNOWN",
        }
    }

    /// Reads one value of this type from a ROW / RETURNVALUE.
    pub(crate) fn read_value(&self, cur: &mut Cursor<'_>) -> Result<Value, Error> {
        match self.len {
            Len::Fixed(0) => Ok(Value::Null),
            Len::Fixed(n) => {
                let at = cur.offset();
                let data = cur.take(n)?;
                decode(self.id, self.scale, self.collation, data, true)
                    .map_err(|w| at_err(cur, at, w))
            }
            Len::Byte => {
                let n = cur.u8()? as usize;
                if n == 0 {
                    return Ok(Value::Null);
                }
                let at = cur.offset();
                let data = cur.take(n)?;
                self.decode_var(data).map_err(|w| at_err(cur, at, w))
            }
            Len::UShort => {
                let n = cur.u16()?;
                if n == 0xFFFF {
                    return Ok(Value::Null);
                }
                let at = cur.offset();
                let data = cur.take(n as usize)?;
                self.decode_var(data).map_err(|w| at_err(cur, at, w))
            }
            Len::Plp => {
                let at = cur.offset();
                match read_plp(cur)? {
                    None => Ok(Value::Null),
                    Some(data) => self.decode_var(&data).map_err(|w| at_err(cur, at, w)),
                }
            }
            Len::Text => {
                let ptr = cur.u8()? as usize;
                if ptr == 0 {
                    return Ok(Value::Null);
                }
                cur.skip(ptr)?; // text pointer
                cur.skip(8)?; // timestamp
                let n = cur.i32()?;
                if n == -1 {
                    return Ok(Value::Null);
                }
                let n = usize::try_from(n).map_err(|_| cur.err("negative TEXT length"))?;
                let at = cur.offset();
                let data = cur.take(n)?;
                self.decode_var(data).map_err(|w| at_err(cur, at, w))
            }
            Len::Variant => read_variant(cur),
        }
    }

    fn decode_var(&self, data: &[u8]) -> Result<Value, Decode> {
        decode(self.id, self.scale, self.collation, data, false)
    }
}

/// Decoding failure: a malformed value (message) or an unsupported code page.
pub(crate) enum Decode {
    Bad(&'static str),
    CodePage(u16),
}

fn at_err(cur: &Cursor<'_>, at: usize, d: Decode) -> Error {
    match d {
        Decode::Bad(what) => cur.err_at(at, what),
        Decode::CodePage(code_page) => Error::UnsupportedCodePage { code_page },
    }
}

/// Bytes of the time part for a scale.
pub(crate) fn time_len(scale: u8) -> usize {
    match scale {
        0..=2 => 3,
        3 | 4 => 4,
        _ => 5,
    }
}

/// Reads a PLP value: `None` for PLP_NULL.
fn read_plp(cur: &mut Cursor<'_>) -> Result<Option<Vec<u8>>, Error> {
    let total = cur.u64()?;
    if total == u64::MAX {
        return Ok(None);
    }
    // Never trust the announced size for allocation beyond what is actually present.
    let cap = if total == u64::MAX - 1 {
        0
    } else {
        (total as usize).min(cur.remaining())
    };
    let mut out = Vec::with_capacity(cap);
    loop {
        let n = cur.u32()? as usize;
        if n == 0 {
            break;
        }
        out.extend_from_slice(cur.take(n)?);
    }
    if total != u64::MAX - 1 && total != out.len() as u64 {
        return Err(cur.err("PLP chunks do not add up to the announced length"));
    }
    Ok(Some(out))
}

fn read_variant(cur: &mut Cursor<'_>) -> Result<Value, Error> {
    let total = cur.u32()? as usize;
    if total == 0 {
        return Ok(Value::Null);
    }
    let at = cur.offset();
    let mut v = cur.sub(total)?;
    let base = v.u8()?;
    let props = v.u8()? as usize;
    let mut p = v.sub(props)?;
    let (scale, collation) = match base {
        ty::TIMEN | ty::DATETIME2N | ty::DATETIMEOFFSETN => {
            let s = p.u8()?;
            if s > 7 {
                return Err(p.err("time scale above 7"));
            }
            (s, None)
        }
        ty::DECIMALN | ty::NUMERICN => {
            p.u8()?; // precision
            let s = p.u8()?;
            if s > 38 {
                return Err(p.err("decimal scale above 38"));
            }
            (s, None)
        }
        ty::BIGVARBIN | ty::BIGBINARY => {
            p.u16()?; // max length
            (0, None)
        }
        ty::BIGVARCHR | ty::BIGCHAR | ty::NVARCHAR | ty::NCHAR => {
            let c = Collation::from_bytes(p.array()?);
            p.u16()?; // max length
            (0, Some(c))
        }
        ty::GUID
        | ty::BIT
        | ty::INT1
        | ty::INT2
        | ty::INT4
        | ty::INT8
        | ty::DATETIME
        | ty::DATETIM4
        | ty::FLT4
        | ty::FLT8
        | ty::MONEY
        | ty::MONEY4
        | ty::DATEN => (0, None),
        _ => return Err(v.err_at(at, "invalid sql_variant base type")),
    };
    let data_at = v.offset();
    let data = v.take(v.remaining())?;
    // Fixed numeric base types must have exactly their size.
    let want = match base {
        ty::BIT | ty::INT1 => Some(1),
        ty::INT2 => Some(2),
        ty::INT4 | ty::FLT4 | ty::MONEY4 | ty::DATETIM4 => Some(4),
        ty::INT8 | ty::FLT8 | ty::MONEY | ty::DATETIME => Some(8),
        _ => None,
    };
    if want.is_some_and(|w| w != data.len()) {
        return Err(v.err_at(data_at, "sql_variant value has the wrong size"));
    }
    // go-mssqldb returns float64 for a variant real.
    let id = if base == ty::FLT4 { ty::FLTN } else { base };
    decode(id, scale, collation, data, false).map_err(|w| at_err(&v, data_at, w))
}

/// Days from 0001-01-01 to 1970-01-01.
const UNIX_DAYS_0001: i64 = -719_162;
/// Days from 1900-01-01 to 1970-01-01.
const UNIX_DAYS_1900: i64 = -25_567;
const NANOS_PER_DAY: i128 = 86_400_000_000_000;

/// A date-time from days since 1970-01-01 plus nanoseconds (may exceed one day).
fn make_datetime(unix_days: i64, nanos: i128) -> Result<DateTime, Decode> {
    let days = unix_days as i128 + nanos.div_euclid(NANOS_PER_DAY);
    let nod = nanos.rem_euclid(NANOS_PER_DAY) as u64;
    let date = i64::try_from(days)
        .ok()
        .and_then(|d| Date::from_unix_days(d).ok())
        .ok_or(Decode::Bad("date out of range"))?;
    let time = Time::from_nanos_of_day(nod).map_err(|_| Decode::Bad("invalid time"))?;
    Ok(date.at(time))
}

fn le_uint(b: &[u8]) -> u64 {
    b.iter().rev().fold(0u64, |acc, &x| (acc << 8) | x as u64)
}

/// Nanoseconds of a TIME(n) value.
fn time_nanos(scale: u8, b: &[u8]) -> Result<i128, Decode> {
    if !(3..=5).contains(&b.len()) {
        return Err(Decode::Bad("invalid time length"));
    }
    Ok(le_uint(b) as i128 * 10i128.pow(9 - scale as u32))
}

fn date_days(b: &[u8]) -> i64 {
    le_uint(b) as i64 + UNIX_DAYS_0001
}

fn decode_decimal(scale: u8, data: &[u8]) -> Result<Decimal, Decode> {
    if data.len() < 2 || data.len() > 17 {
        return Err(Decode::Bad("invalid decimal length"));
    }
    let negative = data[0] == 0;
    let mut mag: u128 = 0;
    for &b in data[1..].iter().rev() {
        mag = (mag << 8) | b as u128;
    }
    let mut scale = scale as u32;
    if scale > dl_decimal::MAX_SCALE {
        // Round half away from zero to the 28 places dl-decimal supports.
        let div = 10u128.pow(scale - dl_decimal::MAX_SCALE);
        let (q, r) = (mag / div, mag % div);
        mag = if r * 2 >= div { q + 1 } else { q };
        scale = dl_decimal::MAX_SCALE;
    }
    Decimal::from_parts(mag, negative && mag != 0, scale)
        .map_err(|_| Decode::Bad("decimal exceeds 38 digits"))
}

fn money(v: i64) -> Value {
    // |v| <= 2^63 < 10^38, so this always fits.
    Value::Decimal(Decimal::from_i128_scale(v as i128, 4))
}

/// Decodes the bytes of one non-NULL value of type `id`. `fixed` distinguishes the
/// fixed-length FLT4 (kept as f32) from FLTN (widened to f64, as go-mssqldb does).
fn decode(
    id: u8,
    scale: u8,
    collation: Option<Collation>,
    d: &[u8],
    fixed: bool,
) -> Result<Value, Decode> {
    let bad = Decode::Bad;
    let arr4 = |d: &[u8]| -> Result<[u8; 4], Decode> {
        d.try_into().map_err(|_| bad("wrong value length"))
    };
    let arr8 = |d: &[u8]| -> Result<[u8; 8], Decode> {
        d.try_into().map_err(|_| bad("wrong value length"))
    };
    Ok(match id {
        ty::INT1 => Value::I8(*d.first().ok_or(bad("empty value"))?),
        ty::BIT | ty::BITN => {
            if d.len() != 1 {
                return Err(bad("invalid bit length"));
            }
            Value::Bool(d[0] != 0)
        }
        ty::INT2 => Value::I16(i16::from_le_bytes(
            d.try_into().map_err(|_| bad("wrong value length"))?,
        )),
        ty::INT4 => Value::I32(i32::from_le_bytes(arr4(d)?)),
        ty::INT8 => Value::I64(i64::from_le_bytes(arr8(d)?)),
        ty::INTN => match d.len() {
            1 => Value::I8(d[0]),
            2 => Value::I16(i16::from_le_bytes([d[0], d[1]])),
            4 => Value::I32(i32::from_le_bytes(arr4(d)?)),
            8 => Value::I64(i64::from_le_bytes(arr8(d)?)),
            _ => return Err(bad("invalid int length")),
        },
        ty::FLT4 if fixed => Value::F32(f32::from_le_bytes(arr4(d)?)),
        ty::FLT4 => Value::F64(f32::from_le_bytes(arr4(d)?) as f64),
        ty::FLT8 => Value::F64(f64::from_le_bytes(arr8(d)?)),
        ty::FLTN => match d.len() {
            4 => Value::F64(f32::from_le_bytes(arr4(d)?) as f64),
            8 => Value::F64(f64::from_le_bytes(arr8(d)?)),
            _ => return Err(bad("invalid float length")),
        },
        ty::MONEY4 => money(i32::from_le_bytes(arr4(d)?) as i64),
        ty::MONEY => {
            let b = arr8(d)?;
            let hi = i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as i64;
            let lo = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as i64;
            money((hi << 32) | lo)
        }
        ty::MONEYN => match d.len() {
            4 => return decode(ty::MONEY4, scale, collation, d, fixed),
            8 => return decode(ty::MONEY, scale, collation, d, fixed),
            _ => return Err(bad("invalid money length")),
        },
        ty::DECIMAL | ty::NUMERIC | ty::DECIMALN | ty::NUMERICN => {
            Value::Decimal(decode_decimal(scale, d)?)
        }
        ty::DATETIM4 => {
            let b = arr4(d)?;
            let days = u16::from_le_bytes([b[0], b[1]]) as i64;
            let mins = u16::from_le_bytes([b[2], b[3]]) as i128;
            Value::DateTime(make_datetime(UNIX_DAYS_1900 + days, mins * 60_000_000_000)?)
        }
        ty::DATETIME => {
            let b = arr8(d)?;
            let days = i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as i64;
            let ticks = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
            let secs = (ticks / 300) as i128;
            // go-mssqldb: int(math.Trunc(float64(ths)/0.3+0.5)) * 1e6 (milliseconds).
            let ms = ((ticks % 300) as f64 / 0.3 + 0.5).trunc() as i128;
            Value::DateTime(make_datetime(
                UNIX_DAYS_1900 + days,
                secs * 1_000_000_000 + ms * 1_000_000,
            )?)
        }
        ty::DATETIMN => match d.len() {
            4 => return decode(ty::DATETIM4, scale, collation, d, fixed),
            8 => return decode(ty::DATETIME, scale, collation, d, fixed),
            _ => return Err(bad("invalid datetime length")),
        },
        ty::DATEN => {
            if d.len() != 3 {
                return Err(bad("invalid date length"));
            }
            Value::Date(make_datetime(date_days(d), 0)?.date())
        }
        ty::TIMEN => {
            let n = time_nanos(scale, d)?;
            if n >= NANOS_PER_DAY {
                return Err(bad("time of day out of range"));
            }
            Value::Time(Time::from_nanos_of_day(n as u64).map_err(|_| bad("invalid time"))?)
        }
        ty::DATETIME2N => {
            if d.len() < 6 {
                return Err(bad("invalid datetime2 length"));
            }
            let (t, dd) = d.split_at(d.len() - 3);
            Value::DateTime(make_datetime(date_days(dd), time_nanos(scale, t)?)?)
        }
        ty::DATETIMEOFFSETN => {
            if d.len() < 8 {
                return Err(bad("invalid datetimeoffset length"));
            }
            let (rest, off) = d.split_at(d.len() - 2);
            let (t, dd) = rest.split_at(rest.len() - 3);
            let utc = make_datetime(date_days(dd), time_nanos(scale, t)?)?;
            let minutes = i16::from_le_bytes([off[0], off[1]]) as i32;
            let offset =
                Offset::from_seconds(minutes * 60).map_err(|_| bad("invalid time zone offset"))?;
            let v = OffsetDateTime::new(utc, Offset::UTC)
                .checked_to_offset(offset)
                .ok_or(bad("datetimeoffset out of range"))?;
            Value::DateTimeOffset(v)
        }
        ty::GUID => {
            let b: [u8; 16] = d
                .try_into()
                .map_err(|_| bad("invalid uniqueidentifier length"))?;
            Value::Uuid(Uuid::from_bytes_le(b))
        }
        ty::CHAR | ty::VARCHAR | ty::BIGCHAR | ty::BIGVARCHR | ty::TEXT => {
            let c = collation.unwrap_or_default();
            match c.decode(d) {
                Ok(s) => Value::String(s),
                Err(Error::UnsupportedCodePage { code_page }) => {
                    return Err(Decode::CodePage(code_page));
                }
                Err(_) => return Err(bad("invalid character data")),
            }
        }
        ty::NCHAR | ty::NVARCHAR | ty::NTEXT | ty::XML => {
            Value::String(utf16le(d).map_err(|_| bad("odd byte count in UTF-16 data"))?)
        }
        ty::BINARY | ty::VARBINARY | ty::BIGBINARY | ty::BIGVARBIN | ty::IMAGE | ty::UDT => {
            Value::Bytes(d.to_vec())
        }
        _ => return Err(bad("unsupported data type")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ti(bytes: &[u8]) -> TypeInfo {
        let mut c = Cursor::new(bytes);
        let t = TypeInfo::read(&mut c, true).unwrap();
        assert!(c.is_empty(), "type info not fully consumed");
        t
    }

    fn val(t: &TypeInfo, bytes: &[u8]) -> Value {
        let mut c = Cursor::new(bytes);
        let v = t.read_value(&mut c).unwrap();
        assert!(c.is_empty(), "value not fully consumed");
        v
    }

    fn dt(s: &str) -> DateTime {
        s.parse().unwrap()
    }

    #[test]
    fn fixed_integers_and_bits() {
        assert_eq!(val(&ti(&[ty::INT1]), &[200]), Value::I8(200));
        assert_eq!(val(&ti(&[ty::INT2]), &[0xFE, 0xFF]), Value::I16(-2));
        assert_eq!(val(&ti(&[ty::INT4]), &[0x2A, 0, 0, 0]), Value::I32(42));
        assert_eq!(val(&ti(&[ty::INT8]), &[0xFF; 8]), Value::I64(-1));
        assert_eq!(val(&ti(&[ty::BIT]), &[1]), Value::Bool(true));
        assert_eq!(val(&ti(&[ty::NULL]), &[]), Value::Null);
    }

    #[test]
    fn nullable_integers_floats_bits() {
        let intn = ti(&[ty::INTN, 4]);
        assert_eq!(val(&intn, &[4, 0x39, 0x30, 0, 0]), Value::I32(12345));
        assert_eq!(val(&intn, &[0]), Value::Null);
        assert_eq!(
            val(&intn, &[8, 1, 0, 0, 0, 0, 0, 0, 0x80]),
            Value::I64(i64::MIN + 1)
        );
        assert_eq!(val(&intn, &[1, 7]), Value::I8(7));
        assert_eq!(val(&intn, &[2, 0xFF, 0x7F]), Value::I16(i16::MAX));
        let mut c = Cursor::new(&[3, 1, 2, 3]);
        assert!(matches!(
            intn.read_value(&mut c),
            Err(Error::Malformed { offset: 1, .. })
        ));

        assert_eq!(val(&ti(&[ty::BITN, 1]), &[1, 0]), Value::Bool(false));
        assert_eq!(val(&ti(&[ty::BITN, 1]), &[0]), Value::Null);
        let fltn = ti(&[ty::FLTN, 8]);
        assert_eq!(
            val(&fltn, &[8, 0, 0, 0, 0, 0, 0, 0xF8, 0x3F]),
            Value::F64(1.5)
        );
        assert_eq!(
            val(&fltn, &[4, 0xCD, 0xCC, 0xCC, 0x3D]),
            Value::F64(0.1f32 as f64)
        );
        assert_eq!(
            val(&ti(&[ty::FLT4]), &[0xCD, 0xCC, 0xCC, 0x3D]),
            Value::F32(0.1)
        );
        assert_eq!(
            val(&ti(&[ty::FLT8]), &1e300f64.to_le_bytes()),
            Value::F64(1e300)
        );
    }

    #[test]
    fn money_and_decimal() {
        // 12.3456 = 123456 ten-thousandths; MONEY stores hi dword first.
        let m: i64 = -1_234_567_890_123;
        let hi = ((m >> 32) as i32).to_le_bytes();
        let lo = (m as u32).to_le_bytes();
        let mut b = Vec::from(hi);
        b.extend_from_slice(&lo);
        let money = Value::Decimal(Decimal::new(m as i128, 4).unwrap());
        assert_eq!(val(&ti(&[ty::MONEY]), &b), money);
        let mut bn = vec![8];
        bn.extend_from_slice(&b);
        assert_eq!(val(&ti(&[ty::MONEYN, 8]), &bn), money);
        assert_eq!(
            val(&ti(&[ty::MONEY4]), &(-50_000i32).to_le_bytes()),
            Value::Decimal(Decimal::new(-50_000, 4).unwrap())
        );
        assert_eq!(val(&ti(&[ty::MONEYN, 4]), &[0]), Value::Null);

        let dec = ti(&[ty::DECIMALN, 17, 38, 4]);
        assert_eq!((dec.precision, dec.scale), (38, 4));
        assert_eq!(
            val(&dec, &[5, 1, 0x40, 0xE2, 0x01, 0x00]),
            Value::Decimal(Decimal::new(123_456, 4).unwrap())
        );
        assert_eq!(
            val(&dec, &[5, 0, 0x40, 0xE2, 0x01, 0x00]),
            Value::Decimal(Decimal::new(-123_456, 4).unwrap())
        );
        // Largest decimal(38,0): 10^38 - 1.
        let max = 10u128.pow(38) - 1;
        let mut b = vec![17, 1];
        b.extend_from_slice(&max.to_le_bytes());
        assert_eq!(
            val(&ti(&[ty::NUMERICN, 17, 38, 0]), &b),
            Value::Decimal(Decimal::MAX)
        );
        // 10^38 does not fit.
        let mut b = vec![17, 1];
        b.extend_from_slice(&(max + 1).to_le_bytes());
        assert!(
            ti(&[ty::DECIMALN, 17, 38, 0])
                .read_value(&mut Cursor::new(&b))
                .is_err()
        );
        // Scale 30 is rounded to 28: 0.000...0015 (30 places) -> 0.000...002 (28).
        let mut b = vec![5, 1];
        b.extend_from_slice(&150u32.to_le_bytes());
        assert_eq!(
            val(&ti(&[ty::DECIMALN, 5, 9, 30]), &b),
            Value::Decimal(Decimal::new(2, 28).unwrap())
        );
        // Negative zero is zero.
        assert_eq!(
            val(&dec, &[5, 0, 0, 0, 0, 0]),
            Value::Decimal(Decimal::ZERO)
        );
    }

    #[test]
    fn datetime_family() {
        // datetime 2024-05-01 10:20:30.997: days since 1900 = 45411, ticks = secs*300 + 299.
        let days = 45_411i32;
        let ticks = (10 * 3600 + 20 * 60 + 30) * 300 + 299u32;
        let mut b = Vec::from(days.to_le_bytes());
        b.extend_from_slice(&ticks.to_le_bytes());
        assert_eq!(
            val(&ti(&[ty::DATETIME]), &b),
            Value::DateTime(dt("2024-05-01T10:20:30.997"))
        );
        // 1/300 s and 2/300 s round to 3 ms and 7 ms (go-mssqldb).
        for (t, ms) in [(1u32, "003"), (2, "007"), (150, "500")] {
            let mut b = Vec::from(0i32.to_le_bytes());
            b.extend_from_slice(&t.to_le_bytes());
            assert_eq!(
                val(&ti(&[ty::DATETIME]), &b),
                Value::DateTime(dt(&format!("1900-01-01T00:00:00.{ms}")))
            );
        }
        // 1753-01-01 is days = -53690.
        let mut b = vec![8];
        b.extend_from_slice(&(-53_690i32).to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            val(&ti(&[ty::DATETIMN, 8]), &b),
            Value::DateTime(dt("1753-01-01T00:00:00"))
        );
        // smalldatetime 2079-06-06 23:59.
        let mut b = Vec::from(65_535u16.to_le_bytes());
        b.extend_from_slice(&1439u16.to_le_bytes());
        assert_eq!(
            val(&ti(&[ty::DATETIM4]), &b),
            Value::DateTime(dt("2079-06-06T23:59:00"))
        );

        // date 0001-01-01 = 0, 9999-12-31 = 3652058.
        let date = ti(&[ty::DATEN]);
        assert_eq!(
            val(&date, &[3, 0, 0, 0]),
            Value::Date("0001-01-01".parse().unwrap())
        );
        let n = 3_652_058u32.to_le_bytes();
        assert_eq!(
            val(&date, &[3, n[0], n[1], n[2]]),
            Value::Date("9999-12-31".parse().unwrap())
        );
        assert_eq!(val(&date, &[0]), Value::Null);

        // time(7) 23:59:59.9999999 = 863999999999 units of 100 ns.
        let time7 = ti(&[ty::TIMEN, 7]);
        assert_eq!(time7.size, 5);
        let u = 863_999_999_999u64.to_le_bytes();
        assert_eq!(
            val(&time7, &[5, u[0], u[1], u[2], u[3], u[4]]),
            Value::Time("23:59:59.9999999".parse().unwrap())
        );
        // time(0) 01:00:00 = 3600.
        assert_eq!(
            val(&ti(&[ty::TIMEN, 0]), &[3, 0x10, 0x0E, 0]),
            Value::Time("01:00:00".parse().unwrap())
        );
        // time(7) past midnight is rejected.
        let u = 864_000_000_000u64.to_le_bytes();
        assert!(
            time7
                .read_value(&mut Cursor::new(&[5, u[0], u[1], u[2], u[3], u[4]]))
                .is_err()
        );

        // datetime2(3) 2024-05-01 10:20:30.123: time = 37230123 ms, date = 739006 days.
        let dt2 = ti(&[ty::DATETIME2N, 3]);
        assert_eq!(dt2.size, 7);
        let t = 37_230_123u32.to_le_bytes();
        let d = 739_006u32.to_le_bytes();
        let v = val(&dt2, &[7, t[0], t[1], t[2], t[3], d[0], d[1], d[2]]);
        assert_eq!(v, Value::DateTime(dt("2024-05-01T10:20:30.123")));

        // datetimeoffset(0) 2024-05-01 03:20:30Z at +07:00 -> local 10:20:30+07:00.
        let dto = ti(&[ty::DATETIMEOFFSETN, 0]);
        let t = (3 * 3600 + 20 * 60 + 30u32).to_le_bytes();
        let off = 420i16.to_le_bytes();
        let v = val(
            &dto,
            &[8, t[0], t[1], t[2], d[0], d[1], d[2], off[0], off[1]],
        );
        let Value::DateTimeOffset(o) = v else {
            panic!()
        };
        assert_eq!(o.to_rfc3339(), "2024-05-01T10:20:30+07:00");
        assert!(TypeInfo::read(&mut Cursor::new(&[ty::TIMEN, 8]), true).is_err());
    }

    #[test]
    fn guid_and_binary() {
        let g = ti(&[ty::GUID, 16]);
        let mut b = vec![16];
        b.extend_from_slice(&[
            0xFF, 0x19, 0x96, 0x6F, 0x86, 0x8B, 0x11, 0xD0, 0xB4, 0x2D, 0x00, 0xC0, 0x4F, 0xC9,
            0x64, 0xFF,
        ]);
        let u: Uuid = "6f9619ff-8b86-d011-b42d-00c04fc964ff".parse().unwrap();
        assert_eq!(val(&g, &b), Value::Uuid(u));
        assert!(g.read_value(&mut Cursor::new(&[4, 1, 2, 3, 4])).is_err());

        let vb = ti(&[ty::BIGVARBIN, 0x40, 0x1F]);
        assert_eq!(vb.size, 8000);
        assert_eq!(
            val(&vb, &[2, 0, 0xDE, 0xAD]),
            Value::Bytes(vec![0xDE, 0xAD])
        );
        assert_eq!(val(&vb, &[0xFF, 0xFF]), Value::Null);
        assert_eq!(val(&vb, &[0, 0]), Value::Bytes(vec![]));
        assert_eq!(
            val(&ti(&[ty::BIGBINARY, 2, 0]), &[2, 0, 1, 2]),
            Value::Bytes(vec![1, 2])
        );
    }

    #[test]
    fn character_types() {
        // varchar(10) COLLATE SQL_Latin1_General_CP1_CI_AS.
        let vc = ti(&[ty::BIGVARCHR, 10, 0, 0x09, 0x04, 0xD0, 0x00, 52]);
        assert_eq!(vc.collation.unwrap().code_page(), Some(1252));
        assert_eq!(
            val(&vc, &[3, 0, b'c', 0xE9, 0x80]),
            Value::String("c\u{e9}\u{20ac}".into())
        );
        // varchar COLLATE Vietnamese_CI_AS.
        let vn = ti(&[ty::BIGCHAR, 4, 0, 0x2A, 0x04, 0xD0, 0x00, 0]);
        assert_eq!(
            val(&vn, &[2, 0, 0xD0, 0xF5]),
            Value::String("\u{0110}\u{01A1}".into())
        );
        // Japanese collation: unsupported code page error.
        let jp = ti(&[ty::BIGVARCHR, 10, 0, 0x11, 0x04, 0xD0, 0x00, 0]);
        assert!(matches!(
            jp.read_value(&mut Cursor::new(&[1, 0, 0x41])),
            Err(Error::UnsupportedCodePage { code_page: 932 })
        ));
        let nv = ti(&[ty::NVARCHAR, 0x40, 0x1F, 0x09, 0x04, 0xD0, 0x00, 52]);
        assert_eq!(
            val(&nv, &[4, 0, 0x54, 0xEA, 0x63, 0]),
            Value::String("\u{EA54}c".into())
        );
        assert!(nv.read_value(&mut Cursor::new(&[3, 0, 1, 2, 3])).is_err());
        assert_eq!(
            val(&ti(&[ty::NCHAR, 4, 0, 0, 0, 0, 0, 0]), &[2, 0, 0x41, 0]),
            Value::String("A".into())
        );
    }

    #[test]
    fn plp_values() {
        let max = ti(&[ty::NVARCHAR, 0xFF, 0xFF, 0x09, 0x04, 0xD0, 0x00, 52]);
        assert_eq!(max.len, Len::Plp);
        // Known length 6, two chunks.
        let mut b = Vec::from(6u64.to_le_bytes());
        b.extend_from_slice(&[
            2, 0, 0, 0, b'h', 0, 4, 0, 0, 0, b'i', 0, b'!', 0, 0, 0, 0, 0,
        ]);
        assert_eq!(val(&max, &b), Value::String("hi!".into()));
        // Unknown length.
        let mut b = Vec::from((u64::MAX - 1).to_le_bytes());
        b.extend_from_slice(&[2, 0, 0, 0, b'x', 0, 0, 0, 0, 0]);
        assert_eq!(val(&max, &b), Value::String("x".into()));
        // NULL.
        assert_eq!(val(&max, &u64::MAX.to_le_bytes()), Value::Null);
        // Empty string: length 0, terminator.
        let mut b = Vec::from(0u64.to_le_bytes());
        b.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(val(&max, &b), Value::String(String::new()));
        // Length mismatch and truncated chunk.
        let mut b = Vec::from(4u64.to_le_bytes());
        b.extend_from_slice(&[2, 0, 0, 0, b'x', 0, 0, 0, 0, 0]);
        assert!(max.read_value(&mut Cursor::new(&b)).is_err());
        let mut b = Vec::from(4u64.to_le_bytes());
        b.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0x7F, 1]);
        assert!(max.read_value(&mut Cursor::new(&b)).is_err());

        let vbmax = ti(&[ty::BIGVARBIN, 0xFF, 0xFF]);
        let mut b = Vec::from(1u64.to_le_bytes());
        b.extend_from_slice(&[1, 0, 0, 0, 9, 0, 0, 0, 0]);
        assert_eq!(val(&vbmax, &b), Value::Bytes(vec![9]));

        // xml with a schema collection.
        let mut x = vec![ty::XML, 1, 1, b'd', 0, 1, b's', 0, 1, 0, b'c', 0];
        let xml = ti(&x);
        let mut b = Vec::from(4u64.to_le_bytes());
        b.extend_from_slice(&[4, 0, 0, 0, b'<', 0, b'a', 0, 0, 0, 0, 0]);
        assert_eq!(val(&xml, &b), Value::String("<a".into()));
        x.truncate(4);
        assert!(TypeInfo::read(&mut Cursor::new(&x), true).is_err());

        // UDT (e.g. hierarchyid).
        let mut u = vec![ty::UDT, 0x92, 0x03, 0, 0, 3];
        u.extend_from_slice(&[b's', 0, b'y', 0, b's', 0]);
        u.extend_from_slice(&[0, 0]);
        let udt = ti(&u);
        assert_eq!(udt.udt_name, "sys");
        let mut b = Vec::from(2u64.to_le_bytes());
        b.extend_from_slice(&[2, 0, 0, 0, 0x58, 0x40, 0, 0, 0, 0]);
        assert_eq!(val(&udt, &b), Value::Bytes(vec![0x58, 0x40]));
    }

    #[test]
    fn text_types() {
        // text COLLATE SQL_Latin1 with a 1-part table name "t".
        let t = ti(&[
            ty::TEXT,
            0xFF,
            0xFF,
            0xFF,
            0x7F,
            0x09,
            0x04,
            0xD0,
            0x00,
            52,
            1,
            1,
            0,
            b't',
            0,
        ]);
        let mut b = vec![16];
        b.extend_from_slice(&[0xAA; 16]);
        b.extend_from_slice(&[0; 8]);
        b.extend_from_slice(&2i32.to_le_bytes());
        b.extend_from_slice(&[b'o', 0xE9]);
        assert_eq!(val(&t, &b), Value::String("o\u{e9}".into()));
        assert_eq!(val(&t, &[0]), Value::Null);
        let n = ti(&[
            ty::NTEXT,
            0xFF,
            0xFF,
            0xFF,
            0x7F,
            0x09,
            0x04,
            0xD0,
            0x00,
            52,
            0,
        ]);
        let mut b = vec![1, 0xAA];
        b.extend_from_slice(&[0; 8]);
        b.extend_from_slice(&2i32.to_le_bytes());
        b.extend_from_slice(&[b'z', 0]);
        assert_eq!(val(&n, &b), Value::String("z".into()));
        let i = ti(&[ty::IMAGE, 0xFF, 0xFF, 0xFF, 0x7F, 0]);
        let mut b = vec![1, 0xAA];
        b.extend_from_slice(&[0; 8]);
        b.extend_from_slice(&(-1i32).to_le_bytes());
        assert_eq!(val(&i, &b), Value::Null);
        let mut b = vec![1, 0xAA];
        b.extend_from_slice(&[0; 8]);
        b.extend_from_slice(&(-5i32).to_le_bytes());
        assert!(i.read_value(&mut Cursor::new(&b)).is_err());
    }

    #[test]
    fn sql_variant_values() {
        let v = ti(&[ty::SSVARIANT, 0x10, 0x1F, 0, 0]);
        // int 7
        assert_eq!(
            val(&v, &[6, 0, 0, 0, ty::INT4, 0, 7, 0, 0, 0]),
            Value::I32(7)
        );
        // NULL
        assert_eq!(val(&v, &[0, 0, 0, 0]), Value::Null);
        // nvarchar 'A' with collation + max length
        let b = [
            11,
            0,
            0,
            0,
            ty::NVARCHAR,
            7,
            9,
            4,
            0xD0,
            0,
            52,
            8,
            0,
            0x41,
            0,
        ];
        assert_eq!(val(&v, &b), Value::String("A".into()));
        // varchar under Vietnamese collation
        let b = [
            10,
            0,
            0,
            0,
            ty::BIGVARCHR,
            7,
            0x2A,
            4,
            0xD0,
            0,
            0,
            8,
            0,
            0xF5,
        ];
        assert_eq!(val(&v, &b), Value::String("\u{01A1}".into()));
        // decimal(5,2) 1.23
        let b = [9, 0, 0, 0, ty::DECIMALN, 2, 5, 2, 1, 123, 0, 0, 0];
        assert_eq!(val(&v, &b), Value::Decimal(Decimal::new(123, 2).unwrap()));
        // datetime2(7)
        let b = [11, 0, 0, 0, ty::DATETIME2N, 1, 7, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(val(&v, &b), Value::DateTime(dt("0001-01-01T00:00:00")));
        // real -> f64 like go-mssqldb
        let f = 0.5f32.to_le_bytes();
        assert_eq!(
            val(&v, &[6, 0, 0, 0, ty::FLT4, 0, f[0], f[1], f[2], f[3]]),
            Value::F64(0.5)
        );
        // bad base type / wrong fixed size / props past the end
        assert!(
            v.read_value(&mut Cursor::new(&[3, 0, 0, 0, 0xE0, 0, 0]))
                .is_err()
        );
        assert!(
            v.read_value(&mut Cursor::new(&[4, 0, 0, 0, ty::INT4, 0, 1, 2]))
                .is_err()
        );
        assert!(
            v.read_value(&mut Cursor::new(&[3, 0, 0, 0, ty::INT4, 9, 1]))
                .is_err()
        );
    }

    #[test]
    fn unknown_type_is_error() {
        let mut c = Cursor::new(&[0x99]);
        c.token = 0x81;
        assert!(matches!(
            TypeInfo::read(&mut c, true),
            Err(Error::Malformed {
                token: 0x81,
                offset: 1,
                ..
            })
        ));
    }
}
