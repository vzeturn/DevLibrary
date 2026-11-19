//! SELECT-only reader for SMILE_POS over dl-tds (`Source/SqlServerCaptureSource.cs`).
//!
//! Every statement runs with a lock timeout, LOW deadlock priority and (unless the
//! database already uses RCSI) READ UNCOMMITTED/NOLOCK, so capture can never block or be
//! chosen over POS Win. Same SQL text and parameter types as the C# source.
//!
//! Differences forced by the driver, behaviour kept:
//! - SqlClient pools connections; this source keeps one open connection and reuses it
//!   (dropped after any failure; a reused connection that fails at the transport level
//!   is replaced and the statement retried once, as a pooled SqlClient connection would
//!   be checked and replaced). The session `SET`s are re-sent with every statement and
//!   the isolation choice is fixed per source, so reuse does not leak settings.
//! - `AddWithValue(DateTime)` declares `datetime`; dl-tds declares `datetime2(7)`. The
//!   values are the ones read from the same columns (or day boundaries), so comparisons
//!   give the same rows.
//! - Cancellation: the context is checked before each statement and bounds the socket
//!   timeout; a statement in flight is not interrupted early.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use dl_tds::{Client, Param, ParamValue};

use super::error::{CaptureError, ErrorKind, Result};
use super::source::{CaptureSource, SourceQuery, VendorRow};
use super::sql_builder::{self, ReadHintPolicy};
use super::util::{contains_ignore_case, distinct_ignore_case};
use crate::contracts::mapping::RawValue;
use crate::contracts::time::{DateTime, DateTimeOffset};
use crate::reporting::sql_error::{
    INVALID_COLUMN, INVALID_OBJECT, MULTI_PART_NOT_BOUND, SqlFailure,
};
use crate::util::ctx::Ctx;

/// SqlClient's timeout error number.
const TIMEOUT_ERROR: i32 = -2;
/// Lock request time out period exceeded.
const LOCK_TIMEOUT_ERROR: i32 = 1222;
/// Chosen as deadlock victim.
const DEADLOCK_VICTIM_ERROR: i32 = 1205;

/// `SqlServerCaptureSource.CaptureTableDataKey` (= `SourceSchemaFault.TableKey`).
pub const CAPTURE_TABLE_DATA_KEY: &str = "pda.capture_table";

/// Settings of a [`SqlServerCaptureSource`] (`SqlServerSourceOptions`).
#[derive(Debug, Clone)]
pub struct SqlServerSourceOptions {
    /// The connection (`ConnectionString`), e.g. from `crate::config::sql::agent_connection_for`.
    pub connection: dl_tds::Config,
    /// Hard cap per statement (15 s). A slow read is abandoned (and the governor backs
    /// off) rather than queued behind POS Win.
    pub command_timeout_seconds: u32,
    /// `SET LOCK_TIMEOUT` for each read (2000 ms), so a blocked read gives up quickly.
    pub lock_timeout_ms: i32,
    /// NOLOCK policy.
    pub hint_policy: ReadHintPolicy,
}

impl SqlServerSourceOptions {
    /// Options with the C# defaults for `connection`.
    pub fn new(connection: dl_tds::Config) -> SqlServerSourceOptions {
        SqlServerSourceOptions {
            connection,
            command_timeout_seconds: 15,
            lock_timeout_ms: 2000,
            hint_policy: ReadHintPolicy::Auto,
        }
    }
}

#[derive(Debug, Clone)]
struct ColumnInfo {
    name: String,
    type_: String,
}

impl ColumnInfo {
    fn is_character(&self) -> bool {
        matches!(self.type_.as_str(), "char" | "varchar" | "text")
    }
}

/// SELECT-only reader for SMILE_POS.
pub struct SqlServerCaptureSource {
    options: SqlServerSourceOptions,
    /// Upper-cased table name to its columns (`OrdinalIgnoreCase` cache).
    schemas: Mutex<HashMap<String, Vec<ColumnInfo>>>,
    rcsi: Mutex<Option<bool>>,
    conn: Mutex<Option<Client>>,
}

impl std::fmt::Debug for SqlServerCaptureSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlServerCaptureSource")
            .finish_non_exhaustive()
    }
}

