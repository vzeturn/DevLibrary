//! Zero-touch SQL setup (port of `Bootstrap/SqlBootstrapper.cs`): find the POS database,
//! check the server, verify the vendor schema fingerprint, then idempotently create the
//! least-privilege `pda_sync` login and the additive `Pda_Sync*` tables, and apply the
//! grants. The scripts are the C# embedded resources, included unchanged.

use super::admin::SqlAdmin;
use super::discovery::SqlInstanceDiscovery;
use super::error::{Error, Result};
use super::fingerprint::SchemaFingerprint;
use super::models::{BootstrapReport, BootstrapStep, SqlInstance, StepKind, StepOutcome};
use super::preflight::{SqlPreflight, check_server};
use crate::config::{AgentError, codes};
use crate::reporting::log::{Exception, Logger, Value};
use crate::security::{SharedSecretStore, names};
use crate::util::ctx::Ctx;
use crate::util::stj::eq_ignore_case;
use std::sync::Arc;

/// `ILogger<SqlBootstrapper>`.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Bootstrap.SqlBootstrapper";
/// The database tried first.
pub const DEFAULT_DATABASE: &str = "SMILE_POS";
/// The runtime login.
pub const LOGIN_NAME: &str = "pda_sync";
const SYSTEM_DATABASES: [&str; 4] = ["master", "model", "msdb", "tempdb"];
/// The alphabet of the generated `pda_sync` password.
pub const PASSWORD_ALPHABET: &str = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";

const fn step(
    id: &'static str,
    kind: StepKind,
    object_name: &'static str,
    resource_name: &'static str,
    server_level: bool,
) -> BootstrapStep {
    BootstrapStep {
        id,
        kind,
        object_name,
        resource_name,
        server_level,
    }
}

/// The steps in order (`SqlBootstrapper.Steps`).
pub const STEPS: [BootstrapStep; 9] = [
    step(
        "login",
        StepKind::Login,
        LOGIN_NAME,
        "Sql.010_login.sql",
        true,
    ),
    step(
        "user",
        StepKind::User,
        LOGIN_NAME,
        "Sql.020_user.sql",
        false,
    ),
    step(
        "command_log",
        StepKind::Table,
        "Pda_SyncCommandLog",
        "Sql.030_command_log.sql",
        false,
    ),
    step(
        "sync_state",
        StepKind::Table,
        "Pda_SyncState",
        "Sql.040_sync_state.sql",
        false,
    ),
    step(
        "grants",
        StepKind::Grants,
        LOGIN_NAME,
        "Sql.050_grants.sql",
        false,
    ),
    step(
        "command_grants",
        StepKind::CommandGrants,
        LOGIN_NAME,
        "Sql.060_command_grants.sql",
        false,
    ),
    step(
        "order_grants",
        StepKind::OrderGrants,
        LOGIN_NAME,
        "Sql.070_order_grants.sql",
        false,
    ),
    step(
        "order_grants_ow1c",
        StepKind::OrderGrantsOw1c,
        LOGIN_NAME,
        "Sql.080_order_grants_ow1c.sql",
        false,
    ),
    step(
        "order_grants_ow1e",
        StepKind::OrderGrantsOw1e,
        LOGIN_NAME,
        "Sql.090_order_grants_ow1e.sql",
        false,
    ),
];

/// The embedded scripts by C# resource name.
const SCRIPTS: [(&str, &str); 9] = [
    (
        "Sql.010_login.sql",
        include_str!("../../assets/sql/010_login.sql"),
    ),
    (
        "Sql.020_user.sql",
        include_str!("../../assets/sql/020_user.sql"),
    ),
    (
        "Sql.030_command_log.sql",
        include_str!("../../assets/sql/030_command_log.sql"),
    ),
    (
        "Sql.040_sync_state.sql",
        include_str!("../../assets/sql/040_sync_state.sql"),
    ),
    (
        "Sql.050_grants.sql",
        include_str!("../../assets/sql/050_grants.sql"),
    ),
    (
        "Sql.060_command_grants.sql",
        include_str!("../../assets/sql/060_command_grants.sql"),
    ),
    (
        "Sql.070_order_grants.sql",
        include_str!("../../assets/sql/070_order_grants.sql"),
    ),
    (
        "Sql.080_order_grants_ow1c.sql",
        include_str!("../../assets/sql/080_order_grants_ow1c.sql"),
    ),
    (
        "Sql.090_order_grants_ow1e.sql",
        include_str!("../../assets/sql/090_order_grants_ow1e.sql"),
    ),
];

