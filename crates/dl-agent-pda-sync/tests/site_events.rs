//! `Reporting/SiteEventStoreTests.cs`, `SiteEventFlusherTests.cs`,
//! `SiteEventLogBridgeTests.cs`, `SiteHealthMonitorTests.cs`, the bridge case of
//! `SqlErrorDetailTests.cs` and the agent half of `SiteEventCloudCompatibilityTests.cs`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use dl_agent_pda_sync::config::codes;
use dl_agent_pda_sync::config::time::{Clock, ManualClock, SharedClock, Timestamp};
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::time::DateTimeOffset;
use dl_agent_pda_sync::contracts::wire::{
    SiteEvent, SiteEventBatch, StreamLag, site_event_levels as lv, site_event_limits,
    site_event_sources,
};
use dl_agent_pda_sync::outbox::CaptureStatus;
use dl_agent_pda_sync::reporting::log::{Exception, Level, LoggerConfig};
use dl_agent_pda_sync::reporting::site_event_bridge::{
    self, DEDUP_WINDOW, PER_MINUTE, SUPPRESSED_CODE, WARNING_CODE,
};
use dl_agent_pda_sync::reporting::site_event_store::{DEFAULT_CAPACITY, age_text};
use dl_agent_pda_sync::reporting::site_events::{DROPPED_EVENTS_FIELD, NOT_SUPPORTED_BACKOFF};
use dl_agent_pda_sync::reporting::site_health;
use dl_agent_pda_sync::reporting::{
    CaptureHealth, Logger, MemorySiteEventStore, SiteEventFlushResult, SiteEventFlusher,
    SiteEventLogBridge, SiteEventQueueStats, SiteEventReporter, SiteEventSendResult,
    SiteEventSendStatus, SiteEventStore, SiteEventTransport, SiteEvents, SiteHealthMonitor,
    SqlFailure, SqliteSiteEventStore, Value,
};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_json::{Map, Value as JsonValue};

type Reported = (String, String, String, Option<Map>);

/// `RecordingEvents`.
#[derive(Default)]
struct RecordingEvents(Mutex<Vec<Reported>>);

impl RecordingEvents {
    fn all(&self) -> Vec<Reported> {
        self.0.lock().unwrap().clone()
    }
    fn codes(&self) -> Vec<String> {
        self.all().into_iter().map(|r| r.1).collect()
    }
}

impl SiteEvents for RecordingEvents {
    fn report(&self, level: &str, code: &str, message: &str, detail: Option<Map>) {
        self.0
            .lock()
            .unwrap()
            .push((level.into(), code.into(), message.into(), detail));
    }
}

type Answer = Box<dyn Fn(&[SiteEvent]) -> SiteEventSendStatus + Send + Sync>;

/// `ScriptedTransport`.
struct ScriptedTransport {
    answer: Mutex<Answer>,
    calls: Mutex<Vec<Vec<SiteEvent>>>,
    accepted: Mutex<Vec<SiteEvent>>,
}

impl ScriptedTransport {
    fn new() -> ScriptedTransport {
        ScriptedTransport {
            answer: Mutex::new(Box::new(|_| SiteEventSendStatus::Accepted)),
            calls: Mutex::default(),
            accepted: Mutex::default(),
        }
    }
    fn set(&self, a: impl Fn(&[SiteEvent]) -> SiteEventSendStatus + Send + Sync + 'static) {
        *self.answer.lock().unwrap() = Box::new(a);
    }
}

impl SiteEventTransport for ScriptedTransport {
    fn send(
        &self,
        events: &[SiteEvent],
        _ctx: &Ctx,
    ) -> Result<SiteEventSendResult, dl_agent_pda_sync::transport::Error> {
        self.calls.lock().unwrap().push(events.to_vec());
        let status = (self.answer.lock().unwrap())(events);
        if status == SiteEventSendStatus::Accepted {
            self.accepted.lock().unwrap().extend(events.iter().cloned());
        }
        let http = match status {
            SiteEventSendStatus::Accepted => 200,
            SiteEventSendStatus::Malformed => 400,
            SiteEventSendStatus::Unauthorized => 401,
            SiteEventSendStatus::NotSupported => 404,
            SiteEventSendStatus::Failed => 503,
        };
        Ok(SiteEventSendResult {
            status,
            http_status: http,
            error: (status != SiteEventSendStatus::Accepted).then(|| format!("HTTP {http}")),
        })
    }
}

