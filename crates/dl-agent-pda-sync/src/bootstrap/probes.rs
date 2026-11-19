//! Read-only checks as `pda_sync` (port of `Bootstrap/AgentLoginProbe.cs` and
//! `Bootstrap/OrderGrantsProbe.cs`).

use super::admin::run;
use super::error::{Error, Result, tds_failure};
use super::models::SqlInstance;
use crate::config::sql::agent_connection;
use crate::config::{AgentError, codes};
use crate::reporting::sql_error::SqlFailure;
use crate::util::ctx::Ctx;
use dl_tds::{Client, Param};
use std::time::Duration;

/// `CommandTimeout` of the probe reads.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Proves the runtime login works before the one-time code is spent (`IAgentLoginProbe`).
pub trait AgentLoginProbe: Send + Sync {
    /// Signs in as `pda_sync` and reads; fails with `SQL_LOGIN_VERIFY_FAILED`.
    fn verify(
        &self,
        instance: &SqlInstance,
        database: &str,
        password: &str,
        ctx: &Ctx,
    ) -> Result<()>;
}

/// The open-check header, the first table capture reads.
pub const CAPTURED_TABLE: &str = "dbo.TmpInvoice";
/// The agent's own tables.
pub const AGENT_TABLES: [&str; 2] = ["dbo.Pda_SyncCommandLog", "dbo.Pda_SyncState"];

/// `SqlAgentLoginProbe.Describe`: the hint for a SQL error of the login check.
pub fn describe_login_failure(number: i32, database: &str, table: Option<&str>) -> String {
    let t = table.unwrap_or("");
    match number {
        18456 => "SQL Server rejected the pda_sync login (error 18456). Check that SQL logins are allowed (mixed mode, service restarted) \
                  and that the login is enabled, then run enroll again."
            .to_owned(),
        4060 => format!(
            "pda_sync signed in but cannot open database {database} (error 4060). Run enroll again as a SQL Server administrator."
        ),
        229 | 230 => format!(
            "pda_sync may not read {t} in {database} (error {number}). Run enroll again as a SQL Server administrator to re-apply the grants."
        ),
        208 => format!("{t} does not exist in {database} or is not visible to pda_sync (error 208)."),
        _ if table.is_none() => {
            format!("pda_sync could not connect to database {database} (error {number}).")
        }
        _ => format!("pda_sync could not read {t} in {database} (error {number})."),
    }
}

/// `SqlAgentLoginProbe.Fail`.
pub fn login_failure(number: i32, database: &str, table: Option<&str>) -> AgentError {
    AgentError::new(
        codes::SQL_LOGIN_VERIFY_FAILED,
        describe_login_failure(number, database, table) + " The enrollment code was not used.",
    )
}

/// The real probe: `SELECT TOP (1) 1 FROM dbo.TmpInvoice` and both agent tables
/// (`SqlAgentLoginProbe`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SqlAgentLoginProbe;

fn connect(cfg: &dl_tds::Config, ctx: &Ctx) -> std::result::Result<Client, Error> {
    if ctx.is_done() {
        return Err(Error::cancelled());
    }
    match Client::connect(cfg) {
        Ok(mut c) => {
            c.take_messages();
            Ok(c)
        }
        Err(_) if ctx.is_done() => Err(Error::cancelled()),
        Err(e) => Err(match tds_failure(&e, &[]) {
            Ok(f) => Error::sql_exception(&f),
            Err(ex) => Error::Unexpected(ex),
        }),
    }
}

fn as_login_failure(e: Error, database: &str, table: Option<&str>) -> Error {
    match e.sql() {
        Some(f) => Error::Agent(login_failure(f.number(), database, table)),
        None => e,
    }
}

impl AgentLoginProbe for SqlAgentLoginProbe {
    fn verify(
        &self,
        instance: &SqlInstance,
        database: &str,
        password: &str,
        ctx: &Ctx,
    ) -> Result<()> {
        let cfg = agent_connection(&instance.server_name, database, password);
        let mut c = connect(&cfg, ctx).map_err(|e| as_login_failure(e, database, None))?;
        let read = |c: &mut Client, sql: &str, table: &str| {
            run(c, sql, &[], PROBE_TIMEOUT, ctx)
                .map(|_| ())
                .map_err(|e| as_login_failure(e, database, Some(table)))
        };
        read(
            &mut c,
            &format!("SELECT TOP (1) 1 FROM {CAPTURED_TABLE}"),
            CAPTURED_TABLE,
        )?;
        for table in AGENT_TABLES {
            read(
                &mut c,
                &format!("SELECT COUNT(*) FROM {table} WHERE 1 = 0"),
                table,
            )?;
        }
        Ok(())
    }
}

