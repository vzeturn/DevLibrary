//! The `pda-enroll` and `pda-grants` sub-commands: the C# `PdaSyncAgent enroll` and
//! `PdaSyncAgent grants` verbs over the module's data folder (`<data>/modules/pda-sync`).
//!
//! ```text
//! dl-agent pda-enroll --code <one-time code> --url https://<tenant>.<pda cloud> [--dry-run]
//!                     [--sql-instance <data source>] [--database SMILE_POS]
//!                     [--sql-admin-user <login>]
//! dl-agent pda-grants [--dry-run] [--sql-instance <data source>] [--database SMILE_POS]
//!                     [--sql-admin-user <login>]
//! ```
//!
//! Without `--sql-admin-user` the SQL administrator is the Windows login running the
//! command; with it, the password comes from `PDA_SQL_ADMIN_PASSWORD` or a hidden prompt,
//! never from argv. The run's log goes to `logs/tools-.log` and its site events are sent
//! once the machine has an identity.

use std::path::{Path, PathBuf};

use dl_agent::Error;
use dl_agent::tool::{Tool, ToolContext};
use dl_cli::{Arg, Command, Matches};
use dl_json::Value;

use super::{NAME as MODULE, settings};
use crate::hosting::cli::Options;
use crate::hosting::enroll::SQL_ADMIN_USER_OPTION;
use crate::hosting::secret_prompt::ConsoleSecretPrompt;
use crate::util::console::{Stderr, Stdout};

/// The enrol sub-command name.
pub const ENROLL: &str = "pda-enroll";
/// The grants sub-command name.
pub const GRANTS: &str = "pda-grants";
/// Where `pda-enroll` reads the one-time code when `--code` is absent, so an installer can
/// keep it out of the process list.
pub const CODE_ENV: &str = "DL_AGENT_PDA_CODE";

/// `dl-agent pda-enroll`, to register with `App::tool`.
pub fn pda_enroll_tool() -> Tool {
    Tool {
        name: ENROLL,
        grammar: enroll_grammar,
        run: run_enroll,
    }
}

/// `dl-agent pda-grants`, to register with `App::tool`.
pub fn pda_grants_tool() -> Tool {
    Tool {
        name: GRANTS,
        grammar: grants_grammar,
        run: run_grants,
    }
}

fn sql_args(cmd: Command) -> Command {
    cmd.arg(Arg::flag("dry-run").help("Check and print the plan; change nothing"))
        .arg(
            Arg::option("sql-instance")
                .help("SQL Server data source (default: discovered on this machine)"),
        )
        .arg(Arg::option("database").help("The POS database (default: SMILE_POS)"))
        .arg(Arg::option(SQL_ADMIN_USER_OPTION).help(
            "A SQL administrator login (password from PDA_SQL_ADMIN_PASSWORD or a hidden \
             prompt); default: the Windows login running this command",
        ))
}

fn enroll_grammar(cmd: Command) -> Command {
    sql_args(
        cmd.about(
            "Enrolls this site with the PDA cloud: creates the pda_sync login and agent \
             tables in SMILE_POS and writes the identity of the pda-sync module",
        )
        .arg(
            Arg::option("code")
                .required()
                .env(CODE_ENV)
                .help("The one-time enrol code from the PDA admin (default: DL_AGENT_PDA_CODE)"),
        )
        .arg(
            Arg::option("url")
                .required()
                .help("The tenant's PDA cloud address, e.g. https://site.example"),
        ),
    )
}

fn grants_grammar(cmd: Command) -> Command {
    sql_args(
        cmd.about(
            "Applies the order grants of pda_sync in SMILE_POS (PostITrn, tmpInvoice, tmpTrn)",
        ),
    )
}

/// The module's data folder under the agent's.
pub fn module_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("modules").join(MODULE)
}

