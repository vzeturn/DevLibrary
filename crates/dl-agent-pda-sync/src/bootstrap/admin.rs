//! SQL Server access with the installer's administrator identity (port of
//! `Bootstrap/ISqlAdmin.cs` and `Bootstrap/SqlServerAdmin.cs`).

use super::error::{Error, Result, tds_failure};
use super::models::{BootstrapStep, SqlInstance, StepKind, TableSet};
use super::preflight::SqlServerInfo;
use crate::capture::failure::code_for;
use crate::config::sql::{agent_connection, data_source};
use crate::config::{AgentError, codes};
use crate::util::ctx::Ctx;
use dl_tds::{Client, Param};
use std::time::Duration;

/// SQL Server access as the setup identity (`ISqlAdmin`). Connection failures are
/// `SQL_UNREACHABLE`, `SQL_LOGIN_FAILED`, `SQL_DATABASE_NOT_FOUND` or else
/// `SQL_NO_ACCESS`.
pub trait SqlAdmin: Send + Sync {
    /// The online databases the identity can open.
    fn list_databases(&self, instance: &SqlInstance, ctx: &Ctx) -> Result<Vec<String>>;
    /// The tables of `database`.
    fn list_tables(&self, instance: &SqlInstance, database: &str, ctx: &Ctx) -> Result<TableSet>;
    /// Whether `step` is already in place.
    fn exists(
        &self,
        instance: &SqlInstance,
        database: &str,
        step: &BootstrapStep,
        ctx: &Ctx,
    ) -> Result<bool>;
    /// Runs one script (already substituted; no `GO` separators).
    fn execute(
        &self,
        instance: &SqlInstance,
        database: &str,
        script: &str,
        ctx: &Ctx,
    ) -> Result<()>;
    /// Version, edition and authentication mode (`SERVERPROPERTY`, read-only).
    fn server_info(&self, instance: &SqlInstance, ctx: &Ctx) -> Result<SqlServerInfo>;
    /// Whether the runtime login `pda_sync` signs in to `master` with `password`. A
    /// rejected sign-in (error 18456) is `false`; any other failure is an error.
    fn signs_in(&self, instance: &SqlInstance, password: &str, ctx: &Ctx) -> Result<bool>;
}

/// SQL Server's "Login failed for user" error.
pub const LOGIN_FAILED_ERROR: i32 = 18456;

/// The identity `enroll` sets SQL Server up with: the Windows account by default, or a
/// SQL Server administrator login (ADR 0093). Never stored (`SqlAdminCredential`).
#[derive(Clone, PartialEq, Eq, Default)]
pub struct SqlAdminCredential {
    /// The SQL login, `None` for Windows authentication.
    pub user: Option<String>,
    /// Its password.
    pub password: Option<String>,
}

impl std::fmt::Debug for SqlAdminCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlAdminCredential")
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .finish()
    }
}

/// `SqlAdminCredential.PasswordEnvVar`: where `--sql-admin-user` takes its password
/// from, else a prompt. Never an argument.
pub const PASSWORD_ENV_VAR: &str = "PDA_SQL_ADMIN_PASSWORD";
/// `ApplicationName` of the setup connection.
pub const SETUP_APP_NAME: &str = "PdaSyncAgent-setup";
/// `ConnectTimeout` of the setup connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// `SqlCommand.CommandTimeout` default.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// `CommandTimeout` of a bootstrap script.
pub const SCRIPT_TIMEOUT: Duration = Duration::from_secs(60);

impl SqlAdminCredential {
    /// Windows integrated security (`SqlAdminCredential.Windows`).
    pub fn windows() -> SqlAdminCredential {
        SqlAdminCredential::default()
    }

    /// A SQL Server administrator login.
    pub fn sql(user: &str, password: &str) -> SqlAdminCredential {
        SqlAdminCredential {
            user: Some(user.to_owned()),
            password: Some(password.to_owned()),
        }
    }

    /// Whether this is a SQL login.
    pub fn is_sql_login(&self) -> bool {
        self.user.is_some()
    }
}

