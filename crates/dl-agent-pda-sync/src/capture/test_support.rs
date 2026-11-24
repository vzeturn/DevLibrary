//! Test fakes (`Support/Fakes.cs`, `Support/Harness.cs`): an in-memory vendor database
//! that evaluates the same [`SourceQuery`] the SQL builder would, a sink that accepts
//! only what the cloud accepts, and a harness wiring an engine over the real catalog.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use super::engine::CaptureEngine;
use super::error::{CaptureError, ErrorKind, Result};
use super::options::CaptureOptions;
use super::sinks::{BatchSink, BlobItem, BlobSink, EchoSuppressor};
use super::source::{CaptureSource, CheckSignature, SourceQuery, VendorRow, value_or_null};
use super::state::{CaptureState, InMemoryCaptureState};
use super::util::{DotNetDict, add_days, contains_ignore_case, invariant_string};
use crate::config::time::{ManualClock, Timestamp};
use crate::contracts::catalog::SyncCatalog;
use crate::contracts::mapping::RawValue;
use crate::contracts::time::{DateTime, TimeZone};
use crate::contracts::wire::{SyncBatch, SyncRow, SyncTombstone, blob_contract};
use crate::util::ctx::Ctx;
use crate::util::stj::eq_ignore_case;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A vendor row from pairs.
pub fn row(values: &[(&str, RawValue)]) -> VendorRow {
    let mut r = VendorRow::new();
    for (k, v) in values {
        r.insert(*k, v.clone());
    }
    r
}

/// A decimal value.
pub fn dec(s: &str) -> RawValue {
    RawValue::Decimal(crate::util::stj::parse_dotnet_decimal(s).unwrap())
}

/// `Harness.Local(daysAgo, hour)`: site-local 2026-10-24 `hour`:00 minus days.
pub fn local(days_ago: i64, hour: u32) -> RawValue {
    RawValue::DateTime(add_days(
        DateTime::new(2026, 10, 24, hour, 0, 0).unwrap(),
        -days_ago,
    ))
}

/// The int value of a column (test predicates).
pub fn int(r: &VendorRow, c: &str) -> Option<i64> {
    match value_or_null(r, c) {
        RawValue::I32(n) => Some(n.into()),
        RawValue::I64(n) => Some(n),
        _ => None,
    }
}

#[derive(Default)]
struct Db {
    tables: Vec<(String, Vec<VendorRow>)>,
    schemas: Vec<(String, Vec<String>)>,
}

impl Db {
    fn table(&mut self, name: &str) -> &mut Vec<VendorRow> {
        let i = match self
            .tables
            .iter()
            .position(|(t, _)| eq_ignore_case(t, name))
        {
            Some(i) => i,
            None => {
                self.tables.push((name.to_owned(), Vec::new()));
                self.tables.len() - 1
            }
        };
        &mut self.tables[i].1
    }
}

/// In-memory stand-in for the vendor database.
#[derive(Default)]
pub struct FakeCaptureSource {
    db: Mutex<Db>,
    /// Every query run.
    pub queries: Mutex<Vec<SourceQuery>>,
    /// Fails every query with a busy error once this many have run.
    pub busy_after: Mutex<Option<usize>>,
    /// Reads of these tables fail with a marked schema error.
    pub schema_broken: Mutex<Vec<String>>,
    /// Set to model a source that cannot compute signatures (another engine).
    pub no_signatures: Mutex<bool>,
    /// Signature reads run.
    pub signature_queries: Mutex<usize>,
    /// Per-table signatures reported instead of the live ones (a checksum collision).
    pub frozen_signatures: Mutex<Vec<(String, DotNetDict<CheckSignature>)>>,
}

fn num(v: &RawValue) -> Option<f64> {
    Some(match v {
        RawValue::Bool(b) => f64::from(u8::from(*b)),
        RawValue::U8(n) => f64::from(*n),
        RawValue::I16(n) => f64::from(*n),
        RawValue::I32(n) => f64::from(*n),
        RawValue::I64(n) => *n as f64,
        RawValue::F64(f) => *f,
        RawValue::Decimal(d) => d.to_string().parse().ok()?,
        _ => return None,
    })
}