fn dto(t: Timestamp) -> DateTimeOffset {
    DateTimeOffset::from_unix_nanos(t.unix_nanos(), 0).unwrap()
}

fn clock_at(y: i32, mo: u32, d: u32, h: u32) -> Arc<ManualClock> {
    let t = DateTimeOffset::utc(y, mo, d, h, 0, 0).unwrap();
    Arc::new(ManualClock::new(Timestamp::from_unix_nanos(t.unix_nanos())))
}

/// `TestEvents.New`.
fn test_event(code: &str, level: &str, at: Option<DateTimeOffset>) -> SiteEvent {
    SiteEvent {
        event_id: dl_uuid::Uuid::new_v4(),
        at_utc: at.unwrap_or_else(DateTimeOffset::now_utc),
        source: site_event_sources::AGENT.into(),
        level: level.into(),
        code: code.into(),
        message: code.to_lowercase(),
        run_id: None,
        detail: None,
    }
}

struct Dir(std::path::PathBuf);

impl Dir {
    fn new() -> Dir {
        let d = std::env::temp_dir()
            .join("pda-sync-agent-tests")
            .join(dl_uuid::Uuid::new_v4().simple().to_string());
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }
    fn db(&self) -> std::path::PathBuf {
        self.0.join("events.db")
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn add(store: &dyn SiteEventStore, code: &str, level: &str, at: Option<DateTimeOffset>) -> i64 {
    let mut dropped = -1;
    store
        .enqueue(&mut |d| {
            dropped = d;
            test_event(code, level, at)
        })
        .unwrap();
    dropped
}

fn codes_of(store: &dyn SiteEventStore) -> Vec<String> {
    store
        .peek(10)
        .unwrap()
        .into_iter()
        .map(|e| e.code)
        .collect()
}

// SiteEventStoreTests

#[test]
fn when_full_the_oldest_info_events_go_first_then_the_oldest_of_any_level() {
    for durable in [true, false] {
        let dir = Dir::new();
        let store: Box<dyn SiteEventStore> = if durable {
            Box::new(SqliteSiteEventStore::try_open(dir.db(), 4).unwrap())
        } else {
            Box::new(MemorySiteEventStore::new(4))
        };
        let s = store.as_ref();
        add(s, "E1", lv::ERROR, None);
        add(s, "I1", lv::INFO, None);
        add(s, "I2", lv::INFO, None);
        add(s, "W1", lv::WARN, None);

        assert_eq!(add(s, "E2", lv::ERROR, None), 1);
        assert_eq!(codes_of(s), ["E1", "I2", "W1", "E2"]);
        assert_eq!(add(s, "E3", lv::ERROR, None), 1);
        assert_eq!(codes_of(s), ["E1", "W1", "E2", "E3"]);
        assert_eq!(
            add(s, "E4", lv::ERROR, None),
            1,
            "no info is left, so the oldest event of any level goes"
        );
        assert_eq!(codes_of(s), ["W1", "E2", "E3", "E4"]);
        assert_eq!(s.depth().unwrap(), 4);
    }
}

#[test]
fn default_capacity_is_5000() {
    assert_eq!(DEFAULT_CAPACITY, 5000);
    let store = MemorySiteEventStore::default();
    for i in 0..5001 {
        add(&store, &format!("E{i}"), lv::INFO, None);
    }
    assert_eq!(store.depth().unwrap(), 5000);
    assert_eq!(store.peek(1).unwrap()[0].code, "E1");
}

fn reporter(store: Arc<dyn SiteEventStore>, log: &Logger) -> SiteEventReporter {
    SiteEventReporter::new(
        store,
        site_event_sources::AGENT,
        None,
        log,
        dl_agent_pda_sync::config::SystemClock::shared(),
    )
    .unwrap()
}

#[test]
fn the_next_event_carries_the_number_dropped_before_it() {
    let store = Arc::new(MemorySiteEventStore::new(2));
    let r = reporter(store.clone(), &Logger::none());
    r.report(lv::INFO, "ONE", "one", None);
    r.report(lv::INFO, "TWO", "two", None);
    r.report(lv::ERROR, "THREE", "three", None);

    let all = store.peek(10).unwrap();
    let last = all.last().unwrap();
    assert_eq!(last.code, "THREE");
    assert_eq!(
        last.detail
            .as_ref()
            .unwrap()
            .get(DROPPED_EVENTS_FIELD)
            .and_then(JsonValue::as_i64),
        Some(1)
    );
}

#[test]
fn adopting_another_queue_moves_its_events_in_order_under_their_own_ids() {
    let dir = Dir::new();
    let other_db = dir.0.join("bundle").join("logs").join("events.db");
    let ids: Vec<dl_uuid::Uuid> = {
        let other = SqliteSiteEventStore::try_open(&other_db, DEFAULT_CAPACITY).unwrap();
        add(&other, "X1", lv::INFO, None);
        add(&other, "X2", lv::ERROR, None);
        other.peek(10).unwrap().iter().map(|e| e.event_id).collect()
    };
    let store = SqliteSiteEventStore::try_open(dir.db(), 10).unwrap();
    add(&store, "A", lv::INFO, None);

    assert_eq!(store.adopt_from(&other_db), (2, None));
    assert_eq!(codes_of(&store), ["A", "X1", "X2"]);
    let got: Vec<_> = store
        .peek(10)
        .unwrap()
        .iter()
        .skip(1)
        .map(|e| e.event_id)
        .collect();
    assert_eq!(got, ids);
    assert_eq!(SiteEventQueueStats::read(&other_db).depth, 0);
    assert_eq!(
        store.adopt_from(&other_db).0,
        0,
        "an emptied queue has nothing left to move"
    );
}

#[test]
fn adopting_stops_when_the_queue_is_full_and_leaves_the_rest_where_it_was() {
    let dir = Dir::new();
    let other_db = dir.0.join("other").join("events.db");
    {
        let other = SqliteSiteEventStore::try_open(&other_db, DEFAULT_CAPACITY).unwrap();
        for c in ["X1", "X2", "X3"] {
            add(&other, c, lv::INFO, None);
        }
    }
    let store = SqliteSiteEventStore::try_open(dir.db(), 3).unwrap();
    add(&store, "A", lv::ERROR, None);

    assert_eq!(store.adopt_from(&other_db).0, 2);
    assert_eq!(codes_of(&store), ["A", "X1", "X2"]);
    assert_eq!(SiteEventQueueStats::read(&other_db).depth, 1);
}

#[test]
fn adopting_a_missing_file_its_own_file_or_a_broken_file_moves_nothing_and_never_fails() {
    let dir = Dir::new();
    let store = SqliteSiteEventStore::try_open(dir.db(), 10).unwrap();
    add(&store, "A", lv::INFO, None);
    let broken = dir.0.join("broken.db");
    std::fs::write(
        &broken,
        "this is not a database, it only has the right name",
    )
    .unwrap();

    assert_eq!(
        store.adopt_from(dir.0.join("nowhere").join("events.db")),
        (0, None)
    );
    assert_eq!(store.adopt_from(dir.db()), (0, None));
    let (n, unreadable) = store.adopt_from(&broken);
    assert_eq!(n, 0);
    assert!(unreadable.is_some_and(|e| !e.is_empty()));
    assert_eq!(codes_of(&store), ["A"]);
}

#[test]
fn peek_keeps_order_and_remove_deletes_only_the_given_events() {
    let dir = Dir::new();
    let store = SqliteSiteEventStore::try_open(dir.db(), 10).unwrap();
    for c in ["A", "B", "C"] {
        add(&store, c, lv::INFO, None);
    }
    let first = store.peek(2).unwrap();
    assert_eq!(
        first.iter().map(|e| e.code.as_str()).collect::<Vec<_>>(),
        ["A", "B"]
    );
    store.remove(&[first[0].event_id]).unwrap();
    assert_eq!(codes_of(&store), ["B", "C"]);
}

#[test]
fn the_queue_survives_a_restart_and_the_stats_show_depth_and_oldest_age() {
    let dir = Dir::new();
    let at = DateTimeOffset::utc(2026, 11, 13, 8, 0, 0).unwrap();
    {
        let store = SqliteSiteEventStore::try_open(dir.db(), 10).unwrap();
        add(&store, "A", lv::WARN, Some(at));
        add(&store, "B", lv::INFO, Some(at.add_seconds(60.0)));
    }
    {
        let reopened = SqliteSiteEventStore::try_open(dir.db(), 10).unwrap();
        assert_eq!(reopened.depth().unwrap(), 2);
        assert_eq!(reopened.oldest_at_utc().unwrap(), Some(at));
    }
    let stats = SiteEventQueueStats::read(dir.db());
    assert_eq!(stats.depth, 2);
    assert_eq!(
        stats.describe(at.add_seconds(192.0)),
        "2 queued, oldest unsent 3m 12s ago"
    );
    assert_eq!(age_text(26 * 3600 * 10_000_000), "1d 2h");
}

#[test]
fn stats_of_a_missing_queue_read_as_empty() {
    let dir = Dir::new();
    assert_eq!(
        SiteEventQueueStats::read(dir.0.join("none.db")).describe(DateTimeOffset::now_utc()),
        "0 queued"
    );
}

#[test]
fn an_unwritable_path_is_reported_instead_of_thrown() {
    let dir = Dir::new();
    let blocker = dir.0.join("file");
    std::fs::write(&blocker, "").unwrap();
    let err = SqliteSiteEventStore::try_open(blocker.join("events.db"), 10).unwrap_err();
    assert!(!err.is_empty());
}

// SiteEventFlusherTests

struct FlushKit {
    time: Arc<ManualClock>,
    store: Arc<MemorySiteEventStore>,
    transport: ScriptedTransport,
}

impl FlushKit {
    fn new() -> FlushKit {
        FlushKit {
            time: clock_at(2026, 11, 13, 8),
            store: Arc::new(MemorySiteEventStore::default()),
            transport: ScriptedTransport::new(),
        }
    }
    fn flusher(&self) -> SiteEventFlusher {
        let s = self.store.clone();
        SiteEventFlusher::new(
            Arc::new(move || s.clone() as Arc<dyn SiteEventStore>),
            self.time.clone(),
            &Logger::none(),
        )
    }
    fn fill(&self, count: usize, code: impl Fn(usize) -> String) -> Vec<String> {
        let codes: Vec<String> = (0..count).map(code).collect();
        for c in &codes {
            add(self.store.as_ref(), c, lv::INFO, None);
        }
        codes
    }
    fn flush(&self, f: &SiteEventFlusher) -> SiteEventFlushResult {
        f.flush(&self.transport, &Ctx::background()).unwrap()
    }
}

fn result(
    sent: i32,
    dropped: i32,
    stopped_by: Option<SiteEventSendStatus>,
) -> SiteEventFlushResult {
    SiteEventFlushResult {
        sent,
        dropped,
        stopped_by,
    }
}

#[test]
fn sends_oldest_first_in_batches_of_200_until_the_queue_is_empty() {
    let k = FlushKit::new();
    let codes = k.fill(450, |i| format!("EVENT_{i}"));
    assert_eq!(k.flush(&k.flusher()), result(450, 0, None));
    let sizes: Vec<usize> = k
        .transport
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(Vec::len)
        .collect();
    assert_eq!(
        sizes,
        [
            site_event_limits::MAX_BATCH,
            site_event_limits::MAX_BATCH,
            50
        ]
    );
    let sent: Vec<String> = k
        .transport
        .accepted
        .lock()
        .unwrap()
        .iter()
        .map(|e| e.code.clone())
        .collect();
    assert_eq!(sent, codes);
    assert_eq!(k.store.depth().unwrap(), 0);
}

#[test]
fn a_400_splits_the_batch_until_the_one_bad_event_is_dropped() {
    let k = FlushKit::new();
    k.fill(10, |i| {
        if i == 7 {
            "POISON".into()
        } else {
            format!("EVENT_{i}")
        }
    });
    k.transport.set(|b| {
        if b.iter().any(|e| e.code == "POISON") {
            SiteEventSendStatus::Malformed
        } else {
            SiteEventSendStatus::Accepted
        }
    });
    let r = k.flush(&k.flusher());
    assert_eq!((r.sent, r.dropped, r.drained()), (9, 1, true));
    let acc = k.transport.accepted.lock().unwrap().clone();
    assert_eq!(acc.len(), 9);
    assert!(acc.iter().all(|e| e.code != "POISON"));
    assert_eq!(k.store.depth().unwrap(), 0);
}

#[test]
fn a_404_backs_off_ten_minutes_and_keeps_the_queue() {
    let k = FlushKit::new();
    k.fill(3, |i| format!("EVENT_{i}"));
    let flusher = k.flusher();
    k.transport.set(|_| SiteEventSendStatus::NotSupported);

    let first = k.flush(&flusher);
    assert_eq!(first.stopped_by, Some(SiteEventSendStatus::NotSupported));
    assert_eq!(
        flusher.backoff_until(),
        dto(k.time.now()).checked_add(NOT_SUPPORTED_BACKOFF)
    );

    k.time.advance(Duration::from_secs(9 * 60));
    k.flush(&flusher);
    assert_eq!(
        k.transport.calls.lock().unwrap().len(),
        1,
        "no request is made during the back-off"
    );

    k.time.advance(Duration::from_secs(60));
    k.transport.set(|_| SiteEventSendStatus::Accepted);
    assert_eq!(k.flush(&flusher), result(3, 0, None));
    assert_eq!(flusher.backoff_until(), None);
    assert_eq!(k.store.depth().unwrap(), 0);
}

#[test]
fn unreachable_or_refused_keeps_every_event_for_the_next_flush() {
    for status in [
        SiteEventSendStatus::Failed,
        SiteEventSendStatus::Unauthorized,
    ] {
        let k = FlushKit::new();
        k.fill(5, |i| format!("EVENT_{i}"));
        k.transport.set(move |_| status);
        assert_eq!(k.flush(&k.flusher()), result(0, 0, Some(status)));
        assert_eq!(k.store.depth().unwrap(), 5);
    }
}

#[test]
fn an_empty_queue_makes_no_request() {
    let k = FlushKit::new();
    assert!(k.flush(&k.flusher()).drained());
    assert!(k.transport.calls.lock().unwrap().is_empty());
}

// SiteEventLogBridgeTests

struct BridgeKit {
    time: Arc<ManualClock>,
    events: Arc<RecordingEvents>,
    bridge: Arc<SiteEventLogBridge>,
    log: Logger,
}

impl BridgeKit {
    fn new() -> BridgeKit {
        let time = clock_at(2026, 11, 13, 8);
        let events = Arc::new(RecordingEvents::default());
        let bridge = Arc::new(SiteEventLogBridge::new(time.clone()));
        bridge.attach(events.clone());
        let log = Logger::new(LoggerConfig {
            minimum: Level::Debug,
            sinks: vec![(bridge.clone(), Level::Verbose)],
            clock: time.clone() as SharedClock,
            ..LoggerConfig::default()
        });
        BridgeKit {
            time,
            events,
            bridge,
            log,
        }
    }
}

fn str_of<'a>(d: &'a Option<Map>, k: &str) -> Option<&'a str> {
    d.as_ref()
        .and_then(|m| m.get(k))
        .and_then(JsonValue::as_str)
}

