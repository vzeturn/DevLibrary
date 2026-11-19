//! Strategy (a): open checks (`Strategies/OpenCheckCapture.cs`). Each cycle reads the
//! whole open set of the Tmp* tables, hashes every check as one unit and compares it with
//! the shadow. A changed check is re-read until two consecutive reads agree (NOLOCK can
//! see half-written checks). A check missing for several polls is tombstoned with the
//! reason found in Invoice (closed) or InvoiceCxl/VoidTrn (void).

use std::collections::HashSet;

use dl_hash::{Digest, Sha256};

use super::codecs::value_codec;
use super::context::CaptureContext;
use super::error::{CaptureError, ErrorKind, Result};
use super::pipeline::{self, CapturedRow, ChangeSet, TombChange};
use super::plan::PlanTable;
use super::report::CaptureReport;
use super::source::{SourceQuery, value_or_null};
use super::state::{Shadow, ShadowEntry, ShadowWrite, StateCommit};
use super::util::{DotNetDict, invariant_string};
use crate::contracts::hashing::ordinal_cmp;
use crate::contracts::mapping::RawValue;
use crate::contracts::wire::SyncTombstone;
use crate::util::ctx::Ctx;
use crate::util::stj::{self, Style, Writer, eq_ignore_case};

/// The stream every open-check table writes to.
pub const STREAM: &str = "open";
/// Shadow table of the per-check hashes.
pub const OPEN_TABLE: &str = "$open";
/// Cursor of the vanish counters (JSON object check key to misses).
pub const MISS_CURSOR: &str = "open:misses";

struct CheckState {
    key: String,
    value: RawValue,
    /// Table name (ignoring case) to its rows.
    rows: Vec<(String, Vec<CapturedRow>)>,
    hash: String,
}

impl CheckState {
    fn rows_of(&self, table: &str) -> &[CapturedRow] {
        self.rows
            .iter()
            .find(|(t, _)| eq_ignore_case(t, table))
            .map_or(&[], |(_, r)| r.as_slice())
    }

    fn seal(&mut self) {
        let mut lines: Vec<String> = self
            .rows
            .iter()
            .flat_map(|(t, rows)| {
                rows.iter()
                    .map(move |r| format!("{t}|{}|{}", r.state_key, r.hash()))
            })
            .collect();
        lines.sort_by(|a, b| ordinal_cmp(a, b));
        self.hash = dl_encoding::hex::encode(Sha256::digest(lines.join("\n").as_bytes()));
    }
}

/// `JsonSerializer.Deserialize<Dictionary<string, int>>`.
fn load_misses(json: Option<&str>) -> Result<DotNetDict<i32>> {
    let mut d = DotNetDict::new();
    let Some(json) = json else { return Ok(d) };
    let bad = || {
        CaptureError::new(
            ErrorKind::Other,
            format!(
                "The JSON value could not be converted to System.Collections.Generic.Dictionary`2[System.String,System.Int32]. {json}"
            ),
        )
    };
    match stj::parse(json).map_err(|e| CaptureError::new(ErrorKind::Other, e.to_string()))? {
        dl_json::Value::Null => {}
        dl_json::Value::Object(map) => {
            for (k, v) in map.iter() {
                let n = stj::read_int::<i32>(v, stj::SNAKE, k).map_err(|_| bad())?;
                d.insert(k, n);
            }
        }
        _ => return Err(bad()),
    }
    Ok(d)
}

fn save_misses(misses: &DotNetDict<i32>) -> String {
    let mut w = Writer::new(Style::Compact);
    w.begin_object();
    for (k, n) in misses.iter() {
        w.key(k).int(i64::from(*n));
    }
    w.end_object();
    w.finish()
}

/// The open-check strategy.
pub struct OpenCheckCapture<'a> {
    cx: &'a CaptureContext,
}

