//! The SQL port the SMILE writers run on ([`Session`]), its `dl_tds::Client` adapter and
//! [`Rec`], a driver-free row so the writers (and their recording fake) never see TDS types.

use dl_decimal::Decimal;
use dl_tds::{Client, Param, ParamValue, ResultSet, Row, Value};
use dl_time::Date;

use crate::smile::SmileError;

/// One row: column names (as the server sent them) and values.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rec(Vec<(String, Value)>);

/// A stored procedure's first result set and return status.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Called {
    /// The rows of the first result set.
    pub rows: Vec<Rec>,
    /// `RETURN` status.
    pub status: Option<i32>,
}

/// What a SMILE writer needs from one connection. Every statement is parameterized; the
/// only SQL text built at run time is column lists from the schema whitelist.
pub trait Session {
    /// The rows of the first result set of `sql`.
    fn rows(&mut self, sql: &str, params: &[Param]) -> Result<Vec<Rec>, SmileError>;
    /// Runs `sql`; the rows affected.
    fn execute(&mut self, sql: &str, params: &[Param]) -> Result<u64, SmileError>;
    /// Calls a stored procedure with named parameters.
    fn call(&mut self, procedure: &str, params: &[Param]) -> Result<Called, SmileError>;
    /// A plain batch without parameters (transaction control, `SET` options).
    fn batch(&mut self, sql: &str) -> Result<(), SmileError>;
}

impl Rec {
    /// A row from `(column, value)` pairs.
    pub fn new(cols: Vec<(&str, Value)>) -> Rec {
        Rec(cols.into_iter().map(|(c, v)| (c.to_owned(), v)).collect())
    }

    fn from_row(row: Row) -> Rec {
        let names: Vec<String> = row.columns().iter().map(|c| c.name().to_owned()).collect();
        Rec(names.into_iter().zip(row.into_values()).collect())
    }

    fn value(&self, col: &'static str) -> Result<&Value, SmileError> {
        self.0
            .iter()
            .find(|(c, _)| c.eq_ignore_ascii_case(col))
            .map(|(_, v)| v)
            .ok_or(SmileError::Decode(col))
    }

    /// A nullable decimal of any numeric shape.
    pub fn opt_dec(&self, col: &'static str) -> Result<Option<Decimal>, SmileError> {
        Ok(match self.value(col)? {
            Value::Null => None,
            Value::Decimal(d) => Some(*d),
            Value::I8(n) => Some(Decimal::from(i64::from(*n))),
            Value::I16(n) => Some(Decimal::from(i64::from(*n))),
            Value::I32(n) => Some(Decimal::from(i64::from(*n))),
            Value::I64(n) => Some(Decimal::from(*n)),
            _ => return Err(SmileError::Decode(col)),
        })
    }

    /// A decimal that must be present.
    pub fn dec(&self, col: &'static str) -> Result<Decimal, SmileError> {
        self.opt_dec(col)?.ok_or(SmileError::Decode(col))
    }

    /// A nullable integer (decimals truncated).
    pub fn opt_int(&self, col: &'static str) -> Result<Option<i64>, SmileError> {
        Ok(match self.value(col)? {
            Value::Bool(b) => Some(i64::from(*b)),
            Value::Decimal(d) => Some(d.to_i64().ok_or(SmileError::Decode(col))?),
            _ => self.opt_dec(col)?.and_then(|d| d.to_i64()),
        })
    }

    /// An integer; `NULL` is an error.
    pub fn int(&self, col: &'static str) -> Result<i64, SmileError> {
        self.opt_int(col)?.ok_or(SmileError::Decode(col))
    }

    /// A nullable string as stored (numbers are rendered).
    pub fn opt_str(&self, col: &'static str) -> Result<Option<String>, SmileError> {
        Ok(match self.value(col)? {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            Value::Uuid(u) => Some(u.to_string()),
            Value::Decimal(d) => Some(d.to_string()),
            Value::I32(n) => Some(n.to_string()),
            Value::I64(n) => Some(n.to_string()),
            _ => return Err(SmileError::Decode(col)),
        })
    }

    /// A nullable date (`datetime` columns keep their date part).
    pub fn opt_date(&self, col: &'static str) -> Result<Option<Date>, SmileError> {
        Ok(match self.value(col)? {
            Value::Null => None,
            Value::Date(d) => Some(*d),
            Value::DateTime(dt) => Some(dt.date()),
            _ => return Err(SmileError::Decode(col)),
        })
    }

    /// The first column as an integer (an unnamed scalar such as `SELECT CASE ... END`).
    pub fn scalar_int(&self) -> Result<Option<i64>, SmileError> {
        match self.0.first() {
            None => Ok(None),
            Some((_, v)) => Rec(vec![("_".to_owned(), v.clone())]).opt_int("_"),
        }
    }

    /// The first column as a date (an unnamed scalar such as `SELECT TOP (1) HotelDate`).
    pub fn scalar_date(&self) -> Result<Option<Date>, SmileError> {
        match self.0.first() {
            None => Ok(None),
            Some((_, v)) => Rec(vec![("_".to_owned(), v.clone())]).opt_date("_"),
        }
    }

