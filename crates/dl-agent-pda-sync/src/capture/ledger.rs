//! Strategy (b): the closed ledger (`Strategies/LedgerCapture.cs`). The tail pass
//! follows a keyset watermark over the monotonic key; a separate rescan re-reads the last
//! N days plus days named by ReOpenLog and diffs them against the shadow (late edits,
//! moved rows, deletions). Tables without a date column re-read their last rows instead.
//! LastChgTime is never used as a signal.

use std::collections::HashSet;

use super::codecs::value_codec;
use super::context::CaptureContext;
use super::error::{CaptureError, Result};
use super::governor::CycleDeadline;
use super::pipeline::{ChangeSet, RowChange, TombChange};
use super::plan::{CapturePlan, PlanTable, exclude_of};
use super::report::CaptureReport;
use super::source::{SourceQuery, VendorRow, date_value};
use super::state::StateCommit;
use super::util::{add_days, date_of, day_key, parse_day_key};
use super::writer::Cursors;
use crate::contracts::catalog::SyncCatalog;
use crate::contracts::hashing::ordinal_cmp;
use crate::contracts::mapping::RawValue;
use crate::contracts::time::{DateTime, DateTimeKind};
use crate::contracts::wire::SyncTombstone;
use crate::util::ctx::Ctx;

const REOPEN_TABLE: &str = "ReOpenLog";
const REOPEN_MARK: &str = "reopen:mark";
/// ReOpenLog time columns that name a day to rescan. OrderTime is absent on some builds.
const REOPEN_DAY_COLUMNS: [&str; 3] = ["CloseTime", "OpenTime", "OrderTime"];

/// Columns never read from ReOpenLog: from its plan when captured, else from the catalog.
pub fn reopen_exclude(catalog: &SyncCatalog, plan: &CapturePlan) -> Vec<String> {
    if let Some(p) = plan.find(REOPEN_TABLE) {
        return p.exclude.clone();
    }
    catalog
        .find(REOPEN_TABLE)
        .map(|t| exclude_of(catalog, t))
        .unwrap_or_default()
}

/// The watermark cursor of a table (`wm:<table>`).
pub fn watermark_cursor(p: &PlanTable) -> String {
    format!("wm:{}", p.name())
}

fn one_cursor(name: String, value: Option<String>) -> Cursors {
    let mut c = Cursors::new();
    c.insert(name, value);
    c
}

struct Pending {
    days: Vec<String>,
    mark: Option<DateTime>,
}

/// The ledger strategy.
pub struct LedgerCapture<'a> {
    cx: &'a CaptureContext,
    reopen_exclude: &'a [String],
}

