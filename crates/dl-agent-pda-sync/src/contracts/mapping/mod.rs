//! Catalog-driven vendor row mapper (port of `src/MM.PdaOrder.Sync.Mapping`): vendor row
//! (column name to raw ADO value) to canonical row, row hash and Postgres row.

mod mapper;
mod plan;
pub mod tcvn3;
pub mod transforms;

pub use mapper::{CanonicalRow, CatalogRowMapper, MappedRow};

use dl_decimal::Decimal;
use dl_uuid::Uuid;

use super::map::OrderedMap;
use super::time::{
    DateTime, DateTimeKind, DateTimeOffset, TICKS_PER_SECOND, TimeZone, ticks_from_civil,
};

/// JSON column that receives vendor columns with no PG home.
pub const EXTRA_COLUMN: &str = "src_extra";

/// A raw column value as `SqlDataReader.GetValue` returns it (the C# `object?`).
#[derive(Debug, Clone, PartialEq)]
pub enum RawValue {
    /// `null` / `DBNull`.
    Null,
    /// `bool` (`bit`).
    Bool(bool),
    /// `byte` (`tinyint`).
    U8(u8),
    /// `sbyte`.
    I8(i8),
    /// `short` (`smallint`).
    I16(i16),
    /// `ushort`.
    U16(u16),
    /// `int`.
    I32(i32),
    /// `uint`.
    U32(u32),
    /// `long` (`bigint`).
    I64(i64),
    /// `ulong`.
    U64(u64),
    /// `float` (`real`).
    F32(f32),
    /// `double` (`float`).
    F64(f64),
    /// `decimal` (`decimal`, `numeric`, `money`).
    Decimal(Decimal),
    /// `string`.
    String(String),
    /// `byte[]`.
    Bytes(Vec<u8>),
    /// `Guid`.
    Guid(Uuid),
    /// `DateTime` (`datetime`, `date`, ...; kind Unspecified from SQL Server).
    DateTime(DateTime),
    /// `DateTimeOffset`.
    DateTimeOffset(DateTimeOffset),
    /// `TimeSpan` (`time`) in ticks.
    TimeSpan(i64),
}

impl RawValue {
    /// The .NET type name used in error messages (`GetType().Name`).
    pub fn type_name(&self) -> &'static str {
        match self {
            RawValue::Null => "DBNull",
            RawValue::Bool(_) => "Boolean",
            RawValue::U8(_) => "Byte",
            RawValue::I8(_) => "SByte",
            RawValue::I16(_) => "Int16",
            RawValue::U16(_) => "UInt16",
            RawValue::I32(_) => "Int32",
            RawValue::U32(_) => "UInt32",
            RawValue::I64(_) => "Int64",
            RawValue::U64(_) => "UInt64",
            RawValue::F32(_) => "Single",
            RawValue::F64(_) => "Double",
            RawValue::Decimal(_) => "Decimal",
            RawValue::String(_) => "String",
            RawValue::Bytes(_) => "Byte[]",
            RawValue::Guid(_) => "Guid",
            RawValue::DateTime(_) => "DateTime",
            RawValue::DateTimeOffset(_) => "DateTimeOffset",
            RawValue::TimeSpan(_) => "TimeSpan",
        }
    }

    /// The value `SqlDataReader.GetValue` gives for a dl-tds column value: `date` and
    /// `datetime*` as an Unspecified `DateTime`, `time` as a `TimeSpan`, `tinyint` as
    /// `byte`. Out-of-range dates read as NULL.
    pub fn from_tds(v: &dl_tds::Value) -> RawValue {
        use dl_tds::Value as V;
        let dt = |d: dl_time::DateTime| {
            ticks_from_civil(
                d.year(),
                u32::from(d.month().number()),
                u32::from(d.day()),
                u32::from(d.hour()),
                u32::from(d.minute()),
                u32::from(d.second()),
            )
            .and_then(|t| {
                DateTime::from_ticks(
                    t + i64::from(d.nanosecond() / 100),
                    DateTimeKind::Unspecified,
                )
            })
            .map_or(RawValue::Null, RawValue::DateTime)
        };
        match v {
            V::Null => RawValue::Null,
            V::Bool(b) => RawValue::Bool(*b),
            V::I8(n) => RawValue::U8(*n),
            V::I16(n) => RawValue::I16(*n),
            V::I32(n) => RawValue::I32(*n),
            V::I64(n) => RawValue::I64(*n),
            V::F32(f) => RawValue::F32(*f),
            V::F64(f) => RawValue::F64(*f),
            V::Decimal(d) => RawValue::Decimal(*d),
            V::String(s) => RawValue::String(s.clone()),
            V::Bytes(b) => RawValue::Bytes(b.clone()),
            V::Uuid(u) => RawValue::Guid(*u),
            V::Date(d) => dt(d.midnight()),
            V::Time(t) => RawValue::TimeSpan((t.nanos_of_day() / 100) as i64),
            V::DateTime(d) => dt(*d),
            V::DateTimeOffset(o) => {
                DateTimeOffset::from_unix_nanos(o.unix_nanos(), o.offset().whole_minutes())
                    .map_or(RawValue::Null, RawValue::DateTimeOffset)
            }
            // dl-tds may add value kinds; none exists today.
            _ => RawValue::Null,
        }
    }
}

