//! The SQL side of Change Tracking (RT-1318): the read-only probe as `pda_sync`, and the
//! enable and disable batches of [`change_tracking`](super::change_tracking) run as a
//! bootstrap identity (ADR 0010: the service's Windows identity, then the SMILE default
//! login). Nothing here alters a login, a user or a password.

use dl_tds::{Client, Param, Row};

use super::admin::{COMMAND_TIMEOUT, SqlAdminCredential, SqlServerAdmin, run};
use super::change_tracking::{
    CtProbe, CtStep, CtTable, PROBE_DATABASE_SQL, StepState, probe_tables_sql, step_state,
};
use super::error::Error;
use super::models::SqlInstance;
use super::probes::{PROBE_TIMEOUT, connect};
use crate::config::sql::agent_connection;
use crate::util::ctx::Ctx;

/// The exception type of an answer no batch gives.
const INVALID_OPERATION: &str = "System.InvalidOperationException";

/// Reads the Change Tracking state of the captured tables (read-only).
pub trait ChangeTrackingProbe: Send + Sync {
    /// The state of `database` and `tables` as `pda_sync` sees it.
    ///
    /// # Errors
    /// The connection or a query failed (an escaped `SqlException`), or `ctx` ended.
    fn probe(
        &self,
        instance: &str,
        database: &str,
        password: &str,
        tables: &[String],
        ctx: &Ctx,
    ) -> Result<CtProbe, Error>;
}

/// The real probe: it connects as `pda_sync` with its stored password.
#[derive(Debug, Clone, Copy, Default)]
pub struct SqlChangeTrackingProbe;

impl ChangeTrackingProbe for SqlChangeTrackingProbe {
    fn probe(
        &self,
        instance: &str,
        database: &str,
        password: &str,
        tables: &[String],
        ctx: &Ctx,
    ) -> Result<CtProbe, Error> {
        let mut c = connect(&agent_connection(instance, database, password), ctx)?;
        read_probe(&mut c, tables, ctx)
    }
}

fn flag(row: &Row, idx: usize) -> Result<bool, Error> {
    let v: Option<i32> = row.get(idx).map_err(|e| unexpected(&e.to_string()))?;
    Ok(v == Some(1))
}

fn unexpected(message: &str) -> Error {
    Error::unexpected(INVALID_OPERATION, message)
}

/// The probe over an open connection: [`PROBE_DATABASE_SQL`], then
/// [`probe_tables_sql`] over `tables` (none: no table query).
///
/// # Errors
/// A query failed, or answered columns of another shape.
pub fn read_probe(client: &mut Client, tables: &[String], ctx: &Ctx) -> Result<CtProbe, Error> {
    let r = run(client, PROBE_DATABASE_SQL, &[], PROBE_TIMEOUT, ctx)?;
    let row = r
        .rows()
        .first()
        .ok_or_else(|| unexpected("the Change Tracking probe returned no row"))?;
    let current_version: Option<i64> = row.get(1).map_err(|e| unexpected(&e.to_string()))?;
    let mut out = CtProbe {
        database_enabled: flag(row, 0)? || current_version.is_some(),
        current_version,
        tables: Vec::with_capacity(tables.len()),
    };
    if tables.is_empty() {
        return Ok(out);
    }
    let params: Vec<Param> = tables
        .iter()
        .enumerate()
        .map(|(i, t)| Param::new(&format!("t{i}"), t.as_str()))
        .collect();
    let r = run(
        client,
        &probe_tables_sql(tables.len()),
        &params,
        PROBE_TIMEOUT,
        ctx,
    )?;
    for row in r.rows() {
        let name: String = row.get(0).map_err(|e| unexpected(&e.to_string()))?;
        out.tables.push(CtTable {
            name,
            present: flag(row, 1)?,
            has_pk: flag(row, 2)?,
            tracked: flag(row, 3)?,
            can_view: flag(row, 4)?,
        });
    }
    Ok(out)
}

/// One open administrator connection that runs [`CtStep`]s.
pub trait CtSession {
    /// Runs `step` and reads its answer.
    ///
    /// # Errors
    /// The batch failed (an escaped `SqlException`: a refusal, a lock timeout, ...), or
    /// `ctx` ended.
    fn run(&mut self, step: &CtStep, ctx: &Ctx) -> Result<StepState, Error>;
}

/// Opens [`CtSession`]s as a bootstrap identity.
pub trait CtExecutor: Send + Sync {
    /// A session on `database` of `instance` as `credential`.
    ///
    /// # Errors
    /// The connection failed: `SQL_NO_ACCESS`, `SQL_LOGIN_FAILED`,
    /// `SQL_DATABASE_NOT_FOUND` (an access refusal another identity may cure) or another
    /// agent code; or `ctx` ended.
    fn open(
        &self,
        credential: &SqlAdminCredential,
        instance: &str,
        database: &str,
        ctx: &Ctx,
    ) -> Result<Box<dyn CtSession>, Error>;
}

/// The real executor: [`SqlServerAdmin`]'s setup connection.
#[derive(Debug, Clone, Copy, Default)]
pub struct SqlCtExecutor;

struct SqlCtSession {
    client: Client,
}

impl CtExecutor for SqlCtExecutor {
    fn open(
        &self,
        credential: &SqlAdminCredential,
        instance: &str,
        database: &str,
        ctx: &Ctx,
    ) -> Result<Box<dyn CtSession>, Error> {
        let admin = SqlServerAdmin::new(credential.clone());
        let client = admin.open(&SqlInstance::new(instance, "agent.json"), database, ctx)?;
        Ok(Box::new(SqlCtSession { client }))
    }
}

impl CtSession for SqlCtSession {
    fn run(&mut self, step: &CtStep, ctx: &Ctx) -> Result<StepState, Error> {
        let params: Vec<Param> = step
            .params()
            .into_iter()
            .map(|(name, value)| Param::new(name, value))
            .collect();
        let r = run(&mut self.client, step.sql(), &params, COMMAND_TIMEOUT, ctx)?;
        // The answer is the batch's last result set (an EXEC may add none).
        let answer: Option<String> = r
            .sets()
            .last()
            .and_then(|s| s.rows().first())
            .and_then(|row| row.try_get(0));
        let answer = answer.unwrap_or_default();
        step_state(&answer).ok_or_else(|| {
            unexpected(&format!(
                "{} answered '{answer}', which no Change Tracking step answers",
                step.describe()
            ))
        })
    }
}
