//! `capture_dry_run`: one capture pass against `SMILE_POS` as the agent would make it on a
//! fresh enrol (the initial load, then one open-check cycle) into memory. Nothing is
//! queued, uploaded or written to `capture.db`: the state is an empty in-memory one and
//! the sinks only count. The answer has, per stream, the batches, rows and tombstones and
//! a SHA-256 over the canonical row hashes in send order, so two runs (or the cloud's
//! `sync.row_state`) can be compared without moving the rows.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use dl_hash::{Digest, Sha256};
use dl_json::{Map, Value};

use crate::capture::runner::{allowlisted, options_for, rule_counts};
use crate::capture::state::InMemoryCaptureState;
use crate::capture::{
    BatchSink, BlobItem, BlobSink, CaptureEngine, CaptureReport, CaptureSource, CaptureTimings,
    Result,
};
use crate::config::RuntimeConfig;
use crate::contracts::catalog::SyncCatalog;
use crate::contracts::time::TimeZone;
use crate::contracts::wire::SyncBatch;
use crate::util::ctx::Ctx;

#[derive(Default)]
struct Tally {
    batches: u64,
    rows: u64,
    tombstones: u64,
    sha: Sha256,
}

/// Counts and hashes batches instead of queueing them.
#[derive(Default)]
pub struct CountingSink {
    streams: Mutex<BTreeMap<String, Tally>>,
}

impl BatchSink for CountingSink {
    fn send(&self, batch: &SyncBatch, _ctx: &Ctx) -> Result<()> {
        let mut streams = self.streams.lock().unwrap_or_else(PoisonError::into_inner);
        let t = streams.entry(batch.stream.clone()).or_default();
        t.batches += 1;
        t.rows += batch.rows.len() as u64;
        t.tombstones += batch.tombstones.len() as u64;
        for row in &batch.rows {
            t.sha.update(row.hash.as_bytes());
            t.sha.update(b"\n");
        }
        Ok(())
    }
}

impl CountingSink {
    /// `{stream: {batches, rows, tombstones, sha256}}`.
    pub fn summary(&self) -> Map {
        let streams = self.streams.lock().unwrap_or_else(PoisonError::into_inner);
        let mut out = Map::new();
        for (name, t) in streams.iter() {
            let mut s = Map::new();
            s.insert("batches", t.batches);
            s.insert("rows", t.rows);
            s.insert("tombstones", t.tombstones);
            s.insert("sha256", dl_encoding::hex::encode(t.sha.clone().finalize()));
            out.insert(name.clone(), Value::Object(s));
        }
        out
    }
}

/// Counts blobs instead of queueing them.
#[derive(Default)]
pub struct BlobCounter(Mutex<(u64, u64)>);

impl BlobSink for BlobCounter {
    fn send(&self, blob: &BlobItem, _ctx: &Ctx) -> Result<()> {
        let mut c = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        c.0 += 1;
        c.1 += blob.content.len() as u64;
        Ok(())
    }
}

fn report_json(r: &CaptureReport) -> Map {
    let mut m = Map::new();
    m.insert("batches", r.batches);
    m.insert("rows_sent", r.rows_sent);
    m.insert("tombstones_sent", r.tombstones_sent);
    m.insert("blobs_sent", r.blobs_sent);
    m.insert("duplicate_keys", r.duplicate_keys);
    m.insert("unstable_checks_deferred", r.unstable_checks_deferred);
    m.insert("deadline_hit", r.deadline_hit);
    m.insert("rejected", r.rejected.len() as u64);
    if !r.rejected.is_empty() {
        m.insert("rejected_rules", rule_counts(&r.rejected));
    }
    m.insert(
        "missing_tables",
        Value::Array(r.missing_tables.iter().cloned().map(Value::from).collect()),
    );
    // The subset absent by design (optional_on_site, ADR 0184): not a gap of the site.
    m.insert(
        "optional_missing_tables",
        Value::Array(
            r.optional_missing_tables
                .iter()
                .cloned()
                .map(Value::from)
                .collect(),
        ),
    );
    m
}

/// Runs the dry pass over `source` with the cloud-pushed `runtime` config (history
/// window, allowlist) and the site time zone `zone`.
///
/// # Errors
/// The capture error of the pass (SQL unreachable, a table that does not read, ...).
pub fn run(
    source: Arc<dyn CaptureSource>,
    zone: Arc<TimeZone>,
    agent_id: &str,
    runtime: &RuntimeConfig,
    ctx: &Ctx,
) -> Result<Map> {
    let catalog = SyncCatalog::load().map_err(|e| {
        crate::capture::CaptureError::invalid_operation(format!("sync catalog: {e}"))
    })?;
    let catalog = allowlisted(catalog, runtime.table_allowlist.as_deref());
    let options = options_for(runtime, zone, agent_id, &CaptureTimings::default());
    let sink = Arc::new(CountingSink::default());
    let blobs = Arc::new(BlobCounter::default());
    let engine = CaptureEngine::new(
        options,
        &catalog,
        source,
        Arc::new(InMemoryCaptureState::new()),
        sink.clone(),
        Some(blobs.clone()),
        None,
        None,
    )?;
    let snapshot = engine.run_initial_snapshot(ctx)?;
    let open = engine.run_open_checks(ctx)?;
    let mut out = Map::new();
    out.insert("snapshot_complete", snapshot.complete);
    out.insert("snapshot", Value::Object(report_json(&snapshot.report)));
    out.insert("open_checks", Value::Object(report_json(&open)));
    out.insert("streams", Value::Object(sink.summary()));
    let (count, bytes) = *blobs.0.lock().unwrap_or_else(PoisonError::into_inner);
    let mut b = Map::new();
    b.insert("count", count);
    b.insert("bytes", bytes);
    out.insert("blobs", Value::Object(b));
    out.insert("uploaded", false);
    Ok(out)
}