fn one(a: &RawValue, b: &RawValue) -> Ordering {
    match (a, b) {
        (RawValue::Null, RawValue::Null) => Ordering::Equal,
        (RawValue::Null, _) => Ordering::Less,
        (_, RawValue::Null) => Ordering::Greater,
        (RawValue::DateTime(x), RawValue::DateTime(y)) => x.ticks().cmp(&y.ticks()),
        (RawValue::String(x), RawValue::String(y)) => x.cmp(y),
        (RawValue::Bytes(x), RawValue::Bytes(y)) => x.cmp(y),
        _ => num(a)
            .zip(num(b))
            .and_then(|(x, y)| x.partial_cmp(&y))
            .unwrap_or(Ordering::Equal),
    }
}

fn cmp_keys(a: &[RawValue], b: &[RawValue]) -> Ordering {
    a.iter()
        .zip(b)
        .map(|(x, y)| one(x, y))
        .find(|o| *o != Ordering::Equal)
        .unwrap_or(Ordering::Equal)
}

fn matches(q: &SourceQuery, r: &VendorRow) -> bool {
    if let Some(w) = &q.where_ {
        for (col, want) in w {
            let have = value_or_null(r, col);
            let ok = if *want == RawValue::Null {
                have == RawValue::Null
            } else {
                have != RawValue::Null && one(&have, want) == Ordering::Equal
            };
            if !ok {
                return false;
            }
        }
    }
    let Some(rc) = &q.range_column else {
        return true;
    };
    let RawValue::DateTime(t) = value_or_null(r, rc) else {
        return q.range_includes_null;
    };
    q.range_from.is_none_or(|f| t.ticks() >= f.ticks())
        && q.range_to.is_none_or(|e| t.ticks() < e.ticks())
}

impl FakeCaptureSource {
    /// Declares a table (empty).
    pub fn table(&self, name: &str) -> &Self {
        lock(&self.db).table(name);
        self
    }

    /// Declares the columns `table` has: an explicit projection naming another column fails.
    pub fn schema(&self, table: &str, columns: &[&str]) -> &Self {
        let mut db = lock(&self.db);
        db.table(table);
        db.schemas.push((
            table.to_owned(),
            columns.iter().map(|c| (*c).to_owned()).collect(),
        ));
        self
    }

    /// Adds rows.
    pub fn add(&self, table: &str, rows: Vec<VendorRow>) -> &Self {
        lock(&self.db).table(table).extend(rows);
        self
    }

    /// The rows of a table.
    pub fn rows(&self, table: &str) -> Vec<VendorRow> {
        lock(&self.db).table(table).clone()
    }

    /// Removes matching rows.
    pub fn remove(&self, table: &str, pred: impl Fn(&VendorRow) -> bool) {
        lock(&self.db).table(table).retain(|r| !pred(r));
    }

    /// Sets a column of matching rows.
    pub fn update(
        &self,
        table: &str,
        pred: impl Fn(&VendorRow) -> bool,
        column: &str,
        value: RawValue,
    ) {
        for r in lock(&self.db).table(table).iter_mut().filter(|r| pred(r)) {
            match r.get_ignore_case(column).map(|(k, _)| k.to_owned()) {
                Some(k) => {
                    if let Some(slot) = r.get_mut(&k) {
                        *slot = value.clone();
                    }
                }
                None => {
                    r.insert(column, value.clone());
                }
            }
        }
    }

    /// The live signatures of `table`: per check, its rows rendered with sorted columns,
    /// sorted and joined (`CheckSignaturesAsync` of the C# fake).
    pub fn live_signatures(&self, table: &str, check_column: &str) -> DotNetDict<CheckSignature> {
        let mut groups: DotNetDict<(RawValue, Vec<String>)> = DotNetDict::new();
        for r in lock(&self.db).table(table).iter() {
            let value = value_or_null(r, check_column);
            let Some(key) = invariant_string(&value) else {
                continue;
            };
            let mut cols: Vec<(String, String)> = r
                .iter()
                .map(|(k, v)| (k.to_uppercase(), format!("{k}={v:?}")))
                .collect();
            cols.sort();
            let line = cols
                .into_iter()
                .map(|(_, c)| c)
                .collect::<Vec<_>>()
                .join(",");
            match groups.get_mut(&key) {
                Some((_, lines)) => lines.push(line),
                None => groups.insert(&key, (value, vec![line])),
            }
        }
        let mut out = DotNetDict::new();
        for (key, (value, lines)) in groups.iter() {
            let mut lines = lines.clone();
            lines.sort();
            out.insert(
                key,
                CheckSignature {
                    value: value.clone(),
                    signature: lines.join("|"),
                },
            );
        }
        out
    }

    /// Signature reads run.
    pub fn signature_query_count(&self) -> usize {
        *lock(&self.signature_queries)
    }

