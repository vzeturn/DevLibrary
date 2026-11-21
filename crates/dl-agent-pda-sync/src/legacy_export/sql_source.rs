//! The SELECTs `legacy-export` runs on `SMILE_POS` (`LegacySqlServerSource.cs`), and the
//! store of a build from before `pda.db` read through them (`LegacySqlStoreSource.cs`,
//! `LegacySqlValues.cs`, `LegacyPrintProfileSplit.cs`, `LegacySqlFailure.cs`, ADR 0094).
//!
//! SELECT only, as the `pda_sync` login. Table and column names come from the catalog,
//! never from the command; every value that comes from the operator is a parameter.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use dl_tds::{Client, Param};

use super::sources::legacy_cell;
use super::sources::{
    Cell, ExportError, LegacySqlSource, LegacyStore, Result, Row, RowSink, bookkeeping_reason,
    cell, is_business_sql_table, stages, unqualified,
};
pub use super::sources::{convert, explain_sql_failure};
use crate::config::codes;
use crate::contracts::legacy::sql_store::{self, PRINT_PROFILE_SOURCE, PRINT_TEMPLATE_TARGET};
use crate::contracts::legacy::{LegacySqlColumn, LegacySqlKind, LegacySqlTable};
use crate::contracts::mapping::RawValue;
use crate::contracts::time::TimeZone;
use crate::reporting::sql_error::SqlFailure;
use crate::util::stj::eq_ignore_case;

/// Per-statement timeout of the export's reads.
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(120);
/// `SET`s of every read: it gives up on a lock and never wins a deadlock against POS Win.
const PREAMBLE: &str = "SET LOCK_TIMEOUT 15000; SET DEADLOCK_PRIORITY LOW; ";

/// Whether a refused connection is worth the integrated-authentication retry: the login
/// was refused or has no access (229, 230, 916, 18456).
pub fn refused_login(e: &ExportError) -> bool {
    matches!(e.sql_number, Some(229 | 230 | 916 | 18456))
        || e.code == codes::SQL_LOGIN_FAILED
        || e.code == codes::SQL_NO_ACCESS
}

/// Which login the run uses: `pda_sync` first; once it is refused (at connect or by a query)
/// and a fallback exists, integrated authentication for the rest of the run.
pub struct Failover {
    has_fallback: bool,
    integrated: std::sync::atomic::AtomicBool,
}

