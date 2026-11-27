//! SQL Server Change Tracking for the captured `SMILE_POS` tables (ADR 0011 D2, plan row 8,
//! RT-1318): what the probe reads, which steps turn it on, and their SQL text.
//!
//! Change Tracking is enabled first on the database, then on each table that has a primary
//! key, and the reader needs `VIEW CHANGE TRACKING` on each table
//! (<https://learn.microsoft.com/en-us/sql/relational-databases/track-changes/enable-and-disable-change-tracking-sql-server>).
//! The statements follow the documented syntax, which SQL Server 2008 R2 to 2022 (Express
//! included) accept: `ALTER DATABASE .. SET CHANGE_TRACKING = ON (CHANGE_RETENTION = 2 DAYS,
//! AUTO_CLEANUP = ON)`, `ALTER TABLE .. ENABLE CHANGE_TRACKING WITH (TRACK_COLUMNS_UPDATED =
//! OFF)` and `GRANT VIEW CHANGE TRACKING ON OBJECT::..`. No CDC, Service Broker or trigger.
//!
//! No name is ever pasted into the SQL text: the database is the connection's own
//! (`DB_NAME()`), a table and the grantee are parameters, and the server quotes them with
//! `QUOTENAME` into the dynamic statement. Every step re-checks the catalog before it alters
//! anything, so running it twice changes nothing the second time, and each batch starts with
//! `SET LOCK_TIMEOUT 5000` so an `ALTER` waits at most 5 s behind POS writes (error 1222
//! then, [`LOCK_TIMEOUT_ERROR`]).
//!
//! Nothing here alters a login, a user or a password.

/// Days of change history kept (`CHANGE_RETENTION`, ADR 0011 D2).
pub const RETENTION_DAYS: u32 = 2;
/// Milliseconds an `ALTER` may wait for a lock (`SET LOCK_TIMEOUT`).
pub const LOCK_TIMEOUT_MS: u32 = 5000;
/// "Lock request time out period exceeded"
/// (<https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/mssqlserver-1222-database-engine-error>).
pub const LOCK_TIMEOUT_ERROR: i32 = 1222;
/// The errors of an `ALTER` that waited past the lock timeout: 1222, and 5061 ("ALTER
/// DATABASE failed because a lock could not be placed on database ... Try again later").
/// Both are retried by a later round.
pub const LOCK_TIMEOUT_ERRORS: [i32; 2] = [LOCK_TIMEOUT_ERROR, 5061];
/// The SQL errors that say the identity lacks a permission (each `sys.messages` text
/// names a permission; the live test reads them back): 229/230 permission denied on an
/// object or column, 262 in a database, 297 and 15247 "does not have permission to
/// perform this action", 300 permission denied, 1088 and 4902 "cannot find the object
/// ... or you do not have permissions", 4613 "Grantor does not have GRANT permission",
/// 5011 "does not have permission to alter database", 15151 "cannot find ... or you do
/// not have permission". Another identity may hold the permission.
pub const REFUSAL_NUMBERS: [i32; 11] = [
    229, 230, 262, 297, 300, 1088, 4613, 4902, 5011, 15151, 15247,
];
/// The principal the grants name (the database user of the `pda_sync` login).
pub const GRANTEE: &str = crate::bootstrap::bootstrapper::LOGIN_NAME;

/// Why a captured table is not tracked.
pub mod reasons {
    /// The table has no primary key; it stays on signature polling.
    pub const NO_PK: &str = "no_pk";
    /// `pda_sync` cannot see the table (absent on this build).
    pub const MISSING: &str = "missing";
    /// Every identity was refused for this table.
    pub const REFUSED: &str = "refused";
    /// The `ALTER` waited past [`LOCK_TIMEOUT_MS`](super::LOCK_TIMEOUT_MS); tried again later.
    pub const LOCK_TIMEOUT: &str = "lock_timeout";
}

/// The batch prefix of every step.
pub const LOCK_TIMEOUT_SQL: &str = "SET LOCK_TIMEOUT 5000;";

/// Turns Change Tracking on for the connection's database unless it is on; answers
/// `enabled` or `not_enabled`.
pub const ENABLE_DATABASE_SQL: &str = "SET LOCK_TIMEOUT 5000;
IF NOT EXISTS (SELECT 1 FROM sys.change_tracking_databases WHERE database_id = DB_ID())
BEGIN
    DECLARE @sql nvarchar(max);
    SET @sql = N'ALTER DATABASE ' + QUOTENAME(DB_NAME()) + N' SET CHANGE_TRACKING = ON (CHANGE_RETENTION = 2 DAYS, AUTO_CLEANUP = ON);';
    EXEC (@sql);
