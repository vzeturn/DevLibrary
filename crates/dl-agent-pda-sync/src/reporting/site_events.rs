//! Site event reporting, upload and flushing (port of `Reporting/SiteEventReporter.cs`,
//! `Reporting/SiteEventTransport.cs` and `Reporting/SiteEventFlusher.cs`).

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use dl_json::{Map, Value as JsonValue};
use dl_uuid::Uuid;

use super::log::{Event, Level, Logger, Property, Value};
use super::scrubber::scrub;
use super::site_event_bridge::suppress;
use super::site_event_store::{MemorySiteEventStore, SiteEventStore, StoreError};
use crate::config::time::{SharedClock, Timestamp};
use crate::contracts::WireJson;
use crate::contracts::time::DateTimeOffset;
use crate::contracts::wire::{
    SiteEvent, SiteEventBatch, site_event_levels, site_event_limits, site_event_sources,
};
use crate::transport::{SyncHttpClient, TokenProvider};
use crate::util::ctx::Ctx;
use crate::util::stj::{Style, Writer};

/// `ILogger<SiteEventReporter>` (`ForContext<SiteEventReporter>`).
pub const REPORTER_CATEGORY: &str = "MM.PdaOrder.SyncAgent.Reporting.SiteEventReporter";
/// Detail field with the number of events dropped before this one.
pub const DROPPED_EVENTS_FIELD: &str = "dropped_events";
/// Log property that marks an event's own log line.
pub const MARKER_PROPERTY: &str = "site_event";

/// Where the service, the verbs and the log bridge report site events (`ISiteEvents`).
pub trait SiteEvents: Send + Sync {
    /// Queues one event for the cloud and writes the same line to the log. Never fails:
    /// a reporting problem is logged and the caller carries on. `detail` holds counts,
    /// names, versions; never row values.
    fn report(&self, level: &str, code: &str, message: &str, detail: Option<Map>);
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// At most `max` UTF-16 units of `s` (whole characters only; C# can cut a surrogate pair).
pub fn truncate_utf16(s: &str, max: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > max {
            return &s[..i];
        }
    }
    s
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// `Guid.CreateVersion7(at)`: 48-bit Unix milliseconds, then random bits.
pub fn uuid_v7(at: DateTimeOffset) -> Uuid {
    let mut b = dl_rand::secure_bytes::<16>().unwrap_or_else(|_| {
        let (x, y) = (dl_rand::u64().to_le_bytes(), dl_rand::u64().to_le_bytes());
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&x);
        out[8..].copy_from_slice(&y);
        out
    });
    let ms = u64::try_from(at.unix_millis()).unwrap_or(0).to_be_bytes();
    b[..6].copy_from_slice(&ms[2..]);
    b[6] = (b[6] & 0x0F) | 0x70;
    b[8] = (b[8] & 0x3F) | 0x80;
    Uuid::from_bytes(b)
}

/// UPPER_SNAKE, at most [`site_event_limits::MAX_CODE`] characters (`NormalizeCode`).
pub fn normalize_code(code: Option<&str>) -> String {
    let mut sb = String::new();
    for c in code.unwrap_or("").trim().chars() {
        let mut up = c.to_uppercase();
        let u = match (up.next(), up.next()) {
            (Some(u), None) => u,
            _ => c,
        };
        if u.is_ascii_uppercase() || u.is_ascii_digit() {
            sb.push(u);
        } else {
            for _ in 0..c.len_utf16() {
                sb.push('_');
            }
        }
    }
    if !sb.as_bytes().first().is_some_and(u8::is_ascii_uppercase) {
        sb.insert_str(0, "E_");
    }
    sb.truncate(site_event_limits::MAX_CODE);
    sb
}

/// `string.ReplaceLineEndings(" ")`.
fn replace_line_endings(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(' ');
            }
            '\n' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

/// One scrubbed line, at most [`site_event_limits::MAX_MESSAGE`] characters
/// (`NormalizeMessage`).
pub fn normalize_message(message: Option<&str>, fallback: &str) -> String {
    let scrubbed = replace_line_endings(&scrub(message.unwrap_or("")));
    let mut s = scrubbed.trim();
    if s.is_empty() {
        s = fallback;
    }
    truncate_utf16(s, site_event_limits::MAX_MESSAGE).to_owned()
}

