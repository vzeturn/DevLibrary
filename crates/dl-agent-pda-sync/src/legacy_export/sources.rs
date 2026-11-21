//! What the export reads and where: the row and error types, the store and SQL Server
//! seams, the legacy registry (brand and VietQR, ADR 0099), the IIS app pool probe and
//! the two gates (maintenance, store) of `legacy-export` (`LegacyExport/*.cs`).
//!
//! Everything here is read-only: SELECTs, registry reads and `appcmd list`.

use std::path::Path;

use dl_decimal::{Decimal, RoundingMode};

use crate::config::codes;
use crate::contracts::legacy::{
    LegacyBuildStanding, LegacyBundleMaintenance, LegacySqlKind, LegacyValue, builds, bundle,
};
use crate::contracts::mapping::RawValue;
use crate::contracts::time::{DateTime, DateTimeKind, TimeZone, time_span_text};

/// One exported value (a `pda.db` cell, or what a SQL Server value becomes).
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    /// NULL.
    Null,
    /// An integer.
    Int(i64),
    /// A float.
    Real(f64),
    /// An exact decimal.
    Decimal(Decimal),
    /// Text.
    Text(String),
    /// Bytes (written as a PostgreSQL `bytea` hex literal).
    Blob(Vec<u8>),
}

/// One row: (column, value) pairs keyed by the `pda.db` column names.
pub type Row = Vec<(String, Cell)>;

/// The value of `column` in `row`.
pub fn cell<'r>(row: &'r Row, column: &str) -> Option<&'r Cell> {
    row.iter().find(|(c, _)| c == column).map(|(_, v)| v)
}

/// A failed or refused stage of the run. Never carries row data: `table`, `sql_number` and
/// `code` are what the failure site event reports.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportError {
    /// The stage that failed (see [`stages`]).
    pub stage: &'static str,
    /// A stable code (`config::codes`, or `HTTP_<status>` / the cloud's refusal code).
    pub code: String,
    /// What the operator reads (names of instances, databases, tables; no row values).
    pub message: String,
    /// The table being read or written.
    pub table: Option<String>,
    /// The SQL Server error number.
    pub sql_number: Option<i32>,
    /// A refusal the operator fixes (maintenance gate, cloud refusal), not a failure.
    pub refusal: bool,
}

/// The stages of a run, as the failure event names them.
pub mod stages {
    /// Payload, guard, SQL probe and gate: before the command is accepted.
    pub const PRECHECK: &str = "precheck";
    /// Reading the store into the bundle.
    pub const EXPORT: &str = "export";
    /// Verifying and zipping the bundle.
    pub const ZIP: &str = "zip";
    /// The dry-run upload.
    pub const DRY_RUN: &str = "dry_run";
    /// The committing upload.
    pub const COMMIT: &str = "commit";
    /// The dry run after the commit.
    pub const POST_COMMIT: &str = "post_commit_dry_run";
    /// Deleting the local bundle and zip.
    pub const CLEANUP: &str = "cleanup";
}

impl ExportError {
    /// A failure of `stage`.
    pub fn new(stage: &'static str, code: &str, message: impl Into<String>) -> ExportError {
        ExportError {
            stage,
            code: code.to_owned(),
            message: message.into(),
            table: None,
            sql_number: None,
            refusal: false,
        }
    }

    /// A refusal the operator fixes.
    pub fn refused(stage: &'static str, code: &str, message: impl Into<String>) -> ExportError {
        ExportError {
            refusal: true,
            ..ExportError::new(stage, code, message)
        }
    }

    /// Names the table being processed.
    pub fn at_table(mut self, table: &str) -> ExportError {
        self.table = Some(table.to_owned());
        self
    }

    /// An I/O failure while exporting.
    pub fn io(stage: &'static str, what: &str, e: &std::io::Error) -> ExportError {
        ExportError::new(
            stage,
            codes::LEGACY_EXPORT_FAILED,
            format!("{what}: {} ({e})", io_kind(e)),
        )
    }
}

fn io_kind(e: &std::io::Error) -> &'static str {
    match e.kind() {
        std::io::ErrorKind::NotFound => "not found",
        std::io::ErrorKind::PermissionDenied => "access denied",
        _ => "I/O error",
    }
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for ExportError {}

