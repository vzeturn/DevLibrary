//! The legacy `pda.db`, opened read-only (`LegacySqliteSource.cs`). One read transaction
//! spans the whole export, so every table comes from the same snapshot even under
//! `allow_live`.

use std::path::Path;

use dl_sqlite::{Connection, OpenFlags, ValueRef};

use super::sources::{
    Cell, ExportError, LegacyStore, Result, Row, RowSink, is_business_sql_table, sql_table_reason,
    stages,
};
use crate::config::codes;

/// `pda.db` as an [`LegacyStore`].
pub struct LegacySqliteSource {
    conn: Connection,
}

fn sqlite_error(what: &str, table: Option<&str>, e: &dl_sqlite::Error) -> ExportError {
    // The SQLite message can name a column but never quotes row values; it is kept out of
    // the text anyway: the extended code is what support needs.
    let mut err = ExportError::new(
        stages::EXPORT,
        codes::LEGACY_EXPORT_FAILED,
        format!(
            "{what} (SQLite error {})",
            e.code().map_or_else(|| "?".to_owned(), |c| c.to_string())
        ),
    );
    err.table = table.map(str::to_owned);
    err
}

/// Quotes a table name read from `sqlite_master` (never operator input).
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

impl LegacySqliteSource {
    /// Opens `path` read-only and begins the read transaction.
    pub fn open(path: &Path) -> Result<LegacySqliteSource> {
        if !path.is_file() {
            return Err(ExportError::new(
                stages::EXPORT,
                codes::LEGACY_STORE_MISSING,
                format!("pda.db not found at '{}'.", path.display()),
            ));
        }
        let conn = Connection::open(path, OpenFlags::READ_ONLY)
            .map_err(|e| sqlite_error("pda.db could not be opened", None, &e))?;
        // Deferred: a read-only connection cannot take the write lock of BEGIN IMMEDIATE.
        conn.execute_batch("BEGIN DEFERRED")
            .map_err(|e| sqlite_error("pda.db could not start a read transaction", None, &e))?;
        Ok(LegacySqliteSource { conn })
    }
}

impl Drop for LegacySqliteSource {
    fn drop(&mut self) {
        // A read transaction: ending it frees the snapshot; there is nothing to report.
        let _ = self.conn.execute_batch("ROLLBACK");
    }
}

impl LegacyStore for LegacySqliteSource {
    fn kind(&self) -> Option<String> {
        None
    }

    fn tables(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .map_err(|e| sqlite_error("pda.db tables could not be listed", None, &e))?;
        stmt.query(&[], |r| r.get::<String>(0))
            .map_err(|e| sqlite_error("pda.db tables could not be listed", None, &e))
    }

    fn schema_version(&self) -> Result<Option<i32>> {
        if !self.tables()?.iter().any(|t| t == "schema_version") {
            return Ok(None);
        }
        let mut stmt = self
            .conn
            .prepare("SELECT max(version) FROM schema_version")
            .map_err(|e| sqlite_error("the pda.db schema version could not be read", None, &e))?;
        let v = stmt
            .query_row(&[], |r| r.get::<Option<i64>>(0))
            .map_err(|e| sqlite_error("the pda.db schema version could not be read", None, &e))?;
        Ok(v.and_then(|v| i32::try_from(v).ok()))
    }

    fn count(&self, table: &str) -> Result<i64> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT count(*) FROM {}", quote(table)))
            .map_err(|e| sqlite_error("rows could not be counted", Some(table), &e))?;
        stmt.query_row(&[], |r| r.get::<i64>(0))
            .map_err(|e| sqlite_error("rows could not be counted", Some(table), &e))
    }

    fn for_each_row(&self, table: &str, sink: RowSink<'_>) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT * FROM {}", quote(table)))
            .map_err(|e| sqlite_error("the table could not be read", Some(table), &e))?;
        let names = stmt
            .column_names()
            .map_err(|e| sqlite_error("the table could not be read", Some(table), &e))?;
        loop {
            let more = stmt
                .step()
                .map_err(|e| sqlite_error("the table could not be read", Some(table), &e))?;
            if !more {
                return Ok(());
            }
            let mut row: Row = Vec::with_capacity(names.len());
            for (i, name) in names.iter().enumerate() {
                let value = stmt
                    .get_ref(i)
                    .map_err(|e| sqlite_error("the table could not be read", Some(table), &e))?;
                row.push((name.clone(), cell_of(value, table)?));
            }
            sink(row)?;
        }
    }

    /// Builds with `pda.db` read nothing from the old SQL Server tables: whatever is there
    /// was left behind.
    fn sql_table_left_out(&self, qualified: &str) -> Option<String> {
        Some(sql_table_reason(qualified))
    }

    fn sql_table_needs_review(&self, qualified: &str) -> bool {
        is_business_sql_table(qualified)
    }
}

fn cell_of(v: ValueRef<'_>, table: &str) -> Result<Cell> {
    Ok(match v {
        ValueRef::Null => Cell::Null,
        ValueRef::Integer(n) => Cell::Int(n),
        ValueRef::Real(f) => Cell::Real(f),
        ValueRef::Text(b) => Cell::Text(String::from_utf8(b.to_vec()).map_err(|_| {
            ExportError::new(
                stages::EXPORT,
                codes::LEGACY_EXPORT_FAILED,
                "a text value of the table is not valid UTF-8",
            )
            .at_table(table)
        })?),
        ValueRef::Blob(b) => Cell::Blob(b.to_vec()),
    })
}