fn scrub_value(v: &mut JsonValue) {
    match v {
        JsonValue::String(s) => *s = scrub(s),
        JsonValue::Object(o) => {
            for (_, item) in o.iter_mut() {
                scrub_value(item);
            }
        }
        JsonValue::Array(a) => a.iter_mut().for_each(scrub_value),
        _ => {}
    }
}

/// `JsonNode.ToJsonString()`: compact, default encoder.
pub fn to_json_string(obj: &Map) -> String {
    let mut w = Writer::new(Style::Compact);
    w.value(&JsonValue::Object(obj.clone()));
    w.finish()
}

/// Scrubs every string, adds the drop count, and replaces a detail over
/// [`site_event_limits::MAX_DETAIL_BYTES`] with `{"truncated":true}` (`NormalizeDetail`).
pub fn normalize_detail(detail: Option<Map>, dropped: i64) -> Option<String> {
    if detail.is_none() && dropped == 0 {
        return None;
    }
    let mut obj = JsonValue::Object(detail.unwrap_or_default());
    scrub_value(&mut obj);
    let JsonValue::Object(mut obj) = obj else {
        return None;
    };
    if dropped > 0 {
        obj.insert(DROPPED_EVENTS_FIELD, dropped);
    }
    let json = to_json_string(&obj);
    if json.len() <= site_event_limits::MAX_DETAIL_BYTES {
        return Some(json);
    }
    let mut small = Map::new();
    small.insert("truncated", true);
    if dropped > 0 {
        small.insert(DROPPED_EVENTS_FIELD, dropped);
    }
    Some(to_json_string(&small))
}

/// A Serilog template that is only `text` (no holes).
fn text_template(text: &str) -> String {
    text.replace('{', "{{").replace('}', "}}")
}

#[derive(Default)]
struct FlushSignal {
    pending: Mutex<bool>,
    cv: Condvar,
}

/// Normalises an event to [`site_event_limits`], scrubs it, queues it and logs it with the
/// marker `site_event=true` so the log bridge never turns it into a second event
/// (`SiteEventReporter`).
pub struct SiteEventReporter {
    source: String,
    run_id: Option<String>,
    log: Logger,
    self_log: Logger,
    clock: SharedClock,
    queue: Mutex<Arc<dyn SiteEventStore>>,
    flush: FlushSignal,
}

impl std::fmt::Debug for SiteEventReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteEventReporter")
            .field("source", &self.source)
            .field("run_id", &self.run_id)
            .finish_non_exhaustive()
    }
}

impl SiteEventReporter {
    /// A reporter writing to `queue` and `log` (the root logger). An unknown `source`
    /// fails like the C# `ArgumentException`.
    pub fn new(
        queue: Arc<dyn SiteEventStore>,
        source: &str,
        run_id: Option<&str>,
        log: &Logger,
        clock: SharedClock,
    ) -> Result<SiteEventReporter, String> {
        if !site_event_sources::is_known(Some(source)) {
            return Err(format!(
                "Unknown site event source '{source}'. (Parameter 'source')"
            ));
        }
        Ok(SiteEventReporter {
            source: source.to_owned(),
            run_id: run_id.map(|r| truncate_utf16(r, site_event_limits::MAX_RUN_ID).to_owned()),
            log: log.clone(),
            self_log: log.for_category(REPORTER_CATEGORY),
            clock,
            queue: Mutex::new(queue),
            flush: FlushSignal::default(),
        })
    }

    /// The queue in use; switches to an in-memory queue if the durable one fails.
    pub fn queue(&self) -> Arc<dyn SiteEventStore> {
        lock(&self.queue).clone()
    }

    /// The event source.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The run id.
    pub fn run_id(&self) -> Option<&str> {
        self.run_id.as_deref()
    }