/// Result of a stage.
pub type Result<T> = std::result::Result<T, ExportError>;

/// Called once per row; an error stops the table.
pub type RowSink<'a> = &'a mut dyn FnMut(Row) -> Result<()>;

/// Where the on-prem app kept its own tables: `pda.db` (builds from 3.11 on) or SQL Server
/// (the builds before it, ADR 0094). Either way the export sees `pda.db`-shaped tables
/// and rows (`ILegacyStore`).
pub trait LegacyStore {
    /// `None` for `pda.db`; `sqlserver:dbo` / `sqlserver:pda` when read from SQL Server.
    fn kind(&self) -> Option<String>;
    /// The store's tables by their `pda.db` names.
    fn tables(&self) -> Result<Vec<String>>;
    /// Row count of `table`.
    fn count(&self, table: &str) -> Result<i64>;
    /// Every row of `table`, in rowid order where the table has one.
    fn for_each_row(&self, table: &str, sink: RowSink<'_>) -> Result<()>;
    /// The `pda.db` schema version; `None` when the store has none.
    fn schema_version(&self) -> Result<Option<i32>>;
    /// Why a schema-qualified SQL Server table is not in the bundle; `None` when exported.
    fn sql_table_left_out(&self, qualified: &str) -> Option<String>;
    /// A SQL Server table whose rows nobody accounted for: the operator must look at it.
    fn sql_table_needs_review(&self, qualified: &str) -> bool;
}

/// The SELECTs the export runs on `SMILE_POS` (`ILegacySqlSource`).
pub trait LegacySqlSource: Send + Sync {
    /// The database name.
    fn database(&self) -> String;
    /// `dbo.DefRVCList`: the outlets that become the tenant's branches (`RVCNo`, name).
    fn rvc_list(&self) -> Result<Vec<(i32, String)>>;
    /// The tables this app once kept in SQL Server, schema-qualified, with row counts.
    fn pda_tables(&self) -> Result<Vec<(String, i64)>>;
    /// Column names of one of [`pda_tables`](Self::pda_tables), in table order.
    fn columns(&self, qualified: &str) -> Result<Vec<String>>;
    /// Every row of one of [`pda_tables`](Self::pda_tables), one value per column.
    fn for_each_row(
        &self,
        qualified: &str,
        columns: &[String],
        sink: &mut dyn FnMut(Vec<dl_tds::Value>) -> Result<()>,
    ) -> Result<()>;
}

/// True for a schema-qualified SQL Server table that once held this app's business data.
pub fn is_business_sql_table(qualified: &str) -> bool {
    (qualified.starts_with("dbo.") || qualified.starts_with("pda."))
        && bookkeeping_reason(qualified).is_none()
}

/// The table name without its schema.
pub fn unqualified(qualified: &str) -> &str {
    qualified.rsplit('.').next().unwrap_or(qualified)
}

/// Why a SQL Server table is bookkeeping and never business data.
pub fn bookkeeping_reason(qualified: &str) -> Option<&'static str> {
    match unqualified(qualified) {
        "Pda_MigrationHistory" => Some("legacy schema runner bookkeeping"),
        "Pda_SyncCommandLog" | "Pda_SyncState" => Some("the SyncAgent's own state"),
        _ => None,
    }
}

/// Why a build that keeps its data in `pda.db` exports nothing of a SQL Server table.
pub fn sql_table_reason(qualified: &str) -> String {
    bookkeeping_reason(qualified).map_or_else(
        || {
            "SQL Server table from before the pda.db store; builds from 3.11 on read pda.db. Review it if it has rows"
                .to_owned()
        },
        str::to_owned,
    )
}

pub use super::bundle::{
    BRAND_NAMES, BrandingSource, MachineRegistry, ProtectedRegistry, REGISTRY_SUB_KEY,
    RegistryVietQr, VIETQR_NAMES, VietQrRead, VietQrSource, machine_vietqr,
};

// ---- IIS app pool and the maintenance gate

/// The state of an IIS application pool (`Started`, `Stopped`, ...); `None` when unknown.
pub trait AppPoolProbe: Send + Sync {
    /// The state of `pool`.
    fn state(&self, pool: &str) -> std::result::Result<Option<String>, String>;
}

