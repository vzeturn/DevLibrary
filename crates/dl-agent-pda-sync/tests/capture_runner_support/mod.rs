//! Fakes of `tests/MM.PdaOrder.SyncAgent.Tests/Capture/CaptureTestKit.cs`.
#![allow(dead_code)]

use std::cmp::Ordering;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dl_agent_pda_sync::capture::source::value_or_null;
use dl_agent_pda_sync::capture::{
    BatchStore, BlobStore, CaptureError, CaptureRunnerDependencies, CaptureSource, CaptureState,
    CaptureTimings, InMemoryCaptureState, PollBudget, Result, RuntimeConfigSource, SourceQuery,
    SqlCaptureRunner, StateFactory, VendorRow, ZoneFactory,
};
use dl_agent_pda_sync::config::{RuntimeConfig, SystemClock};
use dl_agent_pda_sync::contracts::catalog::SyncCatalog;
use dl_agent_pda_sync::contracts::mapping::RawValue;
use dl_agent_pda_sync::contracts::time::{
    DateTime, DateTimeKind, DateTimeOffset, TICKS_PER_DAY, TICKS_PER_MINUTE, TimeZone,
};
use dl_agent_pda_sync::contracts::wire::{SyncBatch, SyncBlobRef, SyncRow};
use dl_agent_pda_sync::outbox::{BlobEnqueueResult, EnqueueResult};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::transport::RetryPolicy;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj::eq_ignore_case;

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Thread-safe in-memory SMILE_POS (`FakeVendorSource`). `fail` makes every call fail.
#[derive(Default)]
pub struct FakeVendorSource {
    tables: Mutex<Vec<(String, Vec<VendorRow>)>>,
    pub fail: Mutex<Option<CaptureError>>,
    queries: Mutex<usize>,
}