/// A failed statement: a SQL Server error (`SqlException`) or another failure.
fn tds_error(e: &dl_tds::Error, client: Option<&mut Client>) -> CaptureError {
    let messages = client.map(Client::take_messages).unwrap_or_default();
    match SqlFailure::from_tds(e, &messages) {
        Some(f) => CaptureError::sql(f),
        None => {
            let kind = match e {
                dl_tds::Error::Param { .. } | dl_tds::Error::Config { .. } => ErrorKind::Argument,
                dl_tds::Error::Conversion { .. } | dl_tds::Error::Null { .. } => {
                    ErrorKind::InvalidOperation
                }
                _ => ErrorKind::Other,
            };
            CaptureError::new(kind, e.to_string())
        }
    }
}

/// Whether a failure on a reused connection is worth one retry on a fresh one.
fn is_transport(e: &dl_tds::Error) -> bool {
    matches!(e, dl_tds::Error::Io(_) | dl_tds::Error::Broken)
}

fn unsupported(type_name: &str) -> CaptureError {
    CaptureError::argument(format!(
        "No mapping exists from object type System.{type_name} to a known managed provider native type."
    ))
}

fn tds_date_time(d: DateTime) -> Result<dl_time::DateTime> {
    let (y, m, day, h, mi, s, frac) = d.parts();
    let bad = |_| {
        CaptureError::argument(format!(
            "DateTime {} is out of range.",
            d.to_round_trip_string()
        ))
    };
    let date = dl_time::Date::new(y, m as u8, day as u8).map_err(bad)?;
    let time = dl_time::Time::new(h as u8, mi as u8, s as u8, (frac * 100) as u32).map_err(bad)?;
    Ok(dl_time::DateTime::new(date, time))
}

fn tds_offset(o: DateTimeOffset) -> Result<dl_time::OffsetDateTime> {
    let local = DateTime::from_ticks(
        o.local_ticks(),
        crate::contracts::time::DateTimeKind::Unspecified,
    )
    .ok_or_else(|| CaptureError::argument("DateTimeOffset is out of range."))?;
    let offset = dl_time::Offset::from_seconds(i32::from(o.offset_minutes()) * 60)
        .map_err(|_| CaptureError::argument("DateTimeOffset offset is out of range."))?;
    Ok(dl_time::OffsetDateTime::new(tds_date_time(local)?, offset))
}

/// The parameter `cmd.Parameters.AddWithValue(name, value ?? DBNull.Value)` sends.
pub fn to_param(name: &str, value: &RawValue) -> Result<Param> {
    let v = match value {
        RawValue::Null => ParamValue::Null,
        RawValue::Bool(b) => ParamValue::Bool(Some(*b)),
        RawValue::U8(n) => ParamValue::U8(Some(*n)),
        RawValue::I16(n) => ParamValue::I16(Some(*n)),
        RawValue::I32(n) => ParamValue::I32(Some(*n)),
        RawValue::I64(n) => ParamValue::I64(Some(*n)),
        RawValue::F32(f) => ParamValue::F32(Some(*f)),
        RawValue::F64(f) => ParamValue::F64(Some(*f)),
        RawValue::Decimal(d) => ParamValue::Decimal(Some(*d)),
        RawValue::String(s) => ParamValue::String(Some(s.clone())),
        RawValue::Bytes(b) => ParamValue::Bytes(Some(b.clone())),
        RawValue::Guid(g) => ParamValue::Uuid(Some(*g)),
        RawValue::DateTime(d) => ParamValue::DateTime(Some(tds_date_time(*d)?)),
        RawValue::DateTimeOffset(o) => ParamValue::DateTimeOffset(Some(tds_offset(*o)?)),
        RawValue::TimeSpan(t) => {
            let nanos = u64::try_from(*t)
                .ok()
                .and_then(|t| t.checked_mul(100))
                .and_then(|n| dl_time::Time::from_nanos_of_day(n).ok())
                .ok_or_else(|| {
                    CaptureError::argument("TimeSpan is out of range for SQL Server time.")
                })?;
            ParamValue::Time(Some(nanos))
        }
        RawValue::I8(_) | RawValue::U16(_) | RawValue::U32(_) | RawValue::U64(_) => {
            return Err(unsupported(value.type_name()));
        }
    };
    Ok(Param::new(name, v))
}

