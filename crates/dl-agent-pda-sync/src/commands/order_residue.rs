//! `pos.order_residue` (RT-1365, ADR 0183) and the `order-residue` command line (RT-1345,
//! ADR 0182): port of the .NET `OrderResidue`, `IOrderResidueSource` and
//! `PosOrderResidueCommandHandler`. A read-only reconcile of the order rows the agent's
//! commands leave in SMILE_POS (empty PDA checks, `tmpTrnInfo` rows without a `tmpTrn` line,
//! stale `checkopenlog` rows, `PDASendOrder` rows of a gone check) and, from the command line
//! with `--apply`, the audited cleanup of the part POS WIN never clears. A row is "ours" only
//! when its check number is named by an applied command in `Pda_SyncCommandLog`; a check POS
//! WIN opened by itself is never listed for cleanup. The SQL is in `sql_order_residue`.

use std::collections::HashSet;
use std::sync::Arc;

use super::error::Result;
use super::executor::{SiteCommandExecutor, reasons};
use super::sql_store::SqlSiteCommandStore;
use crate::contracts::WireJson;
use crate::contracts::time::DateTime;
use crate::contracts::wire::{
    CommandStatus, PosOrderResiduePayload, PosOrderResidueResult, PosResidueCheck, PosResidueInfo,
    SyncCommand, SyncCommandResult,
};
use crate::reporting::Value;
use crate::util::ctx::Ctx;

/// `OrderResidue.DefaultMinAgeMinutes`.
pub const DEFAULT_MIN_AGE_MINUTES: i32 = 120;

/// An open check header with no line: `opened_local` is the store's own clock
/// (`ResidueCheck`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidueCheck {
    /// RVC.
    pub rvc_no: i32,
    /// Check number.
    pub check_no: i64,
    /// `OpenTime` (`DateTime.MinValue` when NULL).
    pub opened_local: DateTime,
}

/// A `tmpTrnInfo` row whose `tmpTrn` line is gone (`ResidueInfoRow`). `sent` rows carry
/// the kitchen void chit and are never cleaned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidueInfoRow {
    /// Check number.
    pub check_no: i64,
    /// `TrnNo` (the `tmpTrn.TrnSeq` of the gone line).
    pub trn_no: i64,
    /// Sent to the kitchen (`POrderTime` set or `PToOrder` = 1).
    pub sent: bool,
}

/// What the store holds beyond a live check (`OrderResidueSnapshot`), before or after it is
/// narrowed to the rows this agent's commands created.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrderResidueSnapshot {
    /// Empty open checks opened by the PDA workstation.
    pub empty_open_checks: Vec<ResidueCheck>,
    /// `tmpTrnInfo` rows without a `tmpTrn` line.
    pub orphan_info_rows: Vec<ResidueInfoRow>,
    /// `checkopenlog` rows still open for a check that no longer exists.
    pub stale_open_logs: Vec<ResidueCheck>,
    /// Check numbers of `PDASendOrder` rows whose check no longer exists.
    pub orphan_send_audits: Vec<i64>,
}

impl OrderResidueSnapshot {
    fn total(&self) -> usize {
        self.empty_open_checks.len()
            + self.orphan_info_rows.len()
            + self.stale_open_logs.len()
            + self.orphan_send_audits.len()
    }
}

/// The rows a cleanup removed or closed, per kind (`ResidueCleaned`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResidueCleaned {
    /// Empty checks cancelled.
    pub empty_checks: i32,
    /// Info rows deleted.
    pub info_rows: i32,
    /// Open-log rows closed.
    pub open_logs: i32,
}

/// `OrderResidue.Report`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidueReport {
    /// The rows of this agent's checks.
    pub ours: OrderResidueSnapshot,
    /// Rows of checks POS WIN opened by itself.
    pub other_rows_ignored: i32,
    /// Orphan info rows of ours already sent (kept for the void chit).
    pub sent_orphan_info_rows: i32,
    /// What `--apply` cleaned.
    pub cleaned: Option<ResidueCleaned>,
}

/// The SMILE_POS side of the report and cleanup (`IOrderResidueSource`).
pub trait OrderResidueSource: Send + Sync {
    /// Check numbers that an applied command of this agent's command log names: the only
    /// checks a cleanup may touch.
    fn our_check_nos(&self, ctx: &Ctx) -> Result<HashSet<i64>>;

    /// Read-only. Empty open checks opened at least `min_age_minutes` ago, by the PDA
    /// workstation.
    fn read(&self, min_age_minutes: i32, ctx: &Ctx) -> Result<OrderResidueSnapshot>;