/// `SqlServerAdmin.ConnectFailure`: why the setup connection failed, by cause.
pub fn connect_failure(
    number: i32,
    severity: u8,
    instance: &str,
    database: &str,
    sql_login: bool,
) -> AgentError {
    let code = code_for(number, severity);
    match code {
        codes::SQL_UNREACHABLE => AgentError::new(
            code,
            format!(
                "Cannot reach SQL Server '{instance}' (error {number}). When SQL Server is on another machine pass \
                 --sql-instance <host>\\<instance> or <host>,<port>: TCP/IP must be enabled there, the firewall must allow its port \
                 (1433 by default) and a named instance needs the SQL Browser service (UDP 1434)."
            ),
        ),
        codes::SQL_LOGIN_FAILED if sql_login => AgentError::new(
            code,
            format!(
                "SQL Server '{instance}' refused the login given with --sql-admin-user (error {number}). Check the name and the \
                 password, and that the server allows SQL Server authentication (mixed mode)."
            ),
        ),
        codes::SQL_LOGIN_FAILED => AgentError::new(
            code,
            format!(
                "SQL Server '{instance}' refused this Windows account (error {number}). Run the installer as a Windows user with \
                 sysadmin rights on the instance; when this machine and the SQL Server share no domain, pass --sql-admin-user \
                 <login> (password from {PASSWORD_ENV_VAR} or a prompt)."
            ),
        ),
        codes::SQL_DATABASE_NOT_FOUND => AgentError::new(
            code,
            format!(
                "Database '{database}' cannot be opened on SQL Server '{instance}' (error {number}). Either it does not exist under \
                 that name, or this account has no access to it (no login or no user in the database): rerun the installer as a \
                 Windows user with sysadmin rights on the instance, which creates the access and applies the grants."
            ),
        ),
        _ => AgentError::new(
            codes::SQL_NO_ACCESS,
            format!(
                "Cannot connect to SQL Server '{instance}' (error {number}). Run the installer as a Windows user with sysadmin rights on the instance."
            ),
        ),
    }
}

/// What script 180 grants: (table, permission, columns). An empty column list is a grant
/// on the whole table; a column grant is checked per column, so a store missing one of
/// them is reported (a table-level `HAS_PERMS_BY_NAME` is 0 under column grants alone).
pub const MONEY_GRANTS: &[(&str, &str, &[&str])] = &[
    ("tmpTrn", "INSERT", &[]),
    ("tmpTrn", "DELETE", &[]),
    ("tmpPayment", "INSERT", &[]),
    ("tmpPayment", "DELETE", &[]),
    (
        "tmpInvoice",
        "UPDATE",
        &["PaymentMode", "PaymentType", "PaymentInfo"],
    ),
    ("tmpTaxTable", "INSERT", &[]),
    ("tmpTaxTable", "UPDATE", &["BillNo", "Sub2"]),
    ("tmpTaxTable", "DELETE", &[]),
    ("Trn", "INSERT", &[]),
    ("Invoice", "INSERT", &[]),
    ("Payment", "INSERT", &[]),
    ("ARInv", "INSERT", &[]),
    ("CCInv", "INSERT", &[]),
    ("TaxTable", "INSERT", &[]),
    ("MemberPoint", "INSERT", &[]),
    ("tmpInvoice", "DELETE", &[]),
    ("tmpInvoiceInfo", "DELETE", &[]),
    ("tmpTrnInfo", "DELETE", &[]),
    ("tmpARInv", "DELETE", &[]),
    ("tmpCCInv", "DELETE", &[]),
    (
        "checkopenlog",
        "UPDATE",
        &["isOpen", "CloseTime", "LastAction"],
    ),
];

/// `SqlServerAdmin.MoneyGrantsExistAsync`'s query: 1 once `@name` holds every grant of
/// [`MONEY_GRANTS`] on every table the POS database has (a column grant: on every column
/// of the list the table has, or on the whole table).
pub fn money_grants_exists_query() -> String {
    let checks: Vec<String> = MONEY_GRANTS
        .iter()
        .map(|(table, permission, columns)| {
            let granted = |minor: &str| {
                format!(
                    "EXISTS (SELECT 1 FROM sys.database_permissions p \
                     JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name \
                     WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = '{permission}' AND p.major_id = OBJECT_ID(N'dbo.{table}', N'U'){minor})"
                )
            };
            let held = if columns.is_empty() {
                granted("")
            } else {
                let per_column: Vec<String> = columns
                    .iter()
                    .map(|col| {
                        format!(
                            "(COL_LENGTH(N'dbo.{table}', N'{col}') IS NULL OR {})",
                            granted(&format!(
                                " AND p.minor_id IN (0, COLUMNPROPERTY(OBJECT_ID(N'dbo.{table}', N'U'), N'{col}', 'ColumnId'))"
                            ))
                        )
                    })
                    .collect();
                format!("({})", per_column.join(" AND "))
            };
            format!("(OBJECT_ID(N'dbo.{table}', N'U') IS NULL OR {held})")
        })
        .collect();
    format!(
        "SELECT CASE WHEN {} THEN 1 ELSE 0 END",
        checks.join(" AND ")
    )
}