    /// Number of queries run.
    pub fn query_count(&self) -> usize {
        lock(&self.queries).len()
    }

    /// The queries run.
    pub fn queries(&self) -> Vec<SourceQuery> {
        lock(&self.queries).clone()
    }
}

fn schema_fault(table: &str, message: String) -> CaptureError {
    let mut e = CaptureError::invalid_operation(message);
    e.mark_schema_fault(table);
    e
}

impl CaptureSource for FakeCaptureSource {
    fn check_signatures(
        &self,
        table: &str,
        check_column: &str,
        _ctx: &Ctx,
    ) -> Result<Option<DotNetDict<CheckSignature>>> {
        *lock(&self.signature_queries) += 1;
        if *lock(&self.no_signatures) {
            return Ok(None);
        }
        if let Some((_, frozen)) = lock(&self.frozen_signatures)
            .iter()
            .find(|(t, _)| eq_ignore_case(t, table))
        {
            return Ok(Some(frozen.clone()));
        }
        Ok(Some(self.live_signatures(table, check_column)))
    }

    fn table_exists(&self, table: &str, _ctx: &Ctx) -> Result<bool> {
        Ok(lock(&self.db)
            .tables
            .iter()
            .any(|(t, _)| eq_ignore_case(t, table)))
    }

    fn query(&self, q: &SourceQuery, _ctx: &Ctx) -> Result<Vec<VendorRow>> {
        let n = {
            let mut qs = lock(&self.queries);
            qs.push(q.clone());
            qs.len()
        };
        if lock(&self.busy_after).is_some_and(|b| n > b) {
            return Err(CaptureError::busy("busy", None));
        }
        if contains_ignore_case(&lock(&self.schema_broken), &q.table) {
            return Err(schema_fault(
                &q.table,
                "Invalid column name 'Gone'.".to_owned(),
            ));
        }
        let mut db = lock(&self.db);
        if let Some(projection) = &q.columns
            && let Some((_, schema)) = db.schemas.iter().find(|(t, _)| eq_ignore_case(t, &q.table))
            && let Some(absent) = projection.iter().find(|c| !contains_ignore_case(schema, c))
        {
            return Err(schema_fault(
                &q.table,
                format!("Invalid column name '{absent}'."),
            ));
        }
        let key = |r: &VendorRow| -> Vec<RawValue> {
            q.order_by.iter().map(|c| value_or_null(r, c)).collect()
        };
        let mut rows: Vec<VendorRow> = db
            .table(&q.table)
            .iter()
            .filter(|r| matches(q, r))
            .cloned()
            .collect();
        if !q.order_by.is_empty() {
            rows.sort_by(|a, b| {
                let o = cmp_keys(&key(a), &key(b));
                if q.descending { o.reverse() } else { o }
            });
        }
        if let Some(after) = &q.after {
            rows.retain(|r| {
                let c = cmp_keys(&key(r), after);
                if q.descending {
                    c == Ordering::Less
                } else {
                    c == Ordering::Greater
                }
            });
        }
        if let Some(limit) = q.limit {
            rows.truncate(limit.max(0) as usize);
        }
        let exclude = q.exclude_columns.clone().unwrap_or_default();
        Ok(rows
            .into_iter()
            .map(|r| {
                let mut out = VendorRow::new();
                for (k, v) in r.iter() {
                    if !contains_ignore_case(&exclude, k) {
                        out.insert(k, v.clone());
                    }
                }
                out
            })
            .collect())
    }
}

/// A sink that accepts only what the cloud accepts.
#[derive(Default)]
pub struct CollectingSink {
    /// Accepted batches.
    pub batches: Mutex<Vec<SyncBatch>>,
    /// Accepted blobs.
    pub blobs: Mutex<Vec<BlobItem>>,
    /// Fails (simulated crash) once this many batches were accepted.
    pub fail_after_batches: Mutex<Option<usize>>,
}

impl CollectingSink {
    /// The rows sent for a vendor table.
    pub fn rows(&self, table: &str) -> Vec<SyncRow> {
        lock(&self.batches)
            .iter()
            .filter(|b| b.table == table)
            .flat_map(|b| b.rows.clone())
            .collect()
    }

    /// The tombstones sent for a vendor table.
    pub fn tombstones(&self, table: &str) -> Vec<SyncTombstone> {
        lock(&self.batches)
            .iter()
            .filter(|b| b.table == table)
            .flat_map(|b| b.tombstones.clone())
            .collect()
    }