/// The port's `--key value` options from the parsed sub-command.
pub fn options_from(m: &Matches, with_enrol: bool) -> Options {
    let mut o = Options::default();
    let mut keys = vec!["sql-instance", "database", SQL_ADMIN_USER_OPTION];
    if with_enrol {
        keys.extend(["code", "url"]);
    }
    for k in keys {
        if let Some(v) = m.value(k) {
            o.insert(k, v);
        }
    }
    if m.flag("dry-run") {
        o.insert("dry-run", "true");
    }
    o
}

fn exit(verb: &str, code: i32) -> Result<(), Error> {
    if code == 0 {
        Ok(())
    } else {
        Err(Error::Config(format!("{verb} failed (exit {code})")))
    }
}

fn run_enroll(ctx: &ToolContext<'_>, m: &Matches) -> Result<(), Error> {
    let host =
        settings::host_context(&Value::Null, &module_dir(ctx.data_dir)).map_err(Error::Config)?;
    let code = crate::hosting::enroll::run_with(
        &host,
        &options_from(m, true),
        &ConsoleSecretPrompt,
        &Stdout,
        &Stderr,
    );
    exit(ENROLL, code)
}

fn run_grants(ctx: &ToolContext<'_>, m: &Matches) -> Result<(), Error> {
    let host =
        settings::host_context(&Value::Null, &module_dir(ctx.data_dir)).map_err(Error::Config)?;
    let code = crate::hosting::grants::run_with(
        &host,
        &options_from(m, false),
        &ConsoleSecretPrompt,
        &Stdout,
        &Stderr,
    );
    exit(GRANTS, code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(tool: Tool, argv: &[&str]) -> Matches {
        let cmd = (tool.grammar)(Command::new(tool.name));
        let args = std::iter::once(tool.name).chain(argv.iter().copied());
        cmd.parse_from(args)
            .unwrap_or_else(|e| panic!("{argv:?}: {e}"))
    }

    #[test]
    fn enrol_options_reach_the_port_verb() {
        let m = parse(
            pda_enroll_tool(),
            &[
                "--code",
                "ABC-123",
                "--url",
                "https://site.example",
                "--dry-run",
                "--database",
                "SMILE_POS",
            ],
        );
        let o = options_from(&m, true);
        let a = crate::hosting::enroll::args_from(&o);
        assert_eq!(a.code.as_deref(), Some("ABC-123"));
        assert_eq!(a.url.as_deref(), Some("https://site.example"));
        assert!(a.dry_run);
        assert_eq!(a.database.as_deref(), Some("SMILE_POS"));
        assert_eq!(a.sql_instance, None);
        assert_eq!(crate::hosting::enroll::sql_auth(&o), "windows");
    }

    #[test]
    fn the_code_may_come_from_the_environment() {
        let tool = pda_enroll_tool();
        let cmd = (tool.grammar)(Command::new(tool.name));
        let m = cmd
            .parse_from_with_env([ENROLL, "--url", "https://site.example"], |k| {
                (k == CODE_ENV).then(|| "XYZ-789".to_string())
            })
            .unwrap_or_else(|e| panic!("{e}"));
        let a = crate::hosting::enroll::args_from(&options_from(&m, true));
        assert_eq!(a.code.as_deref(), Some("XYZ-789"));
        // The command line still wins.
        let m = cmd
            .parse_from_with_env(
                [ENROLL, "--code", "ARG-1", "--url", "https://x.example"],
                |_| Some("ENV-2".to_string()),
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(m.value("code"), Some("ARG-1"));
    }

    #[test]
    fn a_sql_admin_login_switches_to_sql_auth() {
        let m = parse(pda_grants_tool(), &["--sql-admin-user", "sa"]);
        let o = options_from(&m, false);
        assert_eq!(crate::hosting::enroll::sql_auth(&o), "sql");
        assert!(!crate::hosting::grants::args_from(&o).dry_run);
    }

    #[test]
    fn the_module_folder_is_under_modules() {
        assert_eq!(
            module_dir(Path::new("data")),
            Path::new("data").join("modules").join("pda-sync")
        );
    }
}