/// 190: the Front Office database name (`Para InterfaceDB`, default `SMILE_FO`).
pub const FO_DATABASE_SQL: &str = "SELECT TOP 1 LTRIM(RTRIM(ParaStr)) FROM dbo.Para WHERE ParaName = N'InterfaceDB' AND ParaStr IS NOT NULL AND LTRIM(RTRIM(ParaStr)) <> N'' ORDER BY RVC";

/// 190: 1 when there is nothing to grant (no usable Front Office database or no
/// `Folio`) or `@name` already holds SELECT on it. `fo` is validated by the caller.
pub fn fo_inhouse_grants_exists_query(fo: &str) -> String {
    format!(
        "SELECT CASE WHEN DB_ID(@fo) IS NULL OR DB_ID(@fo) = DB_ID() THEN 1 \
         WHEN OBJECT_ID(N'[{fo}].dbo.Folio', N'U') IS NULL THEN 1 \
         WHEN EXISTS (SELECT 1 FROM [{fo}].sys.database_permissions p \
         JOIN [{fo}].sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name \
         WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'SELECT' \
         AND p.major_id = OBJECT_ID(N'[{fo}].dbo.Folio', N'U')) THEN 1 ELSE 0 END"
    )
}

/// 195: 0 when the Front Office has the posting procedure, `Folio` or
/// `FolioBalanceCode` and `@name` lacks the EXECUTE or SELECT on it (the grants of script
/// 195); 1 otherwise, including when there is no usable Front Office database.
pub fn post_to_smile_grants_exists_query(fo: &str) -> String {
    let held = |perm: &str, obj: &str, kind: &str| {
        format!(
            "OBJECT_ID(N'[{fo}].dbo.{obj}', N'{kind}') IS NOT NULL AND NOT EXISTS (SELECT 1 FROM [{fo}].sys.database_permissions p JOIN [{fo}].sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = '{perm}' AND p.major_id = OBJECT_ID(N'[{fo}].dbo.{obj}', N'{kind}')) THEN 0"
        )
    };
    format!(
        "SELECT CASE WHEN DB_ID(@fo) IS NULL OR DB_ID(@fo) = DB_ID() THEN 1\n\
                     WHEN {}\n\
                     WHEN {}\n\
                     WHEN {}\n\
                     ELSE 1 END",
        held("EXECUTE", "PostInterfaceTransactionToFolio", "P"),
        held("SELECT", "Folio", "U"),
        held("SELECT", "FolioBalanceCode", "U"),
    )
}

/// 195: done once `pda_sync` holds the grants of the script in the Front Office
/// database; an invalid name has nothing to grant, as in the script.
fn post_to_smile_grants_exist(c: &mut Client, step: &BootstrapStep, ctx: &Ctx) -> Result<bool> {
    let r = run(c, FO_DATABASE_SQL, &[], COMMAND_TIMEOUT, ctx)?;
    let fo = match r.rows().first().and_then(|row| row.value(0)) {
        Some(dl_tds::Value::String(s)) if !s.trim().is_empty() => s.trim().to_owned(),
        _ => "SMILE_FO".to_owned(),
    };
    if !fo.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return Ok(true);
    }
    let r = run(
        c,
        &post_to_smile_grants_exists_query(&fo),
        &[
            Param::new("fo", fo.as_str()),
            Param::new("name", step.object_name),
        ],
        COMMAND_TIMEOUT,
        ctx,
    )?;
    Ok(matches!(
        r.rows().first().and_then(|row| row.value(0)),
        Some(dl_tds::Value::I32(1))
    ))
}

/// 190: done once `pda_sync` may read `Folio` in the Front Office database; an invalid
/// name has nothing to grant, as in the script.
fn fo_inhouse_grants_exist(c: &mut Client, step: &BootstrapStep, ctx: &Ctx) -> Result<bool> {
    let r = run(c, FO_DATABASE_SQL, &[], COMMAND_TIMEOUT, ctx)?;
    let fo = match r.rows().first().and_then(|row| row.value(0)) {
        Some(dl_tds::Value::String(s)) if !s.is_empty() => s.clone(),
        _ => "SMILE_FO".to_owned(),
    };
    if !fo.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return Ok(true);
    }
    let r = run(
        c,
        &fo_inhouse_grants_exists_query(&fo),
        &[
            Param::new("fo", fo.as_str()),
            Param::new("name", step.object_name),
        ],
        COMMAND_TIMEOUT,
        ctx,
    )?;
    Ok(matches!(
        r.rows().first().and_then(|row| row.value(0)),
        Some(dl_tds::Value::I32(1))
    ))
}

