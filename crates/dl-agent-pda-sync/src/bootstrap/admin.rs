//! SQL Server access with the installer's administrator identity (port of
//! `Bootstrap/ISqlAdmin.cs` and `Bootstrap/SqlServerAdmin.cs`).

use super::error::{Error, Result, tds_failure};
use super::models::{BootstrapStep, SqlInstance, TableSet};
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
              WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = 'UPDATE'
                AND p.major_id = o.object_id))
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

    fn open(&self, instance: &SqlInstance, database: &str, ctx: &Ctx) -> Result<Client> {
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
    fn the_exists_query_keeps_the_csharp_text() {
        let q = exists_query();
        assert!(q.starts_with("SELECT CASE @kind\r\n  WHEN 'Login' THEN"));
        assert!(q.ends_with("THEN 1 ELSE 0 END)\r\n  ELSE 0 END"));
        assert!(!q.replace("\r\n", "").contains('\n'));
    }
}