    /// One transaction; every statement re-checks its row's condition, so a row POS WIN
    /// touched meanwhile is skipped.
    fn clean(
        &self,
        ours: &OrderResidueSnapshot,
        cleaned_by: &str,
        min_age_minutes: i32,
        ctx: &Ctx,
    ) -> Result<ResidueCleaned>;
}

/// `OrderResidue.RunAsync`: reads, narrows to ours and, with `apply`, writes every action
/// to `audit` before and after the cleanup. An audit failure stops the run (nothing is
/// cleaned unaudited).
pub fn run(
    source: &dyn OrderResidueSource,
    apply: bool,
    cleaned_by: &str,
    min_age_minutes: i32,
    audit: &mut dyn FnMut(&str) -> Result<()>,
    ctx: &Ctx,
) -> Result<ResidueReport> {
    let ours = source.our_check_nos(ctx)?;
    let all = source.read(min_age_minutes, ctx)?;
    let mine = narrow(&all, &ours);
    let ignored = count(all.total() - mine.total());
    let sent = count(mine.orphan_info_rows.iter().filter(|r| r.sent).count());
    // Sent orphans stay: the kitchen's void chit joins them (mdlPrintOrder.cs:378). They are
    // reported, never removed.
    let cleanable = OrderResidueSnapshot {
        orphan_info_rows: mine
            .orphan_info_rows
            .iter()
            .filter(|r| !r.sent)
            .copied()
            .collect(),
        ..mine.clone()
    };

    let mut cleaned = None;
    if apply {
        audit(&format!(
            "order-residue apply by={cleaned_by} emptyChecks={} infoRows={} openLogs={}",
            cleanable.empty_open_checks.len(),
            cleanable.orphan_info_rows.len(),
            cleanable.stale_open_logs.len()
        ))?;
        for c in &cleanable.empty_open_checks {
            audit(&format!(
                "cancel-empty rvc={} check={}",
                c.rvc_no, c.check_no
            ))?;
        }
        for r in &cleanable.orphan_info_rows {
            audit(&format!("drop-info check={} trn={}", r.check_no, r.trn_no))?;
        }
        for c in &cleanable.stale_open_logs {
            audit(&format!(
                "close-openlog rvc={} check={}",
                c.rvc_no, c.check_no
            ))?;
        }
        let done = source.clean(&cleanable, cleaned_by, min_age_minutes, ctx)?;
        audit(&format!(
            "order-residue done emptyChecks={} infoRows={} openLogs={}",
            done.empty_checks, done.info_rows, done.open_logs
        ))?;
        cleaned = Some(done);
    }
    Ok(ResidueReport {
        ours: mine,
        other_rows_ignored: ignored,
        sent_orphan_info_rows: sent,
        cleaned,
    })
}

/// `OrderResidue.Narrow`: keeps the rows whose check number is in `ours`.
pub fn narrow(all: &OrderResidueSnapshot, ours: &HashSet<i64>) -> OrderResidueSnapshot {
    OrderResidueSnapshot {
        empty_open_checks: keep(&all.empty_open_checks, |c| ours.contains(&c.check_no)),
        orphan_info_rows: keep(&all.orphan_info_rows, |r| ours.contains(&r.check_no)),
        stale_open_logs: keep(&all.stale_open_logs, |c| ours.contains(&c.check_no)),
        orphan_send_audits: keep(&all.orphan_send_audits, |n| ours.contains(n)),
    }
}

fn keep<T: Copy>(rows: &[T], f: impl Fn(&T) -> bool) -> Vec<T> {
    rows.iter().filter(|r| f(r)).copied().collect()
}

fn count(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// `OrderResidue.Describe`: the operator's report, one line per kind, the first 50 empty
/// checks, and what `--apply` cleaned.
pub fn describe(report: &ResidueReport) -> String {
    let o = &report.ours;
    let mut lines = vec![
        "Order residue (rows of checks this agent's commands touched; read-only unless --apply):"
            .to_owned(),
        format!(
            "  empty open checks (no line, opened by the PDA workstation): {}",
            o.empty_open_checks.len()
        ),
        format!(
            "  tmpTrnInfo rows without a tmpTrn line, never sent (cleanable): {}",
            o.orphan_info_rows.iter().filter(|r| !r.sent).count()
        ),
        format!(
            "  tmpTrnInfo rows without a tmpTrn line, already sent (kept for the void chit): {}",
            report.sent_orphan_info_rows
        ),
        format!(
            "  checkopenlog rows still open for a check that no longer exists: {}",
            o.stale_open_logs.len()
        ),
        format!(
            "  PDASendOrder rows for a check that no longer exists (report only): {}",
            o.orphan_send_audits.len()
        ),
        format!(
            "  rows of checks POS WIN opened by itself, ignored: {}",
            report.other_rows_ignored
        ),
    ];
    for c in o.empty_open_checks.iter().take(50) {
        lines.push(format!(
            "    empty check rvc={} no={} opened={}",
            c.rvc_no,
            c.check_no,
            sortable(c.opened_local)
        ));
    }
    if let Some(done) = report.cleaned {
        lines.push(format!(
            "Cleaned: {} empty checks, {} info rows, {} open-log rows.",
            done.empty_checks, done.info_rows, done.open_logs
        ));
    }
    lines.join(NEW_LINE)
}

/// `Environment.NewLine` of the store machine.
#[cfg(windows)]
const NEW_LINE: &str = "\r\n";
#[cfg(not(windows))]
const NEW_LINE: &str = "\n";

/// `DateTime.ToString("s")`: `yyyy-MM-ddTHH:mm:ss`.
pub fn sortable(t: DateTime) -> String {
    let (y, mo, d, h, mi, s, _) = t.parts();
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}")
}