/// 180: done once `pda_sync` holds every grant of the script on every table the POS
/// database has.
fn money_grants_exist(c: &mut Client, step: &BootstrapStep, ctx: &Ctx) -> Result<bool> {
    let r = run(
        c,
        &money_grants_exists_query(),
        &[Param::new("name", step.object_name)],
        COMMAND_TIMEOUT,
        ctx,
    )?;
    let n = match r.rows().first() {
        Some(row) => row
            .get::<Option<i64>>(0)
            .map_err(|e| Error::unexpected("System.InvalidCastException", e.to_string()))?,
        None => None,
    };
    Ok(n == Some(1))
}

/// `SqlServerAdmin.ExistsAsync`'s query (CRLF line ends, as the C# raw literal built on
/// Windows).
pub fn exists_query() -> String {
    EXISTS_SQL.replace('\n', "\r\n")
}

const EXISTS_SQL: &str = "SELECT CASE @kind
  WHEN 'Login' THEN (SELECT COUNT(*) FROM sys.server_principals WHERE name = @name)
  WHEN 'User'  THEN (SELECT COUNT(*) FROM sys.database_principals WHERE name = @name)
  WHEN 'Table' THEN (SELECT COUNT(*) FROM sys.tables WHERE name = @name AND schema_id = SCHEMA_ID('dbo'))
  WHEN 'Grants' THEN (SELECT CASE WHEN
        EXISTS (SELECT 1 FROM sys.database_role_members m
                JOIN sys.database_principals r ON r.principal_id = m.role_principal_id AND r.name = 'db_datareader'
                JOIN sys.database_principals u ON u.principal_id = m.member_principal_id AND u.name = @name)
        AND (SELECT COUNT(*) FROM sys.database_permissions p
             JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
             JOIN sys.objects o ON o.object_id = p.major_id AND o.name LIKE N'Pda[_]Sync%'
             WHERE p.class = 1 AND p.state = 'G' AND p.permission_name IN ('INSERT','UPDATE')) >= 3
        THEN 1 ELSE 0 END)
  WHEN 'CommandGrants' THEN (SELECT CASE WHEN
        (SELECT COUNT(DISTINCT p.permission_name) FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
          WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.major_id = OBJECT_ID(N'dbo.TableReservation')
            AND p.permission_name IN ('INSERT','DELETE')) = 2
        AND (SELECT COUNT(*) FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
           JOIN sys.columns c ON c.object_id = p.major_id AND c.column_id = p.minor_id
          WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'UPDATE' AND p.major_id = OBJECT_ID(N'dbo.tmpTrnInfo')
            AND c.name IN (N'IsDone', N'DoneTime', N'KdsAcceptedTime', N'KdsAcceptedBy'))
          = (SELECT COUNT(*) FROM sys.columns WHERE object_id = OBJECT_ID(N'dbo.tmpTrnInfo')
              AND name IN (N'IsDone', N'DoneTime', N'KdsAcceptedTime', N'KdsAcceptedBy'))
        THEN 1 ELSE 0 END)
  WHEN 'OrderGrants' THEN (SELECT CASE WHEN
        (SELECT COUNT(*) FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
          WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = 'EXECUTE'
            AND p.major_id IN (OBJECT_ID(N'dbo.GetRVCSeqNumber'), OBJECT_ID(N'dbo.PostITrn'),
                               OBJECT_ID(N'dbo.UpdateTmpInvoice'), OBJECT_ID(N'dbo.Unicode2TCVN'))) = 4
        AND EXISTS (SELECT 1 FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
          WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = 'INSERT'
            AND p.major_id = OBJECT_ID(N'dbo.VoidTrn'))
        THEN 1 ELSE 0 END)
  WHEN 'OrderGrantsOw1c' THEN (SELECT CASE WHEN
        (SELECT COUNT(*) FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
           JOIN sys.columns c ON c.object_id = p.major_id AND c.column_id = p.minor_id
          WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'UPDATE' AND p.major_id = OBJECT_ID(N'dbo.tmpInvoice')
            AND c.name IN (N'NGuest', N'NChild', N'LastChgCashier', N'LastChgTime')) = 4
        THEN 1 ELSE 0 END)
  WHEN 'OrderGrantsOw1e' THEN (SELECT CASE WHEN
        EXISTS (SELECT 1 FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
           JOIN sys.columns c ON c.object_id = p.major_id AND c.column_id = p.minor_id
          WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'UPDATE' AND p.major_id = OBJECT_ID(N'dbo.tmpTrn')
            AND c.name = N'CheckNo')
        AND EXISTS (SELECT 1 FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
          WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = 'DELETE'
            AND p.major_id = OBJECT_ID(N'dbo.tmpInvoice'))
        THEN 1 ELSE 0 END)
  WHEN 'SequenceGrants' THEN (SELECT CASE WHEN
        NOT EXISTS (SELECT 1 FROM sys.objects o
          WHERE o.schema_id = SCHEMA_ID(N'dbo') AND o.type IN ('SO', 'U')
            AND ((o.name LIKE N'LastCheck[0-9]%' AND SUBSTRING(o.name, 10, 128) NOT LIKE N'%[^0-9]%')
              OR (o.name LIKE N'LastTrn[0-9]%' AND SUBSTRING(o.name, 8, 128) NOT LIKE N'%[^0-9]%'))
            AND NOT EXISTS (SELECT 1 FROM sys.database_permissions p
               JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
              WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = CASE o.type WHEN 'U' THEN 'INSERT' ELSE 'UPDATE' END
                AND p.major_id = o.object_id))
        THEN 1 ELSE 0 END)
  WHEN 'TxnApplied' THEN (SELECT CASE WHEN
        OBJECT_ID(N'dbo.Pda_TxnApplied', N'U') IS NOT NULL
        AND (SELECT COUNT(DISTINCT p.permission_name) FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = N'pda_sync'
          WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.major_id = OBJECT_ID(N'dbo.Pda_TxnApplied')
            AND p.permission_name IN ('SELECT','INSERT','UPDATE')) = 3
        THEN 1 ELSE 0 END)
  WHEN 'TxnGrants' THEN (SELECT CASE WHEN
        NOT EXISTS (SELECT 1 FROM (VALUES (N'dbo.TmpInvoice'), (N'dbo.TmpTrn'), (N'dbo.TmpPayment'), (N'dbo.Invoice'),
                                          (N'dbo.Trn'), (N'dbo.Payment')) t(name)
          CROSS JOIN (VALUES ('INSERT'), ('DELETE')) k(perm)
          WHERE OBJECT_ID(t.name, N'U') IS NOT NULL
            AND NOT EXISTS (SELECT 1 FROM sys.database_permissions p
               JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
              WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = k.perm
                AND p.major_id = OBJECT_ID(t.name, N'U')))
        THEN 1 ELSE 0 END)
  WHEN 'TxnStoreKey' THEN (SELECT CASE WHEN
        OBJECT_ID(N'dbo.Pda_TxnApplied', N'U') IS NULL
        OR COL_LENGTH(N'dbo.Pda_TxnApplied', N'StoreKey') IS NOT NULL
        THEN 1 ELSE 0 END)
  WHEN 'TxnShiftGrants' THEN (SELECT CASE WHEN
        (OBJECT_ID(N'dbo.CashierShift', N'U') IS NULL OR (
          EXISTS (SELECT 1 FROM sys.database_permissions p
             JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
            WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = 'INSERT'
              AND p.major_id = OBJECT_ID(N'dbo.CashierShift', N'U'))
          AND EXISTS (SELECT 1 FROM sys.database_permissions p
             JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
             JOIN sys.columns c ON c.object_id = p.major_id AND c.column_id = p.minor_id
            WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'UPDATE'
              AND p.major_id = OBJECT_ID(N'dbo.CashierShift', N'U') AND c.name = N'CashEnd')))
        AND (OBJECT_ID(N'dbo.ShiftMoney', N'U') IS NULL OR (SELECT COUNT(DISTINCT p.permission_name) FROM sys.database_permissions p
             JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
            WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name IN ('INSERT','DELETE')
              AND p.major_id = OBJECT_ID(N'dbo.ShiftMoney', N'U')) = 2)
        AND (OBJECT_ID(N'dbo.DailyCashLog', N'U') IS NULL OR EXISTS (SELECT 1 FROM sys.database_permissions p
             JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
            WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = 'INSERT'
              AND p.major_id = OBJECT_ID(N'dbo.DailyCashLog', N'U')))
        THEN 1 ELSE 0 END)
  WHEN 'PoNumberGrants' THEN (SELECT CASE WHEN
        COL_LENGTH(N'dbo.tmpInvoice', N'PONumber') IS NULL OR EXISTS (SELECT 1 FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
           JOIN sys.columns c ON c.object_id = p.major_id AND c.column_id = p.minor_id
          WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'UPDATE' AND p.major_id = OBJECT_ID(N'dbo.tmpInvoice')
            AND c.name = N'PONumber')
        THEN 1 ELSE 0 END)
  WHEN 'FullRwGrants' THEN (SELECT CASE WHEN
        IS_ROLEMEMBER(N'db_datareader', @name) = 1 AND IS_ROLEMEMBER(N'db_datawriter', @name) = 1
        AND EXISTS (SELECT 1 FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
          WHERE p.class = 0 AND p.state = 'G' AND p.permission_name = 'EXECUTE')
        THEN 1 ELSE 0 END)
  ELSE 0 END";

