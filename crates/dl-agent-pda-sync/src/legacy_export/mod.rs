//! The `legacy.export` site command (RT-0390, ADR 0136): the store agent does what
//! `migrate-legacy.ps1` plus `PdaSyncAgent legacy-export` and `legacy-upload` do by hand.
//!
//! - [`payload`]: the command's payload, its sources and the site event codes.
//! - [`handler`]: pre-check, immediate ack, and the background run (export, zip, dry run,
//!   commit, count check, cleanup).
//! - [`sources`], [`sqlite_source`], [`sql_source`]: what is read (the on-prem `pda.db`, or
//!   the SQL Server tables of a build from before it, plus the registry), SELECT only.
//! - [`bundle`]: the bundle writer (`manifest.json` and gzip JSON lines per table).
//! - [`upload`]: the zip and the signed `PUT /sync/v1/legacy/import/{sha256}` sequence.
//! - [`events`]: the structured site events (`legacy-export` / `legacy-upload`).
//!
//! The bundle layout, file names and hashes are fixed by `contracts::legacy`; the cloud
//! importer validates them.

pub mod bundle;
pub mod events;
pub mod handler;
pub mod payload;
pub mod sources;
pub mod sql_source;
pub mod sqlite_source;
pub mod upload;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;

pub use handler::{AppVersionProbe, LegacyExportCommandHandler, LegacyExportEnv, SqlFactory};
pub use payload::{AGGREGATE_KEY, LEGACY_EXPORT, LegacyExportPayload};

use crate::config::AgentIdentity;
use crate::config::codes;
use crate::config::sql::agent_connection;
use crate::config::time::SharedClock;
use crate::contracts::time::TimeZone;
use crate::security::{SharedSecretStore, names};
use crate::transport::{SyncHttpClient, TokenProvider};

/// What the running agent gives the handler.
pub struct ProductionParts {
    /// `<data dir>/legacy-export`.
    pub work_dir: PathBuf,
    /// The enrolled identity (its SQL target is the default of the command).
    pub identity: AgentIdentity,
    /// The secret store (the `pda_sync` password).
    pub secrets: SharedSecretStore,
    /// The signed cloud client.
    pub http: Arc<SyncHttpClient>,
    /// Access tokens.
    pub tokens: Arc<TokenProvider>,
    /// The clock.
    pub clock: SharedClock,
    /// The site's time zone.
    pub zone: Arc<TimeZone>,
    /// Site events of a run.
    pub events: events::EventsFactory,
    /// `PdaSyncAgent <version>` for the manifest.
    pub exporter: String,
}

/// The environment of the running agent: the `pda_sync` login for SQL Server (the command's
/// `sql_instance` and `database` override the enrolled target), the machine's registry, the
/// agent's signed client for the upload.
pub fn production_env(p: ProductionParts) -> LegacyExportEnv {
    let identity = p.identity;
    let secrets = p.secrets;
    let sql: SqlFactory = Arc::new(move |cmd: &LegacyExportPayload| {
        let not_enrolled = |what: &str| {
            sources::ExportError::new(
                sources::stages::PRECHECK,
                codes::NOT_ENROLLED,
                format!("The agent has no {what} recorded; re-run enroll."),
            )
        };
        let instance = cmd
            .sql_instance
            .clone()
            .or_else(|| identity.sql_instance.clone())
            .ok_or_else(|| not_enrolled("SQL instance"))?;
        let database = cmd
            .database
            .clone()
            .or_else(|| identity.sql_database.clone())
            .ok_or_else(|| not_enrolled("SQL database"))?;
        let password = secrets
            .load(names::SQL_PASSWORD)
            .map_err(|_| {
                sources::ExportError::new(
                    sources::stages::PRECHECK,
                    codes::KEY_STORE_FAILED,
                    "The pda_sync password could not be read from the secret store.",
                )
            })?
            .ok_or_else(|| {
                sources::ExportError::new(
                    sources::stages::PRECHECK,
                    codes::REENROLL_REQUIRED,
                    "The pda_sync password is missing; re-run enroll.",
                )
            })?;
        let config = agent_connection(&instance, &database, &String::from_utf8_lossy(&password));
        Ok(
            Arc::new(sql_source::TdsSqlSource::new(config, &instance, &database))
                as Arc<dyn sources::LegacySqlSource>,
        )
    });
    LegacyExportEnv {
        work_dir: p.work_dir,
        pda_db: crate::config::options::common_application_data()
            .join("MM-PDA")
            .join("pda.db"),
        sql,
        pools: Arc::new(sources::AppCmdPoolProbe),
        branding: Arc::new(sources::MachineRegistry),
        vietqr: Arc::new(sources::machine_vietqr()),
        uploader: Arc::new(upload::HttpBundleUploader::new(p.http, p.tokens)),
        events: p.events,
        clock: p.clock,
        zone: p.zone,
        exporter: p.exporter,
        max_bytes: u64::try_from(crate::contracts::wire::legacy_import_wire::MAX_BYTES)
            .unwrap_or(0),
        app_version: Arc::new(|_| None),
    }
}
