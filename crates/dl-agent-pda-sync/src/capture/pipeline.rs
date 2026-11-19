//! Mapping vendor rows and diffing them against the shadow (`Strategies/RowPipeline.cs`).

use std::collections::{HashMap, HashSet};

use super::codecs::key_codec;
use super::error::{CaptureError, ErrorKind, Result};
use super::plan::PlanTable;
use super::report::{CaptureReport, RejectedRow};
use super::sinks::BlobItem;
use super::source::{VendorRow, column};
use super::state::Shadow;
use crate::contracts::hashing::ordinal_cmp;
use crate::contracts::mapping::{CatalogRowMapper, MappedRow, MappingError, RawValue};
use crate::contracts::wire::{SyncBlobRef, SyncRow, SyncTombstone};

/// A vendor row after mapping, with the identity the shadow uses.
#[derive(Debug, Clone)]
pub struct CapturedRow {
    /// The raw vendor row.
    pub raw: VendorRow,
    /// The mapped row.
    pub mapped: MappedRow,
    /// Encoded wire key.
    pub base_key: String,
    /// Shadow identity: the wire key, plus an occurrence suffix when several rows share it.
    pub state_key: String,
    /// Typed vendor key (keyset encoding) kept in the shadow so the row can be re-read directly.
    pub ref_: Option<String>,
}

impl CapturedRow {
    /// The row hash.
    pub fn hash(&self) -> &str {
        &self.mapped.hash
    }
}

/// An upsert to send (or record).
#[derive(Debug, Clone, PartialEq)]
pub struct RowChange {
    /// Shadow key.
    pub state_key: String,
    /// Bucket.
    pub bucket: String,
    /// Wire row.
    pub row: SyncRow,
    /// Typed vendor key.
    pub ref_: Option<String>,
}

/// A tombstone to send.
#[derive(Debug, Clone, PartialEq)]
pub struct TombChange {
    /// Shadow key.
    pub state_key: String,
    /// Wire tombstone.
    pub tombstone: SyncTombstone,
}

/// A blob to queue.
#[derive(Debug, Clone, PartialEq)]
pub struct BlobChange {
    /// Shadow key of the owning row.
    pub row_key: String,
    /// Blob shadow key.
    pub blob_key: String,
    /// Bucket.
    pub bucket: String,
    /// The blob.
    pub item: BlobItem,
}

/// What one pass found for one target.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChangeSet {
    /// Upserts.
    pub upserts: Vec<RowChange>,
    /// Tombstones.
    pub tombstones: Vec<TombChange>,
    /// Blobs.
    pub blobs: Vec<BlobChange>,
    /// Rows recorded in the shadow without being sent (echoes of agent-applied commands,
    /// silent re-bucketing).
    pub shadow_only: Vec<RowChange>,
}

impl ChangeSet {
    /// Nothing to do.
    pub fn is_empty(&self) -> bool {
        self.upserts.is_empty()
            && self.tombstones.is_empty()
            && self.blobs.is_empty()
            && self.shadow_only.is_empty()
    }
}

/// The failed mapper rule in schema terms (column names only, never a value).
pub fn rule_of(e: &MappingError) -> String {
    match e {
        MappingError::MissingKey { column, .. } => format!("missing_key:{column}"),
        MappingError::ValueFormat { column, .. } => format!("format:{column}"),
        _ => "format".to_owned(),
    }
}

/// Maps vendor rows with the shared mapper and assigns stable shadow keys. Rows the
/// mapper rejects (missing key, bad value) are reported; other mapping errors fail.
pub fn capture(
    mapper: &CatalogRowMapper,
    plan: &PlanTable,
    rows: Vec<VendorRow>,
    report: &mut CaptureReport,
) -> Result<Vec<CapturedRow>> {
    let mut captured = Vec::new();
    for raw in rows {
        match mapper.map(plan.name(), &raw) {
            Ok(mapped) => {
                let base_key = key_codec::encode(&mapped.key);
                captured.push(CapturedRow {
                    raw,
                    mapped,
                    state_key: base_key.clone(),
                    base_key,
                    ref_: None,
                });
            }
            Err(e @ (MappingError::MissingKey { .. } | MappingError::ValueFormat { .. })) => {
                report.rejected.push(RejectedRow {
                    table: plan.name().to_owned(),
                    reason: e.to_string(),
                    rule: rule_of(&e),
                });
            }
            Err(e) => return Err(CaptureError::new(ErrorKind::Other, e.to_string())),
        }
    }
    drop_shadowed_defaults(&mut captured, report);
    assign_occurrences(&mut captured, report);
    Ok(captured)
}