/// `SELECT name FROM sys.databases ...` of `ListDatabasesAsync`.
pub const LIST_DATABASES_SQL: &str =
    "SELECT name FROM sys.databases WHERE state = 0 AND HAS_DBACCESS(name) = 1 ORDER BY name";
/// `ListTablesAsync`'s query.
pub const LIST_TABLES_SQL: &str = "SELECT name FROM sys.tables";

/// Runs `sql` on an open connection with a per-command timeout (bounded by `ctx`); a
/// failure is the escaped `SqlException` (or client-side exception).
pub(crate) fn run(
    client: &mut Client,
    sql: &str,
    params: &[Param],
    timeout: Duration,
    ctx: &Ctx,
) -> std::result::Result<dl_tds::Results, Error> {
    if ctx.is_done() {
        return Err(Error::cancelled());
    }
    let t = ctx
        .remaining()
        .map_or(timeout, |r| r.min(timeout))
        .max(Duration::from_millis(1));
    let result = client
        .set_timeout(Some(t))
        .and_then(|()| client.query(sql, params));
    let messages = client.take_messages();
    match result {
        Ok(r) => Ok(r),
        Err(_) if ctx.is_done() => Err(Error::cancelled()),
        Err(e) => Err(match tds_failure(&e, &messages) {
            Ok(f) => Error::sql_exception(&f),
            Err(ex) => Error::Unexpected(ex),
        }),
    }
}

