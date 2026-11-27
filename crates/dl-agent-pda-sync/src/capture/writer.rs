//! Turns a [`ChangeSet`] into wire batches (`Strategies/BatchWriter.cs`): size-capped
//! batches, each handed to the sink and only then committed (sequence number, shadow
//! hashes, cursors) in one state transaction. A crash in between re-detects the same
//! changes next cycle; the cloud's seq/row guards ack the duplicates.

use std::collections::{HashMap, HashSet};

use super::error::Result;
use super::options::CaptureOptions;
use super::pipeline::{BlobChange, ChangeSet, RowChange, TombChange, blob_table};
use super::report::CaptureReport;
use super::sinks::{BatchSink, BlobSink};
use super::state::{CaptureState, ShadowEntry, ShadowWrite, StateCommit};
use crate::contracts::map::{OrderedMap, StrMap};
use crate::contracts::utf16_len;
use crate::contracts::wire::{BatchHops, SyncBatch, SyncMode, wire_version};
use crate::util::ctx::Ctx;
use crate::util::hop_clock;

/// Where a flush goes: the stream and the vendor table (the catalog key and the batch's
/// wire table name; never the `pg_target`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteTarget {
    /// Stream.
    pub stream: String,
    /// Vendor table.
    pub vendor_table: String,
}

/// Cursor updates of a flush (`None` deletes), in order.
pub type Cursors = OrderedMap<Option<String>>;

/// The batch writer.
pub struct BatchWriter<'a> {
    /// Options (agent id, caps).
    pub options: &'a CaptureOptions,
    /// Batch sink.
    pub sink: &'a dyn BatchSink,
    /// Blob sink (none: blobs are counted as skipped).
    pub blob_sink: Option<&'a dyn BlobSink>,
    /// State.
    pub state: &'a dyn CaptureState,
}

fn entry(r: &RowChange) -> ShadowEntry {
    ShadowEntry {
        key: r.state_key.clone(),
        hash: r.row.hash.clone(),
        bucket: r.bucket.clone(),
        ref_: r.ref_.clone(),
    }
}

fn add_blob_shadow(commit: &mut StateCommit, target: &WriteTarget, sent: &[BlobChange]) {
    if sent.is_empty() {
        return;
    }
    commit.shadow.push(ShadowWrite {
        table: blob_table(&target.vendor_table),
        upserts: sent
            .iter()
            .map(|b| ShadowEntry::new(&b.blob_key, b.item.sha256(), &b.bucket, None))
            .collect(),
        delete_keys: Vec::new(),
    });
}

fn add_cursors(commit: &mut StateCommit, cursors: Option<&Cursors>) {
    if let Some(c) = cursors {
        for (k, v) in c.iter() {
            commit.cursors.insert(k, v.clone());
        }
    }
}

fn size(map: &StrMap) -> i64 {
    map.iter()
        .map(|(k, v)| (utf16_len(k) + v.as_deref().map_or(0, utf16_len) + 8) as i64)
        .sum()
}