impl Failover {
    /// With or without an integrated-authentication fallback.
    pub fn new(has_fallback: bool) -> Failover {
        Failover {
            has_fallback,
            integrated: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Whether the run switched to integrated authentication.
    pub fn is_integrated(&self) -> bool {
        self.integrated.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Runs `attempt(false)`; when the login is refused, runs `attempt(true)` once and keeps
    /// it for later calls. When both are refused the failure names both attempts (no secrets).
    pub fn run<T>(&self, instance: &str, attempt: impl Fn(bool) -> Result<T>) -> Result<T> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.is_integrated() {
            return attempt(true);
        }
        let first = match attempt(false) {
            Ok(v) => return Ok(v),
            Err(e) => e,
        };
        if !(self.has_fallback && refused_login(&first)) {
            return Err(first);
        }
        match attempt(true) {
            Ok(v) => {
                self.integrated.store(true, SeqCst);
                Ok(v)
            }
            Err(e) => {
                let num = |e: &ExportError| {
                    e.sql_number
                        .map_or_else(|| "?".to_owned(), |n| n.to_string())
                };
                let mut both = ExportError::new(
                    stages::EXPORT,
                    codes::SQL_NO_ACCESS,
                    format!(
                        "SQL Server '{instance}' refused both logins: pda_sync with the stored password (error {}) and Windows integrated authentication of the agent's service account (error {}). Give one of them SELECT on the POS database.",
                        num(&first),
                        num(&e)
                    ),
                );
                both.sql_number = e.sql_number.or(first.sql_number);
                Err(both)
            }
        }
    }
}

// ---- SQL Server

/// [`LegacySqlSource`] over `dl_tds`: one connection per statement, like the C# source.
pub struct TdsSqlSource {
    config: dl_tds::Config,
    /// Windows integrated authentication (the service account), tried once when the
    /// primary login is refused.
    fallback: Option<dl_tds::Config>,
    failover: Failover,
    instance: String,
    database: String,
}

impl TdsSqlSource {
    /// A source for `database` on `instance` reached with `config` (`LegacySqlServerSource.ForSite`).
    pub fn new(config: dl_tds::Config, instance: &str, database: &str) -> TdsSqlSource {
        TdsSqlSource {
            config,
            fallback: None,
            failover: Failover::new(false),
            instance: instance.to_owned(),
            database: database.to_owned(),
        }
    }

    /// Adds the integrated-authentication retry for a refused primary login.
    pub fn with_fallback(mut self, fallback: dl_tds::Config) -> TdsSqlSource {
        self.fallback = Some(fallback);
        self.failover = Failover::new(true);
        self
    }

    fn failure(&self, e: &dl_tds::Error, client: Option<&mut Client>) -> ExportError {
        let messages = client.map(Client::take_messages).unwrap_or_default();
        match SqlFailure::from_tds(e, &messages) {
            Some(f) => explain_sql_failure(
                f.number(),
                f.class(),
                &f.message(),
                &self.instance,
                &self.database,
            ),
            None => ExportError::new(
                stages::EXPORT,
                codes::SQL_ERROR,
                format!("SQL Server '{}' could not be read.", self.instance),
            ),
        }
    }

    /// One statement on a fresh connection with the given login.
    fn attempt(
        &self,
        config: &dl_tds::Config,
        sql: &str,
        params: &[Param],
    ) -> Result<Option<dl_tds::ResultSet>> {
        let mut client = Client::connect(config).map_err(|e| self.failure(&e, None))?;
        client.take_messages();
        client
            .set_timeout(Some(STATEMENT_TIMEOUT))
            .map_err(|e| self.failure(&e, Some(&mut client)))?;
        let results = client
            .query(&format!("{PREAMBLE}{sql}"), params)
            .map_err(|e| self.failure(&e, Some(&mut client)))?;
        Ok(results.into_sets().into_iter().next())
    }

    /// Runs one statement and hands its first result set to `f`; a refused login, at
    /// connect or at query time, switches the run to integrated authentication.
    fn query<T>(
        &self,
        sql: &str,
        params: &[Param],
        f: impl FnOnce(dl_tds::ResultSet) -> Result<T>,
    ) -> Result<T> {
        let set = self.failover.run(&self.instance, |integrated| {
            let config = match (&self.fallback, integrated) {
                (Some(fallback), true) => fallback,
                _ => &self.config,
            };
            self.attempt(config, sql, params)
        })?;
        match set {
            Some(set) => f(set),
            None => Err(ExportError::new(
                stages::EXPORT,
                codes::SQL_ERROR,
                "SQL Server answered without a result set.",
            )),
        }
    }
}

fn text_at(r: &dl_tds::Row, i: usize) -> String {
    match r.values().get(i).map(RawValue::from_tds) {
        Some(RawValue::String(s)) => s,
        _ => String::new(),
    }
}

fn int_at(r: &dl_tds::Row, i: usize) -> i64 {
    match r.values().get(i).map(RawValue::from_tds) {
        Some(RawValue::I64(n)) => n,
        Some(RawValue::I32(n)) => i64::from(n),
        Some(RawValue::I16(n)) => i64::from(n),
        Some(RawValue::U8(n)) => i64::from(n),
        _ => 0,
    }
}

fn bracket(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

impl LegacySqlSource for TdsSqlSource {
    fn database(&self) -> String {
        self.database.clone()
    }

    fn rvc_list(&self) -> Result<Vec<(i32, String)>> {
        let list = self.query(
            "SELECT RVCNo, RVCName FROM dbo.DefRVCList ORDER BY RVCNo",
            &[],
            |set| {
                Ok(set
                    .rows()
                    .iter()
                    .map(|r| {
                        let no = i32::try_from(int_at(r, 0)).unwrap_or_default();
                        (no, text_at(r, 1).trim().to_owned())
                    })
                    .collect::<Vec<_>>())
            },
        )?;
        if list.is_empty() {
            return Err(ExportError::new(
                stages::EXPORT,
                codes::LEGACY_EXPORT_FAILED,
                format!(
                    "dbo.DefRVCList in {} is empty: there is no outlet to migrate.",
                    self.database
                ),
            ));
        }
        Ok(list)
    }

    fn pda_tables(&self) -> Result<Vec<(String, i64)>> {
        self.query(
            "SELECT s.name + N'.' + t.name, CAST(SUM(p.rows) AS bigint) \
             FROM sys.tables t \
             JOIN sys.schemas s ON s.schema_id = t.schema_id \
             JOIN sys.partitions p ON p.object_id = t.object_id AND p.index_id IN (0, 1) \
             WHERE (s.name = N'dbo' AND t.name LIKE N'Pda[_]%') OR s.name = N'pda' \
             GROUP BY s.name, t.name ORDER BY s.name, t.name",
            &[],
            |set| {
                Ok(set
                    .rows()
                    .iter()
                    .map(|r| (text_at(r, 0), int_at(r, 1)))
                    .collect())
            },
        )
    }

    fn columns(&self, qualified: &str) -> Result<Vec<String>> {
        let (schema, table) = split(qualified)?;
        self.query(
            "SELECT c.name FROM sys.columns c \
             JOIN sys.tables t ON t.object_id = c.object_id \
             JOIN sys.schemas s ON s.schema_id = t.schema_id \
             WHERE s.name = @schema AND t.name = @table ORDER BY c.column_id",
            &[Param::new("@schema", schema), Param::new("@table", table)],
            |set| Ok(set.rows().iter().map(|r| text_at(r, 0)).collect()),
        )
    }

    fn for_each_row(
        &self,
        qualified: &str,
        columns: &[String],
        sink: &mut dyn FnMut(Vec<dl_tds::Value>) -> Result<()>,
    ) -> Result<()> {
        let (schema, table) = split(qualified)?;
        let list = columns
            .iter()
            .map(|c| bracket(c))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {list} FROM {}.{}",
            bracket(&schema),
            bracket(&table)
        );
        self.query(&sql, &[], |set| {
            for r in set.rows() {
                sink(r.values().to_vec())?;
            }
            Ok(())
        })
        .map_err(|mut e| {
            e.table = Some(qualified.to_owned());
            e
        })
    }
}

fn split(qualified: &str) -> Result<(String, String)> {
    match qualified.split_once('.') {
        Some((s, t)) if !s.is_empty() && !t.is_empty() => Ok((s.to_owned(), t.to_owned())),
        _ => Err(ExportError::new(
            stages::EXPORT,
            codes::LEGACY_EXPORT_FAILED,
            "a table name is not schema-qualified",
        )),
    }
}

// ---- the print profile split (LegacyPrintProfileSplit)

/// The design columns a profile of the oldest shape carries; they move to the template.
fn design_columns() -> Vec<LegacySqlColumn> {
    [
        ("HeaderTpl", "header_tpl"),
        ("FooterTpl", "footer_tpl"),
        ("LayoutJson", "layout_json"),
    ]
    .into_iter()
    .map(|(source, target)| LegacySqlColumn {
        source: source.to_owned(),
        target: target.to_owned(),
        kind: LegacySqlKind::Raw,
        if_null: None,
        if_absent: None,
    })
    .collect()
}

const DESIGN_KEY: [&str; 5] = ["rvc", "doc_type", "header_tpl", "footer_tpl", "layout_json"];

/// Templates (one per distinct design of an outlet and document type) and the profiles
/// pointing at them.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileSplit {
    /// The synthesized `pda_print_template` rows.
    pub templates: Vec<Row>,
    /// The profile rows without design columns, each with its `template_id`.
    pub profiles: Vec<Row>,
}

/// A cell as text (`Convert.ToString`); empty for NULL.
pub fn cell_text(c: Option<&Cell>) -> String {
    match c {
        None | Some(Cell::Null) => String::new(),
        Some(Cell::Int(n)) => n.to_string(),
        Some(Cell::Real(f)) => f.to_string(),
        Some(Cell::Decimal(d)) => d.to_string(),
        Some(Cell::Text(s)) => s.clone(),
        Some(Cell::Blob(b)) => dl_encoding::hex::encode(b),
    }
}

fn profile_order(c: Option<&Cell>) -> i64 {
    match c {
        Some(Cell::Int(n)) => *n,
        Some(Cell::Text(s)) => s.trim().parse().unwrap_or(i64::MAX),
        _ => i64::MAX,
    }
}

struct Design {
    template_id: i64,
    first: Row,
    name: Option<String>,
    created_at: Option<String>,
}

impl Design {
    fn add(&mut self, profile: &Row) {
        let trim = |c| cell_text(c).trim_end_matches(' ').to_owned();
        let name = format!(
            "{} \u{2013} {}",
            trim(cell(profile, "doc_type")),
            trim(cell(profile, "name"))
        );
        if self
            .name
            .as_ref()
            .is_none_or(|n| name.as_str() > n.as_str())
        {
            self.name = Some(name);
        }
        if let Some(Cell::Text(created)) = cell(profile, "created_at")
            && self
                .created_at
                .as_ref()
                .is_none_or(|c| created.as_str() < c.as_str())
        {
            self.created_at = Some(created.clone());
        }
    }

