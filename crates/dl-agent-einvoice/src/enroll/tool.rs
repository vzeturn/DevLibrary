//! The `einvoice-enroll` sub-command: arguments, the "already enrolled" check and the call
//! into [`super::enroll`]. `install.ps1 -Setup einvoice-enroll` runs it after the dl-agent
//! enrolment, with the arguments from `DL_AGENT_SETUP_ARGS`.
//!
//! The one-time token comes from `--token` or, when that is absent, from the environment
//! variable `DL_AGENT_EINVOICE_TOKEN` (the argument wins), so an installer can keep it off
//! the command line. `--sql-auth windows` (or `MMEI_SQL_AUTH=windows`) seals a DSN without
//! a SQL login: the collector then signs in with the service account (SSPI).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use dl_agent::Error;
use dl_agent::tool::{Tool, ToolContext};
use dl_cli::{Arg, Command, Matches};

use super::ping::Ping;
use super::{EnrollOptions, HideInput, TOKEN_ENV, enroll, vn_midnight};
use crate::agent_config::AgentConfig;
use crate::module::settings::default_config_dir;
use crate::runner::{device_name, heartbeat_version};

#[cfg(test)]
mod tests;

/// The registered sub-command name.
pub const NAME: &str = "einvoice-enroll";

/// The tool to register with `App::tool`.
pub fn einvoice_enroll_tool() -> Tool {
    Tool {
        name: NAME,
        grammar,
        run,
    }
}

fn grammar(cmd: Command) -> Command {
    cmd.about("Enrolls this site with the Azolo E-Invoice platform and seals config.enc")
        .arg(Arg::option("token").help(
            "One-time enrollment token (default DL_AGENT_EINVOICE_TOKEN; this argument wins)",
        ))
        .arg(
            Arg::option("platform-url")
                .required()
                .help("Platform base URL"),
        )
        .arg(Arg::option("sql-host").help("SQL Server host (default MMEI_SQL_HOST, then .)"))
        .arg(Arg::option("sql-user").help("SQL login (default MMEI_SQL_USER, then smile)"))
        .arg(Arg::option("sql-password").help("SQL password (default MMEI_SQL_PASSWORD)"))
        .arg(Arg::option("sql-auth").help(
            "sql (a SQL login, default) or windows (the service account); default MMEI_SQL_AUTH",
        ))
        .arg(Arg::flag("sql-prompt").help("Ask for the SQL connection and test it first"))
        .arg(Arg::option("collect-from").help("Collect bills dated from YYYY-MM-DD (Vietnam time)"))
        .arg(Arg::option("config-dir").help("Where config.enc lives (default: the module's)"))
        .arg(Arg::flag("force").help("Enroll again although config.enc already opens"))
}

/// What the command line asked for.
#[derive(Debug, Clone, Default)]
pub(super) struct Args {
    pub token: String,
    pub platform_url: String,
    pub sql_host: String,
    pub sql_user: String,
    pub sql_password: String,
    pub sql_auth: String,
    pub sql_prompt: bool,
    pub collect_from: Option<String>,
    pub config_dir: Option<PathBuf>,
    pub force: bool,
}

impl Args {
    fn from_matches(m: &Matches) -> Args {
        let text = |k: &str| m.value(k).unwrap_or_default().to_owned();
        Args {
            token: text("token"),
            platform_url: text("platform-url"),
            sql_host: text("sql-host"),
            sql_user: text("sql-user"),
            sql_password: text("sql-password"),
            sql_auth: text("sql-auth"),
            sql_prompt: m.flag("sql-prompt"),
            collect_from: m.value("collect-from").map(str::to_owned),
            config_dir: m.value("config-dir").map(PathBuf::from),
            force: m.flag("force"),
        }
    }
}

fn run(_ctx: &ToolContext<'_>, m: &Matches) -> Result<(), Error> {
    let args = Args::from_matches(m);
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    run_args(
        &args,
        Some(&mut input),
        &mut out,
        Some(&hide_console_input),
        None,
        &|k| std::env::var(k).ok(),
    )
    .map_err(Error::Config)
}

/// `hideConsoleInput`: echo off while the SQL password is typed; nothing to hide when
/// standard input is not a console (or off Windows).
fn hide_console_input() -> Box<dyn FnOnce()> {
    let echo = dl_winos::console::hide_input().ok();
    Box::new(move || drop(echo))
}

/// The tool body with its environment injected. An unreadable `config.enc` is an error
/// (never overwritten unless `--force`); a readable one means the site is enrolled and the
/// one-time token stays unspent.
pub(super) fn run_args<'a>(
    args: &Args,
    input: Option<&'a mut dyn BufRead>,
    out: &'a mut dyn Write,
    hide_input: Option<HideInput<'a>>,
    ping: Option<Ping<'a>>,
    env: &'a dyn Fn(&str) -> Option<String>,
) -> Result<(), String> {
    let config_dir = args.config_dir.clone().unwrap_or_else(default_config_dir);
    if !args.force && already_enrolled(&config_dir)? {
        say!(out, "da co cau hinh E-Invoice, giu nguyen\n");
        return Ok(());
    }
    let token = resolve_token(args, env)?;
    let collect_from = args
        .collect_from
        .as_deref()
        .map(|d| vn_midnight(Some(d)))
        .transpose()?;
    enroll(EnrollOptions {
        config_dir,
        platform_url: args.platform_url.clone(),
        enroll_token: token,
        agent_host: device_name(),
        sql_host: args.sql_host.clone(),
        sql_user: args.sql_user.clone(),
        sql_password: args.sql_password.clone(),
        sql_auth: args.sql_auth.clone(),
        collect_from,
        prompt_sql: args.sql_prompt,
        input,
        hide_input,
        out,
        agent_version: heartbeat_version(),
        ping,
        env,
    })
}

/// `--token` wins; otherwise `DL_AGENT_EINVOICE_TOKEN`, which keeps the token off the
/// command line. Neither is an error before anything is spent.
pub(super) fn resolve_token(
    args: &Args,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let token = if args.token.is_empty() {
        env(TOKEN_ENV).unwrap_or_default().trim().to_owned()
    } else {
        args.token.clone()
    };
    if token.is_empty() {
        return Err(format!("{NAME} needs --token or {TOKEN_ENV}"));
    }
    Ok(token)
}

/// `Ok(true)` when `config.enc` in `dir` opens and parses, `Ok(false)` when there is none.
fn already_enrolled(dir: &Path) -> Result<bool, String> {
    match AgentConfig::load(dir) {
        Ok(Some(_)) => Ok(true),
        Ok(None) => Ok(false),
        Err(e) => Err(format!(
            "{}: {e} (fix or remove it, or pass --force to enroll again)",
            dir.join("config.enc").display()
        )),
    }
}