impl<'a> LedgerCapture<'a> {
    /// The strategy over `cx`.
    pub fn new(cx: &'a CaptureContext, reopen_exclude: &'a [String]) -> LedgerCapture<'a> {
        LedgerCapture { cx, reopen_exclude }
    }

    /// The tail pass over `tables`.
    pub fn tail(
        &self,
        tables: &[PlanTable],
        deadline: &CycleDeadline,
        ctx: &Ctx,
    ) -> Result<CaptureReport> {
        let mut report = CaptureReport::new();
        for p in tables {
            if deadline.expired() {
                report.deadline_hit = true;
                break;
            }
            if !self.cx.exists(p.name(), ctx)? {
                report.table_missing(p.name(), p.table.optional_on_site);
                continue;
            }
            CaptureContext::guard_table(p, &mut report, ctx, |r| {
                self.tail_table(p, deadline, r, ctx)
            })?;
        }
        Ok(report)
    }

    fn tail_table(
        &self,
        p: &PlanTable,
        deadline: &CycleDeadline,
        report: &mut CaptureReport,
        ctx: &Ctx,
    ) -> Result<()> {
        let Some(mut cursor) = self.cx.state.get_cursor(&watermark_cursor(p), ctx)? else {
            report.notes.push(format!(
                "{}: no watermark yet (initial snapshot has not started); tail skipped",
                p.name()
            ));
            return Ok(());
        };
        let page_rows = self.cx.options.page_rows;
        while !deadline.expired() {
            let q = SourceQuery {
                after: CaptureContext::after_of(Some(&cursor))?,
                limit: Some(page_rows),
                ..CaptureContext::query(p)
            };
            let page = self.cx.source.query(&q, ctx)?;
            let Some(last) = page.last() else {
                return Ok(());
            };
            cursor = CaptureContext::ref_of(p, last)?;
            let count = page.len();
            self.process_page(p, page, Some(&cursor), report, ctx)?;
            if count < page_rows.max(0) as usize {
                return Ok(());
            }
        }
        report.deadline_hit = true;
        Ok(())
    }

    /// Maps, diffs and sends one page and advances the watermark atomically with the shadow.
    pub fn process_page(
        &self,
        p: &PlanTable,
        page: Vec<VendorRow>,
        new_watermark: Option<&str>,
        report: &mut CaptureReport,
        ctx: &Ctx,
    ) -> Result<()> {
        let bucket = |r: &super::pipeline::CapturedRow| {
            CaptureContext::day_of(p, &r.raw).unwrap_or_else(|| "-".to_owned())
        };
        let (_, changes) = self.cx.diff_page(p, page, &bucket, report, ctx)?;
        let cursors = new_watermark.map(|w| one_cursor(watermark_cursor(p), Some(w.to_owned())));
        self.cx.flush(
            &CaptureContext::target_of(p, None),
            &changes,
            new_watermark,
            cursors.as_ref(),
            report,
            ctx,
        )
    }

    /// The rescan pass over `tables`.
    pub fn rescan(
        &self,
        tables: &[PlanTable],
        deadline: &CycleDeadline,
        ctx: &Ctx,
    ) -> Result<CaptureReport> {
        let mut report = CaptureReport::new();
        let pending = self.load_pending(&mut report, ctx)?;
        let mut days: Vec<String> = Vec::new();
        for d in self
            .recent_days()
            .into_iter()
            .chain(pending.days.iter().cloned())
        {
            if !days.contains(&d) {
                days.push(d);
            }
        }
        days.sort_by(|a, b| ordinal_cmp(a, b));
        let mut complete = true;

        for p in tables {
            if deadline.expired() {
                complete = false;
                break;
            }
            if !self.cx.exists(p.name(), ctx)? {
                report.table_missing(p.name(), p.table.optional_on_site);
                continue;
            }
            let ok = CaptureContext::guard_table(p, &mut report, ctx, |r| {
                if p.day_column.is_none() {
                    return self.rescan_tail(p, r, ctx);
                }
                for day in &days {
                    if deadline.expired() {
                        complete = false;
                        break;
                    }
                    self.rescan_day(p, day, r, ctx)?;
                }
                Ok(())
            })?;
            if !ok {
                // Keep the re-opened days pending until the skipped table has rescanned them.
                complete = false;
            }
        }
        report.deadline_hit = !complete;
        if complete {
            self.clear_pending(&pending, ctx)?;
        }
        Ok(report)
    }

    fn recent_days(&self) -> Vec<String> {
        let today = date_of(self.cx.site_now());
        (0..self.cx.options.rescan_days.max(1))
            .map(|i| day_key(add_days(today, -i64::from(i))))
            .collect()
    }

    fn rescan_tail(&self, p: &PlanTable, report: &mut CaptureReport, ctx: &Ctx) -> Result<()> {
        let q = SourceQuery {
            descending: true,
            limit: Some(self.cx.options.tail_rescan_rows),
            ..CaptureContext::query(p)
        };
        let rows = self.cx.source.query(&q, ctx)?;
        let (_, changes) = self
            .cx
            .diff_page(p, rows, &|_| "-".to_owned(), report, ctx)?;
        self.cx.flush(
            &CaptureContext::target_of(p, None),
            &changes,
            None,
            None,
            report,
            ctx,
        )
    }

    fn rescan_day(
        &self,
        p: &PlanTable,
        day: &str,
        report: &mut CaptureReport,
        ctx: &Ctx,
    ) -> Result<()> {
        let from = parse_day_key(day).ok_or_else(|| {
            CaptureError::format(format!(
                "String '{day}' was not recognized as a valid DateTime."
            ))
        })?;
        let mut seen: HashSet<String> = HashSet::new();
        let mut cursor: Option<String> = None;
        let page_rows = self.cx.options.page_rows;
        loop {
            let q = SourceQuery {
                range_column: p.day_column.clone(),
                range_from: Some(from),
                range_to: Some(add_days(from, 1)),
                after: CaptureContext::after_of(cursor.as_deref())?,
                limit: Some(page_rows),
                ..CaptureContext::query(p)
            };
            let page = self.cx.source.query(&q, ctx)?;
            let Some(last) = page.last() else { break };
            let next = CaptureContext::ref_of(p, last)?;
            let count = page.len();
            let (rows, changes) = self
                .cx
                .diff_page(p, page, &|_| day.to_owned(), report, ctx)?;
            seen.extend(rows.into_iter().map(|r| r.state_key));
            self.cx.flush(
                &CaptureContext::target_of(p, None),
                &changes,
                None,
                None,
                report,
                ctx,
            )?;
            cursor = Some(next);
            if count < page_rows.max(0) as usize {
                break;
            }
        }
        self.reconcile_missing(p, day, &seen, report, ctx)
    }

    /// Shadow rows of the day that the day query no longer returns: moved or deleted.
    fn reconcile_missing(
        &self,
        p: &PlanTable,
        day: &str,
        seen: &HashSet<String>,
        report: &mut CaptureReport,
        ctx: &Ctx,
    ) -> Result<()> {
        let missing: Vec<_> = self
            .cx
            .state
            .get_shadow_entries(p.name(), day, ctx)?
            .into_iter()
            .filter(|e| !seen.contains(&e.key))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let mut changes = ChangeSet::default();
        let src_ver = self.cx.src_ver.next();
        let bucket = |r: &super::pipeline::CapturedRow| {
            CaptureContext::day_of(p, &r.raw).unwrap_or_else(|| "-".to_owned())
        };
        for e in missing {
            let current = match &e.ref_ {
                None => None,
                Some(r) => self.read_by_ref(p, r, ctx)?,
            };
            let Some(current) = current else {
                changes.tombstones.push(TombChange {
                    tombstone: SyncTombstone {
                        key: CaptureContext::wire_key(&e.key)?,
                        src_ver,
                        reason: Some("deleted".to_owned()),
                    },
                    state_key: e.key,
                });
                continue;
            };
            let day_now = CaptureContext::day_of(p, &current).unwrap_or_else(|| "-".to_owned());
            let (rows, moved) = self.cx.diff_page(p, vec![current], &bucket, report, ctx)?;
            let Some(first) = rows.first() else { continue };
            if moved.is_empty() {
                changes.shadow_only.push(RowChange {
                    state_key: first.state_key.clone(),
                    bucket: day_now,
                    row: first.mapped.to_sync_row(src_ver),
                    ref_: first.ref_.clone(),
                });
            } else {
                changes.upserts.extend(moved.upserts);
                changes.blobs.extend(moved.blobs);
                changes.shadow_only.extend(moved.shadow_only);
            }
        }
        self.cx.flush(
            &CaptureContext::target_of(p, None),
            &changes,
            None,
            None,
            report,
            ctx,
        )
    }

    fn read_by_ref(
        &self,
        p: &PlanTable,
        encoded_ref: &str,
        ctx: &Ctx,
    ) -> Result<Option<VendorRow>> {
        let values = value_codec::decode_key(Some(encoded_ref))?.unwrap_or_default();
        // Dictionary(OrdinalIgnoreCase): a repeated column keeps its first position, last value.
        let mut where_: Vec<(String, RawValue)> = Vec::new();
        for (i, col) in p.order_columns.iter().enumerate() {
            let v = values.get(i).cloned().ok_or_else(|| {
                CaptureError::new(
                    super::error::ErrorKind::Argument,
                    "Index was out of range. Must be non-negative and less than the size of the collection. (Parameter 'index')",
                )
            })?;
            match where_
                .iter_mut()
                .find(|(c, _)| crate::util::stj::eq_ignore_case(c, col))
            {
                Some(slot) => slot.1 = v,
                None => where_.push((col.clone(), v)),
            }
        }
        let q = SourceQuery {
            where_: Some(where_),
            limit: Some(2),
            ..CaptureContext::query(p)
        };
        Ok(self.cx.source.query(&q, ctx)?.into_iter().next())
    }

    /// Days touched by ReOpenLog since the last completed rescan.
    fn load_pending(&self, report: &mut CaptureReport, ctx: &Ctx) -> Result<Pending> {
        let mut days: Vec<String> = Vec::new();
        let from = match self.cx.state.get_cursor(REOPEN_MARK, ctx)? {
            None => None,
            Some(text) => {
                let ticks: i64 = text.trim().parse().map_err(|_| {
                    CaptureError::format("The input string was not in a correct format.")
                })?;
                Some(
                    DateTime::from_ticks(ticks, DateTimeKind::Unspecified)
                        .ok_or_else(|| CaptureError::argument("Ticks must be between DateTime.MinValue.Ticks and DateTime.MaxValue.Ticks. (Parameter 'ticks')"))?,
                )
            }
        };
        let mut max = from;
        if self.cx.exists(REOPEN_TABLE, ctx)? {
            // Columns = None: the source selects the columns this build has.
            let range_from = from.unwrap_or_else(|| {
                add_days(
                    date_of(self.cx.site_now()),
                    -i64::from(self.cx.options.rescan_days),
                )
            });
            let q = SourceQuery {
                exclude_columns: (!self.reopen_exclude.is_empty())
                    .then(|| self.reopen_exclude.to_vec()),
                range_column: Some("ReOpenTime".to_owned()),
                range_from: Some(range_from),
                order_by: vec!["ReOpenTime".to_owned()],
                limit: Some(5000),
                ..SourceQuery::new(REOPEN_TABLE)
            };
            let rows = self.cx.source.query(&q, ctx)?;
            for r in &rows {
                for col in REOPEN_DAY_COLUMNS {
                    if let Some(d) = date_value(r, col) {
                        days.push(day_key(d));
                    }
                }
                if let Some(t) = date_value(r, "ReOpenTime")
                    && max.is_none_or(|m| t.ticks() > m.ticks())
                {
                    max = Some(t);
                }
            }
            if rows.len() == 5000 {
                report.notes.push(
                    "ReOpenLog returned the page cap; remaining days are picked up next rescan"
                        .to_owned(),
                );
            }
        }
        let mut distinct: Vec<String> = Vec::new();
        for d in days {
            if !distinct.contains(&d) {
                distinct.push(d);
            }
        }
        Ok(Pending {
            days: distinct,
            mark: max,
        })
    }

    fn clear_pending(&self, pending: &Pending, ctx: &Ctx) -> Result<()> {
        let Some(m) = pending.mark else { return Ok(()) };
        let mut commit = StateCommit::default();
        commit
            .cursors
            .insert(REOPEN_MARK, Some(m.ticks().to_string()));
        self.cx.state.commit(&commit, ctx)
    }
}