/// The rows of a result set as vendor rows (`ReadRowsAsync`): later columns of the same
/// name (ignoring case) overwrite earlier ones, as the C# case-insensitive dictionary does.
fn read_rows(set: dl_tds::ResultSet) -> Vec<VendorRow> {
    let names: Vec<String> = set.columns().iter().map(|c| c.name().to_owned()).collect();
    set.into_rows()
        .into_iter()
        .map(|r| {
            let mut row = VendorRow::new();
            for (name, v) in names.iter().zip(r.values()) {
                let value = RawValue::from_tds(v);
                let existing = row.get_ignore_case(name).map(|(k, _)| k.to_owned());
                match existing {
                    Some(k) => {
                        if let Some(slot) = row.get_mut(&k) {
                            *slot = value;
                        }
                    }
                    None => {
                        row.insert(name.clone(), value);
                    }
                }
            }
            row
        })
        .collect()
}

impl SqlServerCaptureSource {
    /// A source over `options`. No connection is opened until the first read.
    pub fn new(options: SqlServerSourceOptions) -> SqlServerCaptureSource {
        SqlServerCaptureSource {
            options,
            schemas: Mutex::new(HashMap::new()),
            rcsi: Mutex::new(None),
            conn: Mutex::new(None),
        }
    }

    /// `SET LOCK_TIMEOUT n; `: metadata probes get the same lock timeout as row reads.
    fn lock_timeout(&self) -> String {
        format!("SET LOCK_TIMEOUT {}; ", self.options.lock_timeout_ms)
    }

    fn preamble(&self, no_lock: bool) -> String {
        let mut s = self.lock_timeout();
        s.push_str("SET DEADLOCK_PRIORITY LOW; ");
        if no_lock {
            s.push_str("SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED; ");
        }
        s
    }

    /// Runs one statement on the cached connection (opened on demand) and returns its
    /// first result set.
    fn run(&self, sql: &str, params: &[Param], ctx: &Ctx) -> Result<Option<dl_tds::ResultSet>> {
        self.run_phased(sql, params, ctx).map_err(|(e, _)| e)
    }

    /// [`Self::run`], telling whether a failure happened while opening the connection
    /// (`OpenAsync`, outside the C# busy translation) or while executing.
    fn run_phased(
        &self,
        sql: &str,
        params: &[Param],
        ctx: &Ctx,
    ) -> std::result::Result<Option<dl_tds::ResultSet>, (CaptureError, bool)> {
        if ctx.is_done() {
            return Err((CaptureError::cancelled(), false));
        }
        let mut timeout = Duration::from_secs(u64::from(self.options.command_timeout_seconds));
        if let Some(r) = ctx.remaining() {
            timeout = timeout.min(r.max(Duration::from_millis(1)));
        }
        let mut guard = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        let mut attempts = 0;
        loop {
            attempts += 1;
            let reused = guard.as_ref().is_some_and(|c| !c.is_broken());
            if !reused {
                *guard = None;
                let mut c = Client::connect(&self.options.connection)
                    .map_err(|e| (tds_error(&e, None), true))?;
                c.take_messages();
                *guard = Some(c);
            }
            let Some(client) = guard.as_mut() else {
                return Err((
                    CaptureError::invalid_operation("connection unavailable"),
                    true,
                ));
            };
            let result = client
                .set_timeout(Some(timeout))
                .and_then(|()| client.query(sql, params));
            match result {
                Ok(results) => {
                    client.take_messages();
                    return Ok(results.into_sets().into_iter().next());
                }
                Err(e) => {
                    let retry = reused && attempts == 1 && is_transport(&e) && !ctx.is_done();
                    let err = tds_error(&e, Some(client));
                    *guard = None;
                    if retry {
                        continue;
                    }
                    if ctx.is_done() && matches!(e, dl_tds::Error::Timeout) {
                        return Err((CaptureError::cancelled(), false));
                    }
                    return Err((err, false));
                }
            }
        }
    }