fn int_of(d: &Option<Map>, k: &str) -> Option<i64> {
    d.as_ref()
        .and_then(|m| m.get(k))
        .and_then(JsonValue::as_i64)
}

#[test]
fn warnings_and_errors_become_events_with_their_code_and_scalar_detail() {
    let k = BridgeKit::new();
    k.log.info("Ignored {Rows}", &[Value::from(1)]);
    k.log
        .warn("Upload of {Stream} is slow", &[Value::from("orders")]);
    k.log.error_ex(
        Exception::new("System.TimeoutException", "t"),
        "Capture stopped with {Code}",
        &[Value::from("SQL_BUSY")],
    );

    let r = k.events.all();
    let lc: Vec<(&str, &str)> = r.iter().map(|x| (x.0.as_str(), x.1.as_str())).collect();
    assert_eq!(lc, [(lv::WARN, WARNING_CODE), (lv::ERROR, "SQL_BUSY")]);
    assert_eq!(r[0].2, "Upload of orders is slow");
    assert_eq!(str_of(&r[0].3, "stream"), Some("orders"));
    assert_eq!(
        str_of(&r[1].3, "exception_type"),
        Some("System.TimeoutException")
    );
    assert!(
        !r[1].3.as_ref().unwrap().contains_key("code"),
        "the code is the event's own field"
    );
}