fn num(v: &RawValue) -> Option<f64> {
    Some(match v {
        RawValue::I32(n) => f64::from(*n),
        RawValue::I64(n) => *n as f64,
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

impl FakeVendorSource {
    pub fn add(&self, table: &str, rows: Vec<VendorRow>) -> &Self {
        let mut t = lock(&self.tables);
        match t.iter_mut().find(|(n, _)| eq_ignore_case(n, table)) {
            Some(slot) => slot.1.extend(rows),
            None => t.push((table.to_owned(), rows)),
        }
        drop(t);
        self
    }

    pub fn update(
        &self,
        table: &str,
        pred: impl Fn(&VendorRow) -> bool,
        column: &str,
        value: RawValue,
    ) {
        let mut t = lock(&self.tables);
        let Some(slot) = t.iter_mut().find(|(n, _)| eq_ignore_case(n, table)) else {
            return;
        };
        for r in slot.1.iter_mut().filter(|r| pred(r)) {
            r.insert(column, value.clone());
        }
    }

    pub fn set_fail(&self, e: Option<CaptureError>) {
        *lock(&self.fail) = e;
    }

    pub fn queries(&self) -> usize {
        *lock(&self.queries)
    }

    fn check(&self) -> Result<()> {
        match &*lock(&self.fail) {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
}

impl CaptureSource for FakeVendorSource {
    fn table_exists(&self, table: &str, _ctx: &Ctx) -> Result<bool> {
        self.check()?;
        Ok(lock(&self.tables)
            .iter()
            .any(|(t, _)| eq_ignore_case(t, table)))
    }

    fn query(&self, q: &SourceQuery, ctx: &Ctx) -> Result<Vec<VendorRow>> {
        if ctx.is_done() {
            return Err(CaptureError::cancelled());
        }
        self.check()?;
        *lock(&self.queries) += 1;
        let tables = lock(&self.tables);
        let key = |r: &VendorRow| -> Vec<RawValue> {
            q.order_by.iter().map(|c| value_or_null(r, c)).collect()
        };
        let mut rows: Vec<VendorRow> = tables
            .iter()
            .find(|(t, _)| eq_ignore_case(t, &q.table))
            .map(|(_, r)| r.iter().filter(|r| matches(q, r)).cloned().collect())
            .unwrap_or_default();
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
                    if !exclude.iter().any(|x| eq_ignore_case(x, k)) {
                        out.insert(k, v.clone());
                    }
                }
                out
            })
            .collect())
    }
}

/// In-memory outbox front (`MemoryBatchStore`).
#[derive(Default)]
pub struct MemoryBatchStore {
    last: Mutex<Vec<(String, i64)>>,
    batches: Mutex<Vec<SyncBatch>>,
    pub full: AtomicBool,
    pub needs_resnapshot: AtomicBool,
}

impl MemoryBatchStore {
    pub fn batches(&self) -> Vec<SyncBatch> {
        lock(&self.batches).clone()
    }

    pub fn rows(&self, table: &str) -> Vec<SyncRow> {
        self.batches()
            .into_iter()
            .filter(|b| b.table == table)
            .flat_map(|b| b.rows)
            .collect()
    }

    pub fn set_full(&self, v: bool) {
        self.full.store(v, AtomicOrdering::SeqCst);
    }

    pub fn set_needs_resnapshot(&self, v: bool) {
        self.needs_resnapshot.store(v, AtomicOrdering::SeqCst);
    }
}

impl BatchStore for MemoryBatchStore {
    fn needs_resnapshot(&self) -> Result<bool> {
        Ok(self.needs_resnapshot.load(AtomicOrdering::SeqCst))
    }

    fn watermark(&self, _stream: &str) -> Result<Option<String>> {
        Ok(None)
    }

    fn enqueue(
        &self,
        stream: &str,
        build: &mut dyn FnMut(i64) -> SyncBatch,
    ) -> Result<EnqueueResult> {
        let mut last = lock(&self.last);
        if self.full.load(AtomicOrdering::SeqCst) {
            return Ok(EnqueueResult::CapReached);
        }
        let i = match last.iter().position(|(s, _)| s == stream) {
            Some(i) => i,
            None => {
                last.push((stream.to_owned(), 0));
                last.len() - 1
            }
        };
        let seq = last[i].1 + 1;
        lock(&self.batches).push(build(seq));
        last[i].1 = seq;
        Ok(EnqueueResult::Queued)
    }

    fn clear_needs_resnapshot(&self) -> Result<()> {
        self.set_needs_resnapshot(false);
        Ok(())
    }
}

/// `RecordingBlobStore`.
pub struct RecordingBlobStore {
    pub result: Mutex<BlobEnqueueResult>,
    pub refs: Mutex<Vec<SyncBlobRef>>,
    pub calls: Mutex<usize>,
}

impl RecordingBlobStore {
    pub fn new(result: BlobEnqueueResult) -> RecordingBlobStore {
        RecordingBlobStore {
            result: Mutex::new(result),
            refs: Mutex::default(),
            calls: Mutex::default(),
        }
    }

    pub fn refs(&self) -> Vec<SyncBlobRef> {
        lock(&self.refs).clone()
    }

    pub fn calls(&self) -> usize {
        *lock(&self.calls)
    }

    pub fn set_result(&self, r: BlobEnqueueResult) {
        *lock(&self.result) = r;
    }
}

impl BlobStore for RecordingBlobStore {
    fn enqueue(&self, address: &SyncBlobRef, _content: &[u8]) -> Result<BlobEnqueueResult> {
        *lock(&self.calls) += 1;
        let r = *lock(&self.result);
        if r == BlobEnqueueResult::Queued {
            lock(&self.refs).push(address.clone());
        }
        Ok(r)
    }
}

/// `FakeRuntimeConfig`.
pub struct FakeRuntimeConfig(pub Mutex<Arc<RuntimeConfig>>);

impl FakeRuntimeConfig {
    pub fn new(c: RuntimeConfig) -> FakeRuntimeConfig {
        FakeRuntimeConfig(Mutex::new(Arc::new(c)))
    }

    pub fn set(&self, c: RuntimeConfig) {
        *lock(&self.0) = Arc::new(c);
    }
}

impl RuntimeConfigSource for FakeRuntimeConfig {
    fn current(&self) -> Arc<RuntimeConfig> {
        lock(&self.0).clone()
    }
}

/// A runner on a background thread; `stop` cancels it and asserts it stopped promptly.
pub struct RunningCapture {
    ctx: Ctx,
    thread: Option<JoinHandle<()>>,
}

impl RunningCapture {
    pub fn start(
        runner: &SqlCaptureRunner,
        queue: Arc<dyn BatchStore>,
        config: Arc<dyn RuntimeConfigSource>,
    ) -> RunningCapture {
        let ctx = Ctx::background();
        let r = runner.clone();
        let c = ctx.clone();
        let thread = std::thread::spawn(move || r.run(queue, config, &c));
        RunningCapture {
            ctx,
            thread: Some(thread),
        }
    }

    /// Waits until `condition` holds; fails fast if the runner died.
    pub fn until(&self, condition: impl Fn() -> bool, what: &str) {
        let start = Instant::now();
        while !condition() {
            assert!(
                !self.thread.as_ref().is_some_and(JoinHandle::is_finished),
                "the runner stopped while waiting for: {what}"
            );
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "Timed out waiting for: {what}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        self.ctx.cancel();
        if let Some(t) = self.thread.take() {
            let start = Instant::now();
            while !t.is_finished() {
                assert!(
                    start.elapsed() < Duration::from_secs(10),
                    "the runner stops promptly when the host shuts down"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            t.join().expect("runner thread panicked");
        }
    }
}

impl Drop for RunningCapture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.stop_inner();
        } else {
            self.ctx.cancel();
        }
    }
}

pub fn catalog() -> Arc<SyncCatalog> {
    Arc::new(SyncCatalog::load().unwrap().clone())
}

/// Fast pacing: real time, short intervals, no duty-cycle pause.
pub fn fast_timings() -> CaptureTimings {
    CaptureTimings {
        budget: PollBudget {
            max_cycle_duration: Duration::from_secs(5),
            max_duty_cycle: 1.0,
            max_backoff: Duration::from_millis(200),
        },
        snapshot_pass_interval: Duration::from_millis(10),
        ledger_tail_interval: Duration::from_millis(50),
        ledger_rescan_interval: Duration::from_millis(300),
        min_open_interval: Duration::from_millis(20),
        min_master_interval: Duration::from_millis(50),
    }
}

pub fn fast_config() -> RuntimeConfig {
    RuntimeConfig {
        open_check_poll_ms: 20,
        master_scan_seconds: 1,
        closed_rescan_days: 3,
        history_months: 24,
        table_allowlist: None,
    }
}

pub fn allow(tables: &[&str]) -> Option<Vec<Option<String>>> {
    Some(tables.iter().map(|t| Some((*t).to_owned())).collect())
}

pub fn fast_retry() -> RetryPolicy {
    RetryPolicy {
        base_delay: Duration::from_millis(10),
        max_delay: Duration::from_millis(50),
        ..RetryPolicy::default()
    }
}

/// One durable-like state for every session of a runner.
pub fn one_state() -> StateFactory {
    let state: Arc<dyn CaptureState> = Arc::new(InMemoryCaptureState::new());
    Arc::new(move || Ok(state.clone()))
}

pub fn utc_zone() -> ZoneFactory {
    Arc::new(|| Ok(Arc::new(TimeZone::utc())))
}

pub fn runner_with(
    source: Arc<dyn Fn() -> Result<Arc<dyn CaptureSource>> + Send + Sync>,
    state: StateFactory,
    blobs: Arc<dyn BlobStore>,
    zone: Option<ZoneFactory>,
) -> SqlCaptureRunner {
    SqlCaptureRunner::new(
        CaptureRunnerDependencies {
            source,
            state,
            blobs,
            catalog: catalog(),
            agent_id: "agent-test".into(),
            site_time_zone: zone.unwrap_or_else(utc_zone),
            timings: fast_timings(),
            upload_wake: None,
        },
        fast_retry(),
        SystemClock::shared(),
        &Logger::none(),
    )
    .unwrap()
}

pub fn runner(
    site: &Arc<FakeVendorSource>,
    state: StateFactory,
    blobs: Arc<dyn BlobStore>,
) -> SqlCaptureRunner {
    let site = site.clone();
    runner_with(
        Arc::new(move || Ok(site.clone() as Arc<dyn CaptureSource>)),
        state,
        blobs,
        None,
    )
}

/// A naive SMILE_POS datetime `days_ago` days back, in the UTC site zone.
pub fn local(days_ago: i64) -> RawValue {
    let ticks =
        DateTimeOffset::now_utc().utc_ticks() - 5 * TICKS_PER_MINUTE - days_ago * TICKS_PER_DAY;
    RawValue::DateTime(DateTime::from_ticks(ticks, DateTimeKind::Unspecified).unwrap())
}

pub fn dec(s: &str) -> RawValue {
    RawValue::Decimal(dl_agent_pda_sync::util::stj::parse_dotnet_decimal(s).unwrap())
}

pub fn row(values: &[(&str, RawValue)]) -> VendorRow {
    let mut r = VendorRow::new();
    for (k, v) in values {
        r.insert(*k, v.clone());
    }
    r
}

pub fn invoice(check_no: i32, days_ago: i64, sub_total: &str) -> VendorRow {
    row(&[
        ("CheckNo", RawValue::I32(check_no)),
        ("CloseTime", local(days_ago)),
        ("SubTotal", dec(sub_total)),
    ])
}

/// A small store: one open check with a line, two closed checks (today, 400 days ago)
/// and a master with a picture.
pub fn site() -> Arc<FakeVendorSource> {
    let s = Arc::new(FakeVendorSource::default());
    s.add(
        "TmpInvoice",
        vec![row(&[
            ("CheckNo", RawValue::I32(100)),
            ("OpenTime", local(0)),
            ("SubTotal", dec("50000")),
        ])],
    );
    s.add(
        "TmpTrn",
        vec![row(&[
            ("TrnSeq", RawValue::I32(1)),
            ("CheckNo", RawValue::I32(100)),
            ("ItemPrice", dec("50000")),
        ])],
    );
    s.add("Invoice", vec![invoice(1, 0, "10"), invoice(2, 400, "20")]);
    s.add("InvoiceCxl", vec![])
        .add("VoidTrn", vec![])
        .add("ReOpenLog", vec![]);
    s.add(
        "DefRVCList",
        vec![row(&[
            ("RVCNo", RawValue::I32(1)),
            ("RVCName", RawValue::String("Cafe".into())),
            ("Picture", RawValue::Bytes(vec![1, 2, 3])),
        ])],
    );
    s
}

/// Every stream's seqs are 1..n with no gap or repeat.
pub fn should_be_contiguous_per_stream(batches: &[SyncBatch]) {
    let mut streams: Vec<&str> = batches.iter().map(|b| b.stream.as_str()).collect();
    streams.dedup();
    streams.sort_unstable();
    streams.dedup();
    for s in streams {
        let seqs: Vec<i64> = batches
            .iter()
            .filter(|b| b.stream == s)
            .map(|b| b.seq)
            .collect();
        let want: Vec<i64> = (1..=seqs.len() as i64).collect();
        assert_eq!(seqs, want, "stream {s} is gap-free");
    }
}

/// The `column` key of the rows of `table`.
pub fn keys(batches: &[SyncBatch], table: &str, column: &str) -> Vec<String> {
    batches
        .iter()
        .filter(|b| b.table == table)
        .flat_map(|b| b.rows.iter())
        .filter_map(|r| r.key.get(column).cloned().flatten())
        .collect()
}