    /// Waits until an `error` event asked for a flush, up to `max`; true when one did.
    /// A cancelled `ctx` ends the wait with false. The request stays pending until
    /// [`Self::take_flush_request`].
    pub fn wait_flush_request(&self, max: Duration, ctx: &Ctx) -> bool {
        let end = Instant::now().checked_add(max);
        let mut pending = lock(&self.flush.pending);
        loop {
            if *pending {
                return true;
            }
            if ctx.is_done() {
                return false;
            }
            let now = Instant::now();
            let left = match end {
                Some(e) if now >= e => return false,
                Some(e) => e - now,
                None => Duration::from_millis(50),
            };
            pending = self
                .flush
                .cv
                .wait_timeout(pending, left.min(Duration::from_millis(50)))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Takes the pending flush request (a bounded channel of one); true when there was one.
    pub fn take_flush_request(&self) -> bool {
        std::mem::take(&mut *lock(&self.flush.pending))
    }

    fn enqueue(
        &self,
        level: &str,
        code: &str,
        message: &str,
        detail: &Option<Map>,
    ) -> Result<(SiteEvent, Option<String>), StoreError> {
        let mut raw: Option<String> = None;
        let mut build = |dropped: i64| {
            let (ev, json) = self.build(level, code, message, detail.clone(), dropped);
            raw = json;
            ev
        };
        let queue = self.queue();
        let ev = match queue.enqueue(&mut build) {
            Ok(ev) => ev,
            Err(e) => {
                let location: Value = queue.location().map_or(Value::Null, |p: PathBuf| {
                    Value::from(p.display().to_string())
                });
                self.self_log.warn_ex(
                    e.to_exception(),
                    "Site event queue {Location} failed; keeping events in memory for the rest of this run",
                    &[location],
                );
                let memory: Arc<dyn SiteEventStore> = Arc::new(MemorySiteEventStore::default());
                *lock(&self.queue) = memory.clone();
                memory.enqueue(&mut build)?
            }
        };
        Ok((ev, raw))
    }

    fn build(
        &self,
        level: &str,
        code: &str,
        message: &str,
        detail: Option<Map>,
        dropped: i64,
    ) -> (SiteEvent, Option<String>) {
        let json = normalize_detail(detail, dropped);
        let now = DateTimeOffset::from_unix_nanos(self.clock.now().unix_nanos(), 0)
            .unwrap_or(DateTimeOffset::UNIX_EPOCH);
        let ev = SiteEvent {
            event_id: uuid_v7(now),
            at_utc: now,
            source: self.source.clone(),
            level: if site_event_levels::is_known(Some(level)) {
                level.to_owned()
            } else {
                site_event_levels::INFO.to_owned()
            },
            code: normalize_code(Some(code)),
            message: normalize_message(Some(message), code),
            run_id: self.run_id.clone(),
            detail: json
                .as_deref()
                .and_then(|j| crate::util::stj::parse(j).ok()),
        };
        (ev, json)
    }

    fn write(&self, ev: &SiteEvent, raw_detail: Option<String>) {
        let level = match ev.level.as_str() {
            site_event_levels::ERROR => Level::Error,
            site_event_levels::WARN => Level::Warning,
            _ => Level::Information,
        };
        if !self.log.is_enabled(level) {
            return;
        }
        let at = Timestamp::from_unix_nanos(ev.at_utc.unix_nanos());
        let mut e = Event::new(at, level, &text_template(&ev.message));
        e.properties.push(Property::new("code", ev.code.as_str()));
        e.properties.push(Property::new(MARKER_PROPERTY, true));
        e.properties
            .push(Property::new("event_id", ev.event_id.to_string()));
        if let Some(d) = raw_detail {
            e.properties.push(Property {
                name: "detail".to_owned(),
                value: Value::Raw(d),
            });
        }
        self.log.emit(e);
    }
}

impl SiteEvents for SiteEventReporter {
    fn report(&self, level: &str, code: &str, message: &str, detail: Option<Map>) {
        match self.enqueue(level, code, message, &detail) {
            Ok((ev, raw)) => {
                self.write(&ev, raw);
                if ev.level == site_event_levels::ERROR {
                    *lock(&self.flush.pending) = true;
                    self.flush.cv.notify_all();
                }
            }
            Err(e) => {
                // Reporting must never break the caller. The failure is logged under the
                // Reporting context, which the log bridge ignores, so it cannot loop.
                self.self_log.warn_ex(
                    e.to_exception(),
                    "Site event {EventCode} could not be reported",
                    &[Value::from(code)],
                );
            }
        }
    }
}

/// How one upload of site events ended (`SiteEventSendStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteEventSendStatus {
    /// Stored by the cloud.
    Accepted,
    /// HTTP 400: the batch is malformed or too large; split it.
    Malformed,
    /// HTTP 401/403 even after one token refresh.
    Unauthorized,
    /// HTTP 404: the cloud predates site events.
    NotSupported,
    /// Unreachable, timed out, 5xx, or no token.
    Failed,
}

impl SiteEventSendStatus {
    /// The C# enum name.
    pub fn name(self) -> &'static str {
        match self {
            SiteEventSendStatus::Accepted => "Accepted",
            SiteEventSendStatus::Malformed => "Malformed",
            SiteEventSendStatus::Unauthorized => "Unauthorized",
            SiteEventSendStatus::NotSupported => "NotSupported",
            SiteEventSendStatus::Failed => "Failed",
        }
    }
}