/// `appcmd list apppool` (read-only); nothing off Windows.
#[derive(Debug, Default, Clone, Copy)]
pub struct AppCmdPoolProbe;

impl AppPoolProbe for AppCmdPoolProbe {
    fn state(&self, pool: &str) -> std::result::Result<Option<String>, String> {
        if !cfg!(windows) {
            return Ok(None);
        }
        let windir = std::env::var_os("windir").unwrap_or_else(|| r"C:\Windows".into());
        let appcmd = Path::new(&windir).join(r"system32\inetsrv\appcmd.exe");
        if !appcmd.is_file() {
            return Ok(None);
        }
        let out = std::process::Command::new(&appcmd)
            .args([
                "list",
                "apppool",
                &format!("/apppool.name:{pool}"),
                "/text:state",
            ])
            .output()
            .map_err(|e| format!("appcmd.exe did not start: {}", io_kind(&e)))?;
        if !out.status.success() {
            return Err(format!("appcmd could not read app pool '{pool}'"));
        }
        let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        Ok((!text.is_empty()).then_some(text))
    }
}

/// The file whose presence in the site root puts an IIS site in maintenance.
pub const OFFLINE_FILE: &str = "app_offline.htm";

/// Refuses a cutover export while the legacy site can still write: it needs
/// `app_offline.htm` in the site root or a stopped app pool. `allow_live` exports anyway
/// and marks the bundle as a rehearsal (`MaintenanceGate.CheckAsync`). Where the gate cannot
/// be evaluated (no site root and no pool named, or an unreadable pool) it counts as closed
/// and the message says so.
pub fn maintenance_gate(
    site_root: Option<&str>,
    app_pool: Option<&str>,
    allow_live: bool,
    probe: &dyn AppPoolProbe,
) -> Result<LegacyBundleMaintenance> {
    let mut evidence = Vec::new();
    let mut unevaluated = Vec::new();
    if let Some(root) = site_root {
        if !Path::new(root).is_dir() {
            return Err(ExportError::refused(
                stages::PRECHECK,
                codes::BAD_ARGUMENTS,
                format!("site_root '{root}' does not exist on this machine."),
            ));
        }
        if Path::new(root).join(OFFLINE_FILE).is_file() {
            evidence.push(format!("{OFFLINE_FILE} present in {root}"));
        }
    }
    if let Some(pool) = app_pool {
        match probe.state(pool) {
            Ok(Some(s)) if s.eq_ignore_ascii_case("Stopped") => {
                evidence.push(format!("app pool '{pool}' is Stopped"));
            }
            Ok(Some(_)) => {}
            Ok(None) => unevaluated.push(format!(
                "the state of app pool '{pool}' is not available here"
            )),
            Err(e) => unevaluated.push(e),
        }
    }
    if !evidence.is_empty() {
        return Ok(LegacyBundleMaintenance {
            mode: bundle::MAINTENANCE_MODE.to_owned(),
            evidence: evidence.join("; "),
        });
    }
    if allow_live {
        return Ok(LegacyBundleMaintenance {
            mode: bundle::LIVE_REHEARSAL_MODE.to_owned(),
            evidence:
                "allow_live: the site may still be writing; this bundle is for a rehearsal only"
                    .to_owned(),
        });
    }
    let why = if site_root.is_none() && app_pool.is_none() {
        "the command names no site_root or app_pool, so the gate cannot be evaluated and counts as closed".to_owned()
    } else if unevaluated.is_empty() {
        String::new()
    } else {
        format!("{}; the gate counts as closed", unevaluated.join("; "))
    };
    Err(ExportError::refused(
        stages::PRECHECK,
        codes::LEGACY_SITE_LIVE,
        format!(
            "The legacy site is not in maintenance. Put {OFFLINE_FILE} in the site root (site_root) or stop the app pool (app_pool), or order allow_live for a dry rehearsal.{}",
            if why.is_empty() {
                String::new()
            } else {
                format!(" ({why})")
            }
        ),
    ))
}

// ---- the store gate

const ELSEWHERE: &str = "The on-prem app keeps it under %ProgramData%\\MM-PDA on the web server. When SQL Server is another machine, run the export on the web server with sql_instance <host>, or copy pda.db (with pda.db-wal and pda.db-shm, the site in maintenance) to this machine.";