#[test]
fn the_same_line_within_five_minutes_is_one_event_plus_a_repeat_summary() {
    let k = BridgeKit::new();
    for i in 0..4 {
        k.log.warn(
            "Heartbeat failed: {Reason}",
            &[Value::from(format!("timeout {i}"))],
        );
        k.time.advance(Duration::from_secs(30));
    }
    assert_eq!(k.events.all().len(), 1);

    k.time.advance(DEDUP_WINDOW);
    k.bridge.tick();

    let r = k.events.all();
    assert_eq!(r.len(), 2);
    assert_eq!(r[1].1, WARNING_CODE);
    assert_eq!(int_of(&r[1].3, "repeats"), Some(3));
    assert!(
        r[1].2.contains("timeout 3") && r[1].2.contains("repeated 3 more times"),
        "{}",
        r[1].2
    );

    k.log
        .warn("Heartbeat failed: {Reason}", &[Value::from("again")]);
    assert_eq!(
        k.events.all().len(),
        3,
        "a new window starts after the summary"
    );
}

#[test]
fn more_than_sixty_a_minute_are_summarised_as_one_suppression_event() {
    let k = BridgeKit::new();
    for i in 0..100 {
        k.log.warn(&format!("Distinct line {i}"), &[]);
    }
    assert_eq!(k.events.all().len(), PER_MINUTE as usize);

    k.time.advance(Duration::from_secs(60));
    k.bridge.tick();

    let r = k.events.all();
    let last = r.last().unwrap();
    assert_eq!(last.1, SUPPRESSED_CODE);
    assert_eq!(int_of(&last.3, "suppressed"), Some(40));
}