    /// A nullable flag (`bit`, or an integer: non-zero is true).
    pub fn opt_bool(&self, col: &'static str) -> Result<Option<bool>, SmileError> {
        Ok(match self.value(col)? {
            Value::Null => None,
            Value::Bool(b) => Some(*b),
            _ => self.opt_int(col)?.map(|n| n != 0),
        })
    }
}

fn first(sets: Vec<ResultSet>) -> Vec<Rec> {
    sets.into_iter()
        .next()
        .map(|s| s.into_rows().into_iter().map(Rec::from_row).collect())
        .unwrap_or_default()
}

impl Session for Client {
    fn rows(&mut self, sql: &str, params: &[Param]) -> Result<Vec<Rec>, SmileError> {
        let r = self
            .query(sql, params)
            .map_err(|e| SmileError::from_tds(&e))?;
        Ok(first(r.into_sets()))
    }

    fn execute(&mut self, sql: &str, params: &[Param]) -> Result<u64, SmileError> {
        Client::execute(self, sql, params).map_err(|e| SmileError::from_tds(&e))
    }

    fn call(&mut self, procedure: &str, params: &[Param]) -> Result<Called, SmileError> {
        let r = Client::call(self, procedure, params).map_err(|e| SmileError::from_tds(&e))?;
        let status = r.return_status();
        Ok(Called {
            rows: first(r.into_sets()),
            status,
        })
    }

    fn batch(&mut self, sql: &str) -> Result<(), SmileError> {
        Client::batch(self, sql)
            .map(|_| ())
            .map_err(|e| SmileError::from_tds(&e))
    }
}

/// `@name` with a value (the name keeps its `@`, as Dapper's `DynamicParameters`).
pub fn p(name: &str, value: ParamValue) -> Param {
    Param::new(name, value)
}

/// `nvarchar` (`DbType.String`).
pub fn nvarchar(s: Option<&str>) -> ParamValue {
    ParamValue::String(s.map(str::to_owned))
}

/// `varchar` (`DbType.AnsiString`), for TCVN3 text and SMILE's `varchar` keys.
pub fn varchar(s: Option<&str>) -> ParamValue {
    ParamValue::VarChar(s.map(str::to_owned))
}

/// `decimal` (`DbType.Decimal`).
pub fn decimal(d: Decimal) -> ParamValue {
    ParamValue::Decimal(Some(d))
}

/// `int` (`DbType.Int32`).
pub fn int(n: i32) -> ParamValue {
    ParamValue::I32(Some(n))
}

/// `bit` (`DbType.Boolean`).
pub fn bit(b: bool) -> ParamValue {
    ParamValue::Bool(Some(b))
}

/// A date at midnight (`DbType.DateTime` of a `DateOnly`).
pub fn day(d: Date) -> ParamValue {
    ParamValue::DateTime(Some(d.midnight()))
}

/// Whether the value is a typed or untyped `NULL`.
pub fn is_null(v: &ParamValue) -> bool {
    match v {
        ParamValue::Null => true,
        ParamValue::Bool(x) => x.is_none(),
        ParamValue::U8(x) => x.is_none(),
        ParamValue::I16(x) => x.is_none(),
        ParamValue::I32(x) => x.is_none(),
        ParamValue::I64(x) => x.is_none(),
        ParamValue::Decimal(x) => x.is_none(),
        ParamValue::String(x) | ParamValue::VarChar(x) => x.is_none(),
        ParamValue::DateTime(x) => x.is_none(),
        _ => false,
    }
}

/// The text of a string value, for width checks.
pub fn text_of(v: &ParamValue) -> Option<&str> {
    match v {
        ParamValue::String(Some(s)) | ParamValue::VarChar(Some(s)) => Some(s),
        _ => None,
    }
}

/// `.NET DbType` name and invariant text of a value: what the golden SQL fixtures record.
pub fn describe(v: &ParamValue) -> (&'static str, Option<String>) {
    match v {
        ParamValue::Null => ("Object", None),
        ParamValue::Bool(x) => ("Boolean", x.map(|b| b.to_string())),
        ParamValue::U8(x) => ("Byte", x.map(|n| n.to_string())),
        ParamValue::I16(x) => ("Int16", x.map(|n| n.to_string())),
        ParamValue::I32(x) => ("Int32", x.map(|n| n.to_string())),
        ParamValue::I64(x) => ("Int64", x.map(|n| n.to_string())),
        ParamValue::Decimal(x) => ("Decimal", x.map(|d| d.to_string())),
        ParamValue::String(x) => ("String", x.clone()),
        ParamValue::VarChar(x) => ("AnsiString", x.clone()),
        ParamValue::DateTime(x) => ("DateTime", x.map(|d| d.to_string())),
        ParamValue::Date(x) => ("Date", x.map(|d| d.to_string())),
        ParamValue::Uuid(x) => ("Guid", x.map(|u| u.to_string())),
        _ => ("Other", Some(format!("{v:?}"))),
    }
}
