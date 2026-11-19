//! The module's hub commands (`einvoice.<kind>`):
//!
//! | kind | does | writes |
//! |---|---|---|
//! | `status` | counters, health, how SQL is reached, the effective collect settings | nothing |
//! | `collect_now` | queues one collect cycle | what a cycle writes |
//! | `selftest` | the module's self-test checks, as JSON | nothing |
//! | `collect_dry_run` | fetches and builds the bills of a date window ([`crate::dryrun`]) | nothing |
//! | `writeback_probe` | the write-back target and the login's rights on it | nothing |
//!
//! The last three block on SQL Server or the platform, so each runs on its own thread, one
//! at a time, and answers when it ends; the module thread keeps answering everything else.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dl_agent::command::Command;
use dl_agent::selftest::Status;
use dl_agent::supervisor::ModuleContext;
use dl_json::{Map, Value};

use super::collector::{Effective, Shared, report_step_down};
use super::selftest;
use super::settings::Settings;
use crate::dryrun::{self, DryRun};
use crate::runner::sources_for;
use crate::sql::{Sql, WritebackProbe};

/// One blocking check command at a time per process.
static CHECK_BUSY: AtomicBool = AtomicBool::new(false);
/// How long a dry run may spend on SQL.
const DRY_RUN_BUDGET: Duration = Duration::from_secs(120);
/// Per statement of the write-back probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// What the module thread knows when a command arrives.
pub(super) struct Env<'a> {
    /// The module's settings (how SQL is reached).
    pub settings: &'a Settings,
    /// The running collector's state; `None` while not enrolled.
    pub shared: Option<&'a Arc<Shared>>,
}

/// Answers one `einvoice.*` command.
pub(super) fn answer(ctx: &ModuleContext, cmd: &Command, env: &Env<'_>, detail: &str) {
    let kind = cmd.kind.strip_prefix("einvoice.").unwrap_or(&cmd.kind);
    match kind {
        "status" => {
            let out = status(env.shared.map(|s| &**s), detail);
            ctx.complete(&cmd.id, true, &dl_json::to_string(&out));
        }
        "collect_now" => match env.shared {
            Some(s) => {
                s.collect_now.store(true, Ordering::Relaxed);
                ctx.complete(&cmd.id, true, "collect cycle queued");
            }
            None => ctx.complete(&cmd.id, false, detail),
        },
        "selftest" => spawn_check(ctx, cmd, run_selftest),
        "collect_dry_run" | "writeback_probe" => {
            let Some(eff) = env.shared.and_then(|s| s.effective()) else {
                ctx.complete(&cmd.id, false, &format!("module not collecting: {detail}"));
                return;
            };
            let (settings, args) = (env.settings.clone(), cmd.args.clone());
            if kind == "collect_dry_run" {
                spawn_check(ctx, cmd, move |ctx| dry_run(ctx, &settings, &eff, &args));
            } else {
                spawn_check(ctx, cmd, move |ctx| {
                    writeback_probe(ctx, &settings, &eff, &args)
                });
            }
        }
        _ => ctx.complete(
            &cmd.id,
            false,
            &format!("unknown einvoice command {:?}", cmd.kind),
        ),
    }
}

/// `einvoice.status`.
fn status(shared: Option<&Shared>, detail: &str) -> Value {
    let mut m = shared.map(|s| s.stats.to_map()).unwrap_or_default();
    m.insert("enrolled", shared.is_some());
    m.insert("detail", detail);
    if let Some(s) = shared {
        m.insert("sql", s.sql());
        if let Some(e) = s.effective() {
            m.insert("collect", collect_settings(&e));
        }
    }
    Value::Object(m)
}

/// The collect settings in force: sealed config with the hub's refresh applied.
fn collect_settings(e: &Effective) -> Value {
    let mut m = Map::new();
    let c = &e.cfg;
    m.insert(
        "collect_from",
        c.collect_from
            .map_or(Value::Null, |t| Value::from(t.to_rfc3339())),
    );
    m.insert(
        "collect_sources",
        if c.collect_sources.is_empty() {
            "FO,POS"
        } else {
            &c.collect_sources
        },
    );
    m.insert("recalc_tax_into_sc", c.recalc_tax_into_sc);
    m.insert(
        "pos_item_name",
        format!("{:?}", e.pos_item_name).to_lowercase(),
    );
    m.insert("batch_size", c.batch_size);
    Value::Object(m)
}

