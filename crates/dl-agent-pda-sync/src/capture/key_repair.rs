//! Resend by key (`Strategies/KeyRepairCapture.cs`, ADR 0128): the rows `sync.verify`
//! found different between the shadow and the cloud are sent again, or tombstoned when the
//! store no longer has them.
//!
//! The mapped key cannot be turned back into a source query (text codecs, defaults and
//! constants are one-way), so the table is read whole by keyset pages, SELECT-only, and
//! each mapped row is matched by its cloud row key. A matched row is sent with a fresh
//! `src_ver` whatever the shadow says. A key the full scan never met is gone from the
//! store: it is tombstoned, which also clears it from the cloud and the shadow. Tombstones
//! are withheld when a row of the scan failed mapping (that row may be the key). Progress
//! is kept in the cursor `repair:{table}`, so a scan cut by the cycle deadline resumes
//! where it stopped.

use std::collections::HashMap;

use super::codecs::key_codec;
use super::context::CaptureContext;
use super::error::Result;
use super::governor::CycleDeadline;
use super::pipeline::{self, ChangeSet, RowChange, TombChange};
use super::plan::{CaptureStrategy, PlanTable};
use super::report::CaptureReport;
use super::source::{SourceQuery, value_or_null};
use super::writer::Cursors;
use crate::contracts::hashing::row_key;
use crate::contracts::mapping::RawValue;
use crate::contracts::wire::SyncTombstone;
use crate::util::ctx::Ctx;

/// One cloud row key whose row the store and the cloud disagree on (ADR 0128), queued by
/// `sync.verify`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRepairTarget {
    /// Queue order; a scan only settles keys queued before it started.
    pub id: i64,
    /// Vendor table.
    pub table: String,
    /// The cloud row key ([`row_key::of`] of the wire key).
    pub row_key: String,
}

impl KeyRepairTarget {
    /// A target.
    pub fn new(id: i64, table: &str, row_key: &str) -> KeyRepairTarget {
        KeyRepairTarget {
            id,
            table: table.to_owned(),
            row_key: row_key.to_owned(),
        }
    }
}

/// Durable list of [`KeyRepairTarget`]s, owned by the agent (its outbox, `IKeyRepairList`).
pub trait KeyRepairList: Send + Sync {
    /// The oldest waiting keys.
    fn pending(&self, ctx: &Ctx) -> Result<Vec<KeyRepairTarget>>;
    /// Removes settled keys.
    fn complete(&self, ids: &[i64], ctx: &Ctx) -> Result<()>;
}

/// The capture cursor of a table's key repair scan.
pub fn cursor_of(table: &str) -> String {
    format!("repair:{table}")
}

/// Cursor value `maxId|rejected|after`: the newest key the scan settles, a rejected-row
/// flag, the keyset position.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScanState {
    max_id: i64,
    rejected: bool,
    after: Option<String>,
}

impl ScanState {
    fn format(&self) -> String {
        format!(
            "{}|{}|{}",
            self.max_id,
            if self.rejected { "1" } else { "0" },
            self.after.as_deref().unwrap_or("")
        )
    }

    /// `long.TryParse(.., NumberStyles.None, ..)`: ASCII digits only.
    fn parse(text: Option<&str>) -> Option<ScanState> {
        let mut parts = text?.splitn(3, '|');
        let (max, rejected, after) = (parts.next()?, parts.next()?, parts.next()?);
        if max.is_empty() || !max.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(ScanState {
            max_id: max.parse().ok()?,
            rejected: rejected == "1",
            after: (!after.is_empty()).then(|| after.to_owned()),
        })
    }
}

/// The key repair strategy.
pub struct KeyRepairCapture<'a> {
    cx: &'a CaptureContext,
    queue: &'a dyn KeyRepairList,
}