#[test]
fn drain_emits_pending_summaries_at_once() {
    let k = BridgeKit::new();
    k.log.warn("Same {N}", &[Value::from(1)]);
    k.log.warn("Same {N}", &[Value::from(2)]);
    k.bridge.drain();
    let r = k.events.all();
    assert_eq!(r.len(), 2);
    assert_eq!(int_of(&r[1].3, "repeats"), Some(1));
}

#[test]
fn reporting_lines_event_lines_and_suppressed_scopes_never_feed_back() {
    let k = BridgeKit::new();
    k.log
        .for_category("MM.PdaOrder.SyncAgent.Reporting.SiteEventFlusher")
        .warn("Flush failed", &[]);
    k.log
        .for_context("site_event", true)
        .error("Already an event", &[]);
    {
        let _q = site_event_bridge::suppress();
        k.log.error("During a flush", &[]);
    }
    assert!(k.events.all().is_empty());
}

#[test]
fn lines_logged_before_attach_are_reported_on_attach() {
    let time = clock_at(2026, 11, 13, 8);
    let bridge = Arc::new(SiteEventLogBridge::new(time.clone()));
    let log = Logger::new(LoggerConfig {
        sinks: vec![(bridge.clone(), Level::Verbose)],
        ..LoggerConfig::default()
    });
    log.warn("Early", &[]);
    let events = Arc::new(RecordingEvents::default());
    bridge.attach(events.clone());
    assert_eq!(events.codes(), [WARNING_CODE]);
}