/// Clears [`CHECK_BUSY`] when the check thread ends, panics included.
struct Busy;

impl Drop for Busy {
    fn drop(&mut self) {
        CHECK_BUSY.store(false, Ordering::SeqCst);
    }
}

/// Runs `f` on a thread of its own and answers `cmd` with its result.
fn spawn_check(
    ctx: &ModuleContext,
    cmd: &Command,
    f: impl FnOnce(&ModuleContext) -> Result<String, String> + Send + 'static,
) {
    if CHECK_BUSY.swap(true, Ordering::SeqCst) {
        ctx.complete(
            &cmd.id,
            false,
            "another einvoice check is still running; send this one again when it answers",
        );
        return;
    }
    let (tctx, id) = (ctx.clone(), cmd.id.clone());
    let spawned = std::thread::Builder::new()
        .name("einvoice-check".into())
        .spawn(move || {
            let _busy = Busy;
            match f(&tctx) {
                Ok(out) => tctx.complete(&id, true, &out),
                Err(e) => tctx.complete(&id, false, &e),
            }
        });
    if let Err(e) = spawned {
        CHECK_BUSY.store(false, Ordering::SeqCst);
        ctx.complete(&cmd.id, false, &format!("start einvoice check: {e}"));
    }
}

/// `einvoice.selftest`: fails when a check fails; the output lists every check either way.
fn run_selftest(ctx: &ModuleContext) -> Result<String, String> {
    let checks = selftest::run(ctx);
    let count = |s: Status| checks.iter().filter(|c| c.status == s).count() as u64;
    let mut summary = Map::new();
    summary.insert("pass", count(Status::Pass));
    summary.insert("warn", count(Status::Warn));
    summary.insert("fail", count(Status::Fail));
    summary.insert("skip", count(Status::Skip));
    let failed = count(Status::Fail) > 0;
    let mut m = Map::new();
    m.insert("summary", Value::Object(summary));
    m.insert(
        "checks",
        Value::Array(checks.iter().map(|c| c.to_json()).collect()),
    );
    let out = dl_json::to_string(&Value::Object(m));
    if failed { Err(out) } else { Ok(out) }
}

/// Opens the SQL connections the collector would use, on this thread.
fn open(ctx: &ModuleContext, settings: &Settings, eff: &Effective) -> Result<Sql, String> {
    let mut sql = settings.open_sql(ctx, &eff.cfg)?;
    sql.set_pos_item_name(eff.pos_item_name);
    Ok(sql)
}

/// Reports what the command's own connections had to step down.
fn report_steps(ctx: &ModuleContext, sql: &Sql) {
    for step in sql.take_step_downs() {
        report_step_down(ctx, &step);
    }
}

/// `einvoice.collect_dry_run`: fails only when every source failed.
fn dry_run(
    ctx: &ModuleContext,
    settings: &Settings,
    eff: &Effective,
    args: &Value,
) -> Result<String, String> {
    let today = crate::enroll::vn_midnight(None)?.date();
    let d = DryRun::parse(args, &sources_for(&eff.cfg.collect_sources), today)?;
    let sql = open(ctx, settings, eff)?;
    let out = dryrun::run(&sql, &eff.cfg.rules(), &d, Instant::now() + DRY_RUN_BUDGET);
    report_steps(ctx, &sql);
    let all_failed = out
        .get("sources")
        .and_then(Value::as_object)
        .is_some_and(|m| m.iter().all(|(_, v)| v.get("error").is_some()));
    let text = dl_json::to_string(&out);
    if all_failed { Err(text) } else { Ok(text) }
}

