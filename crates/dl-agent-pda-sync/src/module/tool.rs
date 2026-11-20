//! The `pda-enroll` and `pda-grants` sub-commands: the C# `PdaSyncAgent enroll` and
//! `PdaSyncAgent grants` verbs over the module's data folder (`<data>/modules/pda-sync`).
//!
//! ```text
//! dl-agent pda-enroll --code <one-time code> --url https://<tenant>.<pda cloud> [--dry-run]
//!                     [--sql-instance <data source>] [--database SMILE_POS]
//!                     [--sql-admin-user <login> | --sql-admin-saved]
//! dl-agent pda-grants [--dry-run] [--sql-instance <data source>] [--database SMILE_POS]
//!                     [--sql-admin-user <login> | --sql-admin-saved]
//! ```
//!
//! Without `--sql-admin-user` (or `--sql-admin-saved`) `pda-enroll` tries the Windows login
//! running the command, then the SMILE default SQL login `smile` when SQL Server refuses
//! the first ([`crate::hosting::enroll_attempts`]); with `--sql-admin-user`, the password comes from `DL_AGENT_PDA_SQL_ADMIN_PASSWORD`,
//! `PDA_SQL_ADMIN_PASSWORD` or a hidden prompt, never from argv. `--sql-admin-saved` uses
//! the SQL login dl-agent keeps in `sql.json` ([`super::admin_login`]), so a remote attach
//! works where the service account is not a SQL Server administrator. The run's log goes to `logs/tools-.log` and its site events are sent
//! once the machine has an identity.

use std::path::{Path, PathBuf};

use dl_agent::Error;
use dl_agent::tool::{Tool, ToolContext};
use dl_cli::{Arg, Command, Matches};
use dl_json::Value;

use super::admin_login::{AdminPassword, DEFAULT_DATABASE, SQL_ADMIN_SAVED_FLAG, saved_login};
use super::{NAME as MODULE, settings};
use crate::hosting::cli::Options;
use crate::hosting::enroll::SQL_ADMIN_USER_OPTION;
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
        .arg(Arg::option("sql-admin-user").help(
            "A SQL administrator login (password from DL_AGENT_PDA_SQL_ADMIN_PASSWORD, \
             PDA_SQL_ADMIN_PASSWORD or a hidden prompt); default (pda-enroll): the Windows login \
             running this command, then the SMILE default login smile if it is refused",
        ))
        .arg(Arg::flag("sql-admin-saved").help(
            "Use the SQL login dl-agent saved in sql.json as the administrator (the POS \
             database's, else the first SQL login there)",
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

/// The options and password source after `--sql-admin-saved`: the saved login becomes
/// `--sql-admin-user` (and `--sql-instance` when none is given) with its password fixed.
pub fn admin_from(
    data_dir: &Path,
    m: &Matches,
    mut o: Options,
) -> Result<(Options, AdminPassword), Error> {
    if !m.flag(SQL_ADMIN_SAVED_FLAG) {
        return Ok((o, AdminPassword::from_environment()));
    }
    if o.get(SQL_ADMIN_USER_OPTION).is_some() {
        return Err(Error::Config(format!(
            "--{SQL_ADMIN_SAVED_FLAG} and --{SQL_ADMIN_USER_OPTION} exclude each other"
        )));
    }
    let database = o.get("database").unwrap_or(DEFAULT_DATABASE).to_string();
    let login = saved_login(data_dir, &database)?;
    if o.get("sql-instance").is_none() {
        o.insert("sql-instance", &login.data_source);
    }
    o.insert(SQL_ADMIN_USER_OPTION, &login.user);
    Ok((o, AdminPassword::fixed(login.password)))
}

fn run_enroll(ctx: &ToolContext<'_>, m: &Matches) -> Result<(), Error> {
    let host =
        settings::host_context(&Value::Null, &module_dir(ctx.data_dir)).map_err(Error::Config)?;
    let (opts, passwords) = admin_from(ctx.data_dir, m, options_from(m, true))?;
    let code = crate::hosting::enroll::run_with(&host, &opts, &passwords, &Stdout, &Stderr);
    exit(ENROLL, code)
}

fn run_grants(ctx: &ToolContext<'_>, m: &Matches) -> Result<(), Error> {
    let host =
        settings::host_context(&Value::Null, &module_dir(ctx.data_dir)).map_err(Error::Config)?;
    let (opts, passwords) = admin_from(ctx.data_dir, m, options_from(m, false))?;
    let code = crate::hosting::grants::run_with(&host, &opts, &passwords, &Stdout, &Stderr);
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
    fn the_saved_login_becomes_the_sql_administrator() {
        let dir = std::env::temp_dir().join(format!("pda-tool-saved-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(
            dir.join(dl_agent::sql::SQL_FILE),
            r#"{"databases":{"SMILE_POS":{"connection":"Server=SRV\\POS;Database=SMILE_POS;User Id=sa;Password=pw-1"}}}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let m = parse(pda_grants_tool(), &["--sql-admin-saved"]);
        let (o, p) =
            admin_from(&dir, &m, options_from(&m, false)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(o.get(SQL_ADMIN_USER_OPTION), Some("sa"));
        assert_eq!(o.get("sql-instance"), Some(r"SRV\POS"));
        use crate::hosting::secret_prompt::SecretPrompt;
        let pw = p.read(crate::bootstrap::admin::PASSWORD_ENV_VAR, "");
        assert_eq!(pw.as_deref(), Some("pw-1"));
        // An explicit instance stays; an explicit admin user conflicts.
        let m = parse(
            pda_grants_tool(),
            &["--sql-admin-saved", "--sql-instance", "OTHER"],
        );
        let (o, _) =
            admin_from(&dir, &m, options_from(&m, false)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(o.get("sql-instance"), Some("OTHER"));
        let m = parse(
            pda_grants_tool(),
            &["--sql-admin-saved", "--sql-admin-user", "x"],
        );
        assert!(admin_from(&dir, &m, options_from(&m, false)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_module_folder_is_under_modules() {
        assert_eq!(
            module_dir(Path::new("data")),
            Path::new("data").join("modules").join("pda-sync")
        );
    }
}
