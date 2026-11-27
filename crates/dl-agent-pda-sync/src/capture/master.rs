//! Strategy (c): masters, configuration and small logs (`Strategies/MasterScanCapture.cs`).
//! Every scan reads the whole table by keyset pages, sends rows whose hash differs from
//! the shadow, and tombstones keys that vanished from a complete scan.

use std::collections::HashSet;

use super::context::CaptureContext;
use super::error::Result;
use super::pipeline::{ChangeSet, TombChange};
use super::plan::PlanTable;
use super::report::CaptureReport;
use super::source::{SourceQuery, value_or_null};
use crate::contracts::mapping::RawValue;
use crate::contracts::wire::SyncTombstone;
use crate::util::ctx::Ctx;

/// The master scan strategy.
pub struct MasterScanCapture<'a> {
    cx: &'a CaptureContext,
}

impl<'a> MasterScanCapture<'a> {
    /// The strategy over `cx`.
    pub fn new(cx: &'a CaptureContext) -> MasterScanCapture<'a> {
        MasterScanCapture { cx }
    }

    /// Scans every table.
    pub fn run(&self, tables: &[PlanTable], ctx: &Ctx) -> Result<CaptureReport> {
        let mut report = CaptureReport::new();
        for p in tables {
            if !self.cx.exists(p.name(), ctx)? {
                report.table_missing(p.name(), p.table.optional_on_site);
                continue;
            }
            CaptureContext::guard_table(p, &mut report, ctx, |r| self.scan_table(p, r, ctx))?;
        }
        Ok(report)
    }

    /// Scans one table.
    pub fn scan_table(&self, p: &PlanTable, report: &mut CaptureReport, ctx: &Ctx) -> Result<()> {
        let before: Vec<String> = self
            .cx
            .state
            .get_shadow(p.name(), None, ctx)?
            .keys()
            .map(str::to_owned)
            .collect();
        let mut seen: HashSet<String> = HashSet::new();
        let rejected_at_start = report.rejected.len();
        // Tables that tolerate duplicate rows cannot be keyset-paged; they are small, read whole.
        let mut paged = !p.table.duplicate_rows_tolerated;
        let page_rows = self.cx.options.page_rows;
        let mut cursor: Option<String> = None;
        loop {
            let q = SourceQuery {
                after: CaptureContext::after_of(cursor.as_deref())?,
                limit: paged.then_some(page_rows),
                ..CaptureContext::query(p)
            };
            let page = self.cx.source.query(&q, ctx)?;
            let Some(last) = page.last().cloned() else {
                break;
            };
            let count = page.len();
            let (rows, changes) = self
                .cx
                .diff_page(p, page, &|_| "-".to_owned(), report, ctx)?;
            seen.extend(rows.into_iter().map(|r| r.state_key));
            self.cx.flush(
                &CaptureContext::target_of(p, None),
                &changes,
                None,
                None,
                report,
                ctx,
            )?;
            if !paged || count < page_rows.max(0) as usize {
                break;
            }
            if p.order_columns
                .iter()
                .any(|c| value_or_null(&last, c) == RawValue::Null)
            {
                // A keyset cannot continue past a NULL key part: masters are small, read whole.
                report.notes.push(format!(
                    "{}: NULL in key, scanned whole instead of by keyset",
                    p.name()
                ));
                paged = false;
                cursor = None;
                continue;
            }
            cursor = Some(CaptureContext::ref_of(p, &last)?);
        }

        // A row that failed mapping is not in `seen`; deleting it from the cloud would be wrong.
        if report.rejected.len() > rejected_at_start {
            report.notes.push(format!(
                "{}: tombstones withheld because some rows were rejected",
                p.name()
            ));
            return Ok(());
        }
        let mut gone = ChangeSet::default();
        let src_ver = self.cx.src_ver.next();
        for k in before.into_iter().filter(|k| !seen.contains(k)) {
            gone.tombstones.push(TombChange {
                tombstone: SyncTombstone {
                    key: CaptureContext::wire_key(&k)?,
                    src_ver,
                    reason: Some("deleted".to_owned()),
                },
                state_key: k,
            });
        }
        self.cx.flush(
            &CaptureContext::target_of(p, None),
            &gone,
            None,
            None,
            report,
            ctx,
        )
    }
}
