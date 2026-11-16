//! Materialised results: result sets, rows, columns, messages.

use std::sync::Arc;

use crate::collation::Collation;
use crate::types::TypeInfo;
use crate::value::FromValue;
use crate::{Error, Value};

/// Metadata of one result column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub(crate) name: String,
    pub(crate) nullable: bool,
    pub(crate) ti: TypeInfo,
}

impl Column {
    /// Column name (empty for unnamed expressions).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `true` if the column may contain NULL.
    pub fn nullable(&self) -> bool {
        self.nullable
    }

    /// SQL Server type name in upper case (`"INT"`, `"NVARCHAR"`, `"DECIMAL"`, …), the same
    /// strings go-mssqldb's `ColumnTypeDatabaseTypeName` returns.
    pub fn type_name(&self) -> &'static str {
        self.ti.type_name()
    }

    /// Declared maximum length in bytes for character/binary types; `None` for `(max)`,
    /// XML and fixed-size types.
    pub fn max_length(&self) -> Option<u32> {
        use crate::types::{Len, ty};
        match self.ti.len {
            Len::UShort | Len::Text => Some(self.ti.size),
            Len::Byte
                if matches!(
                    self.ti.id,
                    ty::CHAR | ty::VARCHAR | ty::BINARY | ty::VARBINARY
                ) =>
            {
                Some(self.ti.size)
            }
            _ => None,
        }
    }

    /// Precision of `decimal`/`numeric` columns, 0 otherwise.
    pub fn precision(&self) -> u8 {
        self.ti.precision
    }

    /// Scale of `decimal`/`numeric` and fractional-second digits of `time`, `datetime2`,
    /// `datetimeoffset` columns; 0 otherwise.
    pub fn scale(&self) -> u8 {
        self.ti.scale
    }

    /// Collation of character columns.
    pub fn collation(&self) -> Option<Collation> {
        self.ti.collation
    }
}

/// Selects a column by position (`usize`) or by name (`&str`, exact match first, then
/// ASCII case-insensitive).
pub trait ColumnIndex {
    /// The 0-based position of the column, if it exists.
    fn index(&self, columns: &[Column]) -> Option<usize>;
    /// Description for error messages.
    fn describe(&self) -> String;
}

impl ColumnIndex for usize {
    fn index(&self, columns: &[Column]) -> Option<usize> {
        (*self < columns.len()).then_some(*self)
    }
    fn describe(&self) -> String {
        format!("#{self}")
    }
}

impl ColumnIndex for &str {
    fn index(&self, columns: &[Column]) -> Option<usize> {
        columns.iter().position(|c| c.name == *self).or_else(|| {
            columns
                .iter()
                .position(|c| c.name.eq_ignore_ascii_case(self))
        })
    }
    fn describe(&self) -> String {
        format!("{self:?}")
    }
}

/// One row of a result set.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub(crate) columns: Arc<[Column]>,
    pub(crate) values: Vec<Value>,
}

impl Row {
    /// Reads column `idx` converted to `T` (see [`FromValue`] for the rules).
    ///
    /// ```no_run
    /// # fn f(row: &dl_tds::Row) -> Result<(), dl_tds::Error> {
    /// let id: i64 = row.get(0)?;
    /// let name: String = row.get("Name")?;
    /// let issued: Option<dl_time::DateTime> = row.get("IssuedAt")?;
    /// # Ok(()) }
    /// ```
    ///
    /// # Errors
    /// [`Error::ColumnNotFound`], [`Error::Null`] (NULL into a non-`Option`), or
    /// [`Error::Conversion`].
    pub fn get<T: FromValue>(&self, idx: impl ColumnIndex) -> Result<T, Error> {
        let i = idx
            .index(&self.columns)
            .ok_or_else(|| Error::ColumnNotFound(idx.describe()))?;
        let v = &self.values[i];
        if v.is_null() {
            return T::from_null().ok_or(Error::Null { column: i });
        }
        T::from_value(v).map_err(|e| match e {
            Error::Conversion {
                from, to, reason, ..
            } => Error::Conversion {
                column: i,
                from,
                to,
                reason,
            },
            Error::Null { .. } => Error::Null { column: i },
            other => other,
        })
    }