/// The real [`SqlAdmin`]: Windows integrated security unless a SQL administrator login
/// is given (`SqlServerAdmin`).
#[derive(Debug, Clone)]
pub struct SqlServerAdmin {
    credential: SqlAdminCredential,
}

impl SqlServerAdmin {
    /// An admin connecting as `credential`.
    pub fn new(credential: SqlAdminCredential) -> SqlServerAdmin {
        SqlServerAdmin { credential }
    }

    /// The credential.
    pub fn credential(&self) -> &SqlAdminCredential {
        &self.credential
    }

    /// `AdminConnectionString`: like the C# `SqlConnectionStringBuilder` (encryption
    /// mandatory with a trusted certificate, 10 s connect timeout, application
    /// `PdaSyncAgent-setup`).
    pub fn admin_connection(&self, instance: &SqlInstance, database: &str) -> dl_tds::Config {
        let c = data_source(&instance.server_name)
            .with_database(database)
            .with_encrypt(dl_tds::Encrypt::Required)
            .with_trust_server_certificate(true)
            .with_connect_timeout(CONNECT_TIMEOUT)
            .with_app_name(SETUP_APP_NAME);
        match (&self.credential.user, &self.credential.password) {
            (Some(u), p) => c.credentials(u, p.as_deref().unwrap_or("")),
            (None, _) => c.with_integrated_auth(),
        }
    }

    /// Opens the setup connection; a failure is mapped to the agent's connection codes
    /// ([`connect_failure`]).
    pub(crate) fn open(&self, instance: &SqlInstance, database: &str, ctx: &Ctx) -> Result<Client> {
        if ctx.is_done() {
            return Err(Error::cancelled());
        }
        match Client::connect(&self.admin_connection(instance, database)) {
            Ok(mut c) => {
                c.take_messages();
                Ok(c)
            }
            Err(_) if ctx.is_done() => Err(Error::cancelled()),
            Err(e) => match tds_failure(&e, &[]) {
                Ok(f) => Err(Error::Agent(connect_failure(
                    f.number(),
                    f.class(),
                    &instance.server_name,
                    database,
                    self.credential.is_sql_login(),
                ))),
                Err(ex) => Err(Error::Unexpected(ex)),
            },
        }
    }

    fn names(
        &self,
        instance: &SqlInstance,
        database: &str,
        sql: &str,
        ctx: &Ctx,
    ) -> Result<Vec<String>> {
        let mut c = self.open(instance, database, ctx)?;
        let r = run(&mut c, sql, &[], COMMAND_TIMEOUT, ctx)?;
        r.rows()
            .iter()
            .map(|row| {
                row.get::<String>(0)
                    .map_err(|e| Error::unexpected("System.InvalidCastException", e.to_string()))
            })
            .collect()
    }
}

impl SqlAdmin for SqlServerAdmin {
    fn list_databases(&self, instance: &SqlInstance, ctx: &Ctx) -> Result<Vec<String>> {
        self.names(instance, "master", LIST_DATABASES_SQL, ctx)
    }