impl<'a> KeyRepairCapture<'a> {
    /// The strategy over `cx` and `queue`.
    pub fn new(cx: &'a CaptureContext, queue: &'a dyn KeyRepairList) -> KeyRepairCapture<'a> {
        KeyRepairCapture { cx, queue }
    }

    /// Settles the waiting keys of every table, within `deadline`.
    pub fn run(
        &self,
        plan: &super::plan::CapturePlan,
        deadline: &CycleDeadline,
        ctx: &Ctx,
    ) -> Result<CaptureReport> {
        let mut report = CaptureReport::new();
        let pending = self.queue.pending(ctx)?;
        // GroupBy(Table, OrdinalIgnoreCase): first-seen order and spelling.
        let mut groups: Vec<(String, Vec<KeyRepairTarget>)> = Vec::new();
        for t in pending {
            match groups
                .iter_mut()
                .find(|(name, _)| name.to_lowercase() == t.table.to_lowercase())
            {
                Some((_, list)) => list.push(t),
                None => groups.push((t.table.clone(), vec![t])),
            }
        }
        for (table, targets) in groups {
            if deadline.expired() {
                report.deadline_hit = true;
                break;
            }
            let ids: Vec<i64> = targets.iter().map(|t| t.id).collect();
            let p = plan.find(&table).filter(|p| {
                p.strategy != CaptureStrategy::OpenSnapshotDiff && !p.table.duplicate_rows_tolerated
            });
            let Some(p) = p else {
                report.notes.push(format!(
                    "{table}: key repair dropped, the table is not repaired by key"
                ));
                self.queue.complete(&ids, ctx)?;
                continue;
            };
            if !self.cx.exists(p.name(), ctx)? {
                report.table_missing(p.name());
                self.queue.complete(&ids, ctx)?;
                continue;
            }
            CaptureContext::guard_table(p, &mut report, ctx, |r| {
                self.repair_table(p, &targets, deadline, r, ctx)
            })?;
        }
        Ok(report)
    }

    fn repair_table(
        &self,
        p: &PlanTable,
        targets: &[KeyRepairTarget],
        deadline: &CycleDeadline,
        report: &mut CaptureReport,
        ctx: &Ctx,
    ) -> Result<()> {
        let cx = self.cx;
        let cursor_name = cursor_of(p.name());
        let mut scan = match ScanState::parse(cx.state.get_cursor(&cursor_name, ctx)?.as_deref()) {
            Some(s) => s,
            None => ScanState {
                max_id: targets.iter().map(|t| t.id).max().unwrap_or(0),
                rejected: false,
                after: None,
            },
        };
        let mut wanted: HashMap<String, Vec<i64>> = HashMap::new();
        for t in targets {
            wanted.entry(t.row_key.clone()).or_default().push(t.id);
        }
        let page_rows = cx.options.page_rows;
        // Tables whose key can hold a NULL cannot continue a keyset past it; they are small
        // and read whole.
        let mut paged = true;
        loop {
            if deadline.expired() {
                report.deadline_hit = true;
                return Ok(());
            }
            let q = SourceQuery {
                after: if paged {
                    CaptureContext::after_of(scan.after.as_deref())?
                } else {
                    None
                },
                limit: paged.then_some(page_rows),
                ..CaptureContext::query(p)
            };
            let page = cx.source.query(&q, ctx)?;
            let full_page = page.len() == page_rows.max(0) as usize;
            if paged
                && full_page
                && let Some(last) = page.last()
                && p.order_columns
                    .iter()
                    .any(|c| value_or_null(last, c) == RawValue::Null)
            {
                report.notes.push(format!(
                    "{}: NULL in key, key repair reads the table whole",
                    p.name()
                ));
                paged = false;
                continue;
            }
            let last_ref = match page.last() {
                Some(last) => Some(CaptureContext::ref_of(p, last)?),
                None => None,
            };
            let count = page.len();

            let rejected_before = report.rejected.len();
            let rows = pipeline::capture(&cx.mapper, p, page, report)?;
            let rejected = scan.rejected || report.rejected.len() > rejected_before;
            let mut changes = ChangeSet::default();
            let mut settled: Vec<i64> = Vec::new();
            let src_ver = cx.src_ver.next();
            for r in rows {
                // A "#n" duplicate of a broken logical key: the cloud keeps one row per key,
                // the first occurrence.
                if r.state_key != r.base_key {
                    continue;
                }
                let Some(ids) = wanted.remove(&row_key::of(&r.mapped.key)) else {
                    continue;
                };
                let ref_ = CaptureContext::ref_of(p, &r.raw)?;
                changes.upserts.push(RowChange {
                    state_key: r.state_key.clone(),
                    bucket: CaptureContext::day_of(p, &r.raw).unwrap_or_else(|| "-".to_owned()),
                    row: r.mapped.to_sync_row(src_ver),
                    ref_: Some(ref_),
                });
                settled.extend(ids);
            }

            let end = !paged || count < page_rows.max(0) as usize;
            if !end {
                scan.rejected = rejected;
                scan.after = last_ref;
                let mut cursors = Cursors::new();
                cursors.insert(cursor_name.clone(), Some(scan.format()));
                cx.flush(
                    &CaptureContext::target_of(p, None),
                    &changes,
                    None,
                    Some(&cursors),
                    report,
                    ctx,
                )?;
                self.queue.complete(&settled, ctx)?;
                continue;
            }

            // The scan is complete: a key queued before it started and never met is gone
            // from the store.
            let mut leftover: Vec<(String, Vec<i64>)> = wanted.drain().collect();
            leftover.sort_by(|a, b| a.1.iter().min().cmp(&b.1.iter().min()));
            for (key, ids) in leftover {
                let due: Vec<i64> = ids.into_iter().filter(|&id| id <= scan.max_id).collect();
                if due.is_empty() {
                    continue;
                }
                settled.extend(due);
                if rejected {
                    continue;
                }
                let Some(wire) = row_key::try_parse(&key) else {
                    report.notes.push(format!(
                        "{}: key repair skipped a key that is not a canonical row key",
                        p.name()
                    ));
                    continue;
                };
                changes.tombstones.push(TombChange {
                    state_key: key_codec::encode(&wire),
                    tombstone: SyncTombstone {
                        key: wire,
                        src_ver,
                        reason: Some("deleted".to_owned()),
                    },
                });
            }
            if rejected {
                report.notes.push(format!(
                    "{}: key repair tombstones withheld because some rows were rejected",
                    p.name()
                ));
            }
            let mut cursors = Cursors::new();
            cursors.insert(cursor_name.clone(), None);
            cx.flush(
                &CaptureContext::target_of(p, None),
                &changes,
                None,
                Some(&cursors),
                report,
                ctx,
            )?;
            self.queue.complete(&settled, ctx)?;
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ScanState;

    #[test]
    fn the_cursor_round_trips_and_rejects_what_csharp_rejects() {
        let s = ScanState {
            max_id: 42,
            rejected: true,
            after: Some("[\"i:1\"]|x".to_owned()),
        };
        assert_eq!(s.format(), "42|1|[\"i:1\"]|x");
        assert_eq!(ScanState::parse(Some(&s.format())), Some(s));
        let fresh = ScanState::parse(Some("7|0|")).unwrap();
        assert_eq!(
            (fresh.max_id, fresh.rejected, fresh.after),
            (7, false, None)
        );
        for bad in ["", "7|0", "-1|0|", "+1|0|", " 1|0|", "x|0|"] {
            assert_eq!(ScanState::parse(Some(bad)), None, "{bad}");
        }
        assert_eq!(ScanState::parse(None), None);
    }
}