/// Whether the order-verb grants are in place, read as `pda_sync` (`IOrderGrantsProbe`,
/// GR1).
pub trait OrderGrantsProbe: Send + Sync {
    /// The missing permissions (`EXECUTE dbo.PostITrn`); empty when in place. A SQL
    /// failure is an escaped `SqlException` ([`Error::sql`]).
    fn missing(
        &self,
        instance: &str,
        database: &str,
        password: &str,
        ctx: &Ctx,
    ) -> Result<Vec<String>>;
}

/// The permissions that stand for the order scripts (`SqlOrderGrantsProbe.Checked`):
/// object, permission, column.
pub const CHECKED: [(&str, &str, Option<&str>); 4] = [
    ("dbo.PostITrn", "EXECUTE", None),
    ("dbo.tmpInvoice", "INSERT", None),
    ("dbo.tmpInvoice", "UPDATE", Some("NGuest")),
    ("dbo.tmpTrn", "UPDATE", Some("CheckNo")),
];

/// The real grants probe (`SqlOrderGrantsProbe`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SqlOrderGrantsProbe;

impl OrderGrantsProbe for SqlOrderGrantsProbe {
    fn missing(
        &self,
        instance: &str,
        database: &str,
        password: &str,
        ctx: &Ctx,
    ) -> Result<Vec<String>> {
        let mut c = connect(&agent_connection(instance, database, password), ctx)?;
        let mut missing = Vec::new();
        for (obj, permission, column) in CHECKED {
            let mut params = vec![Param::new("obj", obj), Param::new("permission", permission)];
            let sql = match column {
                None => "SELECT HAS_PERMS_BY_NAME(@obj, N'OBJECT', @permission);",
                Some(col) => {
                    params.push(Param::new("column", col));
                    "SELECT HAS_PERMS_BY_NAME(@obj, N'OBJECT', @permission, @column, N'COLUMN');"
                }
            };
            let r = run(&mut c, sql, &params, PROBE_TIMEOUT, ctx)?;
            let granted = r.rows().first().and_then(|row| row.value(0)).cloned();
            // NULL: the object is not in this build, which no grant can fix.
            if granted == Some(dl_tds::Value::I32(0)) {
                missing.push(match column {
                    None => format!("{permission} {obj}"),
                    Some(col) => format!("{permission} {obj}({col})"),
                });
            }
        }
        Ok(missing)
    }
}

/// `SqlPermissionDenied.Is`: 229 object, 230 column, 262 database.
pub fn is_permission_denied(f: &SqlFailure) -> bool {
    f.errors.iter().any(|e| matches!(e.number, 229 | 230 | 262))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_failures_name_their_cause() {
        assert!(describe_login_failure(18456, "SMILE_POS", None).contains("mixed mode"));
        assert_eq!(
            describe_login_failure(4060, "SMILE_POS", None),
            "pda_sync signed in but cannot open database SMILE_POS (error 4060). Run enroll again as a SQL Server administrator."
        );
        assert!(
            describe_login_failure(229, "SMILE_POS", Some(CAPTURED_TABLE))
                .starts_with("pda_sync may not read dbo.TmpInvoice in SMILE_POS (error 229).")
        );
        assert_eq!(
            describe_login_failure(53, "SMILE_POS", None),
            "pda_sync could not connect to database SMILE_POS (error 53)."
        );
        assert_eq!(
            describe_login_failure(53, "SMILE_POS", Some("dbo.Pda_SyncState")),
            "pda_sync could not read dbo.Pda_SyncState in SMILE_POS (error 53)."
        );
        let e = login_failure(208, "D", Some("dbo.X"));
        assert_eq!(e.code(), codes::SQL_LOGIN_VERIFY_FAILED);
        assert!(e.message().ends_with(" The enrollment code was not used."));
        assert!(is_permission_denied(&SqlFailure::single(262, 14, "x")));
        assert!(!is_permission_denied(&SqlFailure::single(208, 16, "x")));
    }
}
