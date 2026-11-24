//! The read-only view of the vendor database (`Source/ICaptureSource.cs`,
//! `Source/SourceSchemaFault.cs`).

use super::error::Result;
use super::util::DotNetDict;
use crate::contracts::mapping::RawValue;
use crate::contracts::time::DateTime;
use crate::util::ctx::Ctx;

pub use crate::contracts::mapping::VendorRow;

/// Read-only view of the vendor database. The only operations are existence checks and
/// SELECTs; there is deliberately no write method, so a capture bug cannot touch vendor
/// tables.
pub trait CaptureSource: Send + Sync {
    /// True when the (dbo) table exists on this site. Sites differ: dormant modules may
    /// be absent.
    fn table_exists(&self, table: &str, ctx: &Ctx) -> Result<bool>;

    /// Runs one bounded SELECT and returns the rows (column name to raw value; look
    /// columns up ignoring case).
    fn query(&self, query: &SourceQuery, ctx: &Ctx) -> Result<Vec<VendorRow>>;

    /// A cheap change signature per check of an open-check table (one small row per
    /// check, not the check's rows), keyed by the invariant string of the check value, or
    /// `None` when the source cannot compute one: the caller then reads every row
    /// (RT-0759, ADR 0153). Rows with a NULL check column are not reported.
    fn check_signatures(
        &self,
        table: &str,
        check_column: &str,
        ctx: &Ctx,
    ) -> Result<Option<DotNetDict<CheckSignature>>> {
        let _ = (table, check_column, ctx);
        Ok(None)
    }
}

/// The raw check value (to filter a re-read with) and an opaque signature that changes
/// when any row of the check changes.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckSignature {
    /// The check column value.
    pub value: RawValue,
    /// The signature.
    pub signature: String,
}

/// One SELECT. All filters are ANDed: equality [`SourceQuery::where_`], a half-open date
/// range, and a keyset position [`SourceQuery::after`] (exclusive) over
/// [`SourceQuery::order_by`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SourceQuery {
    /// The dbo table.
    pub table: String,
    /// Explicit projection; `None` selects every column except `exclude_columns`.
    pub columns: Option<Vec<String>>,
    /// Columns that must not even be read (never-sync credentials, PAN).
    pub exclude_columns: Option<Vec<String>>,
    /// Character columns to fetch as raw stored bytes (TCVN3 text is decoded by the
    /// mapper from bytes).
    pub bytes_columns: Option<Vec<String>>,
    /// Keyset order.
    pub order_by: Vec<String>,
    /// Descending order.
    pub descending: bool,
    /// Last key already seen, aligned with `order_by`; rows strictly after it.
    pub after: Option<Vec<RawValue>>,
    /// `TOP (n)`.
    pub limit: Option<i32>,
    /// Equality filters in order ([`RawValue::Null`] means IS NULL).
    pub where_: Option<Vec<(String, RawValue)>>,
    /// Date column for `range_from` (inclusive) / `range_to` (exclusive).
    pub range_column: Option<String>,
    /// Inclusive lower bound.
    pub range_from: Option<DateTime>,
    /// Exclusive upper bound.
    pub range_to: Option<DateTime>,
    /// When true rows whose range column is NULL also match (so nothing falls between
    /// windows).
    pub range_includes_null: bool,
}

impl SourceQuery {
    /// A query of `table` with no filters.
    pub fn new(table: &str) -> SourceQuery {
        SourceQuery {
            table: table.to_owned(),
            ..SourceQuery::default()
        }
    }
}

/// `VendorRow.GetValueOrDefault(column)`: the value of a column ignoring case, `None`
/// when the row has no such column. SQL NULL is `Some(RawValue::Null)`.
pub fn column<'a>(row: &'a VendorRow, name: &str) -> Option<&'a RawValue> {
    row.get_ignore_case(name).map(|(_, v)| v)
}

/// The value of a column, with a missing column read as NULL (`GetValueOrDefault` is
/// null for both).
pub fn value_or_null(row: &VendorRow, name: &str) -> RawValue {
    column(row, name).cloned().unwrap_or(RawValue::Null)
}

/// The `DateTime` value of a column (`row.GetValueOrDefault(c) is DateTime d`).
pub fn date_value(row: &VendorRow, name: &str) -> Option<DateTime> {
    match column(row, name) {
        Some(RawValue::DateTime(d)) => Some(*d),
        _ => None,
    }
}
