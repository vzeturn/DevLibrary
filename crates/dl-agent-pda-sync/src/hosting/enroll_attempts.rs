//! Which SQL Server administrator `enroll` tries (RT-0365).
//!
//! With no explicit administrator (`--sql-admin-user`, `--sql-admin-saved`) the order is:
//!
//! 1. Windows authentication as the identity running dl-agent (LocalSystem for the service);
//! 2. the SMILE default SQL login (`smile`), whose password comes from
//!    `DL_AGENT_PDA_SQL_DEFAULT_PASSWORD` or the build-time default; it only creates
//!    `pda_sync` and is never altered.
//!
//! Only an access refusal (`SQL_NO_ACCESS`, `SQL_LOGIN_FAILED`, `SQL_DATABASE_NOT_FOUND`)
//! moves to the next identity. That is safe to repeat: the workflow does its SQL before it
//! redeems the one-time code and every bootstrap step is idempotent. Every attempt is a
//! console line and a site event; no password is ever written to either.

use super::cli::Options;
use super::enroll::{SQL_ADMIN_USER_OPTION, admin_credential, finish};
use super::secret_prompt::SecretPrompt;
use crate::bootstrap::admin::SqlAdminCredential;
use crate::bootstrap::error::Error;
use crate::bootstrap::workflow::{EnrollArgs, EnrollResult, EnrollWorkflow};
use crate::config::{AgentError, AgentPaths, codes};
use crate::contracts::wire::site_event_levels::{INFO, WARN};
use crate::module::admin_login::ADMIN_PASSWORD_ENV;
use crate::reporting::ToolRun;
use crate::reporting::log::Exception;
use crate::reporting::run_stages::RunStages;
use crate::util::console::LineWriter;
use crate::util::ctx::Ctx;
use dl_json::Map;

/// The event code of one administrator attempt.
pub const ATTEMPT_CODE: &str = "ENROLL_SQL_ADMIN_ATTEMPT";
/// The environment variable naming the default SQL login.
pub const DEFAULT_USER_ENV: &str = "DL_AGENT_PDA_SQL_DEFAULT_USER";
/// The environment variable holding the default SQL login's password.
pub const DEFAULT_PASSWORD_ENV: &str = "DL_AGENT_PDA_SQL_DEFAULT_PASSWORD";
/// The SMILE default SQL login.
pub const DEFAULT_USER: &str = "smile";
/// The label of the Windows identity attempt.
pub const WINDOWS_LABEL: &str = "windows";

/// The default login's password baked into the release build (empty in other builds).
pub fn build_default_password() -> &'static str {
    option_env!("MMEI_BUILD_DEFAULT_SQL_PASSWORD").unwrap_or("")
}

/// The SMILE default login: where its name and password come from.
pub struct DefaultLogin {
    user: String,
    password: String,
}

impl DefaultLogin {
    /// From the process environment and this build's default.
    pub fn from_process() -> DefaultLogin {
        DefaultLogin::from_env(&|name| std::env::var(name).ok(), build_default_password())
    }

    /// From an environment reader and a build-time default password.
    pub fn from_env(env: &dyn Fn(&str) -> Option<String>, build_password: &str) -> DefaultLogin {
        let non_empty = |name: &str| env(name).filter(|v| !v.is_empty());
        DefaultLogin {
            user: non_empty(DEFAULT_USER_ENV).unwrap_or_else(|| DEFAULT_USER.to_owned()),
            password: non_empty(DEFAULT_PASSWORD_ENV).unwrap_or_else(|| build_password.to_owned()),
        }
    }
}

/// One administrator identity `enroll` may try.
#[derive(Debug)]
pub enum Attempt {
    /// Run the workflow as `credential`.
    Try {
        /// `windows` or `sql:<login>`.
        label: String,
        /// The identity.
        credential: SqlAdminCredential,
    },
    /// An identity that cannot be tried; recorded and passed over.
    Skipped {
        /// `windows` or `sql:<login>`.
        label: String,
        /// Why.
        reason: String,
    },
}