/// `OrderResidue.CheckNoOf`: the check number an applied log row's result names
/// (`result.checkNo`, any case, an integer); anything else in the log names no check.
pub fn check_no_of(result_json: &str) -> Option<i64> {
    // A log row that is not a command result names no check.
    let parsed = SyncCommandResult::from_json(result_json).ok()?;
    if parsed.status != CommandStatus::Applied {
        return None;
    }
    let result = parsed.result?;
    let object = result.as_object()?;
    object
        .iter()
        .filter(|(name, _)| crate::util::stj::eq_ignore_case(name, "checkNo"))
        .find_map(|(_, v)| v.as_i64())
}

impl SiteCommandExecutor {
    /// Runs `pos.order_residue` (`PosOrderResidueCommandHandler`): the read half of
    /// `order-residue`, never the cleanup; no write transaction and no `Pda_SyncCommandLog`
    /// row. The log carries counts only, never a check number. An error (cancellation, SQL)
    /// propagates: the command wrote nothing, so a redelivery reads again.
    pub(super) fn pos_order_residue(
        &self,
        c: &SyncCommand,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        let Some(source) = self.residue.clone() else {
            return Ok(self.reject(c, reasons::UNKNOWN_TYPE));
        };
        let p = match self.payload(c, "a pos.order_residue", PosOrderResiduePayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(*r),
        };
        let report = run(
            &*source,
            false,
            "pos.order_residue",
            p.min_age_minutes,
            &mut |_| Ok(()),
            ctx,
        )?;
        let o = &report.ours;
        let max = PosOrderResiduePayload::MAX_ROWS;
        let truncated = o.empty_open_checks.len() > max
            || o.orphan_info_rows.len() > max
            || o.stale_open_logs.len() > max
            || o.orphan_send_audits.len() > max;
        let check = |c: &ResidueCheck| PosResidueCheck {
            rvc_no: c.rvc_no,
            check_no: c.check_no,
            opened_local: sortable(c.opened_local),
        };
        let result = PosOrderResidueResult {
            min_age_minutes: p.min_age_minutes,
            empty_open_checks: o.empty_open_checks.iter().take(max).map(check).collect(),
            orphan_info_rows: o
                .orphan_info_rows
                .iter()
                .take(max)
                .map(|r| PosResidueInfo {
                    check_no: r.check_no,
                    trn_no: r.trn_no,
                    sent: r.sent,
                })
                .collect(),
            stale_open_logs: o.stale_open_logs.iter().take(max).map(check).collect(),
            orphan_send_audits: o.orphan_send_audits.iter().take(max).copied().collect(),
            other_rows_ignored: report.other_rows_ignored,
            sent_orphan_info_rows: report.sent_orphan_info_rows,
            truncated,
        };
        self.log.info(
            "pos.order_residue {Id}: reason {Reason}: {Empty} empty check(s), {Info} orphan info row(s), {Logs} stale open-log row(s), {Audits} send audit(s), {Ignored} ignored",
            &[
                Value::from(c.command_id),
                p.reason.as_deref().map_or(Value::Null, Value::from),
                Value::from(o.empty_open_checks.len()),
                Value::from(o.orphan_info_rows.len()),
                Value::from(o.stale_open_logs.len()),
                Value::from(o.orphan_send_audits.len()),
                Value::from(report.other_rows_ignored),
            ],
        );
        Ok(self.applied(c, Some(result.to_element())))
    }
}

/// Convenience for the host: the store as a residue source.
pub fn source_of(store: &Arc<SqlSiteCommandStore>) -> Arc<dyn OrderResidueSource> {
    store.clone()
}