/// `einvoice.writeback_probe` (`{"source": "FO"|"POS", "bill": "<id>"}`, both optional):
/// fails when a write-back would not land in some source.
fn writeback_probe(
    ctx: &ModuleContext,
    settings: &Settings,
    eff: &Effective,
    args: &Value,
) -> Result<String, String> {
    let sources = match args.get("source").and_then(Value::as_str).map(str::trim) {
        None | Some("") => sources_for(&eff.cfg.collect_sources),
        Some("FO") => vec!["FO"],
        Some("POS") => vec!["POS"],
        Some(other) => return Err(format!("source {other:?}: expected FO or POS")),
    };
    let bill = args
        .get("bill")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if bill.is_some() && sources.len() != 1 {
        return Err("bill needs a source (FO or POS)".into());
    }
    let sql = open(ctx, settings, eff)?;
    let mut out = Map::new();
    let mut blocked = false;
    for source in sources {
        let v = match sql.writeback_probe(source, bill, PROBE_TIMEOUT) {
            Ok(p) => {
                blocked |= p.blocker().is_some();
                probe_json(&p)
            }
            Err(e) => {
                blocked = true;
                let mut m = Map::new();
                m.insert("error", e);
                Value::Object(m)
            }
        };
        out.insert(source, v);
    }
    report_steps(ctx, &sql);
    out.insert("written", false);
    let text = dl_json::to_string(&Value::Object(out));
    if blocked { Err(text) } else { Ok(text) }
}

fn probe_json(p: &WritebackProbe) -> Value {
    let mut m = Map::new();
    m.insert("database", p.database.as_str());
    m.insert("login", p.login.as_str());
    m.insert("user", p.user.as_str());
    m.insert("procedure", p.procedure.as_str());
    m.insert("proc_exists", p.proc_exists);
    m.insert("table_exists", p.table_exists);
    m.insert("can_execute", p.can_execute);
    m.insert("can_update", p.can_update);
    m.insert("same_owner", p.same_owner);
    m.insert("can_install", p.can_install);
    m.insert("read_write", p.read_write);
    m.insert("may_write", p.may_write());
    m.insert("blocker", p.blocker().map_or(Value::Null, Value::from));
    m.insert("marked", p.marked.map_or(Value::Null, Value::from));
    m.insert("invoiced", p.invoiced.map_or(Value::Null, Value::from));
    match &p.bill {
        None => {}
        Some(None) => {
            m.insert(
                "bill",
                "no MMEI_Collected row: the bill was never collected, a write-back changes nothing",
            );
        }
        Some(Some(b)) => {
            let mut row = Map::new();
            row.insert("ok", b.ok);
            row.insert(
                "invoice_number",
                b.invoice_number.clone().map_or(Value::Null, Value::from),
            );
            row.insert(
                "ky_hieu",
                b.ky_hieu.clone().map_or(Value::Null, Value::from),
            );
            m.insert("bill", Value::Object(row));
        }
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_config::AgentConfig;
    use crate::profile::PosItemName;

    #[test]
    fn status_names_the_effective_collect_settings() {
        let shared = Shared::default();
        shared.set_effective(Effective {
            cfg: AgentConfig {
                collect_sources: "POS".into(),
                ..AgentConfig::default()
            },
            pos_item_name: PosItemName::IName1,
        });
        let v = status(Some(&shared), "collecting");
        let c = v.get("collect").unwrap();
        assert_eq!(c.get("collect_sources").unwrap(), &Value::from("POS"));
        assert_eq!(c.get("pos_item_name").unwrap(), &Value::from("iname1"));
        assert_eq!(v.get("enrolled").unwrap(), &Value::from(true));
    }

    #[test]
    fn a_probe_without_rows_says_so() {
        let p = WritebackProbe {
            procedure: "sp_ApplyInvoiceWriteback".into(),
            bill: Some(None),
            ..WritebackProbe::default()
        };
        let v = probe_json(&p);
        assert!(
            v.get("bill")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("never collected")
        );
        assert_eq!(v.get("may_write").unwrap(), &Value::from(false));
        assert!(v.get("blocker").unwrap().as_str().is_some());
    }
}