    fn scalar_i32(&self, sql: &str, params: &[Param], ctx: &Ctx) -> Result<Option<i32>> {
        let sql = self.lock_timeout() + sql;
        let set = self.run(&sql, params, ctx)?;
        let value = set
            .as_ref()
            .and_then(|s| s.rows().first())
            .and_then(|r| r.values().first());
        Ok(match value.map(RawValue::from_tds) {
            Some(RawValue::I32(n)) => Some(n),
            Some(RawValue::U8(n)) => Some(i32::from(n)),
            Some(RawValue::I16(n)) => Some(i32::from(n)),
            Some(RawValue::I64(n)) => i32::try_from(n).ok(),
            Some(RawValue::Bool(b)) => Some(i32::from(b)),
            _ => None,
        })
    }

    fn get_schema(&self, table: &str, ctx: &Ctx) -> Result<Vec<ColumnInfo>> {
        let cache_key = table.to_uppercase();
        if let Some(cached) = self
            .schemas
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&cache_key)
        {
            return Ok(cached.clone());
        }
        sql_builder::quote(table)?;
        let sql = self.lock_timeout()
            + "SELECT c.name, t.name FROM sys.columns c JOIN sys.types t ON t.user_type_id = c.user_type_id "
            + "WHERE c.object_id = OBJECT_ID(@n, 'U') ORDER BY c.column_id";
        let set = self.run(&sql, &[Param::new("@n", format!("dbo.{table}"))], ctx)?;
        let mut list = Vec::new();
        if let Some(set) = set {
            for r in set.rows() {
                let text = |i: usize| match r.values().get(i).map(RawValue::from_tds) {
                    Some(RawValue::String(s)) => Ok(s),
                    _ => Err(CaptureError::invalid_operation(
                        "Data is Null. This method or property cannot be called on Null values.",
                    )),
                };
                list.push(ColumnInfo {
                    name: text(0)?,
                    type_: text(1)?,
                });
            }
        }
        if list.is_empty() {
            return Err(CaptureError::invalid_operation(format!(
                "Table dbo.{table} does not exist or has no columns."
            )));
        }
        self.schemas
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(cache_key, list.clone());
        Ok(list)
    }

    /// The select list and the columns to read as bytes.
    fn resolve_columns(&self, q: &SourceQuery, ctx: &Ctx) -> Result<(Vec<String>, Vec<String>)> {
        let bytes = distinct_ignore_case(q.bytes_columns.clone().unwrap_or_default());
        if let Some(columns) = &q.columns {
            return Ok((columns.clone(), bytes));
        }
        let schema = self.get_schema(&q.table, ctx)?;
        let excluded = q.exclude_columns.clone().unwrap_or_default();
        let selected: Vec<&ColumnInfo> = schema
            .iter()
            .filter(|c| !contains_ignore_case(&excluded, &c.name))
            .collect();
        // Only genuine single-byte character columns can be re-read as stored bytes;
        // nvarchar already holds Unicode.
        let char_columns: Vec<&str> = selected
            .iter()
            .filter(|c| c.is_character())
            .map(|c| c.name.as_str())
            .collect();
        let bytes = bytes
            .into_iter()
            .filter(|b| contains_ignore_case(&char_columns, b))
            .collect();
        Ok((selected.iter().map(|c| c.name.clone()).collect(), bytes))
    }

    fn use_no_lock(&self, ctx: &Ctx) -> Result<bool> {
        match self.options.hint_policy {
            ReadHintPolicy::NoLock => return Ok(true),
            ReadHintPolicy::None => return Ok(false),
            ReadHintPolicy::Auto => {}
        }
        if let Some(rcsi) = *self.rcsi.lock().unwrap_or_else(PoisonError::into_inner) {
            return Ok(!rcsi);
        }
        let on = self.scalar_i32(
            "SELECT CAST(is_read_committed_snapshot_on AS int) FROM sys.databases WHERE database_id = DB_ID()",
            &[],
            ctx,
        )?;
        // Convert.ToInt32(null) is 0: no row reads as RCSI off.
        let rcsi = on.unwrap_or(0) == 1;
        *self.rcsi.lock().unwrap_or_else(PoisonError::into_inner) = Some(rcsi);
        Ok(!rcsi)
    }

    fn query_core(&self, query: &SourceQuery, ctx: &Ctx) -> Result<Vec<VendorRow>> {
        let (columns, bytes) = self.resolve_columns(query, ctx)?;
        let no_lock = self.use_no_lock(ctx)?;
        let built = sql_builder::build(query, &columns, &bytes, no_lock)?;
        let params = built
            .parameters
            .iter()
            .map(|(n, v)| to_param(n, v))
            .collect::<Result<Vec<_>>>()?;
        let sql = self.preamble(no_lock) + &built.sql;
        match self.run_phased(&sql, &params, ctx) {
            Ok(set) => Ok(set.map(read_rows).unwrap_or_default()),
            Err((e, true)) => Err(e),
            Err((e, false)) => {
                let number = e.sql.as_ref().map(|s| s.number());
                if matches!(
                    number,
                    Some(TIMEOUT_ERROR | LOCK_TIMEOUT_ERROR | DEADLOCK_VICTIM_ERROR)
                ) {
                    let n = number.unwrap_or_default();
                    return Err(CaptureError::busy(
                        format!("Read of {} abandoned (SQL error {n}).", query.table),
                        Some(e),
                    ));
                }
                Err(e)
            }
        }
    }
}