/// `SiteEventSendResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteEventSendResult {
    /// The outcome.
    pub status: SiteEventSendStatus,
    /// The HTTP status (0 without an answer).
    pub http_status: i32,
    /// Why it failed.
    pub error: Option<String>,
}

/// Sends one batch of site events (`ISiteEventTransport`). Errors are what the C# code
/// lets escape (cancellation, unexpected exceptions).
pub trait SiteEventTransport: Send + Sync {
    /// Sends `events`.
    fn send(
        &self,
        events: &[SiteEvent],
        ctx: &Ctx,
    ) -> Result<SiteEventSendResult, crate::transport::Error>;
}

/// `POST /sync/v1/events`, authorised and signed like the heartbeat; refreshes the token
/// once on 401/403 (`CloudSiteEventTransport`).
pub struct CloudSiteEventTransport {
    http: Arc<SyncHttpClient>,
    tokens: Arc<TokenProvider>,
    agent_id: String,
    agent_version: String,
}

impl std::fmt::Debug for CloudSiteEventTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudSiteEventTransport")
            .field("agent_id", &self.agent_id)
            .finish_non_exhaustive()
    }
}

/// The site events route.
pub const EVENTS_PATH: &str = "/sync/v1/events";

impl CloudSiteEventTransport {
    /// A transport for the enrolled agent.
    pub fn new(
        http: Arc<SyncHttpClient>,
        tokens: Arc<TokenProvider>,
        agent_id: &str,
        agent_version: &str,
    ) -> CloudSiteEventTransport {
        CloudSiteEventTransport {
            http,
            tokens,
            agent_id: agent_id.to_owned(),
            agent_version: agent_version.to_owned(),
        }
    }
}

impl SiteEventTransport for CloudSiteEventTransport {
    fn send(
        &self,
        events: &[SiteEvent],
        ctx: &Ctx,
    ) -> Result<SiteEventSendResult, crate::transport::Error> {
        let body = SiteEventBatch {
            agent_id: self.agent_id.clone(),
            agent_version: self.agent_version.clone(),
            events: events.to_vec(),
        }
        .to_json();
        let result = |status, http: i32, error: Option<String>| SiteEventSendResult {
            status,
            http_status: http,
            error,
        };
        // One retry, after a 401/403 with a refreshed token; every other answer ends the call.
        for attempt in 1..=2 {
            let resp = match self
                .tokens
                .get_access_token(ctx)
                .and_then(|bearer| self.http.post_json(ctx, EVENTS_PATH, &body, Some(&bearer)))
            {
                Ok(r) => r,
                Err(crate::transport::Error::Agent(e)) => {
                    return Ok(result(
                        SiteEventSendStatus::Failed,
                        0,
                        Some(format!("{}: {}", e.code(), e.message())),
                    ));
                }
                Err(e) => return Err(e),
            };
            let status = i32::from(resp.status);
            if resp.is_success() {
                return Ok(result(SiteEventSendStatus::Accepted, status, None));
            }
            let text = Some(format!("HTTP {status}"));
            match resp.status {
                401 | 403 => {
                    self.tokens.invalidate();
                    if attempt == 1 {
                        continue;
                    }
                    return Ok(result(SiteEventSendStatus::Unauthorized, status, text));
                }
                400 => return Ok(result(SiteEventSendStatus::Malformed, status, text)),
                404 => return Ok(result(SiteEventSendStatus::NotSupported, status, text)),
                _ => return Ok(result(SiteEventSendStatus::Failed, status, text)),
            }
        }
        Ok(result(
            SiteEventSendStatus::Unauthorized,
            401,
            Some("HTTP 401".to_owned()),
        ))
    }
}