#[test]
fn the_bridge_reports_schema_detail_and_keeps_the_latest_in_the_repeat_summary() {
    let k = BridgeKit::new();
    let tpl = "Capture failed with {Code} ({Failures} in a row)";
    k.log.error_ex(
        SqlFailure::single(207, 16, "Invalid column name 'A1'.").to_exception(),
        tpl,
        &[Value::from("SQL_SCHEMA_MISMATCH"), Value::from(1)],
    );
    k.log.error_ex(
        SqlFailure::single(207, 16, "Invalid column name 'B2'.").to_exception(),
        tpl,
        &[Value::from("SQL_SCHEMA_MISMATCH"), Value::from(2)],
    );
    k.time.advance(DEDUP_WINDOW);
    k.bridge.tick();

    let r = k.events.all();
    assert_eq!(r.len(), 2, "one event plus one repeat summary per window");
    assert_eq!(r[0].1, "SQL_SCHEMA_MISMATCH");
    assert_eq!(str_of(&r[0].3, "sql_identifier"), Some("A1"));
    assert!(
        !r[0].2.contains("Invalid column"),
        "the exception message is not reported"
    );
    assert_eq!(int_of(&r[1].3, "repeats"), Some(1));
    assert_eq!(str_of(&r[1].3, "sql_identifier"), Some("B2"));
    assert_eq!(int_of(&r[1].3, "sql_number"), Some(207));
}

// SiteHealthMonitorTests

#[derive(Default)]
struct FakeCaptureStatus {
    unreachable: Mutex<bool>,
    error: Mutex<Option<String>>,
    detail: Mutex<Option<Map>>,
}

impl CaptureStatus for FakeCaptureStatus {
    fn sql_reachable(&self) -> bool {
        !*self.unreachable.lock().unwrap()
    }
    fn streams(&self) -> Vec<StreamLag> {
        Vec::new()
    }
    fn capture_error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }
}

impl CaptureHealth for FakeCaptureStatus {
    fn capture_error_detail(&self) -> Option<Map> {
        self.detail.lock().unwrap().clone()
    }
}

struct HealthKit {
    events: Arc<RecordingEvents>,
    capture: Arc<FakeCaptureStatus>,
    backlog: Arc<Mutex<i64>>,
    token: Arc<Mutex<Option<String>>>,
    monitor: SiteHealthMonitor,
}