impl CaptureSource for SqlServerCaptureSource {
    fn table_exists(&self, table: &str, ctx: &Ctx) -> Result<bool> {
        sql_builder::quote(table)?;
        let n = self.scalar_i32(
            "SELECT CASE WHEN OBJECT_ID(@n, 'U') IS NULL THEN 0 ELSE 1 END",
            &[Param::new("@n", format!("dbo.{table}"))],
            ctx,
        )?;
        Ok(n == Some(1))
    }

    fn query(&self, query: &SourceQuery, ctx: &Ctx) -> Result<Vec<VendorRow>> {
        match self.query_core(query, ctx) {
            Err(mut e) if e.kind == ErrorKind::Sql => {
                // Which capture table the failing read was for (AD1).
                if e.table.is_none() {
                    e.table = Some(query.table.clone());
                }
                // A missing column or object belongs to this table only: the strategies
                // skip it and keep the others going.
                let schema = e.sql.as_ref().is_some_and(|s| {
                    s.errors.iter().any(|x| {
                        matches!(
                            x.number,
                            INVALID_COLUMN | INVALID_OBJECT | MULTI_PART_NOT_BOUND
                        )
                    })
                });
                if schema {
                    e.mark_schema_fault(&query.table);
                }
                Err(e)
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_get_the_types_add_with_value_infers() {
        let p = to_param("@a0", &RawValue::I64(5)).unwrap();
        assert_eq!(p.name(), "a0");
        assert_eq!(p.value(), &ParamValue::I64(Some(5)));
        assert_eq!(
            to_param("@w0", &RawValue::from("x")).unwrap().value(),
            &ParamValue::String(Some("x".into()))
        );
        let d = DateTime::new(2026, 10, 24, 12, 30, 1).unwrap();
        let ParamValue::DateTime(Some(t)) = to_param("@rf", &RawValue::DateTime(d))
            .unwrap()
            .value()
            .clone()
        else {
            panic!("datetime param");
        };
        assert_eq!((t.year(), t.hour(), t.second()), (2026, 12, 1));
        assert_eq!(
            to_param("@x", &RawValue::Null).unwrap().value(),
            &ParamValue::Null
        );
        assert_eq!(
            to_param("@x", &RawValue::U64(1)).unwrap_err().kind,
            ErrorKind::Argument
        );
    }

    #[test]
    fn the_preamble_matches_the_csharp_text() {
        let s = SqlServerCaptureSource::new(SqlServerSourceOptions::new(dl_tds::Config::new(
            "localhost",
        )));
        assert_eq!(
            s.preamble(true),
            "SET LOCK_TIMEOUT 2000; SET DEADLOCK_PRIORITY LOW; SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED; "
        );
        assert_eq!(
            s.preamble(false),
            "SET LOCK_TIMEOUT 2000; SET DEADLOCK_PRIORITY LOW; "
        );
    }
}
