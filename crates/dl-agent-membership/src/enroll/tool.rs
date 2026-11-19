//! The `membership-enroll` sub-command.
//!
//! ```text
//! DL_AGENT_MEMBERSHIP_TOKEN=<code> DL_AGENT_MEMBERSHIP_SMILE=<connection string> \
//!   dl-agent membership-enroll --hub-url https://hub.example
//! ```
//!
//! The code may also be piped on stdin. Neither value is ever taken from argv.
//!
//! `--switch` takes the site over from the .NET `MM.Membership.Agent` (keeps its
//! `config.dpapi`, stops and disables its service, records what it did) and `--rollback`
//! hands it back; see [`super::switch`].

use std::path::PathBuf;
use std::sync::Arc;

use dl_agent::Error;
use dl_agent::tool::{Tool, ToolContext};
use dl_cli::{Arg, Command, Matches};

use super::legacy::Places;
use super::service::{LegacyService, Scm};
use super::switch::{Switch, hub_probe, run_rollback, run_switch};
use super::{EnrollArgs, run_enroll};
use crate::config::{Dpapi, default_dir};
use crate::module::NAME as MODULE;

/// The registered sub-command name.
pub const NAME: &str = "membership-enroll";

/// The tool to register with `App::tool`.
pub fn membership_enroll_tool() -> Tool {
    Tool {
        name: NAME,
        grammar,
        run,
    }
}

fn grammar(cmd: Command) -> Command {
    cmd.about(
        "Enrolls this site with the Azolo Membership hub and writes config.dpapi \
         (code from DL_AGENT_MEMBERSHIP_TOKEN or stdin, SMILE from DL_AGENT_MEMBERSHIP_SMILE)",
    )
    .arg(
        Arg::option("hub-url")
            .help("Hub origin, e.g. https://membership.example (required unless --switch/--rollback)"),
    )
    .arg(
        Arg::option("config-dir").help(
            "Where config.dpapi lives (default %ProgramData%\\MM.Membership.Agent on Windows)",
        ),
    )
    .arg(Arg::flag("force").help("Enroll again although config.dpapi already opens"))
    .arg(Arg::flag("switch").help(
        "Take over from the .NET MM.Membership.Agent: keep its config.dpapi, stop and disable its service",
    ))
    .arg(Arg::flag("rollback").help(
        "Undo --switch: restore the backed-up config.dpapi if one was replaced, restart the .NET service",
    ))
}

fn run(ctx: &ToolContext<'_>, m: &Matches) -> Result<(), Error> {
    let fallback = ctx.data_dir.join("modules").join(MODULE);
    let args = EnrollArgs {
        hub_url: m.value("hub-url").unwrap_or_default().to_owned(),
        config_dir: m
            .value("config-dir")
            .map_or_else(|| default_dir(&fallback), PathBuf::from),
        force: m.flag("force"),
    };
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    let env = |k: &str| std::env::var(k).ok();
    if m.flag("switch") || m.flag("rollback") {
        if m.flag("switch") && m.flag("rollback") {
            return Err(Error::Config(
                "--switch and --rollback exclude each other".into(),
            ));
        }
        let service = Scm;
        let sw = Switch {
            hub_url: args.hub_url,
            config_dir: args.config_dir,
            record_dir: fallback,
            force: args.force,
            sealer: Arc::new(Dpapi),
            service: &service,
            places: Places::detect(service.exe_dir(), &env),
            probe: &hub_probe,
        };
        return if m.flag("rollback") {
            run_rollback(&sw, &mut out)
        } else {
            run_switch(&sw, Some(&mut input), &mut out, &env)
        }
        .map_err(Error::Config);
    }
    if args.hub_url.trim().is_empty() {
        return Err(Error::Config("--hub-url is required".into()));
    }
    run_enroll(&args, Arc::new(Dpapi), Some(&mut input), &mut out, &|k| {
        std::env::var(k).ok()
    })
    .map_err(Error::Config)
}
