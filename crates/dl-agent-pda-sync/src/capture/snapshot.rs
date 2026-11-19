//! First-load mode (`Strategies/InitialSnapshotRunner.cs`): walks every captured table in
//! keyset chunks and persists its position after every chunk, so a restart resumes where
//! it stopped. Ledger tables load a recent window first (24 months, 6 for logs) and
//! optionally older history afterwards; masters resume through their shadow; open checks
//! run one normal cycle. The tail watermark is pinned to the table head before any chunk
//! is read, so rows arriving meanwhile are not lost.

use super::context::CaptureContext;
use super::error::Result;
use super::governor::CycleDeadline;
use super::ledger::watermark_cursor;
use super::master::MasterScanCapture;
use super::open_check::OpenCheckCapture;
use super::plan::{CapturePlan, PlanTable};
use super::report::CaptureReport;
use super::source::SourceQuery;
use super::state::StateCommit;
use super::util::{add_months, date_of};
use super::writer::Cursors;
use crate::util::ctx::Ctx;
use crate::util::stj::eq_ignore_case;

const DONE: &str = "done";

/// Outcome of one snapshot call; `complete == false` means call again (progress is persisted).
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotResult {
    /// The pass report.
    pub report: CaptureReport,
    /// Whether the load finished.
    pub complete: bool,
}

/// The progress cursor of a ledger table's phase (`snap:<table>:<phase>`).
fn cursor_of(p: &PlanTable, phase: &str) -> String {
    format!("snap:{}:{phase}", p.name())
}

/// Every snapshot progress cursor of a ledger table (window and backfill phases).
pub fn cursors_of(p: &PlanTable) -> [String; 2] {
    [cursor_of(p, "win"), cursor_of(p, "back")]
}

/// The snapshot runner.
pub struct InitialSnapshotRunner<'a> {
    cx: &'a CaptureContext,
    masters: MasterScanCapture<'a>,
    open: OpenCheckCapture<'a>,
}