/// `SqlBootstrapper.ReadScript`: the embedded script (a UTF-8 BOM dropped, as
/// `StreamReader` does); `None` for an unknown resource.
pub fn read_script(resource_name: &str) -> Option<&'static str> {
    SCRIPTS
        .iter()
        .find(|(n, _)| *n == resource_name)
        .map(|(_, s)| s.strip_prefix('\u{feff}').unwrap_or(s))
}

/// `SqlBootstrapper.ThrowIfBlocked`: the first blocking finding as a coded failure.
pub fn throw_if_blocked(preflight: &SqlPreflight) -> std::result::Result<(), AgentError> {
    match preflight.findings.iter().find(|f| f.is_error()) {
        Some(b) => Err(AgentError::new(b.code.clone(), b.message.clone())),
        None => Ok(()),
    }
}

/// `"Aa1"` and 32 characters of [`PASSWORD_ALPHABET`] from the OS generator.
pub fn generate_password() -> Result<String> {
    let alphabet = PASSWORD_ALPHABET.as_bytes();
    let mut s = String::from("Aa1");
    for _ in 0..32 {
        let i = dl_rand::secure_below(alphabet.len() as u64).map_err(|e| {
            Error::unexpected(
                "System.Security.Cryptography.CryptographicException",
                e.to_string(),
            )
        })?;
        s.push(char::from(alphabet[usize::try_from(i).unwrap_or(0)]));
    }
    Ok(s)
}

/// The SQL bootstrap (`SqlBootstrapper`).
pub struct SqlBootstrapper {
    discovery: Arc<dyn SqlInstanceDiscovery>,
    admin: Arc<dyn SqlAdmin>,
    fingerprint: SchemaFingerprint,
    secrets: SharedSecretStore,
    log: Logger,
}

impl std::fmt::Debug for SqlBootstrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlBootstrapper").finish_non_exhaustive()
    }
}

impl SqlBootstrapper {
    /// A bootstrapper over its parts.
    pub fn new(
        discovery: Arc<dyn SqlInstanceDiscovery>,
        admin: Arc<dyn SqlAdmin>,
        fingerprint: SchemaFingerprint,
        secrets: SharedSecretStore,
        log: &Logger,
    ) -> SqlBootstrapper {
        SqlBootstrapper {
            discovery,
            admin,
            fingerprint,
            secrets,
            log: log.for_category(CATEGORY),
        }
    }

    /// The secret store.
    pub fn secrets(&self) -> &SharedSecretStore {
        &self.secrets
    }

    /// Preflight, then the bootstrap; a blocking finding fails before any write, dry run
    /// included (`RunAsync`).
    pub fn run(
        &self,
        dry_run: bool,
        instance_hint: Option<&str>,
        database_hint: Option<&str>,
        ctx: &Ctx,
    ) -> Result<BootstrapReport> {
        let preflight = self.preflight(instance_hint, database_hint, ctx)?;
        throw_if_blocked(&preflight)?;
        self.apply(&preflight, dry_run, ctx)
    }