impl From<&str> for RawValue {
    fn from(s: &str) -> Self {
        RawValue::String(s.to_owned())
    }
}

impl From<i32> for RawValue {
    fn from(n: i32) -> Self {
        RawValue::I32(n)
    }
}

impl From<i64> for RawValue {
    fn from(n: i64) -> Self {
        RawValue::I64(n)
    }
}

impl From<bool> for RawValue {
    fn from(b: bool) -> Self {
        RawValue::Bool(b)
    }
}

impl From<Decimal> for RawValue {
    fn from(d: Decimal) -> Self {
        RawValue::Decimal(d)
    }
}

impl From<DateTime> for RawValue {
    fn from(d: DateTime) -> Self {
        RawValue::DateTime(d)
    }
}

impl From<Vec<u8>> for RawValue {
    fn from(b: Vec<u8>) -> Self {
        RawValue::Bytes(b)
    }
}

/// A vendor row: column name to raw value, in reader order.
pub type VendorRow = OrderedMap<RawValue>;

/// Why a row cannot be mapped (the C# mapping exceptions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingError {
    /// `UnknownTableException`: the table is not in the catalog.
    UnknownTable {
        /// Table.
        table: String,
    },
    /// `NotMappableException`: skip/unclassified, or no PG target where one is needed.
    NotMappable {
        /// Table.
        table: String,
        /// Reason.
        reason: String,
    },
    /// `MissingKeyException`: a key column is absent or null.
    MissingKey {
        /// Table.
        table: String,
        /// Column.
        column: String,
    },
    /// `ValueFormatException`: a value the transform cannot read (may quote the value).
    ValueFormat {
        /// Column.
        column: String,
        /// Message.
        message: String,
    },
}

impl std::fmt::Display for MappingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MappingError::UnknownTable { table } => {
                write!(f, "Table '{table}' is not in sync-catalog.json.")
            }
            MappingError::NotMappable { table, reason } => {
                write!(f, "Table '{table}' cannot be mapped: {reason}.")
            }
            MappingError::MissingKey { table, column } => {
                write!(
                    f,
                    "Table '{table}': key column '{column}' is missing or null."
                )
            }
            MappingError::ValueFormat { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for MappingError {}

/// Per-tenant mapper configuration (`MapperOptions`).
pub struct MapperOptions {
    /// Zone the vendor's naive datetimes are expressed in.
    pub site_time_zone: TimeZone,
    /// Whether a PG target has the [`EXTRA_COLUMN`]; when false unmapped columns are
    /// dropped and listed.
    pub target_has_extra: Box<dyn Fn(&str) -> bool + Send + Sync>,
    /// Explicit vendor to PG renames per vendor table (table matched ignoring case),
    /// applied before snake_case.
    pub column_overrides: Vec<(String, Vec<(String, String)>)>,
}

impl std::fmt::Debug for MapperOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MapperOptions")
            .field("site_time_zone", &self.site_time_zone)
            .field("column_overrides", &self.column_overrides)
            .finish_non_exhaustive()
    }
}

impl MapperOptions {
    /// Options for a site zone: every target has `src_extra`, no overrides.
    pub fn new(site_time_zone: TimeZone) -> MapperOptions {
        MapperOptions {
            site_time_zone,
            target_has_extra: Box::new(|_| true),
            column_overrides: Vec::new(),
        }
    }
}

/// Ticks per second, re-exported for callers building `RawValue::TimeSpan`.
pub const TIME_SPAN_TICKS_PER_SECOND: i64 = TICKS_PER_SECOND;