impl<'a> OpenCheckCapture<'a> {
    /// The strategy over `cx`.
    pub fn new(cx: &'a CaptureContext) -> OpenCheckCapture<'a> {
        OpenCheckCapture { cx }
    }

    /// One open-check cycle over `plans`.
    pub fn run(&self, plans: &[PlanTable], ctx: &Ctx) -> Result<CaptureReport> {
        let mut report = CaptureReport::new();
        let mut tables: Vec<&PlanTable> = Vec::new();
        for p in plans {
            if self.cx.exists(p.name(), ctx)? {
                tables.push(p);
            } else {
                report.table_missing(p.name());
            }
        }
        if tables.is_empty() {
            return Ok(report);
        }

        let current = self.read(&tables, None, &mut report, ctx)?;
        let shadow = self.cx.state.get_shadow(OPEN_TABLE, None, ctx)?;

        let mut accepted: Vec<CheckState> = Vec::new();
        for (_, check) in current.iter() {
            if shadow.get(&check.key) == Some(check.hash.as_str()) {
                continue;
            }
            match self.stabilize(&tables, check, &mut report, ctx)? {
                Some(stable) => accepted.push(stable),
                None => report.unstable_checks_deferred += 1,
            }
        }

        let missing: Vec<String> = shadow
            .keys()
            .filter(|k| !current.contains_key(k))
            .map(str::to_owned)
            .collect();
        let present: Vec<&str> = current.keys().collect();
        let vanished = self.vanished(&missing, &present, ctx)?;
        self.emit(&tables, &accepted, &vanished, &mut report, ctx)?;
        Ok(report)
    }

    fn read(
        &self,
        tables: &[&PlanTable],
        only_check: Option<&RawValue>,
        report: &mut CaptureReport,
        ctx: &Ctx,
    ) -> Result<DotNetDict<CheckState>> {
        let mut checks: DotNetDict<CheckState> = DotNetDict::new();
        for p in tables {
            let col = p.check_column.as_deref().unwrap_or("CheckNo");
            let mut q = CaptureContext::query(p);
            if let Some(v) = only_check {
                q.where_ = Some(vec![(col.to_owned(), v.clone())]);
            }
            let raw = self.cx.source.query(&q, ctx)?;
            for mut row in pipeline::capture(&self.cx.mapper, p, raw, report)? {
                let value = value_or_null(&row.raw, col);
                let Some(key) = invariant_string(&value) else {
                    continue;
                };
                row.ref_ = Some(value_codec::encode(&value)?);
                if !checks.contains_key(&key) {
                    checks.insert(
                        &key,
                        CheckState {
                            key: key.clone(),
                            value: value.clone(),
                            rows: Vec::new(),
                            hash: String::new(),
                        },
                    );
                }
                if let Some(state) = checks.get_mut(&key) {
                    match state
                        .rows
                        .iter_mut()
                        .find(|(t, _)| eq_ignore_case(t, p.name()))
                    {
                        Some((_, list)) => list.push(row),
                        None => state.rows.push((p.name().to_owned(), vec![row])),
                    }
                }
            }
        }
        let keys: Vec<String> = checks.keys().map(str::to_owned).collect();
        for k in keys {
            if let Some(c) = checks.get_mut(&k) {
                c.seal();
            }
        }
        Ok(checks)
    }

    /// Re-reads a changed check until two consecutive reads agree; `None` when it never
    /// settles within the retry budget.
    fn stabilize(
        &self,
        tables: &[&PlanTable],
        first: &CheckState,
        report: &mut CaptureReport,
        ctx: &Ctx,
    ) -> Result<Option<CheckState>> {
        let mut prev_key = first.key.clone();
        let mut prev_value = first.value.clone();
        let mut prev_hash = first.hash.clone();
        for _ in 0..self.cx.options.stable_read_retries.max(0) {
            let mut again = self.read(tables, Some(&prev_value), &mut CaptureReport::new(), ctx)?;
            let Some(next) = again.take(&prev_key) else {
                return Ok(None);
            };
            if next.hash == prev_hash {
                return Ok(Some(next));
            }
            prev_key = next.key.clone();
            prev_value = next.value.clone();
            prev_hash = next.hash.clone();
        }
        report
            .notes
            .push(format!("open check {} kept changing; deferred", first.key));
        Ok(None)
    }

    fn vanished(
        &self,
        missing: &[String],
        present: &[&str],
        ctx: &Ctx,
    ) -> Result<Vec<(String, String, Option<String>)>> {
        let mut misses = load_misses(self.cx.state.get_cursor(MISS_CURSOR, ctx)?.as_deref())?;
        for k in present {
            misses.remove(k);
        }
        let mut confirmed = Vec::new();
        for key in missing {
            let n = misses.get(key).copied().unwrap_or(0) + 1;
            if n < self.cx.options.vanish_confirm_polls {
                misses.insert(key, n);
                continue;
            }
            misses.remove(key);
            let entries = self.cx.state.get_shadow_entries(OPEN_TABLE, key, ctx)?;
            let ref_ = entries.into_iter().next().and_then(|e| e.ref_);
            let reason = self.reason(ref_.as_deref(), key, ctx)?;
            confirmed.push((key.clone(), reason, ref_));
        }
        let mut commit = StateCommit::default();
        commit.cursors.insert(
            MISS_CURSOR,
            (!misses.is_empty()).then(|| save_misses(&misses)),
        );
        self.cx.state.commit(&commit, ctx)?;
        Ok(confirmed)
    }

    fn reason(&self, encoded_ref: Option<&str>, key: &str, ctx: &Ctx) -> Result<String> {
        let value = match encoded_ref {
            None => RawValue::from(key),
            Some(r) => match value_codec::decode(r)? {
                RawValue::Null => RawValue::from(key),
                v => v,
            },
        };
        if self.any("Invoice", &value, ctx)? {
            return Ok("closed".to_owned());
        }
        if self.any("InvoiceCxl", &value, ctx)? || self.any("VoidTrn", &value, ctx)? {
            return Ok("void".to_owned());
        }
        Ok("vanished".to_owned())
    }

    fn any(&self, table: &str, value: &RawValue, ctx: &Ctx) -> Result<bool> {
        if !self.cx.exists(table, ctx)? {
            return Ok(false);
        }
        let q = SourceQuery {
            columns: Some(vec!["CheckNo".to_owned()]),
            limit: Some(1),
            where_: Some(vec![("CheckNo".to_owned(), value.clone())]),
            ..SourceQuery::new(table)
        };
        Ok(!self.cx.source.query(&q, ctx)?.is_empty())
    }

    fn emit(
        &self,
        tables: &[&PlanTable],
        accepted: &[CheckState],
        vanished: &[(String, String, Option<String>)],
        report: &mut CaptureReport,
        ctx: &Ctx,
    ) -> Result<()> {
        let mut per_table: Vec<ChangeSet> = tables.iter().map(|_| ChangeSet::default()).collect();
        let src_ver = self.cx.src_ver.next();
        for check in accepted {
            self.diff_check(tables, check, &mut per_table, src_ver, report, ctx)?;
        }
        for (key, reason, _) in vanished {
            self.tombstone_check(tables, key, reason, &mut per_table, src_ver, ctx)?;
        }

        let active: Vec<usize> = (0..tables.len())
            .filter(|&i| !per_table[i].is_empty())
            .collect();
        let cycle = src_ver.to_string();
        for (n, &i) in active.iter().enumerate() {
            self.cx.flush(
                &CaptureContext::target_of(tables[i], Some(STREAM)),
                &per_table[i],
                Some(&format!("{cycle}:{}/{}", n + 1, active.len())),
                None,
                report,
                ctx,
            )?;
        }

        let mut commit = StateCommit::default();
        let mut upserts = Vec::with_capacity(accepted.len());
        for c in accepted {
            upserts.push(ShadowEntry {
                key: c.key.clone(),
                hash: c.hash.clone(),
                bucket: c.key.clone(),
                ref_: Some(value_codec::encode(&c.value)?),
            });
        }
        commit.shadow.push(ShadowWrite {
            table: OPEN_TABLE.to_owned(),
            upserts,
            delete_keys: vanished.iter().map(|v| v.0.clone()).collect(),
        });
        self.cx.state.commit(&commit, ctx)
    }

    /// Index of the first table named `name` ignoring case (`perTable[name]`).
    fn slot(tables: &[&PlanTable], name: &str) -> usize {
        tables
            .iter()
            .position(|t| eq_ignore_case(t.name(), name))
            .unwrap_or(0)
    }

    fn diff_check(
        &self,
        tables: &[&PlanTable],
        check: &CheckState,
        per_table: &mut [ChangeSet],
        src_ver: i64,
        report: &mut CaptureReport,
        ctx: &Ctx,
    ) -> Result<()> {
        let echo = self.cx.echo.is_echo(&check.key, &check.hash);
        for p in tables {
            let name = p.name();
            let rows = check.rows_of(name);
            let known: Shadow = self
                .cx
                .state
                .get_shadow_entries(name, &check.key, ctx)?
                .into_iter()
                .map(|e| (e.key, e.hash))
                .collect();
            let mut changes = ChangeSet::default();
            let bucket = |_: &CapturedRow| check.key.clone();
            pipeline::diff_rows(rows, &known, &bucket, src_ver, &mut changes);
            let blob_shadow =
                self.cx
                    .state
                    .get_shadow(&pipeline::blob_table(name), Some(&check.key), ctx)?;
            pipeline::diff_blobs(p, rows, &blob_shadow, &bucket, &mut changes);

            let target = &mut per_table[Self::slot(tables, name)];
            if echo {
                report.echoes_suppressed += changes.upserts.len() as i32;
                target.shadow_only.append(&mut changes.upserts);
            } else {
                target.upserts.append(&mut changes.upserts);
            }
            target.blobs.append(&mut changes.blobs);

            let now: HashSet<&str> = rows.iter().map(|r| r.state_key.as_str()).collect();
            for gone in known.keys().filter(|k| !now.contains(k)) {
                target.tombstones.push(TombChange {
                    state_key: gone.to_owned(),
                    tombstone: SyncTombstone {
                        key: CaptureContext::wire_key(gone)?,
                        src_ver,
                        reason: Some("removed".to_owned()),
                    },
                });
            }
        }
        Ok(())
    }

    fn tombstone_check(
        &self,
        tables: &[&PlanTable],
        check_key: &str,
        reason: &str,
        per_table: &mut [ChangeSet],
        src_ver: i64,
        ctx: &Ctx,
    ) -> Result<()> {
        for p in tables {
            let name = p.name();
            for e in self.cx.state.get_shadow_entries(name, check_key, ctx)? {
                per_table[Self::slot(tables, name)]
                    .tombstones
                    .push(TombChange {
                        tombstone: SyncTombstone {
                            key: CaptureContext::wire_key(&e.key)?,
                            src_ver,
                            reason: Some(reason.to_owned()),
                        },
                        state_key: e.key,
                    });
            }
        }
        Ok(())
    }
}