    /// The accepted batches.
    pub fn batches(&self) -> Vec<SyncBatch> {
        lock(&self.batches).clone()
    }

    /// The accepted blobs.
    pub fn blobs(&self) -> Vec<BlobItem> {
        lock(&self.blobs).clone()
    }

    /// Forgets the batches.
    pub fn clear(&self) {
        lock(&self.batches).clear();
    }

    /// Forgets the blobs.
    pub fn clear_blobs(&self) {
        lock(&self.blobs).clear();
    }
}

impl BatchSink for CollectingSink {
    fn send(&self, batch: &SyncBatch, _ctx: &Ctx) -> Result<()> {
        let mut batches = lock(&self.batches);
        if lock(&self.fail_after_batches).is_some_and(|n| batches.len() >= n) {
            return Err(CaptureError::new(ErrorKind::Io, "sink crashed"));
        }
        let ok = catalog()
            .find(&batch.table)
            .is_some_and(|t| t.is_classified() && t.has_pg_target());
        if !ok {
            return Err(CaptureError::invalid_operation(format!(
                "The cloud rejects batch table '{}'.",
                batch.table
            )));
        }
        batches.push(batch.clone());
        Ok(())
    }
}

impl BlobSink for CollectingSink {
    fn send(&self, blob: &BlobItem, _ctx: &Ctx) -> Result<()> {
        if let Some(reason) = blob_contract::check(Some(&blob.ref_), catalog()) {
            return Err(CaptureError::invalid_operation(format!(
                "The cloud rejects blob ref: {reason}."
            )));
        }
        lock(&self.blobs).push(blob.clone());
        Ok(())
    }
}

/// Echo suppressor of a set of aggregates.
#[derive(Default)]
pub struct FakeEchoSuppressor {
    /// Aggregates whose state is an echo.
    pub aggregates: Mutex<HashSet<String>>,
}

impl EchoSuppressor for FakeEchoSuppressor {
    fn is_echo(&self, aggregate_key: &str, _state_hash: &str) -> bool {
        lock(&self.aggregates).contains(aggregate_key)
    }
}

/// The embedded catalog.
pub fn catalog() -> &'static SyncCatalog {
    SyncCatalog::load().unwrap()
}

/// `CaptureOptionsBuilder` defaults.
pub fn opts() -> CaptureOptions {
    let zone = TimeZone::find_system_time_zone_by_id("Asia/Ho_Chi_Minh")
        .unwrap_or_else(|| TimeZone::fixed("SE Asia Standard Time", 420));
    let mut o = CaptureOptions::new("agent-test", Arc::new(zone));
    o.page_rows = 3;
    o.snapshot_chunk_rows = 2;
    o.rescan_days = 3;
    o.vanish_confirm_polls = 2;
    o.stable_read_retries = 3;
    o.tail_rescan_rows = 10;
    o.max_rows_per_batch = 2000;
    o.include_backfill = true;
    o
}

/// An engine over the real catalog with fakes; reusable to simulate an agent restart.
pub struct Harness {
    /// Vendor database.
    pub source: Arc<FakeCaptureSource>,
    /// Sink.
    pub sink: Arc<CollectingSink>,
    /// State.
    pub state: Arc<dyn CaptureState>,
    /// Clock at 2026-10-24T05:00Z (12:00 site time).
    pub clock: Arc<ManualClock>,
    /// Echo suppressor.
    pub echo: Arc<FakeEchoSuppressor>,
    /// Options.
    pub options: CaptureOptions,
}

impl Harness {
    /// A fresh harness.
    pub fn new() -> Harness {
        let now = crate::contracts::time::DateTimeOffset::utc(2026, 10, 24, 5, 0, 0).unwrap();
        Harness {
            source: Arc::default(),
            sink: Arc::default(),
            state: Arc::new(InMemoryCaptureState::new()),
            clock: Arc::new(ManualClock::new(Timestamp::from_unix_nanos(
                now.unix_nanos(),
            ))),
            echo: Arc::default(),
            options: opts(),
        }
    }

    /// A new engine over the harness (same state).
    pub fn engine(&self) -> CaptureEngine {
        CaptureEngine::new(
            self.options.clone(),
            catalog(),
            self.source.clone(),
            self.state.clone(),
            self.sink.clone(),
            Some(self.sink.clone()),
            Some(self.echo.clone()),
            Some(self.clock.clone()),
        )
        .unwrap()
    }
}

/// A background context.
pub fn bg() -> Ctx {
    Ctx::background()
}