/// How a flush ended: everything queued was sent, or the first status that stopped it
/// (`SiteEventFlushResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SiteEventFlushResult {
    /// Events sent.
    pub sent: i32,
    /// Events the cloud refused one by one and that were dropped.
    pub dropped: i32,
    /// The status that stopped the flush.
    pub stopped_by: Option<SiteEventSendStatus>,
}

impl SiteEventFlushResult {
    /// Everything queued was sent.
    pub fn drained(&self) -> bool {
        self.stopped_by.is_none()
    }
}

/// Why a flush stopped with an error.
#[derive(Debug)]
pub enum FlushError {
    /// The queue failed.
    Store(StoreError),
    /// The transport let an error escape (cancelled, unexpected answer).
    Transport(crate::transport::Error),
}

impl FlushError {
    /// As a logged exception.
    pub fn to_exception(&self) -> super::log::Exception {
        match self {
            FlushError::Store(e) => e.to_exception(),
            FlushError::Transport(e) => e.to_exception(),
        }
    }

    /// The context was cancelled.
    pub fn is_cancelled(&self) -> bool {
        matches!(
            self,
            FlushError::Transport(crate::transport::Error::Cancelled)
        )
    }
}

impl std::fmt::Display for FlushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlushError::Store(e) => e.fmt(f),
            FlushError::Transport(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for FlushError {}

impl From<StoreError> for FlushError {
    fn from(e: StoreError) -> FlushError {
        FlushError::Store(e)
    }
}

impl From<crate::transport::Error> for FlushError {
    fn from(e: crate::transport::Error) -> FlushError {
        FlushError::Transport(e)
    }
}

/// The queue a flusher drains (the reporter's queue can change).
pub type QueueSource = Arc<dyn Fn() -> Arc<dyn SiteEventStore> + Send + Sync>;

#[derive(Default)]
struct FlushState {
    backoff_until: Option<DateTimeOffset>,
    not_supported_logged: bool,
}

#[derive(Default)]
struct Tally {
    sent: i32,
    dropped: i32,
}

/// Sends the queue oldest first in batches of [`site_event_limits::MAX_BATCH`]
/// (`SiteEventFlusher`). A 400 splits the batch in halves until the single bad event is
/// found and dropped; a 404 backs off 10 minutes (logged once per episode); anything else
/// keeps the queue for the next attempt. Logs during a flush never become events.
pub struct SiteEventFlusher {
    queue: QueueSource,
    clock: SharedClock,
    log: Logger,
    gate: Mutex<FlushState>,
}

impl std::fmt::Debug for SiteEventFlusher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteEventFlusher").finish_non_exhaustive()
    }
}

/// How long a 404 pauses the uploads.
pub const NOT_SUPPORTED_BACKOFF: Duration = Duration::from_secs(600);
const MAX_ROUNDS: usize = 25;

impl SiteEventFlusher {
    /// A flusher of `queue`, logging to `log` as is.
    pub fn new(queue: QueueSource, clock: SharedClock, log: &Logger) -> SiteEventFlusher {
        SiteEventFlusher {
            queue,
            clock,
            log: log.clone(),
            gate: Mutex::default(),
        }
    }

    /// A flusher of the reporter's current queue.
    pub fn for_reporter(
        reporter: Arc<SiteEventReporter>,
        clock: SharedClock,
        log: &Logger,
    ) -> SiteEventFlusher {
        SiteEventFlusher::new(Arc::new(move || reporter.queue()), clock, log)
    }

