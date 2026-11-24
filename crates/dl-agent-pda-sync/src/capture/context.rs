//! Dependencies and helpers shared by the strategies and the snapshot runner
//! (`Strategies/CaptureContext.cs`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use super::codecs::{key_codec, value_codec};
use super::error::Result;
use super::open_check::SignatureMemory;
use super::options::CaptureOptions;
use super::pipeline::{self, CapturedRow, ChangeSet};
use super::plan::PlanTable;
use super::report::{CaptureReport, TableFault};
use super::sinks::{BatchSink, BlobSink, EchoSuppressor};
use super::source::{CaptureSource, SourceQuery, VendorRow, date_value, value_or_null};
use super::src_ver::SrcVerClock;
use super::state::CaptureState;
use super::util::{day_key, invariant_string, site_now};
use super::writer::{BatchWriter, Cursors, WriteTarget};
use crate::config::time::SharedClock;
use crate::contracts::map::StrMap;
use crate::contracts::mapping::{CatalogRowMapper, RawValue};
use crate::contracts::time::DateTime;
use crate::contracts::wire::SyncMode;
use crate::util::ctx::Ctx;

/// Dependencies shared by the strategies.
pub struct CaptureContext {
    /// Options.
    pub options: CaptureOptions,
    /// Vendor database.
    pub source: Arc<dyn CaptureSource>,
    /// Agent state.
    pub state: Arc<dyn CaptureState>,
    /// Batch sink.
    pub sink: Arc<dyn BatchSink>,
    /// Blob sink.
    pub blob_sink: Option<Arc<dyn BlobSink>>,
    /// Row mapper.
    pub mapper: CatalogRowMapper,
    /// Echo suppression.
    pub echo: Arc<dyn EchoSuppressor>,
    /// Clock.
    pub clock: SharedClock,
    /// `src_ver` source.
    pub src_ver: SrcVerClock,
    /// Table existence cache (upper-cased name).
    exists: Mutex<HashMap<String, bool>>,
    /// What the open-check strategy remembers between cycles (ADR 0153); shared by the
    /// engine's cycles and the snapshot, like the one C# `OpenCheckCapture` instance.
    pub open_signatures: Mutex<SignatureMemory>,
}

impl std::fmt::Debug for CaptureContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureContext").finish_non_exhaustive()
    }
}

