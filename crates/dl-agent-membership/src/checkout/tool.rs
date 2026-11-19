//! The `membership-backfill-walkin` sub-command: the .NET agent's `--backfill-from` mode
//! (`WalkInBackfillPlan` + `WalkInBackfillRunner` + `WalkInBackfillExit`).
//!
//! ```text
//! dl-agent membership-backfill-walkin --from 2026-01-01 [--to 2026-09-19]
//!     [--page-size 200] [--state <file>] [--config-dir <dir>]
//!     [--phone-column MobileNum] [--extra-bed-code EB] [--member-discount-code MD]
//!     [--earn-charge-codes 110,111]
//! ```
//!
//! It reads `config.dpapi` (hub, token, SMILE), only reads SMILE, reports every
//! checked-out walk-in group of the range to `earn/walk-in` and exits non-zero unless the
//! whole range was read without a failed group. Re-running the same command resumes from
//! the saved cursor (`backfill-walkin.json` next to `config.dpapi`). The module settings
//! live in the remote config, which a sub-command does not read: pass the phone column and
//! earn codes the hotel uses when they are not the defaults.

use std::path::PathBuf;
use std::sync::Arc;

use dl_agent::Error;
use dl_agent::tool::{Tool, ToolContext};
use dl_cli::{Arg, Command, Matches};

use super::backfill::{self, Plan, STATE_FILE};
use crate::config::{ConfigStore, Dpapi, default_dir};
use crate::earn::EarnEnv;
use crate::earn::pairs::{EarnSettings, PairSource};
use crate::hub::HubClient;
use crate::hub::gate::TokenGate;
use crate::identity::{Refresh, Tracker};
use crate::log::{self, V};
use crate::module::{NAME as MODULE, TOKEN_RETRY, agent_version};
use crate::smile::{TdsSmile, hotel_now};

/// The registered sub-command name.
pub const NAME: &str = "membership-backfill-walkin";

/// The tool to register with `App::tool`.
pub fn membership_backfill_tool() -> Tool {
    Tool {
        name: NAME,
        grammar,
        run,
    }
}

fn grammar(cmd: Command) -> Command {
    cmd.about(
        "Reports the walk-in stays checked out in a date range to the Membership hub \
         (earn after the fact; SMILE is only read; re-run to resume)",
    )
    .arg(
        Arg::option("from")
            .required()
            .help("First checkout day, yyyy-MM-dd"),
    )
    .arg(Arg::option("to").help("Last checkout day, yyyy-MM-dd (default: today)"))
    .arg(Arg::option("page-size").help("Groups per page (1-500, default 200)"))
    .arg(
        Arg::option("state")
            .help("Cursor file (default: backfill-walkin.json next to config.dpapi)"),
    )
    .arg(Arg::option("config-dir").help("Where config.dpapi lives"))
    .arg(Arg::option("phone-column").help("AdditionalName phone column (default MobileNum)"))
    .arg(Arg::option("extra-bed-code").help("SMILE extra-bed special code (default EB)"))
    .arg(Arg::option("member-discount-code").help("Earn:MemberDiscountCode (default none)"))
    .arg(Arg::option("earn-charge-codes").help("Earn:ChargeCodes override, comma separated"))
}

struct Env(Option<String>);

impl EarnEnv for Env {
    fn fingerprint(&self) -> Option<String> {
        self.0.clone()
    }
    fn hub_extra_bed_code(&self) -> Option<String> {
        None
    }
}

fn run(ctx: &ToolContext<'_>, m: &Matches) -> Result<(), Error> {
    let fallback = ctx.data_dir.join("modules").join(MODULE);
    let dir = m
        .value("config-dir")
        .map_or_else(|| default_dir(&fallback), PathBuf::from);
    let cfg = ConfigStore::new(&dir, Arc::new(Dpapi))
        .load()
        .map_err(|e| Error::Config(format!("config.dpapi: {e}")))?
        .ok_or(Error::NotEnrolled)?;
    let state = m
        .value("state")
        .map_or_else(|| dir.join(STATE_FILE), PathBuf::from);
    let page_size = match m.value("page-size") {
        None => None,
        Some(v) => Some(
            v.trim()
                .parse::<i32>()
                .map_err(|_| Error::Config(format!("--page-size '{v}' is not a number")))?,
        ),
    };
    let plan = Plan::try_create(
        m.value("from"),
        m.value("to"),
        page_size,
        &state,
        hotel_now().date(),
    )
    .map_err(Error::Config)?;
    let smile = Arc::new(
        TdsSmile::new(&cfg.smile_connection_string)
            .map_err(|e| Error::Config(format!("SMILE connection string: {e}")))?,
    );
    let hub = HubClient::new(
        &cfg.hub_base_url,
        &cfg.agent_token,
        cfg.hotel_id,
        agent_version(),
        Arc::new(TokenGate::new(TOKEN_RETRY)),
    )
    .map_err(|e| Error::Config(format!("hub address: {e}")))?;
    let tracker = Tracker::new(smile.clone());
    if let Refresh::Failed(e) = tracker.refresh_if_due(true) {
        log::warn(
            "SMILE identity not read; reports go without a fingerprint",
            &[("error", V::S(&e))],
        );
    }
    let codes: Vec<String> = m
        .value("earn-charge-codes")
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let pairs = PairSource::new(EarnSettings {
        charge_codes: codes,
        member_discount_code: m
            .value("member-discount-code")
            .unwrap_or("")
            .trim()
            .to_owned(),
        extra_bed_special_code: m.value("extra-bed-code").unwrap_or("EB").trim().to_owned(),
    });
    let phone = m
        .value("phone-column")
        .unwrap_or("MobileNum")
        .trim()
        .to_owned();
    let (s, result) = backfill::run(
        &plan,
        smile.as_ref(),
        &hub,
        &Env(tracker.current_fingerprint()),
        &pairs,
        &phone,
    );
    if let Err(e) = &result {
        log::error(
            "backfill stopped; re-run the same command to resume from the saved cursor",
            &[("error", V::S(&e.to_string()))],
        );
    }
    println!(
        "backfill {}..{}: {} pages, {} groups reported, {} awarded, {} failed, range {}",
        crate::stj::format_date(plan.from),
        crate::stj::format_date(plan.to),
        s.pages,
        s.processed,
        s.awarded,
        s.failed,
        if s.completed { "done" } else { "not done" }
    );
    if s.is_clean() {
        Ok(())
    } else {
        Err(Error::Module(
            "backfill not finished: re-run the same command to continue".into(),
        ))
    }
}