impl HealthKit {
    fn new() -> HealthKit {
        let events = Arc::new(RecordingEvents::default());
        let capture = Arc::new(FakeCaptureStatus::default());
        let backlog = Arc::new(Mutex::new(0));
        let token = Arc::new(Mutex::new(None));
        let (b, t) = (backlog.clone(), token.clone());
        let monitor = SiteHealthMonitor::new(
            events.clone(),
            capture.clone(),
            Arc::new(move || Ok(*b.lock().unwrap())),
            Arc::new(move || t.lock().unwrap().clone()),
            500,
        );
        HealthKit {
            events,
            capture,
            backlog,
            token,
            monitor,
        }
    }
    fn check(&self) {
        self.monitor.check().unwrap();
    }
    fn set(&self, reachable: bool, error: Option<&str>) {
        *self.capture.unreachable.lock().unwrap() = !reachable;
        *self.capture.error.lock().unwrap() = error.map(str::to_owned);
    }
}

#[test]
fn database_down_then_up_is_one_error_and_one_recovery() {
    let k = HealthKit::new();
    k.check();
    k.set(false, Some(codes::SQL_UNREACHABLE));
    k.check();
    k.check();
    k.set(true, None);
    k.check();
    k.check();
    assert_eq!(
        k.events.codes(),
        [codes::SQL_UNREACHABLE, site_health::SQL_RECOVERED]
    );
    assert_eq!(k.events.all()[0].0, lv::ERROR);
}

#[test]
fn a_capture_error_is_reported_once_and_its_clearing_once() {
    let k = HealthKit::new();
    k.set(true, Some(codes::SQL_BUSY));
    k.check();
    k.check();
    k.set(true, None);
    k.check();
    assert_eq!(
        k.events.codes(),
        [codes::SQL_BUSY, site_health::CAPTURE_ERROR_CLEARED]
    );
    assert_eq!(
        k.events.all()[0].0,
        lv::WARN,
        "a busy database is a warning, not an outage"
    );
}

#[test]
fn a_first_capture_stop_carries_the_code_and_the_failure_facts() {
    let k = HealthKit::new();
    k.set(true, Some(codes::SQL_SCHEMA_MISMATCH));
    let mut d = Map::new();
    d.insert("skipped_tables", "ledger-rescan/ReOpenLog");
    *k.capture.detail.lock().unwrap() = Some(d);
    k.check();

    let r = k.events.all();
    assert_eq!(r.len(), 1);
    let detail = &r[0].3;
    assert_eq!(
        str_of(detail, "capture_error"),
        Some(codes::SQL_SCHEMA_MISMATCH)
    );
    assert_eq!(
        detail
            .as_ref()
            .unwrap()
            .get("sql_reachable")
            .and_then(JsonValue::as_bool),
        Some(true)
    );
    assert_eq!(
        str_of(detail, "skipped_tables"),
        Some("ledger-rescan/ReOpenLog")
    );
    assert!(
        !detail.as_ref().unwrap().contains_key("previous"),
        "there was no earlier error"
    );
}

#[test]
fn token_refresh_failing_and_recovering_are_transitions() {
    let k = HealthKit::new();
    *k.token.lock().unwrap() = Some(codes::CLOUD_UNREACHABLE.into());
    k.check();
    k.check();
    *k.token.lock().unwrap() = None;
    k.check();
    assert_eq!(
        k.events.codes(),
        [
            site_health::TOKEN_REFRESH_FAILING,
            site_health::TOKEN_REFRESH_RECOVERED
        ]
    );
    assert_eq!(
        str_of(&k.events.all()[0].3, "error_code"),
        Some(codes::CLOUD_UNREACHABLE)
    );
}

#[test]
fn upload_backlog_alerts_above_the_threshold_and_clears_at_a_tenth() {
    let k = HealthKit::new();
    for depth in [501, 100, 50] {
        *k.backlog.lock().unwrap() = depth;
        k.check();
    }
    assert_eq!(
        k.events.codes(),
        [
            site_health::UPLOAD_BACKLOG,
            site_health::UPLOAD_BACKLOG_DRAINED
        ]
    );
}

// SiteEventCloudCompatibilityTests (agent half: the limits the cloud's sanitizer checks)