    /// Like [`get`](Self::get) but returns `None` instead of an error (missing column,
    /// NULL, or failed conversion).
    pub fn try_get<T: FromValue>(&self, idx: impl ColumnIndex) -> Option<T> {
        self.get(idx).ok()
    }

    /// The raw value of column `idx`.
    pub fn value(&self, idx: impl ColumnIndex) -> Option<&Value> {
        idx.index(&self.columns).map(|i| &self.values[i])
    }

    /// All values in column order.
    pub fn values(&self) -> &[Value] {
        &self.values
    }

    /// Consumes the row, returning its values.
    pub fn into_values(self) -> Vec<Value> {
        self.values
    }

    /// Column metadata.
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// Number of columns.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// `true` if the row has no columns.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// One result set: its columns and all rows.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultSet {
    pub(crate) columns: Arc<[Column]>,
    pub(crate) rows: Vec<Row>,
}

impl ResultSet {
    /// Column metadata.
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// The rows.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Consumes the set, returning its rows.
    pub fn into_rows(self) -> Vec<Row> {
        self.rows
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// `true` if the set has no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Position of the column named `name`.
    pub fn column_index(&self, name: &str) -> Option<usize> {
        name.index(&self.columns)
    }
}

/// Everything a request returned: result sets in order, row counts, the procedure return
/// status and output parameters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Results {
    pub(crate) sets: Vec<ResultSet>,
    pub(crate) row_counts: Vec<u64>,
    pub(crate) return_statuses: Vec<i32>,
    pub(crate) output: Vec<(String, Value)>,
}

impl Results {
    /// The result sets (one per SELECT that produced columns), in order.
    pub fn sets(&self) -> &[ResultSet] {
        &self.sets
    }

    /// Consumes the results, returning the sets.
    pub fn into_sets(self) -> Vec<ResultSet> {
        self.sets
    }

    /// The first result set, if any.
    pub fn first(&self) -> Option<&ResultSet> {
        self.sets.first()
    }

    /// Rows of the first result set (empty if there is none).
    pub fn rows(&self) -> &[Row] {
        self.sets.first().map(|s| s.rows()).unwrap_or(&[])
    }

    /// Sum of all row counts the server reported (DONE, DONEPROC and DONEINPROC tokens with
    /// a count), the number go-mssqldb returns from `RowsAffected`. SELECTs count too.
    pub fn rows_affected(&self) -> u64 {
        self.row_counts.iter().sum()
    }

    /// Each reported row count, in order.
    pub fn row_counts(&self) -> &[u64] {
        &self.row_counts
    }

    /// The last `RETURN` value the server sent (RETURNSTATUS token), which is what
    /// go-mssqldb stores in a `ReturnStatus` output. For a plain batch `EXEC dbo.proc` this
    /// is the procedure's `RETURN` value. When parameters are used the request runs inside
    /// `sp_executesql`, whose own status (0) comes last; capture the procedure's value
    /// explicitly there (`DECLARE @r int; EXEC @r = dbo.proc ...; SELECT @r`) or see
    /// [`return_statuses`](Self::return_statuses).
    pub fn return_status(&self) -> Option<i32> {
        self.return_statuses.last().copied()
    }

    /// Every RETURNSTATUS value received, in order.
    pub fn return_statuses(&self) -> &[i32] {
        &self.return_statuses
    }

    /// Output parameters (RETURNVALUE tokens) as `(name without '@', value)`.
    pub fn output_params(&self) -> &[(String, Value)] {
        &self.output
    }
}

/// Severity of a server message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// INFO token: `PRINT`, `RAISERROR` with severity ≤ 10, and informational messages such
    /// as "Changed database context".
    Info,
    /// ERROR token (severity ≥ 11).
    Error,
}

/// An INFO or ERROR message from the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Info or error.
    pub kind: MessageKind,
    /// Message number.
    pub number: i32,
    /// State.
    pub state: u8,
    /// Severity class.
    pub class: u8,
    /// Text.
    pub message: String,
    /// Server name.
    pub server: String,
    /// Procedure name (empty for batches).
    pub procedure: String,
    /// Line number.
    pub line: i32,
}

impl Message {
    pub(crate) fn to_error(&self) -> Error {
        Error::Server {
            number: self.number,
            state: self.state,
            class: self.class,
            message: self.message.clone(),
            server: self.server.clone(),
            procedure: self.procedure.clone(),
            line: self.line,
        }
    }
}