    fn now(&self) -> DateTimeOffset {
        DateTimeOffset::from_unix_nanos(self.clock.now().unix_nanos(), 0)
            .unwrap_or(DateTimeOffset::UNIX_EPOCH)
    }

    /// Set after a 404; no request is made before it.
    pub fn backoff_until(&self) -> Option<DateTimeOffset> {
        lock(&self.gate).backoff_until
    }

    /// One flush.
    pub fn flush(
        &self,
        transport: &dyn SiteEventTransport,
        ctx: &Ctx,
    ) -> Result<SiteEventFlushResult, FlushError> {
        let _quiet = suppress();
        let mut st = lock(&self.gate);
        if let Some(until) = st.backoff_until
            && self.now() < until
        {
            return Ok(SiteEventFlushResult {
                sent: 0,
                dropped: 0,
                stopped_by: Some(SiteEventSendStatus::NotSupported),
            });
        }
        let mut tally = Tally::default();
        for _ in 0..MAX_ROUNDS {
            let batch = (self.queue)().peek(site_event_limits::MAX_BATCH)?;
            if batch.is_empty() {
                break;
            }
            if let Some(stop) = self.send(&mut st, transport, &batch, &mut tally, ctx)? {
                return Ok(SiteEventFlushResult {
                    sent: tally.sent,
                    dropped: tally.dropped,
                    stopped_by: Some(stop),
                });
            }
        }
        Ok(SiteEventFlushResult {
            sent: tally.sent,
            dropped: tally.dropped,
            stopped_by: None,
        })
    }

    fn send(
        &self,
        st: &mut FlushState,
        transport: &dyn SiteEventTransport,
        batch: &[SiteEvent],
        tally: &mut Tally,
        ctx: &Ctx,
    ) -> Result<Option<SiteEventSendStatus>, FlushError> {
        let result = transport.send(batch, ctx)?;
        let reason = Value::from(result.error.clone());
        match result.status {
            SiteEventSendStatus::Accepted => {
                let ids: Vec<Uuid> = batch.iter().map(|e| e.event_id).collect();
                (self.queue)().remove(&ids)?;
                tally.sent += i32::try_from(batch.len()).unwrap_or(i32::MAX);
                st.not_supported_logged = false;
                st.backoff_until = None;
                Ok(None)
            }
            SiteEventSendStatus::Malformed if batch.len() == 1 => {
                (self.queue)().remove(&[batch[0].event_id])?;
                tally.dropped += 1;
                self.log.warn(
                    "Site event {EventCode} {EventId} was refused by the cloud ({Reason}) and dropped",
                    &[
                        Value::from(&batch[0].code),
                        Value::from(batch[0].event_id),
                        reason,
                    ],
                );
                Ok(None)
            }
            SiteEventSendStatus::Malformed => {
                let half = batch.len() / 2;
                if let Some(stop) = self.send(st, transport, &batch[..half], tally, ctx)? {
                    return Ok(Some(stop));
                }
                self.send(st, transport, &batch[half..], tally, ctx)
            }
            SiteEventSendStatus::NotSupported => {
                st.backoff_until = self.now().checked_add(NOT_SUPPORTED_BACKOFF);
                if !st.not_supported_logged {
                    self.log.warn(
                        "The cloud does not accept site events yet ({Reason}); keeping them queued and retrying every {Minutes} min",
                        &[reason, Value::from(10)],
                    );
                }
                st.not_supported_logged = true;
                Ok(Some(result.status))
            }
            SiteEventSendStatus::Unauthorized => {
                self.log.warn(
                    "The cloud refused the site event upload ({Reason}) after a token refresh; keeping them queued",
                    &[reason],
                );
                Ok(Some(result.status))
            }
            SiteEventSendStatus::Failed => {
                self.log.debug(
                    "Site event upload failed ({Reason}); keeping them queued",
                    &[reason],
                );
                Ok(Some(result.status))
            }
        }
    }
}

/// The UTF-16 length of `s` (exposed for tests of the limits).
pub fn message_len(s: &str) -> usize {
    utf16_len(s)
}