END;
SELECT CASE WHEN EXISTS (SELECT 1 FROM sys.change_tracking_databases WHERE database_id = DB_ID()) THEN N'enabled' ELSE N'not_enabled' END;";

/// Tracks `dbo.@table` when it has a primary key and is not tracked yet; answers
/// `tracked` (read back from the catalog), `not_tracked`, `no_pk` or `not_found`
/// (absent, or invisible to this identity).
pub const ENABLE_TABLE_SQL: &str = "SET LOCK_TIMEOUT 5000;
DECLARE @id int, @sql nvarchar(max);
SET @id = OBJECT_ID(N'dbo.' + QUOTENAME(@table), N'U');
IF @id IS NULL
    SELECT N'not_found';
ELSE IF ISNULL(OBJECTPROPERTY(@id, 'TableHasPrimaryKey'), 0) = 0
    SELECT N'no_pk';
ELSE
BEGIN
    IF NOT EXISTS (SELECT 1 FROM sys.change_tracking_tables WHERE object_id = @id)
    BEGIN
        SET @sql = N'ALTER TABLE dbo.' + QUOTENAME(@table) + N' ENABLE CHANGE_TRACKING WITH (TRACK_COLUMNS_UPDATED = OFF);';
        EXEC (@sql);
    END;
    SELECT CASE WHEN EXISTS (SELECT 1 FROM sys.change_tracking_tables WHERE object_id = @id) THEN N'tracked' ELSE N'not_tracked' END;
END;";

/// Grants `VIEW CHANGE TRACKING` on `dbo.@table` to `@grantee` (a repeated GRANT is a
/// no-op); answers `granted` or `not_found`.
pub const GRANT_SQL: &str = "SET LOCK_TIMEOUT 5000;
DECLARE @sql nvarchar(max);
IF OBJECT_ID(N'dbo.' + QUOTENAME(@table), N'U') IS NULL
    SELECT N'not_found';
ELSE
BEGIN
    SET @sql = N'GRANT VIEW CHANGE TRACKING ON OBJECT::dbo.' + QUOTENAME(@table) + N' TO ' + QUOTENAME(@grantee) + N';';
    EXEC (@sql);
    SELECT N'granted';
END;";

/// Stops tracking `dbo.@table` (reversal for tests and operators, never automatic);
/// answers `untracked` or `not_found`.
pub const DISABLE_TABLE_SQL: &str = "SET LOCK_TIMEOUT 5000;
DECLARE @id int, @sql nvarchar(max);
SET @id = OBJECT_ID(N'dbo.' + QUOTENAME(@table), N'U');
IF @id IS NULL
    SELECT N'not_found';
ELSE
BEGIN
    IF EXISTS (SELECT 1 FROM sys.change_tracking_tables WHERE object_id = @id)
    BEGIN
        SET @sql = N'ALTER TABLE dbo.' + QUOTENAME(@table) + N' DISABLE CHANGE_TRACKING;';
        EXEC (@sql);
    END;
    SELECT N'untracked';
END;";

/// Turns Change Tracking off for the database only when no table of it is tracked any
/// more; answers `disabled` or `kept` (tables still tracked).
pub const DISABLE_DATABASE_SQL: &str = "SET LOCK_TIMEOUT 5000;
IF EXISTS (SELECT 1 FROM sys.change_tracking_databases WHERE database_id = DB_ID())
    AND NOT EXISTS (SELECT 1 FROM sys.change_tracking_tables)
BEGIN
    DECLARE @sql nvarchar(max);
    SET @sql = N'ALTER DATABASE ' + QUOTENAME(DB_NAME()) + N' SET CHANGE_TRACKING = OFF;';
    EXEC (@sql);
END;
SELECT CASE WHEN EXISTS (SELECT 1 FROM sys.change_tracking_databases WHERE database_id = DB_ID()) THEN N'kept' ELSE N'disabled' END;";