/// Refuses from-sql where the site's data is in `pda.db`: the SQL Server tables are not the
/// truth there (`LegacyStoreGate.RequireSqlStore`).
pub fn require_sql_store(pda_db: &Path, app_version: Option<&str>) -> Result<()> {
    if pda_db.is_file() {
        return Err(ExportError::new(
            stages::PRECHECK,
            codes::BAD_ARGUMENTS,
            format!(
                "from-sql is for a site from before the pda.db store, but pda.db exists at '{}': this site keeps its data there. Order the export with source pda-db or auto.",
                pda_db.display()
            ),
        ));
    }
    if let Some((a, b, c)) = builds::parse(app_version)
        && builds::standing(app_version) != LegacyBuildStanding::Unverified
    {
        return Err(ExportError::new(
            stages::PRECHECK,
            codes::BAD_ARGUMENTS,
            format!(
                "from-sql is for a build older than {}, but the site in site_root runs {a}.{b}.{c}, which keeps its data in pda.db. Order the export with source pda-db.",
                builds::OLDEST_VERIFIED
            ),
        ));
    }
    Ok(())
}

/// The refusal when `pda.db` is missing (`LegacyStoreGate.Explain`): what SQL Server holds
/// says whether the site runs a build from before the store.
pub fn explain_missing_store(pda_db: &Path, database: &str, tables: &[(String, i64)]) -> String {
    let with_rows: Vec<&(String, i64)> = tables
        .iter()
        .filter(|(t, rows)| *rows > 0 && is_business_sql_table(t))
        .collect();
    let in_schema = |schema: &str| -> Vec<&(String, i64)> {
        with_rows
            .iter()
            .copied()
            .filter(|(t, _)| t.starts_with(&format!("{schema}.")))
            .collect()
    };
    let count = |l: &[&(String, i64)]| {
        if l.len() == 1 {
            "1 table".to_owned()
        } else {
            format!("{} tables", l.len())
        }
    };
    let sample = |l: &[&(String, i64)]| {
        let mut l = l.to_vec();
        l.sort_by_key(|(_, r)| std::cmp::Reverse(*r));
        l.iter()
            .take(3)
            .map(|(t, r)| format!("{t}: {r} rows"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let shown = pda_db.display();
    let dbo = in_schema("dbo");
    let pda = in_schema("pda");
    if !dbo.is_empty() {
        format!(
            "pda.db not found at '{shown}', and {database} holds this site's data in {} ({}): the site runs a 1.x build. Do NOT install a newer on-prem build over it: builds from 3.11 on delete those tables without moving their rows. Take a full backup of {database}, then order the export with source from-sql: it reads those tables where they are.",
            count(&dbo),
            sample(&dbo)
        )
    } else if !pda.is_empty() {
        format!(
            "pda.db not found at '{shown}', and {database} holds this site's data in {} of schema pda ({}): the site runs a build from before the pda.db store. Take a full backup of {database}, then order the export with source from-sql: it reads those tables where they are. No newer on-prem build is needed.",
            count(&pda),
            sample(&pda)
        )
    } else {
        format!(
            "pda.db not found at '{shown}'. {ELSEWHERE} {database} holds no data of an older build of this app."
        )
    }
}

const UNTRUSTED_DOMAIN: i32 = 18452;

// ---- SqlFailure

/// Turns a SQL Server error of the export into a failure the operator can act on
/// (`LegacySqlFailure.Explain`). Server text is kept only where it cannot quote row data.
pub fn explain_sql_failure(
    number: i32,
    severity: u8,
    message: &str,
    instance: &str,
    database: &str,
) -> ExportError {
    let code = crate::capture::failure::code_for(number, severity);
    let trim = |m: &str| m.trim().trim_end_matches('.').to_owned();
    let text = match code {
        codes::SQL_UNREACHABLE => format!(
            "SQL Server '{instance}' cannot be reached (error {number}: {}). When SQL Server is on another machine order the export with sql_instance <host>\\<instance> or <host>,<port>: TCP/IP must be enabled there, the firewall must allow its port (1433 by default) and a named instance needs the SQL Browser service (UDP 1434).",
            trim(message)
        ),
        codes::SQL_LOGIN_FAILED if number == UNTRUSTED_DOMAIN => format!(
            "SQL Server '{instance}' does not trust this machine's Windows accounts (error {number}); the agent signs in as pda_sync with its stored password."
        ),
        codes::SQL_LOGIN_FAILED => format!(
            "SQL Server '{instance}' refused the agent's pda_sync login (error {number}). Check that the login exists there and that the server allows SQL Server authentication (mixed mode); re-run enroll when the password changed."
        ),
        codes::SQL_DATABASE_NOT_FOUND => format!(
            "Database '{database}' cannot be opened on '{instance}' (error {number}). Order the export with database <name> for the POS database, or give pda_sync access to it."
        ),
        codes::SQL_SCHEMA_MISMATCH => format!(
            "'{database}' on '{instance}' is not the POS database (error {number}: {}). Order the export with database <name>.",
            trim(message)
        ),
        codes::SQL_NO_ACCESS => format!(
            "The pda_sync login may not read '{database}' on '{instance}' (error {number}: {}). It needs SELECT on dbo.DefRVCList.",
            trim(message)
        ),
        // Other errors can quote row values: the number is all that is kept.
        _ => format!("SQL Server error {number} reading '{database}' on '{instance}'."),
    };
    let mut e = ExportError::new(stages::EXPORT, code, text);
    e.sql_number = Some(number);
    e
}

// ---- values (LegacySqlValues)

fn stamp_parts(p: (i32, u32, u32, u32, u32, u32, i64)) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        p.0, p.1, p.2, p.3, p.4, p.5
    )
}

fn date_parts(p: (i32, u32, u32, u32, u32, u32, i64)) -> String {
    format!("{:04}-{:02}-{:02}", p.0, p.1, p.2)
}

fn stamp(v: &RawValue) -> String {
    match v {
        RawValue::DateTime(d) => stamp_parts(d.parts()),
        RawValue::DateTimeOffset(o) => stamp_parts(o.to_utc().utc_date_time().parts()),
        other => text(other),
    }
}

/// A wall-clock time of the site as UTC. A time the zone skips (a clock change) moves by
/// the standard offset.
fn local_to_utc(local: DateTime, zone: &TimeZone) -> DateTime {
    let t = local.ticks();
    let utc = if zone.is_invalid_time(t) {
        t - zone.base_utc_offset_ticks()
    } else {
        zone.convert_time_to_utc(t)
            .unwrap_or(t - zone.base_utc_offset_ticks())
    };
    DateTime::from_ticks(utc, DateTimeKind::Utc).unwrap_or(local)
}

fn number(v: &RawValue) -> Option<Decimal> {
    match v {
        RawValue::Decimal(d) => Some(*d),
        RawValue::Bool(b) => Some(Decimal::from(i32::from(*b))),
        RawValue::U8(n) => Some(Decimal::from(*n)),
        RawValue::I8(n) => Some(Decimal::from(*n)),
        RawValue::I16(n) => Some(Decimal::from(*n)),
        RawValue::U16(n) => Some(Decimal::from(*n)),
        RawValue::I32(n) => Some(Decimal::from(*n)),
        RawValue::U32(n) => Some(Decimal::from(*n)),
        RawValue::I64(n) => Some(Decimal::from(*n)),
        RawValue::U64(n) => Some(Decimal::from(*n)),
        RawValue::F64(f) if f.is_finite() => f.to_string().parse().ok(),
        RawValue::F32(f) if f.is_finite() => f.to_string().parse().ok(),
        RawValue::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `Convert.ToString(value, InvariantCulture)` with the stamp format for dates.
fn text(v: &RawValue) -> String {
    match v {
        RawValue::Null => String::new(),
        RawValue::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        RawValue::U8(n) => n.to_string(),
        RawValue::I8(n) => n.to_string(),
        RawValue::I16(n) => n.to_string(),
        RawValue::U16(n) => n.to_string(),
        RawValue::I32(n) => n.to_string(),
        RawValue::U32(n) => n.to_string(),
        RawValue::I64(n) => n.to_string(),
        RawValue::U64(n) => n.to_string(),
        RawValue::F32(f) => f.to_string(),
        RawValue::F64(f) => f.to_string(),
        RawValue::Decimal(d) => d.to_string(),
        RawValue::String(s) => s.clone(),
        RawValue::Bytes(b) => dl_encoding::hex::encode(b),
        RawValue::Guid(g) => g.to_string(),
        RawValue::DateTime(_) | RawValue::DateTimeOffset(_) => stamp(v),
        RawValue::TimeSpan(t) => time_span_text(*t),
    }
}

fn raw_cell(v: &RawValue) -> Cell {
    match v {
        RawValue::Null => Cell::Null,
        RawValue::Bool(b) => Cell::Int(i64::from(*b)),
        RawValue::U8(n) => Cell::Int(i64::from(*n)),
        RawValue::I8(n) => Cell::Int(i64::from(*n)),
        RawValue::I16(n) => Cell::Int(i64::from(*n)),
        RawValue::U16(n) => Cell::Int(i64::from(*n)),
        RawValue::I32(n) => Cell::Int(i64::from(*n)),
        RawValue::U32(n) => Cell::Int(i64::from(*n)),
        RawValue::I64(n) => Cell::Int(*n),
        RawValue::U64(n) => i64::try_from(*n).map_or_else(|_| Cell::Text(n.to_string()), Cell::Int),
        RawValue::F32(f) => Cell::Real(f64::from(*f)),
        RawValue::F64(f) => Cell::Real(*f),
        RawValue::Decimal(d) => Cell::Decimal(*d),
        RawValue::String(s) => Cell::Text(s.clone()),
        RawValue::Bytes(b) => Cell::Blob(b.clone()),
        other => Cell::Text(text(other)),
    }
}

/// Turns one SQL Server value into what the on-prem app's own move into `pda.db` stored
/// for it: numbers stay numbers, dates become ISO text, money fixed-point text. A value of
/// a type the map did not expect is kept as text, never dropped.
pub fn convert(kind: LegacySqlKind, value: &dl_tds::Value, zone: &TimeZone) -> Cell {
    let raw = RawValue::from_tds(value);
    if matches!(raw, RawValue::Null) {
        return Cell::Null;
    }
    let fixed = |dp: usize| match number(&raw) {
        Some(n) => Cell::Text(format!("{n:.dp$}")),
        None => Cell::Text(text(&raw)),
    };
    match kind {
        LegacySqlKind::Trim => Cell::Text(text(&raw).trim_end_matches(' ').to_owned()),
        LegacySqlKind::Flag => Cell::Int(flag(&raw)),
        LegacySqlKind::Stamp => Cell::Text(stamp(&raw)),
        LegacySqlKind::LocalStamp => Cell::Text(match &raw {
            RawValue::DateTime(d) => stamp_parts(local_to_utc(*d, zone).parts()),
            other => stamp(other),
        }),
        LegacySqlKind::Date => Cell::Text(match &raw {
            RawValue::DateTime(d) => date_parts(d.parts()),
            other => text(other),
        }),
        LegacySqlKind::Fixed0 => fixed(0),
        LegacySqlKind::Fixed2 => fixed(2),
        LegacySqlKind::Number | LegacySqlKind::Text => Cell::Text(text(&raw)),
        LegacySqlKind::Truncated => match number(&raw) {
            Some(n) => n
                .round_dp(0, RoundingMode::Down)
                .to_i64()
                .map_or_else(|| Cell::Text(text(&raw)), Cell::Int),
            None => Cell::Text(text(&raw)),
        },
        LegacySqlKind::Blob => match &raw {
            RawValue::Bytes(b) => Cell::Blob(b.clone()),
            other => raw_cell(other),
        },
        LegacySqlKind::Raw => raw_cell(&raw),
    }
}

fn flag(v: &RawValue) -> i64 {
    match v {
        RawValue::Bool(b) => i64::from(*b),
        RawValue::String(s) => {
            let t = s.trim();
            i64::from(t == "1" || t.eq_ignore_ascii_case("true"))
        }
        other => i64::from(number(other).is_some_and(|n| n != Decimal::ZERO)),
    }
}

/// What a fixed value of the store map is as a cell.
pub fn legacy_cell(v: &LegacyValue) -> Cell {
    match v {
        LegacyValue::Str(s) => Cell::Text((*s).to_owned()),
        LegacyValue::Int(n) => Cell::Int(*n),
    }
}
