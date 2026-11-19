//! `dl-agent pms-migrate`: retired. MM_PMS lives in the cloud since the SMILE-only contract
//! (SMILE-PMS ADR 2026-11-19): no migration bundle is ever applied on a site and no object
//! is created on a customer database. The sub-command stays so an old script gets a clear
//! answer instead of "unknown command"; it accepts the old options and changes nothing.

use dl_agent::Error;
use dl_agent::tool::{Tool, ToolContext};
use dl_cli::{Arg, Command, Matches};

/// The registered sub-command name.
pub const NAME: &str = "pms-migrate";

/// What the sub-command answers.
pub const RETIRED: &str = "pms-migrate is retired: MM_PMS migrations run in the SMILE PMS cloud since the SMILE-only contract; the site agent creates nothing on SMILE_FO, SMILE_POS or SMILE_BO";

/// The tool to register with `App::tool`.
pub fn pms_migrate_tool() -> Tool {
    Tool {
        name: NAME,
        grammar,
        run,
    }
}

fn grammar(cmd: Command) -> Command {
    cmd.about("Retired: MM_PMS migrations run in the cloud (prints why and exits non-zero)")
        .arg(Arg::option("bundle").value_name("DIR").help("Ignored"))
        .arg(Arg::flag("dry-run").help("Ignored"))
        .arg(Arg::flag("check-drift").help("Ignored"))
        .arg(Arg::option("rerun-partial").value_name("N").help("Ignored"))
        .arg(Arg::option("config").value_name("PATH").help("Ignored"))
}

fn run(_ctx: &ToolContext<'_>, _m: &Matches) -> Result<(), Error> {
    Err(Error::Config(RETIRED.into()))
}