/// The database half of the probe: CT on (1/0) and `CHANGE_TRACKING_CURRENT_VERSION()`.
pub const PROBE_DATABASE_SQL: &str = "SELECT CASE WHEN EXISTS (SELECT 1 FROM sys.change_tracking_databases WHERE database_id = DB_ID()) THEN 1 ELSE 0 END, CHANGE_TRACKING_CURRENT_VERSION();";

/// The table half of the probe over `n` parameters `@t0..@t{n-1}`: per name, visible,
/// has a primary key, tracked, and `VIEW CHANGE TRACKING` held (each 1/0).
pub fn probe_tables_sql(n: usize) -> String {
    let values = (0..n)
        .map(|i| format!("(@t{i})"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "SELECT v.name, \
CASE WHEN o.id IS NULL THEN 0 ELSE 1 END, \
CASE WHEN OBJECTPROPERTY(o.id, 'TableHasPrimaryKey') = 1 THEN 1 ELSE 0 END, \
CASE WHEN EXISTS (SELECT 1 FROM sys.change_tracking_tables c WHERE c.object_id = o.id) THEN 1 ELSE 0 END, \
CASE WHEN HAS_PERMS_BY_NAME(N'dbo.' + QUOTENAME(v.name), N'OBJECT', N'VIEW CHANGE TRACKING') = 1 THEN 1 ELSE 0 END \
FROM (VALUES {values}) AS v(name) \
CROSS APPLY (SELECT OBJECT_ID(N'dbo.' + QUOTENAME(v.name), N'U') AS id) AS o;"
    )
}

/// One captured table as `pda_sync` sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtTable {
    /// The vendor table (`dbo`).
    pub name: String,
    /// `pda_sync` sees the table.
    pub present: bool,
    /// It has a primary key (`OBJECTPROPERTY(.., 'TableHasPrimaryKey')`).
    pub has_pk: bool,
    /// It is in `sys.change_tracking_tables`.
    pub tracked: bool,
    /// `pda_sync` holds `VIEW CHANGE TRACKING` on it.
    pub can_view: bool,
}

/// What the read-only probe found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CtProbe {
    /// Change Tracking is on for the database.
    pub database_enabled: bool,
    /// `CHANGE_TRACKING_CURRENT_VERSION()` (NULL while CT is off).
    pub current_version: Option<i64>,
    /// The captured tables, in the order asked.
    pub tables: Vec<CtTable>,
}

impl CtProbe {
    /// The tables tracked and readable by `pda_sync`.
    pub fn ready(&self) -> impl Iterator<Item = &CtTable> {
        self.tables.iter().filter(|t| t.tracked && t.can_view)
    }

    /// `database on, 12/20 tables tracked, version 42` for logs and the self-test.
    pub fn describe(&self) -> String {
        let tracked = self.tables.iter().filter(|t| t.tracked).count();
        let version = self
            .current_version
            .map_or_else(|| "none".to_owned(), |v| v.to_string());
        format!(
            "database {}, {tracked}/{} captured tables tracked, version {version}",
            if self.database_enabled { "on" } else { "off" },
            self.tables.len()
        )
    }
}

/// One statement batch of the enable or disable path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CtStep {
    /// [`ENABLE_DATABASE_SQL`].
    EnableDatabase,
    /// [`ENABLE_TABLE_SQL`] for the table.
    EnableTable(String),
    /// [`GRANT_SQL`] for the table.
    Grant(String),
    /// [`DISABLE_TABLE_SQL`] for the table.
    DisableTable(String),
    /// [`DISABLE_DATABASE_SQL`].
    DisableDatabase,
}

impl CtStep {
    /// The batch text (a constant: names travel as parameters).
    pub fn sql(&self) -> &'static str {
        match self {
            CtStep::EnableDatabase => ENABLE_DATABASE_SQL,
            CtStep::EnableTable(_) => ENABLE_TABLE_SQL,
            CtStep::Grant(_) => GRANT_SQL,
            CtStep::DisableTable(_) => DISABLE_TABLE_SQL,
            CtStep::DisableDatabase => DISABLE_DATABASE_SQL,
        }
    }

    /// The table the step is about, if any.
    pub fn table(&self) -> Option<&str> {
        match self {
            CtStep::EnableTable(t) | CtStep::Grant(t) | CtStep::DisableTable(t) => Some(t),
            CtStep::EnableDatabase | CtStep::DisableDatabase => None,
        }
    }

    /// The parameters as `(name, value)`: `@table`, and `@grantee` for a grant.
    pub fn params(&self) -> Vec<(&'static str, String)> {
        match self {
            CtStep::EnableTable(t) | CtStep::DisableTable(t) => vec![("table", t.clone())],
            CtStep::Grant(t) => vec![("table", t.clone()), ("grantee", GRANTEE.to_owned())],
            CtStep::EnableDatabase | CtStep::DisableDatabase => Vec::new(),
        }
    }

    /// `database`, `table TmpInvoice`, `grant TmpInvoice` for logs.
    pub fn describe(&self) -> String {
        match self {
            CtStep::EnableDatabase => "enable database".to_owned(),
            CtStep::EnableTable(t) => format!("enable table {t}"),
            CtStep::Grant(t) => format!("grant on {t}"),
            CtStep::DisableTable(t) => format!("disable table {t}"),
            CtStep::DisableDatabase => "disable database".to_owned(),
        }
    }
}