impl CaptureContext {
    /// A context over the given dependencies.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        options: CaptureOptions,
        source: Arc<dyn CaptureSource>,
        state: Arc<dyn CaptureState>,
        sink: Arc<dyn BatchSink>,
        blob_sink: Option<Arc<dyn BlobSink>>,
        mapper: CatalogRowMapper,
        echo: Arc<dyn EchoSuppressor>,
        clock: SharedClock,
    ) -> CaptureContext {
        CaptureContext {
            src_ver: SrcVerClock::new(clock.clone()),
            options,
            source,
            state,
            sink,
            blob_sink,
            mapper,
            echo,
            clock,
            exists: Mutex::new(HashMap::new()),
            open_signatures: Mutex::new(SignatureMemory::default()),
        }
    }

    /// Whether the table exists (cached per context, ignoring case).
    pub fn exists(&self, table: &str, ctx: &Ctx) -> Result<bool> {
        let key = table.to_uppercase();
        if let Some(&known) = self
            .exists
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
        {
            return Ok(known);
        }
        let exists = self.source.table_exists(table, ctx)?;
        self.exists
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, exists);
        Ok(exists)
    }

    /// Runs one table's work; a table schema failure is recorded on the report and
    /// `false` returned so the caller goes on with the next table. Every other failure
    /// propagates.
    pub fn guard_table(
        p: &PlanTable,
        report: &mut CaptureReport,
        ctx: &Ctx,
        body: impl FnOnce(&mut CaptureReport) -> Result<()>,
    ) -> Result<bool> {
        match body(report) {
            Ok(()) => Ok(true),
            Err(e) if !ctx.is_done() && e.is_schema_fault() => {
                report.table_faults.push(TableFault {
                    table: e.table_of().unwrap_or(p.name()).to_owned(),
                    error: e,
                });
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    /// The write target of a table (its own stream unless given).
    pub fn target_of(p: &PlanTable, stream: Option<&str>) -> WriteTarget {
        WriteTarget {
            stream: stream.unwrap_or(p.name()).to_owned(),
            vendor_table: p.name().to_owned(),
        }
    }

    /// The base query of a table.
    pub fn query(p: &PlanTable) -> SourceQuery {
        SourceQuery {
            exclude_columns: (!p.exclude.is_empty()).then(|| p.exclude.clone()),
            bytes_columns: (!p.bytes.is_empty()).then(|| p.bytes.clone()),
            order_by: p.order_columns.clone(),
            ..SourceQuery::new(p.name())
        }
    }

    /// The site's wall clock now.
    pub fn site_now(&self) -> DateTime {
        site_now(&*self.clock, &self.options.site_time_zone)
    }

    /// The day bucket of a row (its day column), if any.
    pub fn day_of(p: &PlanTable, raw: &VendorRow) -> Option<String> {
        let col = p.day_column.as_deref()?;
        date_value(raw, col).map(day_key)
    }

    /// Typed keyset position of a row over the table's order columns.
    pub fn ref_of(p: &PlanTable, raw: &VendorRow) -> Result<String> {
        let values: Vec<RawValue> = p
            .order_columns
            .iter()
            .map(|c| value_or_null(raw, c))
            .collect();
        value_codec::encode_key(&values)
    }

    /// The keyset position of a cursor (`None` for no cursor or an empty position).
    pub fn after_of(cursor: Option<&str>) -> Result<Option<Vec<RawValue>>> {
        Ok(value_codec::decode_key(cursor)?.filter(|k| !k.is_empty()))
    }

    /// Maps rows, stamps their refs, and diffs them against the shadow; echoes are
    /// recorded silently.
    pub fn diff_page(
        &self,
        p: &PlanTable,
        raw: Vec<VendorRow>,
        bucket: &dyn Fn(&CapturedRow) -> String,
        report: &mut CaptureReport,
        ctx: &Ctx,
    ) -> Result<(Vec<CapturedRow>, ChangeSet)> {
        let mut rows = pipeline::capture(&self.mapper, p, raw, report)?;
        for r in &mut rows {
            r.ref_ = Some(Self::ref_of(p, &r.raw)?);
        }
        let keys: Vec<String> = rows.iter().map(|r| r.state_key.clone()).collect();
        let shadow = self.state.get_shadow_by_keys(p.name(), &keys, ctx)?;
        let mut changes = ChangeSet::default();
        pipeline::diff_rows(&rows, &shadow, bucket, self.src_ver.next(), &mut changes);
        let blob_keys: Vec<String> = rows
            .iter()
            .flat_map(|r| {
                r.mapped
                    .blob_hashes
                    .keys()
                    .map(|c| pipeline::blob_key_of(&r.state_key, c))
                    .collect::<Vec<_>>()
            })
            .collect();
        let blob_shadow =
            self.state
                .get_shadow_by_keys(&pipeline::blob_table(p.name()), &blob_keys, ctx)?;
        pipeline::diff_blobs(p, &rows, &blob_shadow, bucket, &mut changes);
        self.suppress_echoes(p, &rows, &mut changes, report);
        Ok((rows, changes))
    }

    /// Moves upserts whose aggregate is an echo to the shadow-only list.
    pub fn suppress_echoes(
        &self,
        p: &PlanTable,
        rows: &[CapturedRow],
        changes: &mut ChangeSet,
        report: &mut CaptureReport,
    ) {
        if changes.upserts.is_empty() {
            return;
        }
        // ToDictionary: state keys are unique after occurrence assignment.
        let by_key: HashMap<&str, &CapturedRow> =
            rows.iter().map(|r| (r.state_key.as_str(), r)).collect();
        let mut kept = Vec::new();
        for u in std::mem::take(&mut changes.upserts) {
            let echo = by_key
                .get(u.state_key.as_str())
                .is_some_and(|r| self.echo.is_echo(&aggregate_of(p, r), &u.row.hash));
            if echo {
                changes.shadow_only.push(u);
                report.echoes_suppressed += 1;
            } else {
                kept.push(u);
            }
        }
        changes.upserts = kept;
    }

    /// The batch writer over this context.
    pub fn writer(&self) -> BatchWriter<'_> {
        BatchWriter {
            options: &self.options,
            sink: &*self.sink,
            blob_sink: self.blob_sink.as_deref(),
            state: &*self.state,
        }
    }

    /// Flushes a delta.
    pub fn flush(
        &self,
        t: &WriteTarget,
        c: &ChangeSet,
        watermark: Option<&str>,
        cursors: Option<&Cursors>,
        report: &mut CaptureReport,
        ctx: &Ctx,
    ) -> Result<()> {
        self.writer()
            .flush(t, c, SyncMode::Delta, watermark, cursors, report, ctx)
    }

    /// Wire key of a shadow entry (shadow keys are encoded wire keys with an optional
    /// `#n` suffix).
    pub fn wire_key(state_key: &str) -> Result<StrMap> {
        key_codec::decode(state_key)
    }
}

fn aggregate_of(p: &PlanTable, r: &CapturedRow) -> String {
    let col = p.check_column.as_deref().unwrap_or("CheckNo");
    invariant_string(&value_or_null(&r.raw, col)).unwrap_or_else(|| r.base_key.clone())
}
