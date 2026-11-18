//! `dl-agent pms-migrate`: applies (or plans) a signed migration bundle on MM_PMS, the port
//! of `pms-agent migrate`. It prints the JSON result the port prints (`bundle_id`,
//! `dry_run`, `plan`, `report`, `drift`, `error{code,message}`) and fails with the error's
//! code and message.

use std::io::Write;
use std::path::{Path, PathBuf};

use dl_agent::Error;
use dl_agent::tool::{Tool, ToolContext};
use dl_cli::{Arg, Command, Matches};

use crate::agent;
use crate::bundle;
use crate::config;
use crate::ctx::Ctx;
use crate::db;
use crate::json::{self, Obj};
use crate::log::{self, Logger};
use crate::migrate::{self, DriftFinding};
use crate::module::hello_version;

#[cfg(test)]
mod tests;

/// The registered sub-command name.
pub const NAME: &str = "pms-migrate";

/// The tool to register with `App::tool`.
pub fn pms_migrate_tool() -> Tool {
    Tool {
        name: NAME,
        grammar,
        run,
    }
}

fn grammar(cmd: Command) -> Command {
    cmd.about("Applies a signed SMILE PMS migration bundle to MM_PMS (pms-agent migrate)")
        .arg(
            Arg::option("bundle")
                .required()
                .value_name("DIR")
                .help("The unpacked, signed bundle directory"),
        )
        .arg(Arg::flag("dry-run").help("Plan only; change nothing"))
        .arg(
            Arg::flag("check-drift")
                .help("Compare the ledger with the database objects first (implies --dry-run)"),
        )
        .arg(
            Arg::option("rerun-partial")
                .value_name("N")
                .help("Run the partially applied version N again"),
        )
        .arg(
            Arg::option("config").value_name("PATH").help(
                "The PMS agent config (default: PMS_AGENT_CONFIG, then the SmilePmsAgent one)",
            ),
        )
}

/// What the command line asked for.
#[derive(Debug, Clone, Default)]
pub(crate) struct Args {
    pub bundle: PathBuf,
    pub dry_run: bool,
    pub check_drift: bool,
    pub rerun: i64,
    pub config: Option<PathBuf>,
}

impl Args {
    fn from_matches(m: &Matches) -> Result<Args, String> {
        let rerun = m
            .get::<i64>("rerun-partial")
            .map_err(|e| format!("invalid value for --rerun-partial: {e}"))?
            .unwrap_or(0);
        Ok(Args {
            bundle: PathBuf::from(m.value("bundle").unwrap_or_default()),
            dry_run: m.flag("dry-run"),
            check_drift: m.flag("check-drift"),
            rerun,
            config: m.value("config").map(PathBuf::from),
        })
    }
}

fn run(_ctx: &ToolContext<'_>, m: &Matches) -> Result<(), Error> {
    let args = Args::from_matches(m).map_err(Error::Config)?;
    let mut out = std::io::stdout();
    run_args(&args, &mut out, &|k| std::env::var(k).unwrap_or_default()).map_err(Error::Config)
}

/// The tool body with its environment injected: prints the result and returns the error.
pub(crate) fn run_args(
    args: &Args,
    out: &mut dyn Write,
    getenv: &dyn Fn(&str) -> String,
) -> Result<(), String> {
    let mut res = MigrateResult {
        dry_run: args.dry_run,
        ..MigrateResult::default()
    };
    let r = if args.bundle.as_os_str().is_empty() {
        Err(config_err("--bundle is required"))
    } else {
        run_migrate(&mut res, args, getenv)
    };
    if let Err(e) = &r {
        res.error = Some((e.code.clone(), e.msg.clone()));
    }
    let v = json::parse(res.to_json().as_bytes())
        .map_err(|e| format!("{NAME}: encode the result: {e}"))?;
    writeln!(out, "{}", json::to_pretty(&v, "  "))
        .map_err(|e| format!("{NAME}: write the result: {e}"))?;
    r.map_err(|e| format!("{NAME}: {}: {}", e.code, e.msg))
}

/// The JSON result, field for field as the port prints it.
#[derive(Default)]
struct MigrateResult {
    bundle_id: String,
    dry_run: bool,
    plan: Option<migrate::Plan>,
    report: Option<migrate::Report>,
    drift: Vec<DriftFinding>,
    error: Option<(String, String)>,
}

impl MigrateResult {
    fn to_json(&self) -> String {
        let mut o = Obj::new();
        if !self.bundle_id.is_empty() {
            o = o.str("bundle_id", &self.bundle_id);
        }
        o = o.bool("dry_run", self.dry_run);
        if let Some(p) = &self.plan {
            o = o.raw("plan", &p.to_json());
        }
        if let Some(r) = &self.report {
            o = o.raw("report", &r.to_json());
        }
        if !self.drift.is_empty() {
            let items: Vec<String> = self
                .drift
                .iter()
                .map(|d| {
                    Obj::new()
                        .str("kind", &d.kind)
                        .str("object", &d.object)
                        .str("code", &d.code)
                        .finish()
                })
                .collect();
            o = o.raw("drift", &format!("[{}]", items.join(",")));
        }
        if let Some((code, message)) = &self.error {
            o = o.raw(
                "error",
                &Obj::new()
                    .str("code", code)
                    .str("message", message)
                    .finish(),
            );
        }
        o.finish()
    }
}

/// A failure of the command: its code and message.
struct CmdErr {
    code: String,
    msg: String,
}

fn config_err(msg: impl Into<String>) -> CmdErr {
    CmdErr {
        code: "CONFIG".into(),
        msg: msg.into(),
    }
}

fn from_merr(e: &migrate::MErr) -> CmdErr {
    CmdErr {
        code: e.code.into(),
        msg: e.to_string(),
    }
}

fn run_migrate(
    res: &mut MigrateResult,
    args: &Args,
    getenv: &dyn Fn(&str) -> String,
) -> Result<(), CmdErr> {
    let cfg = config::load(args.config.as_deref(), getenv).map_err(config_err)?;
    let keys = agent::pinned_keyring(&cfg).map_err(config_err)?;
    let b = bundle::open(Path::new(&args.bundle), &keys).map_err(|e| CmdErr {
        code: format!("BUNDLE_{}", e.code),
        msg: e.to_string(),
    })?;
    res.bundle_id = b.manifest.bundle_id.clone();
    let pools = db::open_pools(&cfg.state_dir.join(db::DB_CONFIG_FILE))
        .map_err(|e| config_err(e.to_string()))?;
    let Some(pms) = pools.get(db::DB_PMS).cloned() else {
        db::close_all(&pools);
        return Err(config_err("database connection file: MM_PMS is required"));
    };
    let logger = Logger::new(std::io::stderr(), log::parse_level(&cfg.log_level));
    let out = (|| {
        let version = hello_version();
        let r = agent::migration_runner(&cfg, pms, &version, args.rerun, Box::new(|_| {}), &logger)
            .map_err(config_err)?;
        let ctx = Ctx::background();
        if args.check_drift {
            res.drift = r.check_drift(&ctx, &b).map_err(|e| from_merr(&e))?;
        }
        if res.dry_run || args.check_drift {
            res.dry_run = true;
            let (plan, out) = r.dry_run(&ctx, &b);
            res.plan = Some(plan);
            return out.map_err(|e| from_merr(&e));
        }
        let (rep, r) = r.apply(&ctx, &b);
        res.report = Some(rep);
        r.map_err(|e| from_merr(&e))
    })();
    db::close_all(&pools);
    out
}
