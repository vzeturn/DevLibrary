//! `dl-agent backup-enroll --hub-url <origin> [--force]`: the enroll key comes from env
//! `DL_AGENT_BACKUP_KEY` or one line of standard input, never from the command line (argv is
//! visible to every local user and lands in installer logs).

use std::io::{BufRead, Write};
use std::path::Path;

use dl_agent::tool::{Tool, ToolContext};
use dl_cli::{Arg, Command, Matches};

use super::{Outcome, enroll};

/// The sub-command name.
pub const NAME: &str = "backup-enroll";
/// The environment variable holding the enroll key.
pub const KEY_ENV: &str = "DL_AGENT_BACKUP_KEY";

/// The `backup-enroll` tool to register with `App::tool`.
pub fn backup_enroll_tool() -> Tool {
    Tool {
        name: NAME,
        grammar,
        run,
    }
}

fn grammar(cmd: Command) -> Command {
    cmd.about(
        "Enrolls this site with the SRBackup hub (enroll key from DL_AGENT_BACKUP_KEY or stdin)",
    )
    .arg(
        Arg::option("hub-url")
            .required()
            .help("Hub origin, e.g. https://backupapi.sr.io.vn"),
    )
    .arg(Arg::flag("force").help("Enroll again although an enrollment exists"))
}

fn run(ctx: &ToolContext<'_>, m: &Matches) -> Result<(), dl_agent::Error> {
    let module_dir = ctx.data_dir.join("modules").join(crate::module::NAME);
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    let hub_url = m.value("hub-url").unwrap_or_default().to_owned();
    run_args(
        &module_dir,
        &hub_url,
        m.flag("force"),
        &|k| std::env::var(k).ok(),
        &mut input,
        &mut out,
        &dl_agent::hub::hostname(),
    )
    .map_err(dl_agent::Error::from)
}

/// The testable body of the tool.
pub fn run_args(
    module_dir: &Path,
    hub_url: &str,
    force: bool,
    env: &dyn Fn(&str) -> Option<String>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    machine_name: &str,
) -> Result<(), crate::Error> {
    let key = match env(KEY_ENV).filter(|k| !k.trim().is_empty()) {
        Some(k) => k,
        None => read_key(input, out)?,
    };
    let say = |out: &mut dyn Write, text: String| {
        writeln!(out, "{text}").map_err(|e| crate::Error::io("write to stdout", &e))
    };
    match enroll(module_dir, hub_url, &key, machine_name, force)? {
        Outcome::AlreadyEnrolled { agent_id } => say(
            out,
            format!("already enrolled as agent {agent_id}; pass --force to enroll again"),
        ),
        Outcome::Enrolled {
            agent_id,
            replaced_unreadable,
        } => {
            if let Some(why) = replaced_unreadable {
                say(
                    out,
                    format!("replaced an enrollment file that did not open: {why}"),
                )?;
            }
            say(out, format!("enrolled as agent {agent_id}"))
        }
    }
}

fn read_key(input: &mut dyn BufRead, out: &mut dyn Write) -> Result<String, crate::Error> {
    write!(out, "enroll key: ").map_err(|e| crate::Error::io("write to stdout", &e))?;
    out.flush()
        .map_err(|e| crate::Error::io("write to stdout", &e))?;
    // Hiding the echo only works on a Windows console; elsewhere the key is typed visibly or
    // piped in, which is the installer's path.
    let echo = dl_winos::console::hide_input();
    let mut line = String::new();
    let read = input.read_line(&mut line);
    drop(echo);
    read.map_err(|e| crate::Error::io("read the enroll key from stdin", &e))?;
    writeln!(out).map_err(|e| crate::Error::io("write to stdout", &e))?;
    let key = line.trim().to_owned();
    if key.is_empty() {
        return Err(crate::Error::Auth(format!(
            "no enroll key: set {KEY_ENV} or pipe the key on standard input"
        )));
    }
    Ok(key)
}