impl BatchWriter<'_> {
    /// Sends `changes` to `target` and commits the matching state.
    #[allow(clippy::too_many_arguments)]
    pub fn flush(
        &self,
        target: &WriteTarget,
        changes: &ChangeSet,
        mode: SyncMode,
        watermark: Option<&str>,
        cursors: Option<&Cursors>,
        report: &mut CaptureReport,
        ctx: &Ctx,
    ) -> Result<()> {
        // RT-1318: the rows are read and mapped once they reach the writer.
        let t_read = hop_clock::now_field();
        let chunks = self.split(changes);
        let mut blobs_by_row: HashMap<&str, Vec<&BlobChange>> = HashMap::new();
        for b in &changes.blobs {
            blobs_by_row.entry(b.row_key.as_str()).or_default().push(b);
        }
        let mut sent_blob_rows: HashSet<&str> = HashSet::new();

        for (i, (rows, tombs)) in chunks.iter().enumerate() {
            let chunk_blobs: Vec<BlobChange> = rows
                .iter()
                .filter_map(|r| blobs_by_row.get(r.state_key.as_str()))
                .flatten()
                .map(|b| (*b).clone())
                .collect();
            for r in rows {
                sent_blob_rows.insert(r.state_key.as_str());
            }
            let sent_blobs = self.send_blobs(chunk_blobs, report, ctx)?;

            let seq = self.state.peek_next_seq(&target.stream, ctx)?;
            let batch = SyncBatch {
                agent_id: self.options.agent_id.clone(),
                stream: target.stream.clone(),
                seq,
                mapper_ver: wire_version::MAPPER,
                table: target.vendor_table.clone(),
                mode,
                rows: rows.iter().map(|r| r.row.clone()).collect(),
                tombstones: tombs.iter().map(|t| t.tombstone.clone()).collect(),
                watermark: watermark.map(str::to_owned),
                hops: BatchHops {
                    t_detected: hop_clock::pass_started(),
                    t_read,
                    t_sent: None,
                },
            };
            self.sink.send(&batch, ctx)?;
            report.batches += 1;
            report.rows_sent += rows.len() as i32;
            report.tombstones_sent += tombs.len() as i32;

            let mut commit = StateCommit {
                stream: Some(target.stream.clone()),
                seq: Some(seq),
                ..StateCommit::default()
            };
            commit.shadow.push(ShadowWrite {
                table: target.vendor_table.clone(),
                upserts: rows.iter().map(|r| entry(r)).collect(),
                delete_keys: tombs.iter().map(|t| t.state_key.clone()).collect(),
            });
            add_blob_shadow(&mut commit, target, &sent_blobs);
            if i == chunks.len() - 1
                && changes
                    .blobs
                    .iter()
                    .all(|b| sent_blob_rows.contains(b.row_key.as_str()))
            {
                add_cursors(&mut commit, cursors);
            }
            self.state.commit(&commit, ctx)?;
        }

        let leftover: Vec<BlobChange> = changes
            .blobs
            .iter()
            .filter(|b| !sent_blob_rows.contains(b.row_key.as_str()))
            .cloned()
            .collect();
        if !leftover.is_empty() || chunks.is_empty() || !changes.shadow_only.is_empty() {
            let sent = self.send_blobs(leftover, report, ctx)?;
            let mut commit = StateCommit::default();
            if !changes.shadow_only.is_empty() {
                commit.shadow.push(ShadowWrite {
                    table: target.vendor_table.clone(),
                    upserts: changes.shadow_only.iter().map(entry).collect(),
                    delete_keys: Vec::new(),
                });
            }
            add_blob_shadow(&mut commit, target, &sent);
            add_cursors(&mut commit, cursors);
            if !commit.shadow.is_empty() || !commit.cursors.is_empty() {
                self.state.commit(&commit, ctx)?;
            }
        }
        Ok(())
    }

    fn send_blobs(
        &self,
        blobs: Vec<BlobChange>,
        report: &mut CaptureReport,
        ctx: &Ctx,
    ) -> Result<Vec<BlobChange>> {
        if blobs.is_empty() {
            return Ok(blobs);
        }
        let Some(sink) = self.blob_sink else {
            report.blobs_skipped_no_sink += blobs.len() as i32;
            return Ok(Vec::new());
        };
        for b in &blobs {
            sink.send(&b.item, ctx)?;
            report.blobs_sent += 1;
        }
        Ok(blobs)
    }

    #[allow(clippy::type_complexity)]
    fn split<'c>(&self, changes: &'c ChangeSet) -> Vec<(Vec<&'c RowChange>, Vec<&'c TombChange>)> {
        let max_rows = self.options.max_rows_per_batch.max(0) as usize;
        let max_bytes = i64::from(self.options.max_batch_bytes);
        let mut chunks = Vec::new();
        let mut rows: Vec<&RowChange> = Vec::new();
        let mut tombs: Vec<&TombChange> = Vec::new();
        let mut bytes: i64 = 0;
        let full = |rows: &Vec<&RowChange>, tombs: &Vec<&TombChange>, bytes: i64, add: i64| {
            rows.len() + tombs.len() >= max_rows || (bytes > 0 && bytes + add > max_bytes)
        };
        macro_rules! cut {
            () => {
                if rows.len() + tombs.len() > 0 {
                    chunks.push((std::mem::take(&mut rows), std::mem::take(&mut tombs)));
                    bytes = 0;
                }
            };
        }
        for u in &changes.upserts {
            let s = size(&u.row.key) + size(&u.row.columns) + 96;
            if full(&rows, &tombs, bytes, s) {
                cut!();
            }
            rows.push(u);
            bytes += s;
        }
        for t in &changes.tombstones {
            let s = size(&t.tombstone.key) + 64;
            if full(&rows, &tombs, bytes, s) {
                cut!();
            }
            tombs.push(t);
            bytes += s;
        }
        cut!();
        let _ = bytes;
        chunks
    }
}