    /// Read-only: locates the POS database and reads the server (`PreflightAsync`).
    /// Locating failures fail; the server checks are findings.
    pub fn preflight(
        &self,
        instance_hint: Option<&str>,
        database_hint: Option<&str>,
        ctx: &Ctx,
    ) -> Result<SqlPreflight> {
        let (instance, database, score) = self.locate(instance_hint, database_hint, ctx)?;
        let server = self.admin.server_info(&instance, ctx)?;
        let findings = check_server(&instance, &server);
        self.log.info(
            "Using {Instance}/{Database} (schema match {Score:P0}) on SQL Server {SqlVersion} {SqlLevel} ({SqlEdition})",
            &[
                Value::from(instance.server_name.as_str()),
                Value::from(database.as_str()),
                Value::Float(score),
                Value::from(server.product_version.as_str()),
                Value::from(server.product_level.as_str()),
                Value::from(server.edition.as_str()),
            ],
        );
        Ok(SqlPreflight {
            instance,
            database,
            fingerprint_score: score,
            server,
            findings,
        })
    }

    fn have_secret(&self) -> Result<bool> {
        self.secrets
            .load(names::SQL_PASSWORD)
            .map(|v| v.is_some())
            .map_err(|e| Error::from_secret(&e))
    }

    /// The idempotent steps on a preflighted target; `dry_run` only reports what would
    /// run (`ApplyAsync`).
    pub fn apply(
        &self,
        preflight: &SqlPreflight,
        dry_run: bool,
        ctx: &Ctx,
    ) -> Result<BootstrapReport> {
        if !dry_run {
            throw_if_blocked(preflight)?;
        }
        let instance = &preflight.instance;
        let database = &preflight.database;
        self.log.info(
            "Bootstrap of {Instance}/{Database}{Mode}",
            &[
                Value::from(instance.server_name.as_str()),
                Value::from(database.as_str()),
                Value::from(if dry_run { " [dry-run]" } else { "" }),
            ],
        );
        let mut outcomes = Vec::new();
        for step in &STEPS {
            let db = if step.server_level {
                "master"
            } else {
                database
            };
            let have_secret = self.have_secret()?;
            let exists = self.admin.exists(instance, db, step, ctx)?;
            // An existing login is only reusable while we still hold its password.
            let skip = exists && (step.kind != StepKind::Login || have_secret);
            if skip || dry_run {
                outcomes.push(StepOutcome {
                    id: step.id.to_owned(),
                    skipped: skip,
                    executed: false,
                });
                continue;
            }
            let script = self.render(step)?;
            self.admin.execute(instance, db, &script, ctx)?;
            outcomes.push(StepOutcome {
                id: step.id.to_owned(),
                skipped: false,
                executed: true,
            });
            self.log
                .info("Bootstrap step {Step} applied", &[Value::from(step.id)]);
        }
        Ok(BootstrapReport {
            instance: instance.clone(),
            database: database.clone(),
            dry_run,
            steps: outcomes,
            fingerprint_score: preflight.fingerprint_score,
        })
    }

    fn render(&self, step: &BootstrapStep) -> Result<String> {
        let script = read_script(step.resource_name).ok_or_else(|| {
            Error::unexpected(
                "System.InvalidOperationException",
                format!("Embedded SQL resource '{}' is missing.", step.resource_name),
            )
        })?;
        if step.kind != StepKind::Login {
            return Ok(script.to_owned());
        }
        let password = generate_password()?;
        crate::reporting::scrubber::register(Some(&password));
        // Persisted before use.
        self.secrets
            .save(names::SQL_PASSWORD, password.as_bytes())
            .map_err(|e| Error::from_secret(&e))?;
        Ok(script.replace("{{PASSWORD}}", &password))
    }

