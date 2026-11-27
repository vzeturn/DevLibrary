//! The Change Tracking step of `grants` (RT-1318, ADR 0011 D2). With the flag
//! `capture.change_tracking` on, once the order grants are verified, `grants` probes the
//! captured tables as `pda_sync` and runs one enable round
//! ([`run_round`](crate::commands::change_tracking_round::run_round)) as the operator's
//! identity: `--sql-admin-user` alone, else the operator's Windows login and then the SMILE
//! default login (ADR 0010). Its stages are `CAPTURE_CT_ENABLED` and `CAPTURE_CT_PARTIAL`;
//! `CAPTURE_CT_FAILED` fails the run. With the flag off `grants` issues no Change Tracking
//! statement, and nothing here alters a login or a password.

use super::cli::{EXIT_FAILED, EXIT_OK};
use super::enroll_attempts::{Attempt, DefaultLogin, default_attempts};
use crate::bootstrap::admin::SqlAdminCredential;
use crate::bootstrap::change_tracking::plan;
use crate::bootstrap::change_tracking_sql::{
    ChangeTrackingProbe, CtExecutor, SqlChangeTrackingProbe, SqlCtExecutor,
};
use crate::bootstrap::error::Error;
use crate::commands::change_tracking_round::{classify, report, run_round};
use crate::config::codes;
use crate::contracts::SyncCatalog;
use crate::reporting::Logger;
use crate::reporting::log::Exception;
use crate::reporting::run_stages::RunStages;
use crate::security::{SecretStore, names};
use crate::util::console::LineWriter;
use crate::util::ctx::Ctx;

/// What the step runs on.
pub struct ChangeTrackingStep<'a> {
    /// The read-only probe as `pda_sync`.
    pub probe: &'a dyn ChangeTrackingProbe,
    /// The administrator sessions.
    pub exec: &'a dyn CtExecutor,
    /// The identities, in order ([`verb_attempts`]).
    pub attempts: Vec<Attempt>,
    /// The captured tables.
    pub tables: Vec<String>,
    /// The run's logger.
    pub log: Logger,
}

/// The identities of the step: the verb's `--sql-admin-user` login alone, else the
/// operator's Windows login and then the SMILE default login (`login`).
pub fn verb_attempts(credential: &SqlAdminCredential, login: &DefaultLogin) -> Vec<Attempt> {
    match &credential.user {
        Some(user) => vec![Attempt::Try {
            label: format!("sql:{user}"),
            credential: credential.clone(),
        }],
        None => default_attempts(login),
    }
}

/// The real step when `enabled` (the flag `capture.change_tracking`), `None` when off.
///
/// # Errors
/// The embedded catalog does not load.
pub fn real_step(
    enabled: bool,
    credential: &SqlAdminCredential,
    log: &Logger,
) -> Result<Option<ChangeTrackingStep<'static>>, Box<Exception>> {
    if !enabled {
        return Ok(None);
    }
    let catalog = SyncCatalog::load().map_err(|e| {
        Box::new(
            Error::unexpected("System.InvalidOperationException", e.to_string()).to_exception(),
        )
    })?;
    Ok(Some(ChangeTrackingStep {
        probe: &SqlChangeTrackingProbe,
        exec: &SqlCtExecutor,
        attempts: verb_attempts(credential, &DefaultLogin::from_process()),
        tables: crate::capture::plan::captured_tables(catalog),
        log: log.clone(),
    }))
}

impl ChangeTrackingStep<'_> {
    /// Probes `database` of `instance` as `pda_sync` and, while Change Tracking is not on
    /// for every captured table that can have it, runs one round and reports it. The exit
    /// code is the result: [`EXIT_FAILED`] when the probe failed or no identity applied
    /// anything.
    ///
    /// # Errors
    /// The secret store failed, or the probe failed with neither an agent code nor a SQL
    /// error.
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &self,
        secrets: &dyn SecretStore,
        instance: &str,
        database: &str,
        run: &dyn RunStages,
        stdout: &dyn LineWriter,
        stderr: &dyn LineWriter,
        ctx: &Ctx,
    ) -> Result<i32, Box<Exception>> {
        let password = secrets
            .load(names::SQL_PASSWORD)
            .map_err(|e| Box::new(Error::from_secret(&e).to_exception()))?;
        let Some(password) = password else {
            stdout.write_line(
                "Change Tracking not checked: the pda_sync password is not in the agent secret store.",
            );
            return Ok(EXIT_OK);
        };
        let password = String::from_utf8_lossy(&password).into_owned();
        let probe = match self
            .probe
            .probe(instance, database, &password, &self.tables, ctx)
        {
            Ok(p) => p,
            Err(e) if e.code().is_none() && e.sql().is_none() => {
                return Err(Box::new(e.to_exception()));
            }
            Err(e) => {
                let message = format!(
                    "Change Tracking could not be probed as pda_sync ({}).",
                    classify(&e).0
                );
                return Ok(failed(&message, run, stderr));
            }
        };
        if plan(&probe, true).is_done() {
            stdout.write_line(&format!("Change Tracking: {}.", probe.describe()));
            return Ok(EXIT_OK);
        }
        let outcome = run_round(
            &plan(&probe, true),
            &self.attempts,
            self.exec,
            instance,
            database,
            ctx,
            &self.log,
        );
        let r = report(&outcome, database);
        if r.code == codes::CAPTURE_CT_FAILED {
            return Ok(failed(&r.message, run, stderr));
        }
        stdout.write_line(&r.message);
        run.stage(r.level, r.code, &r.message, Some(r.detail));
        Ok(EXIT_OK)
    }
}

fn failed(message: &str, run: &dyn RunStages, stderr: &dyn LineWriter) -> i32 {
    run.fail_error(codes::CAPTURE_CT_FAILED, message);
    stderr.write_line(&format!("[{}] {message}", codes::CAPTURE_CT_FAILED));
    EXIT_FAILED
}

#[cfg(test)]
#[path = "grants_ct_tests.rs"]
mod tests;