    fn row(&self) -> Row {
        let first = |c: &str| cell(&self.first, c).cloned().unwrap_or(Cell::Null);
        let opt = |s: &Option<String>| s.clone().map_or(Cell::Null, Cell::Text);
        vec![
            ("template_id".to_owned(), Cell::Int(self.template_id)),
            ("rvc".to_owned(), first("rvc")),
            ("name".to_owned(), opt(&self.name)),
            ("doc_type".to_owned(), first("doc_type")),
            ("header_tpl".to_owned(), first("header_tpl")),
            ("footer_tpl".to_owned(), first("footer_tpl")),
            ("layout_json".to_owned(), first("layout_json")),
            ("is_active".to_owned(), Cell::Int(1)),
            ("created_at".to_owned(), opt(&self.created_at)),
        ]
    }
}

/// Splits profile rows (in `pda.db` column names, each with its design columns).
pub fn split_print_profiles(mut profiles: Vec<Row>) -> ProfileSplit {
    profiles.sort_by_key(|r| profile_order(cell(r, "profile_id")));
    let design = design_columns();
    let mut designs: Vec<Design> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::with_capacity(profiles.len());
    for row in &profiles {
        let key = DESIGN_KEY
            .iter()
            .map(|c| match cell(row, c) {
                None | Some(Cell::Null) => "~".to_owned(),
                other => format!("={}", cell_text(other)),
            })
            .collect::<Vec<_>>()
            .join("\u{1f}");
        let index = *by_key.entry(key).or_insert_with(|| {
            designs.push(Design {
                template_id: i64::try_from(designs.len() + 1).unwrap_or(i64::MAX),
                first: row.clone(),
                name: None,
                created_at: None,
            });
            designs.len() - 1
        });
        designs[index].add(row);
        let mut profile: Row = row
            .iter()
            .filter(|(c, _)| design.iter().all(|d| d.target != *c))
            .cloned()
            .collect();
        profile.push((
            "template_id".to_owned(),
            Cell::Int(designs[index].template_id),
        ));
        out.push(profile);
    }
    ProfileSplit {
        templates: designs.iter().map(Design::row).collect(),
        profiles: out,
    }
}

// ---- the SQL Server store (LegacySqlStoreSource)

struct Resolved {
    map: &'static LegacySqlTable,
    qualified: String,
    columns: Vec<String>,
    rows: i64,
}

impl Resolved {
    /// The table's own spelling of a mapped column; `None` when this build lacks it.
    fn actual(&self, column: &str) -> Option<&str> {
        self.columns
            .iter()
            .find(|c| eq_ignore_case(c, column))
            .map(String::as_str)
    }
}

/// The store of an on-prem build from before `pda.db`: this app's own tables in SQL
/// Server, `dbo.Pda_*` (1.x) or schema `pda` (ADR 0094), read with SELECT only and handed
/// out as the rows the first `pda.db` build would have moved across.
pub struct LegacySqlStoreSource {
    sql: Arc<dyn LegacySqlSource>,
    zone: Arc<TimeZone>,
    tables: Vec<Resolved>,
    duplicates: HashMap<String, String>,
    split_profile: Option<usize>,
    split: RefCell<Option<ProfileSplit>>,
}

fn schema_of(qualified: &str) -> &str {
    qualified.split('.').next().unwrap_or("")
}

fn failed(message: String) -> ExportError {
    ExportError::new(stages::EXPORT, codes::LEGACY_EXPORT_FAILED, message)
}

impl LegacySqlStoreSource {
    /// Resolves the tables of `sql` against the store map. `zone` is the site's time zone,
    /// for the few columns the oldest builds stamped in local time.
    pub fn open(
        sql: Arc<dyn LegacySqlSource>,
        zone: Arc<TimeZone>,
    ) -> Result<LegacySqlStoreSource> {
        let database = sql.database();
        let found: Vec<(String, i64)> = sql
            .pda_tables()?
            .into_iter()
            .filter(|(t, _)| is_business_sql_table(t))
            .collect();
        let mut tables = Vec::new();
        let mut duplicates = HashMap::new();
        for map in sql_store::tables() {
            let candidates: Vec<&(String, i64)> = found
                .iter()
                .filter(|(t, _)| eq_ignore_case(unqualified(t), &map.source))
                .collect();
            if candidates.is_empty() {
                continue;
            }
            let with_rows: Vec<&&(String, i64)> =
                candidates.iter().filter(|(_, r)| *r > 0).collect();
            if with_rows.len() > 1 {
                let names = with_rows
                    .iter()
                    .map(|(t, r)| format!("{t}: {r} rows"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(failed(format!(
                    "{database} holds rows of {} in more than one schema ({names}). The export cannot tell which one the site uses; nothing was written.",
                    map.source
                )));
            }
            // Rows decide; between two empty tables the later schema (pda) is the one the
            // build last used.
            let chosen: &(String, i64) = match with_rows.first() {
                Some(w) => w,
                None => {
                    let mut sorted = candidates.clone();
                    sorted.sort_by_key(|(t, _)| eq_ignore_case(schema_of(t), "pda"));
                    sorted[sorted.len() - 1]
                }
            };
            for other in candidates.iter().filter(|(t, _)| *t != chosen.0) {
                duplicates.insert(
                    other.0.clone(),
                    format!("empty copy of {}, which was exported", chosen.0),
                );
            }
            tables.push(Resolved {
                map,
                qualified: chosen.0.clone(),
                columns: sql.columns(&chosen.0)?,
                rows: chosen.1,
            });
        }
        if tables.is_empty() {
            return Err(ExportError::new(
                stages::EXPORT,
                codes::LEGACY_STORE_MISSING,
                format!(
                    "from-sql: {database} holds none of this app's tables from before the pda.db store (dbo.Pda_* or schema pda). A site on build 3.11 or later keeps its data in pda.db: order the export with source pda-db. If this is the wrong database, order it with database <name>."
                ),
            ));
        }
        let split_profile = tables
            .iter()
            .position(|t| t.map.source == PRINT_PROFILE_SOURCE && t.actual("TemplateId").is_none());
        if let Some(i) = split_profile
            && tables.iter().any(|t| t.map.target == PRINT_TEMPLATE_TARGET)
        {
            return Err(failed(format!(
                "{} has no TemplateId column, yet {database} has a print template table: a half-finished upgrade this export does not know. Nothing was written; keep the database backup and report this site.",
                tables[i].qualified
            )));
        }
        Ok(LegacySqlStoreSource {
            sql,
            zone,
            tables,
            duplicates,
            split_profile,
            split: RefCell::new(None),
        })
    }

    fn find(&self, table: &str) -> Result<&Resolved> {
        self.tables
            .iter()
            .find(|t| t.map.target == table)
            .ok_or_else(|| failed(format!("'{table}' is not a table of this store.")))
    }

    fn is_synthesized_template(&self, table: &str) -> bool {
        self.split_profile.is_some() && table == PRINT_TEMPLATE_TARGET
    }

    fn split_result(&self, profile: &Resolved) -> Result<()> {
        if self.split.borrow().is_some() {
            return Ok(());
        }
        let design = design_columns();
        let columns: Vec<LegacySqlColumn> = profile
            .map
            .columns
            .iter()
            .filter(|c| design.iter().all(|d| d.target != c.target))
            .chain(design.iter())
            .cloned()
            .collect();
        let mut rows = Vec::new();
        self.read(profile, &columns, &mut |row| {
            rows.push(row);
            Ok(())
        })?;
        *self.split.borrow_mut() = Some(split_print_profiles(rows));
        Ok(())
    }

    /// The rows of one SQL Server table in `pda.db` shape. A mapped column the build does
    /// not have yet is left out of the row (the cloud default applies) unless the map says
    /// what the later build wrote for the rows it found.
    fn read(&self, table: &Resolved, columns: &[LegacySqlColumn], sink: RowSink<'_>) -> Result<()> {
        let present: Vec<(&LegacySqlColumn, &str)> = columns
            .iter()
            .filter_map(|c| table.actual(&c.source).map(|a| (c, a)))
            .collect();
        let absent: Vec<&LegacySqlColumn> = columns
            .iter()
            .filter(|c| table.actual(&c.source).is_none() && c.if_absent.is_some())
            .collect();
        if present.is_empty() {
            return Err(failed(format!(
                "{} has none of the columns this export knows for {}; nothing was written.",
                table.qualified, table.map.source
            ))
            .at_table(&table.map.target));
        }
        let names: Vec<String> = present.iter().map(|(_, a)| (*a).to_owned()).collect();
        self.sql
            .for_each_row(&table.qualified, &names, &mut |values| {
                let mut row: Row =
                    Vec::with_capacity(present.len() + absent.len() + table.map.constants.len());
                for ((column, _), value) in present.iter().zip(values.iter()) {
                    let mut c = convert(column.kind, value, &self.zone);
                    if c == Cell::Null
                        && let Some(fallback) = &column.if_null
                    {
                        c = legacy_cell(fallback);
                    }
                    row.push((column.target.clone(), c));
                }
                for column in &absent {
                    if let Some(v) = &column.if_absent {
                        row.push((column.target.clone(), legacy_cell(v)));
                    }
                }
                for (name, v) in table.map.constants.iter() {
                    row.push((name.to_owned(), legacy_cell(v)));
                }
                sink(row)
            })
    }
}

impl LegacyStore for LegacySqlStoreSource {
    fn kind(&self) -> Option<String> {
        let mut schemas: Vec<&str> = self
            .tables
            .iter()
            .map(|t| schema_of(&t.qualified))
            .collect();
        schemas.sort_unstable();
        schemas.dedup();
        Some(format!("sqlserver:{}", schemas.join("+")))
    }

    fn tables(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for (i, t) in self.tables.iter().enumerate() {
            // The template a profile points at comes first, as in a store that always had
            // templates.
            if self.split_profile == Some(i) {
                names.push(PRINT_TEMPLATE_TARGET.to_owned());
            }
            names.push(t.map.target.clone());
        }
        Ok(names)
    }

    fn count(&self, table: &str) -> Result<i64> {
        match self.split_profile {
            Some(i) if self.is_synthesized_template(table) => {
                self.split_result(&self.tables[i])?;
                let n = self
                    .split
                    .borrow()
                    .as_ref()
                    .map_or(0, |s| s.templates.len());
                Ok(i64::try_from(n).unwrap_or(i64::MAX))
            }
            _ => Ok(self.find(table)?.rows),
        }
    }

    fn schema_version(&self) -> Result<Option<i32>> {
        Ok(None)
    }

    fn for_each_row(&self, table: &str, sink: RowSink<'_>) -> Result<()> {
        if let Some(i) = self.split_profile
            && (self.is_synthesized_template(table) || table == self.tables[i].map.target)
        {
            self.split_result(&self.tables[i])?;
            let guard = self.split.borrow();
            let split = guard
                .as_ref()
                .ok_or_else(|| failed("print split missing".to_owned()))?;
            let rows = if self.is_synthesized_template(table) {
                &split.templates
            } else {
                &split.profiles
            };
            for row in rows {
                sink(row.clone())?;
            }
            return Ok(());
        }
        let resolved = self.find(table)?;
        self.read(resolved, &resolved.map.columns, sink)
    }

    fn sql_table_left_out(&self, qualified: &str) -> Option<String> {
        if self.tables.iter().any(|t| t.qualified == qualified) {
            return None;
        }
        if let Some(b) = bookkeeping_reason(qualified) {
            return Some(b.to_owned());
        }
        if let Some(d) = self.duplicates.get(qualified) {
            return Some(d.clone());
        }
        Some(
            sql_store::never_exported_reason(unqualified(qualified))
                .unwrap_or("not a table this export knows: review it if it has rows")
                .to_owned(),
        )
    }

    fn sql_table_needs_review(&self, qualified: &str) -> bool {
        is_business_sql_table(qualified)
            && self.tables.iter().all(|t| t.qualified != qualified)
            && !self.duplicates.contains_key(qualified)
            && sql_store::never_exported_reason(unqualified(qualified)).is_none()
    }
}