/// Windows first, then the default SQL login (skipped, and said so, without a password).
/// The password is registered with the scrubber here, before anything can print it.
pub fn default_attempts(login: &DefaultLogin) -> Vec<Attempt> {
    let windows = Attempt::Try {
        label: WINDOWS_LABEL.to_owned(),
        credential: SqlAdminCredential::windows(),
    };
    let label = format!("sql:{}", login.user);
    let second = if login.password.is_empty() {
        Attempt::Skipped {
            label,
            reason: format!(
                "no default password in this build and {DEFAULT_PASSWORD_ENV} is not set"
            ),
        }
    } else {
        crate::reporting::scrubber::register(Some(&login.password));
        Attempt::Try {
            label,
            credential: SqlAdminCredential::sql(&login.user, &login.password),
        }
    };
    vec![windows, second]
}

/// The attempts of a run: the explicit login alone with `--sql-admin-user`, else the
/// default order.
pub fn attempts_for(
    opts: &Options,
    passwords: &dyn SecretPrompt,
    login: &DefaultLogin,
) -> Result<Vec<Attempt>, AgentError> {
    if opts.contains(SQL_ADMIN_USER_OPTION) {
        let credential = admin_credential(opts, passwords)?;
        let label = format!("sql:{}", credential.user.as_deref().unwrap_or_default());
        return Ok(vec![Attempt::Try { label, credential }]);
    }
    Ok(default_attempts(login))
}

/// The refusals that another identity may cure.
pub fn is_access_refusal(code: &str) -> bool {
    matches!(
        code,
        codes::SQL_NO_ACCESS | codes::SQL_LOGIN_FAILED | codes::SQL_DATABASE_NOT_FOUND
    )
}

/// The access refusal of a run, if it was one: a returned error, or a failed preflight
/// finding of a dry run.
fn refusal(result: &Result<EnrollResult, Error>, dry_run: bool) -> Option<(String, String)> {
    match result {
        Err(Error::Agent(e)) if is_access_refusal(e.code()) => {
            Some((e.code().to_owned(), e.message().to_owned()))
        }
        Ok(r) if dry_run => r
            .failed_codes()
            .into_iter()
            .find(|c| is_access_refusal(c))
            .map(|c| (c, "preflight refused the SQL administrator".to_owned())),
        _ => None,
    }
}

struct Record {
    label: String,
    code: String,
    message: String,
}

fn event(run: &ToolRun, level: &str, n: usize, of: usize, label: &str, code: &str, text: &str) {
    let mut d = Map::new();
    d.insert("attempt", i64::try_from(n).unwrap_or(i64::MAX));
    d.insert("of", i64::try_from(of).unwrap_or(i64::MAX));
    d.insert("identity", label);
    d.insert("code", code);
    run.stage(level, ATTEMPT_CODE, text, Some(d));
}