#[test]
fn every_event_the_reporter_queues_fits_the_wire_limits_after_a_round_trip() {
    for (source, verb) in [
        (site_event_sources::AGENT, "agent"),
        (site_event_sources::ENROLL, "enroll"),
        (site_event_sources::LEGACY_EXPORT, "legacy-export"),
        (site_event_sources::LEGACY_ARCHIVE, "legacy-archive"),
    ] {
        let queue = Arc::new(MemorySiteEventStore::default());
        let run_id =
            dl_agent_pda_sync::reporting::agent_logging::new_run_id(verb, Timestamp::now_utc());
        assert!(run_id.len() <= site_event_limits::MAX_RUN_ID);
        let r = SiteEventReporter::new(
            queue.clone(),
            source,
            Some(&run_id),
            &Logger::none(),
            dl_agent_pda_sync::config::SystemClock::shared(),
        )
        .unwrap();
        let mut d1 = Map::new();
        d1.insert("table", "pda_printer");
        d1.insert("rows", 12);
        r.report(lv::INFO, "LEGACY_EXPORT_TABLE", "table written", Some(d1));
        r.report(lv::WARN, "lower case code with spaces", "a warning", None);
        r.report(lv::ERROR, &"X".repeat(200), &"m".repeat(5000), None);
        r.report("fatal", "UNKNOWN_LEVEL", "line one\nline two\ttabbed", None);
        let mut big = Map::new();
        big.insert("blob", "d".repeat(3 * site_event_limits::MAX_DETAIL_BYTES));
        r.report(lv::INFO, "BIG_DETAIL", "detail over the cap", Some(big));
        let mut uni = Map::new();
        uni.insert("note", "tiếng Việt");
        r.report(
            lv::INFO,
            "UNICODE",
            "Cửa hàng số 1 — đã xuất gói",
            Some(uni),
        );

        let queued = queue.peek(site_event_limits::MAX_BATCH).unwrap();
        assert_eq!(queued.len(), 6);
        let wire = SiteEventBatch {
            agent_id: "0f8fad5b-d9cb-469f-a165-708677289b0b".into(),
            agent_version: "4.2.0".into(),
            events: queued.clone(),
        }
        .to_json();
        let parsed = SiteEventBatch::from_json(&wire).unwrap();
        assert_eq!(parsed.events.len(), 6);
        let codes: Vec<&str> = parsed.events.iter().map(|e| e.code.as_str()).collect();
        assert_eq!(codes[1], "LOWER_CASE_CODE_WITH_SPACES");
        assert_eq!(codes[2], "X".repeat(site_event_limits::MAX_CODE));
        assert_eq!(parsed.events[3].level, lv::INFO);
        assert_eq!(parsed.events[3].message, "line one line two\ttabbed");
        assert_eq!(
            parsed.events[4].detail.as_ref().unwrap().to_string(),
            "{\"truncated\":true}"
        );
        for sent in &parsed.events {
            assert_eq!(sent.source, source);
            assert_eq!(sent.run_id.as_deref(), Some(run_id.as_str()));
            assert!(
                sent.message.chars().map(char::len_utf16).sum::<usize>()
                    <= site_event_limits::MAX_MESSAGE
            );
            assert!(
                sent.code.len() <= site_event_limits::MAX_CODE
                    && sent.code.as_bytes()[0].is_ascii_uppercase()
            );
            assert!(
                sent.code
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            );
            if let Some(d) = &sent.detail {
                let mut w = dl_agent_pda_sync::util::stj::Writer::new(
                    dl_agent_pda_sync::util::stj::Style::Compact,
                );
                w.value(d);
                assert!(w.finish().len() <= site_event_limits::MAX_DETAIL_BYTES);
            }
        }
    }
}

#[test]
fn an_error_event_asks_for_a_flush_and_every_event_is_logged_with_the_marker() {
    let sink = Arc::new(dl_agent_pda_sync::reporting::MemorySink::default());
    let log = Logger::new(LoggerConfig {
        sinks: vec![(sink.clone(), Level::Verbose)],
        ..LoggerConfig::default()
    });
    let r = reporter(Arc::new(MemorySiteEventStore::default()), &log);
    r.report(lv::INFO, "A", "plain {not a hole}", None);
    assert!(!r.take_flush_request());
    r.report(lv::ERROR, "B", "bad", None);
    assert!(r.take_flush_request());
    let ev = sink.events();
    assert_eq!(ev.len(), 2);
    assert_eq!(ev[0].render_message(), "plain {not a hole}");
    assert_eq!(ev[0].property("site_event"), Some(&Value::Bool(true)));
    assert_eq!(ev[1].level, Level::Error);
}