    fn list_tables(&self, instance: &SqlInstance, database: &str, ctx: &Ctx) -> Result<TableSet> {
        Ok(self
            .names(instance, database, LIST_TABLES_SQL, ctx)?
            .into_iter()
            .collect())
    }

    fn exists(
        &self,
        instance: &SqlInstance,
        database: &str,
        step: &BootstrapStep,
        ctx: &Ctx,
    ) -> Result<bool> {
        let mut c = self.open(instance, database, ctx)?;
        if step.kind == StepKind::MoneyGrants {
            return money_grants_exist(&mut c, step, ctx);
        }
        if step.kind == StepKind::FoInhouseGrants {
            return fo_inhouse_grants_exist(&mut c, step, ctx);
        }
        if step.kind == StepKind::PostToSmileGrants {
            return post_to_smile_grants_exist(&mut c, step, ctx);
        }
        if step.kind == StepKind::FoRoomChargeGrants {
            return super::fo_room_charge_check::fo_room_charge_grants_exist(&mut c, step, ctx);
        }
        let r = run(
            &mut c,
            &exists_query(),
            &[
                Param::new("kind", step.kind.name()),
                Param::new("name", step.object_name),
            ],
            COMMAND_TIMEOUT,
            ctx,
        )?;
        let n = match r.rows().first() {
            Some(row) => row
                .get::<Option<i64>>(0)
                .map_err(|e| Error::unexpected("System.InvalidCastException", e.to_string()))?,
            None => None,
        };
        // Convert.ToInt32(null) is 0.
        Ok(n.unwrap_or(0) > 0)
    }

    fn execute(
        &self,
        instance: &SqlInstance,
        database: &str,
        script: &str,
        ctx: &Ctx,
    ) -> Result<()> {
        let mut c = self.open(instance, database, ctx)?;
        match run(&mut c, script, &[], SCRIPT_TIMEOUT, ctx) {
            Ok(_) => Ok(()),
            Err(e) => match e.sql() {
                Some(f) => Err(Error::Agent(AgentError::new(
                    codes::SQL_NO_ACCESS,
                    format!(
                        "SQL setup failed on {}/{database} (error {}). The installer must run as a SQL Server administrator.",
                        instance.server_name,
                        f.number()
                    ),
                ))),
                None => Err(e),
            },
        }
    }

    fn server_info(&self, instance: &SqlInstance, ctx: &Ctx) -> Result<SqlServerInfo> {
        let mut c = self.open(instance, "master", ctx)?;
        if ctx.is_done() {
            return Err(Error::cancelled());
        }
        c.set_timeout(Some(COMMAND_TIMEOUT))
            .map_err(|e| Error::unexpected("System.InvalidOperationException", e.to_string()))?;
        SqlServerInfo::read(&mut c)
    }