    fn locate(
        &self,
        instance_hint: Option<&str>,
        database_hint: Option<&str>,
        ctx: &Ctx,
    ) -> Result<(SqlInstance, String, f64)> {
        let instances = match instance_hint {
            Some(h) if !h.is_empty() => vec![SqlInstance::new(h, "argument")],
            _ => self.discovery.discover(ctx)?,
        };
        if instances.is_empty() {
            return Err(AgentError::new(
                codes::SQL_INSTANCE_NOT_FOUND,
                "No local SQL Server instance was found. Check that the SQL Server service is running.",
            )
            .into());
        }
        let preferred = database_hint.unwrap_or(DEFAULT_DATABASE);
        let mut denied: Option<Error> = None;
        let mut mismatch: Option<AgentError> = None;
        let mut reachable = false;
        for instance in &instances {
            let databases = match self.admin.list_databases(instance, ctx) {
                Ok(d) => d,
                Err(Error::Agent(e))
                    if matches!(
                        e.code(),
                        codes::SQL_NO_ACCESS
                            | codes::SQL_UNREACHABLE
                            | codes::SQL_LOGIN_FAILED
                            | codes::SQL_DATABASE_NOT_FOUND
                    ) =>
                {
                    self.log.warn_ex(
                        Exception::from_agent_error(&e),
                        "No access to {Instance}",
                        &[Value::from(instance.server_name.as_str())],
                    );
                    denied = Some(Error::Agent(e));
                    continue;
                }
                Err(e) => return Err(e),
            };
            reachable = true;
            for db in order(&databases, preferred) {
                let tables = self.admin.list_tables(instance, db, ctx)?;
                if self.fingerprint.matches(&tables) {
                    return Ok((
                        instance.clone(),
                        db.to_owned(),
                        self.fingerprint.score(&tables),
                    ));
                }
                if eq_ignore_case(db, preferred)
                    && let Err(e) = self.fingerprint.require(&tables, db)
                {
                    mismatch = Some(e);
                }
            }
        }
        if let Some(m) = mismatch {
            return Err(m.into());
        }
        if let (false, Some(d)) = (reachable, denied) {
            return Err(d);
        }
        let list: Vec<&str> = instances.iter().map(|i| i.server_name.as_str()).collect();
        Err(AgentError::new(
            codes::SQL_DATABASE_NOT_FOUND,
            format!(
                "No on-premise POS database was found on the local SQL Server instance(s) ({}).",
                list.join(", ")
            ),
        )
        .into())
    }
}

/// User databases, the preferred one first (`Order`).
fn order<'a>(databases: &'a [String], preferred: &str) -> Vec<&'a str> {
    let user: Vec<&str> = databases
        .iter()
        .map(String::as_str)
        .filter(|d| !SYSTEM_DATABASES.iter().any(|s| eq_ignore_case(s, d)))
        .collect();
    let mut out: Vec<&str> = user
        .iter()
        .copied()
        .filter(|d| eq_ignore_case(d, preferred))
        .collect();
    out.extend(
        user.iter()
            .copied()
            .filter(|d| !eq_ignore_case(d, preferred)),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_step_has_its_embedded_script_in_order() {
        let ids: Vec<&str> = STEPS.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            [
                "login",
                "user",
                "command_log",
                "sync_state",
                "grants",
                "command_grants",
                "order_grants",
                "order_grants_ow1c",
                "order_grants_ow1e"
            ]
        );
        for s in &STEPS {
            let sql = read_script(s.resource_name).unwrap();
            assert!(!sql.is_empty(), "{}", s.id);
        }
        assert!(
            read_script("Sql.010_login.sql")
                .unwrap()
                .contains("{{PASSWORD}}")
        );
        assert!(read_script("Sql.999.sql").is_none());
        assert!(STEPS[0].server_level && STEPS[1..].iter().all(|s| !s.server_level));
    }

    #[test]
    fn passwords_are_long_and_from_the_alphabet() {
        let p = generate_password().unwrap();
        assert_eq!(p.len(), 35);
        assert!(p.starts_with("Aa1"));
        assert!(p[3..].chars().all(|c| PASSWORD_ALPHABET.contains(c)));
        assert_ne!(p, generate_password().unwrap());
    }

    #[test]
    fn the_preferred_database_comes_first_and_system_databases_never() {
        let dbs: Vec<String> = ["master", "A_POS", "smile_pos", "tempdb", "Z"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(order(&dbs, "SMILE_POS"), ["smile_pos", "A_POS", "Z"]);
    }
}