/// Runs the attempts in order until one is not refused; only that last outcome goes
/// through the normal result handling ([`finish`]).
#[allow(clippy::too_many_arguments)]
pub fn run_attempts(
    attempts: Vec<Attempt>,
    build: &dyn Fn(&SqlAdminCredential) -> Result<EnrollWorkflow, Box<Exception>>,
    paths: &AgentPaths,
    args: &EnrollArgs,
    run: &ToolRun,
    stdout: &dyn LineWriter,
    stderr: &dyn LineWriter,
    ctx: &Ctx,
) -> Result<i32, Box<Exception>> {
    let of = attempts.len();
    let machine = crate::diagnostics::machine_name();
    let mut records: Vec<Record> = Vec::new();
    let mut last: Option<Result<EnrollResult, Error>> = None;
    for (i, attempt) in attempts.into_iter().enumerate() {
        let n = i + 1;
        let (label, credential) = match attempt {
            Attempt::Skipped { label, reason } => {
                let text =
                    format!("SQL administrator attempt {n}/{of} ({label}) skipped: {reason}");
                run.line(&text);
                event(run, WARN, n, of, &label, "SKIPPED", &text);
                records.push(Record {
                    label,
                    code: "SKIPPED".to_owned(),
                    message: reason,
                });
                continue;
            }
            Attempt::Try { label, credential } => (label, credential),
        };
        if of == 1 {
            // An explicit administrator: one attempt, no fallback, no extra lines.
            let result = run_one(build, &credential, args, &machine, run, ctx)?;
            return finish(result, paths, args, run, stdout, stderr);
        }
        run.line(&format!(
            "SQL administrator attempt {n}/{of}: trying {label}"
        ));
        let result = run_one(build, &credential, args, &machine, run, ctx)?;
        match refusal(&result, args.dry_run) {
            Some((code, message)) => {
                let text = format!(
                    "SQL administrator attempt {n}/{of} ({label}) refused: [{code}] {message}"
                );
                run.line(&text);
                event(run, WARN, n, of, &label, &code, &text);
                records.push(Record {
                    label,
                    code,
                    message,
                });
                last = Some(result);
            }
            None => {
                let text = if n > 1 {
                    format!(
                        "SQL administrator identity used: {label} (attempt {n}/{of}, after a refusal)"
                    )
                } else {
                    format!("SQL administrator identity used: {label} (attempt {n}/{of})")
                };
                if n > 1 {
                    run.line(&text);
                }
                event(run, INFO, n, of, &label, "used", &text);
                return finish(result, paths, args, run, stdout, stderr);
            }
        }
    }
    let summary = exhausted_message(&records);
    let result = match last {
        // A dry run keeps its preflight verdict; the attempts are listed above it.
        Some(Ok(r)) => {
            stdout.write_line(&summary);
            Ok(r)
        }
        Some(Err(Error::Agent(e))) => Err(Error::Agent(AgentError::new(e.code(), summary))),
        Some(Err(other)) => Err(other),
        None => Err(Error::Agent(AgentError::new(codes::SQL_NO_ACCESS, summary))),
    };
    finish(result, paths, args, run, stdout, stderr)
}

fn run_one(
    build: &dyn Fn(&SqlAdminCredential) -> Result<EnrollWorkflow, Box<Exception>>,
    credential: &SqlAdminCredential,
    args: &EnrollArgs,
    machine: &str,
    run: &ToolRun,
    ctx: &Ctx,
) -> Result<Result<EnrollResult, Error>, Box<Exception>> {
    let workflow = build(credential)?;
    Ok(workflow.run(args, machine, &super::agent_version(), run, ctx))
}

/// The error text when every identity was refused or skipped.
fn exhausted_message(records: &[Record]) -> String {
    let list = records
        .iter()
        .enumerate()
        .map(|(i, r)| format!("{}. {}: [{}] {}", i + 1, r.label, r.code, r.message))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "No SQL Server administrator identity worked. Tried: {list}. Pass --sql-admin-user <login> with its \
         password in {ADMIN_PASSWORD_ENV} (or PDA_SQL_ADMIN_PASSWORD, or a prompt)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::preflight::PreflightFinding;

    fn dry_ok_with(code: &str) -> Result<EnrollResult, Error> {
        Ok(EnrollResult {
            sql: None,
            identity: None,
            findings: vec![PreflightFinding::error(code, "blocked")],
            cloud: None,
        })
    }

    #[test]
    fn only_access_refusals_move_to_the_next_identity() {
        let table = [
            (codes::SQL_NO_ACCESS, true),
            (codes::SQL_LOGIN_FAILED, true),
            (codes::SQL_DATABASE_NOT_FOUND, true),
            (codes::SQL_UNREACHABLE, false),
            (codes::BAD_ARGUMENTS, false),
        ];
        for (code, expected) in table {
            assert_eq!(is_access_refusal(code), expected, "{code}");
            let err = Err(Error::Agent(AgentError::new(code, "m")));
            assert_eq!(refusal(&err, false).is_some(), expected, "Err {code}");
            assert_eq!(
                refusal(&dry_ok_with(code), true).is_some(),
                expected,
                "dry {code}"
            );
            // A real run's findings are warnings, never a refusal.
            assert!(refusal(&dry_ok_with(code), false).is_none(), "real {code}");
        }
    }
}