impl<'a> InitialSnapshotRunner<'a> {
    /// The runner over `cx`.
    pub fn new(cx: &'a CaptureContext) -> InitialSnapshotRunner<'a> {
        InitialSnapshotRunner {
            cx,
            masters: MasterScanCapture::new(cx),
            open: OpenCheckCapture::new(cx),
        }
    }

    /// Runs (or resumes) the initial load.
    pub fn run(
        &self,
        plan: &CapturePlan,
        deadline: &CycleDeadline,
        ctx: &Ctx,
    ) -> Result<SnapshotResult> {
        let mut report = CaptureReport::new();
        if let Some(first) = plan.open.first() {
            // Open checks share one stream across tables: a schema failure skips the whole job.
            CaptureContext::guard_table(first, &mut report, ctx, |r| {
                r.merge(self.open.run(&plan.open, ctx)?);
                Ok(())
            })?;
        }
        report.merge(self.masters.run(&plan.masters, ctx)?);
        let complete = self.ledger_phases(plan, deadline, &mut report, ctx)?;
        report.deadline_hit = !complete;
        Ok(SnapshotResult { report, complete })
    }

    fn ledger_phases(
        &self,
        plan: &CapturePlan,
        deadline: &CycleDeadline,
        report: &mut CaptureReport,
        ctx: &Ctx,
    ) -> Result<bool> {
        let all: Vec<&PlanTable> = plan.ledger.iter().collect();
        let mut complete = self.ledger_phase(&all, "win", deadline, report, ctx)?;
        if complete && self.cx.options.include_backfill {
            let dated: Vec<&PlanTable> = plan
                .ledger
                .iter()
                .filter(|p| p.day_column.is_some())
                .collect();
            complete = self.ledger_phase(&dated, "back", deadline, report, ctx)?;
        }
        Ok(complete)
    }

    fn ledger_phase(
        &self,
        tables: &[&PlanTable],
        phase: &str,
        deadline: &CycleDeadline,
        report: &mut CaptureReport,
        ctx: &Ctx,
    ) -> Result<bool> {
        for p in tables {
            if !self.cx.exists(p.name(), ctx)? {
                report.table_missing(p.name());
                continue;
            }
            // Already skipped by the win phase of this run: one fault per table and run.
            if report
                .table_faults
                .iter()
                .any(|f| eq_ignore_case(&f.table, p.name()))
            {
                continue;
            }
            let mut done = false;
            let ok = CaptureContext::guard_table(p, report, ctx, |r| {
                self.pin_watermark(p, ctx)?;
                done = self.chunk_table(p, phase, deadline, r, ctx)?;
                Ok(())
            })?;
            // A skipped table does not hold the others back.
            if ok && !done {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Finishes the ledger load of tables skipped after a schema error (AD1).
    pub fn resume_ledger(
        &self,
        plan: &CapturePlan,
        deadline: &CycleDeadline,
        ctx: &Ctx,
    ) -> Result<CaptureReport> {
        let mut report = CaptureReport::new();
        let complete = self.ledger_phases(plan, deadline, &mut report, ctx)?;
        report.deadline_hit = !complete;
        Ok(report)
    }

    fn pin_watermark(&self, p: &PlanTable, ctx: &Ctx) -> Result<()> {
        if self
            .cx
            .state
            .get_cursor(&watermark_cursor(p), ctx)?
            .is_some()
        {
            return Ok(());
        }
        let q = SourceQuery {
            descending: true,
            limit: Some(1),
            ..CaptureContext::query(p)
        };
        let head = self.cx.source.query(&q, ctx)?;
        let pin = match head.first() {
            None => "[]".to_owned(),
            Some(r) => CaptureContext::ref_of(p, r)?,
        };
        let mut commit = StateCommit::default();
        commit.cursors.insert(watermark_cursor(p), Some(pin));
        self.cx.state.commit(&commit, ctx)
    }

    fn chunk_table(
        &self,
        p: &PlanTable,
        phase: &str,
        deadline: &CycleDeadline,
        report: &mut CaptureReport,
        ctx: &Ctx,
    ) -> Result<bool> {
        let name = cursor_of(p, phase);
        let mut cursor = self.cx.state.get_cursor(&name, ctx)?;
        if cursor.as_deref() == Some(DONE) {
            return Ok(true);
        }
        let o = &self.cx.options;
        // C# uses LogTables.Contains (the set's OrdinalIgnoreCase comparer).
        let months = if o.is_log_table(p.name()) {
            o.log_snapshot_window_months
        } else {
            o.snapshot_window_months
        };
        let window_start = add_months(date_of(self.cx.site_now()), -months);
        let chunk = o.snapshot_chunk_rows;

        while !deadline.expired() {
            let mut q = SourceQuery {
                after: CaptureContext::after_of(cursor.as_deref())?,
                limit: Some(chunk),
                ..CaptureContext::query(p)
            };
            if p.day_column.is_some() {
                q.range_column = p.day_column.clone();
                if phase == "win" {
                    q.range_from = Some(window_start);
                    q.range_includes_null = true;
                } else {
                    q.range_to = Some(window_start);
                }
            }
            let page = self.cx.source.query(&q, ctx)?;
            let last = page.len() < chunk.max(0) as usize;
            let next = match page.last() {
                Some(r) if !last => CaptureContext::ref_of(p, r)?,
                _ => DONE.to_owned(),
            };
            cursor = Some(next.clone());

            let bucket = |r: &super::pipeline::CapturedRow| {
                CaptureContext::day_of(p, &r.raw).unwrap_or_else(|| "-".to_owned())
            };
            let (_, changes) = self.cx.diff_page(p, page, &bucket, report, ctx)?;
            let mut cursors = Cursors::new();
            cursors.insert(name.clone(), Some(next.clone()));
            let watermark = (next != DONE).then_some(next.as_str());
            self.cx.flush(
                &CaptureContext::target_of(p, None),
                &changes,
                watermark,
                Some(&cursors),
                report,
                ctx,
            )?;
            if last {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