/// ADR 0106: an explicit row wins over a row whose key took a `null_key_defaults` constant.
fn drop_shadowed_defaults(rows: &mut Vec<CapturedRow>, report: &mut CaptureReport) {
    if !rows.iter().any(|r| !r.mapped.defaulted_keys.is_empty()) {
        return;
    }
    let explicit: HashSet<String> = rows
        .iter()
        .filter(|r| r.mapped.defaulted_keys.is_empty())
        .map(|r| r.base_key.clone())
        .collect();
    let before = rows.len();
    rows.retain(|r| r.mapped.defaulted_keys.is_empty() || !explicit.contains(&r.base_key));
    report.null_keys_shadowed += (before - rows.len()) as i32;
}

/// Rows sharing a key get ordered `#n` suffixes (by hash, ordinal).
fn assign_occurrences(rows: &mut [CapturedRow], report: &mut CaptureReport) {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        match index.get(&r.base_key) {
            Some(&g) => groups[g].1.push(i),
            None => {
                index.insert(r.base_key.clone(), groups.len());
                groups.push((r.base_key.clone(), vec![i]));
            }
        }
    }
    for (_, mut members) in groups.into_iter().filter(|g| g.1.len() > 1) {
        // OrderBy is stable.
        members.sort_by(|&a, &b| ordinal_cmp(rows[a].hash(), rows[b].hash()));
        for (n, &i) in members.iter().enumerate() {
            if n > 0 {
                rows[i].state_key = format!("{}#{n}", rows[i].base_key);
            }
        }
        report.duplicate_keys += members.len() as i32 - 1;
    }
}

/// Upserts for rows whose hash differs from the shadow (new or changed).
pub fn diff_rows(
    rows: &[CapturedRow],
    shadow: &Shadow,
    bucket: &dyn Fn(&CapturedRow) -> String,
    src_ver: i64,
    into: &mut ChangeSet,
) {
    for r in rows {
        if shadow.get(&r.state_key) == Some(r.hash()) {
            continue;
        }
        into.upserts.push(RowChange {
            state_key: r.state_key.clone(),
            bucket: bucket(r),
            row: r.mapped.to_sync_row(src_ver),
            ref_: r.ref_.clone(),
        });
    }
}

/// Image/blob columns travel by content hash; a changed hash queues the bytes.
pub fn diff_blobs(
    plan: &PlanTable,
    rows: &[CapturedRow],
    blob_shadow: &Shadow,
    bucket: &dyn Fn(&CapturedRow) -> String,
    into: &mut ChangeSet,
) {
    for r in rows {
        for (col, sha) in r.mapped.blob_hashes.iter() {
            let blob_key = blob_key_of(&r.state_key, col);
            if blob_shadow.get(&blob_key) == Some(sha.as_str()) {
                continue;
            }
            let Some(RawValue::Bytes(content)) = column(&r.raw, col) else {
                continue;
            };
            let address = SyncBlobRef {
                table: plan.table.table.clone(),
                key: r.mapped.key.clone(),
                column: col.to_owned(),
                sha256: sha.clone(),
            };
            into.blobs.push(BlobChange {
                row_key: r.state_key.clone(),
                blob_key,
                bucket: bucket(r),
                item: BlobItem {
                    ref_: address,
                    content: content.clone(),
                },
            });
        }
    }
}

/// Blob shadow key of a row's column.
pub fn blob_key_of(state_key: &str, column: &str) -> String {
    format!("{state_key}|{column}")
}

/// Blob shadow table of a vendor table.
pub fn blob_table(vendor_table: &str) -> String {
    format!("{vendor_table}$blob")
}