    fn signs_in(&self, instance: &SqlInstance, password: &str, ctx: &Ctx) -> Result<bool> {
        let cfg = agent_connection(&instance.server_name, "master", password);
        match super::probes::connect(&cfg, ctx) {
            Ok(_) => Ok(true),
            Err(e) if e.sql().is_some_and(|f| f.number() == LOGIN_FAILED_ERROR) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_is_the_default_and_a_sql_login_carries_its_name() {
        let w = SqlServerAdmin::new(SqlAdminCredential::windows());
        let c = w.admin_connection(&SqlInstance::new("SQLBOX", "argument"), "master");
        assert!(c.integrated_auth());
        assert_eq!(c.app_name(), SETUP_APP_NAME);
        assert_eq!(c.encrypt(), dl_tds::Encrypt::Required);
        assert!(c.trust_server_certificate());
        assert_eq!(c.connect_timeout(), CONNECT_TIMEOUT);
        let s = SqlServerAdmin::new(SqlAdminCredential::sql("sa", "pw"));
        let c = s.admin_connection(&SqlInstance::new(r"SQLBOX\POS", "argument"), "SMILE_POS");
        assert!(!c.integrated_auth());
        assert_eq!((c.user(), c.password()), ("sa", "pw"));
        assert_eq!((c.host(), c.instance()), ("SQLBOX", Some("POS")));
        assert_eq!(c.database(), "SMILE_POS");
        assert!(!format!("{:?}", s.credential()).contains("pw"));
    }

    /// `SqlAdminCredentialTests.A_failed_setup_connection_names_its_cause`.
    #[test]
    fn a_failed_setup_connection_names_its_cause() {
        let cases: [(i32, u8, bool, &str, &str); 8] = [
            (53, 20, false, codes::SQL_UNREACHABLE, "--sql-instance"),
            (-2, 11, true, codes::SQL_UNREACHABLE, "SQL Browser"),
            (
                18452,
                14,
                false,
                codes::SQL_LOGIN_FAILED,
                "--sql-admin-user",
            ),
            (18456, 14, false, codes::SQL_LOGIN_FAILED, PASSWORD_ENV_VAR),
            (18456, 14, true, codes::SQL_LOGIN_FAILED, "mixed mode"),
            (4060, 11, true, codes::SQL_DATABASE_NOT_FOUND, "'SMILE_POS'"),
            (229, 14, false, codes::SQL_NO_ACCESS, "sysadmin"),
            (8134, 16, false, codes::SQL_NO_ACCESS, "sysadmin"),
        ];
        for (number, severity, sql_login, code, hint) in cases {
            let e = connect_failure(number, severity, "SQLBOX", "SMILE_POS", sql_login);
            assert_eq!(e.code(), code, "{number}");
            assert!(e.message().contains(hint), "{number}: {}", e.message());
            assert!(e.message().contains(&format!("error {number}")), "{number}");
        }
    }

    #[test]
    fn the_money_grants_query_checks_every_grant_of_script_180() {
        let q = money_grants_exists_query();
        assert!(q.starts_with("SELECT CASE WHEN (OBJECT_ID(N'dbo.tmpTrn', N'U') IS NULL OR EXISTS (SELECT 1 FROM sys.database_permissions p JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'INSERT' AND p.major_id = OBJECT_ID(N'dbo.tmpTrn', N'U'))) AND "));
        assert!(q.ends_with("p.permission_name = 'UPDATE' AND p.major_id = OBJECT_ID(N'dbo.checkopenlog', N'U') AND p.minor_id IN (0, COLUMNPROPERTY(OBJECT_ID(N'dbo.checkopenlog', N'U'), N'LastAction', 'ColumnId')))))) THEN 1 ELSE 0 END"));
        // A column grant is checked on each column of script 180's list, skipping a column the build lacks.
        assert!(q.contains("(COL_LENGTH(N'dbo.tmpInvoice', N'PaymentInfo') IS NULL OR EXISTS ("));
        assert_eq!(q.matches("COL_LENGTH(").count(), 8);
        assert_eq!(
            q.matches(" AND (OBJECT_ID(").count(),
            MONEY_GRANTS.len() - 1
        );
    }

    #[test]
    fn the_fo_inhouse_grants_query_checks_select_on_folio_in_the_front_office_database() {
        let q = fo_inhouse_grants_exists_query("SMILE_FO");
        assert!(q.contains("OBJECT_ID(N'[SMILE_FO].dbo.Folio', N'U')"));
        assert!(q.contains("p.permission_name = 'SELECT'"));
        assert!(q.contains("DB_ID(@fo) = DB_ID() THEN 1"));
        let sql = crate::bootstrap::read_script("Sql.190_fo_inhouse_grants.sql").unwrap();
        assert!(sql.contains("GRANT SELECT ON OBJECT::dbo.Folio TO [pda_sync];"));
        assert!(
            !sql.contains("INSERT ON") && !sql.contains("UPDATE ON") && !sql.contains("EXECUTE ON")
        );
    }

    #[test]
    fn the_post_to_smile_grants_query_and_script_cover_the_posting_procedure_and_two_reads() {
        let q = post_to_smile_grants_exists_query("SMILE_FO");
        for obj in [
            "PostInterfaceTransactionToFolio",
            "Folio",
            "FolioBalanceCode",
        ] {
            assert!(
                q.contains(&format!("OBJECT_ID(N'[SMILE_FO].dbo.{obj}'")),
                "{obj}"
            );
        }
        assert!(q.contains("p.permission_name = 'EXECUTE'"));
        assert_eq!(q.matches("p.permission_name = 'SELECT'").count(), 2);
        assert!(q.contains("DB_ID(@fo) = DB_ID() THEN 1"));
        let sql = crate::bootstrap::read_script("Sql.195_post_to_smile_grants.sql").unwrap();
        assert!(sql.contains(
            "GRANT EXECUTE ON OBJECT::dbo.PostInterfaceTransactionToFolio TO [pda_sync];"
        ));
        assert!(
            !sql.contains("INSERT ON") && !sql.contains("UPDATE ON") && !sql.contains("DELETE ON")
        );
        assert!(!sql.contains("ALTER ") && !sql.contains("DROP "));
    }

    #[test]
    fn the_exists_query_keeps_the_csharp_text() {
        let q = exists_query();
        assert!(q.starts_with("SELECT CASE @kind\r\n  WHEN 'Login' THEN"));
        assert!(q.ends_with("THEN 1 ELSE 0 END)\r\n  ELSE 0 END"));
        assert!(!q.replace("\r\n", "").contains('\n'));
    }
}