/// What a step's batch answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    /// The step's effect is in place (`enabled`, `tracked`, `granted`, `untracked`,
    /// `disabled`).
    Done,
    /// The table has no primary key (`no_pk`).
    NoPk,
    /// This identity cannot see the table, or its `ALTER` left nothing in place
    /// (`not_found`, `not_enabled`, `not_tracked`): another identity may.
    NotFound,
    /// The database stays on because tables are still tracked (`kept`).
    Kept,
}

/// The [`StepState`] of a batch's answer; `None` for a text no step answers.
pub fn step_state(answer: &str) -> Option<StepState> {
    match answer {
        "enabled" | "tracked" | "granted" | "untracked" | "disabled" => Some(StepState::Done),
        "no_pk" => Some(StepState::NoPk),
        "not_found" | "not_enabled" | "not_tracked" => Some(StepState::NotFound),
        "kept" => Some(StepState::Kept),
        _ => None,
    }
}

/// A captured table left out, with one of [`reasons`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtSkip {
    /// The vendor table.
    pub table: String,
    /// [`reasons::NO_PK`], [`reasons::MISSING`], [`reasons::REFUSED`],
    /// [`reasons::LOCK_TIMEOUT`] or `SQL error <n>`.
    pub reason: String,
}

/// What the enable path has to do after a probe.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CtPlan {
    /// The batches to run, database first, then per table enable and grant.
    pub steps: Vec<CtStep>,
    /// Tables that cannot be tracked (no primary key, absent).
    pub skipped: Vec<CtSkip>,
    /// Tables already tracked and readable by `pda_sync`.
    pub ready: Vec<String>,
}

impl CtPlan {
    /// Nothing to run.
    pub fn is_done(&self) -> bool {
        self.steps.is_empty()
    }
}

/// The enable plan of `probe`; empty when the flag `capture.change_tracking` is off, so
/// no statement is ever issued then. A table without a primary key (or that `pda_sync`
/// cannot see) is skipped; a tracked table only gets the missing grant.
pub fn plan(probe: &CtProbe, flag: bool) -> CtPlan {
    let mut out = CtPlan::default();
    if !flag {
        return out;
    }
    if !probe.database_enabled {
        out.steps.push(CtStep::EnableDatabase);
    }
    for t in &probe.tables {
        let skip = |reason: &str| CtSkip {
            table: t.name.clone(),
            reason: reason.to_owned(),
        };
        if !t.present {
            out.skipped.push(skip(reasons::MISSING));
            continue;
        }
        if !t.has_pk {
            out.skipped.push(skip(reasons::NO_PK));
            continue;
        }
        if !t.tracked {
            out.steps.push(CtStep::EnableTable(t.name.clone()));
        }
        if !t.can_view {
            out.steps.push(CtStep::Grant(t.name.clone()));
        }
        if t.tracked && t.can_view {
            out.ready.push(t.name.clone());
        }
    }
    out
}

/// The reversal of `probe` (tests and operators only, never automatic): stop tracking
/// each tracked captured table, then turn the database off if no table is tracked any more.
pub fn disable_plan(probe: &CtProbe) -> Vec<CtStep> {
    let mut steps: Vec<CtStep> = probe
        .tables
        .iter()
        .filter(|t| t.tracked)
        .map(|t| CtStep::DisableTable(t.name.clone()))
        .collect();
    if probe.database_enabled {
        steps.push(CtStep::DisableDatabase);
    }
    steps
}

#[cfg(test)]
#[path = "change_tracking_tests.rs"]
mod tests;
