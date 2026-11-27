//! Drains the outbox to `POST /sync/v1/batches` (port of `Outbox/BatchUploader.cs`).
//!
//! Duplicate seq is acked by the cloud, a gap (409) re-aligns the queue. A batch the cloud
//! refuses for good is quarantined and reported (`SYNC_BATCH_QUARANTINED`) with the
//! cloud's code and reason. After a quarantine the next batch of the stream answers 409
//! naming the quarantined seq; the uploader reports the gap (`SYNC_STREAM_GAP`), sets the
//! batch aside and renumbers the stream ([`SqliteOutbox::resequence`]), then reports
//! `SYNC_STREAM_RECOVERED` (or `SYNC_STREAM_RECOVERY_FAILED` and retries). Automatic
//! resnapshots are paced by [`RECOVERY_RESNAPSHOT_COOLDOWN`].
//!
//! HTTP 424 `SYNC_PARENT_PENDING` (ADR 0102) parks the stream ([`ParentPendingTracker`])
//! while the others flow; after `ParentPendingEscalateMinutes` the parent stream and the
//! stream are resent; once the resent parent still lacks the rows the batch is sent once
//! more with `?orphans=skip` (ADR 0109).

use std::sync::Arc;
use std::time::Duration;

use dl_http::Method;
use dl_json::Value as Json;

use super::backlog::BacklogReporter;
use super::batches::{OutboxItem, SqliteOutbox};
use super::error::{OutboxError, UploadError};
use super::parent_pending::{ParentPendingEpisode, ParentPendingTracker};
use crate::config::codes;
use crate::config::time::{SharedClock, SystemClock, Timestamp};
use crate::contracts::time::{DateTimeOffset, TICKS_PER_SECOND};
use crate::contracts::wire::{sync_streams, with_t_sent};
use crate::reporting::{Exception, Logger, Value};
use crate::transport::retry::is_transient;
use crate::transport::{SyncHttpClient, SyncResponse, TokenProvider};
use crate::util::ctx::Ctx;
use crate::util::hop_clock;

mod drain;

/// `MM.PdaOrder.SyncAgent.Outbox.BatchUploader`: the log category.
pub const BATCH_CATEGORY: &str = "MM.PdaOrder.SyncAgent.Outbox.BatchUploader";

/// The batch route.
pub const BATCHES_PATH: &str = "/sync/v1/batches";

/// Longest rejection reason kept (UTF-16 units).
pub const MAX_REASON: usize = 300;

/// Least time between two automatic (recovery) resnapshots.
pub const RECOVERY_RESNAPSHOT_COOLDOWN: Duration = Duration::from_secs(6 * 3600);

/// The cloud's code of a batch given up after the parent stream was resent and still
/// lacked the rows.
pub const PARENT_GONE_CODE: &str = "SYNC_PARENT_GONE";

/// What one upload cycle did (`UploadCycleResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadCycleResult {
    /// Nothing to send.
    Idle,
    /// Something was sent or settled.
    Progress,
    /// The cloud did not take the first item (outage, 401, 403): back off.
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Done,
    Retry,
    Repeek,
    Parked,
}

pub(super) fn ticks(d: Duration) -> i64 {
    i64::try_from(d.as_nanos() / 100).unwrap_or(i64::MAX)
}

pub(super) fn to_dto(t: Timestamp) -> DateTimeOffset {
    DateTimeOffset::from_unix_nanos(t.unix_nanos(), t.offset_minutes())
        .unwrap_or(DateTimeOffset::UNIX_EPOCH)
}

fn to_timestamp(t: DateTimeOffset) -> Timestamp {
    Timestamp::with_offset(t.unix_nanos(), t.offset_minutes())
}

/// The body as `JsonDocument.Parse` reads it, `None` when it is empty or not JSON.
pub(super) fn parse_body(body: &[u8]) -> Option<Json> {
    if body.is_empty() {
        return None;
    }
    let text = std::str::from_utf8(body).ok()?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    crate::util::stj::parse(text).ok()
}

/// `TryGetProperty(name)` with a string value.
fn text<'a>(obj: &'a Json, name: &str) -> Option<&'a str> {
    match obj {
        Json::Object(m) => m
            .iter()
            .rev()
            .find(|(k, _)| k.as_str() == name)
            .and_then(|(_, v)| v.as_str()),
        _ => None,
    }
}

/// `s.Length > max ? s[..max] : s` in UTF-16 units (a surrogate pair is not split).
fn cap(s: &str, max: usize) -> String {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > max {
            return s[..i].to_owned();
        }
    }
    s.to_owned()
}

/// The cloud's `code` and `message` (or problem `detail`) of a refusal, each length-capped.
pub fn rejection_of(resp: &SyncResponse) -> (Option<String>, Option<String>) {
    let Some(root) = parse_body(&resp.body).filter(Json::is_object) else {
        return (None, None);
    };
    let code = text(&root, "code").map(|c| cap(c, 64));
    let reason = text(&root, "message")
        .or_else(|| text(&root, "detail"))
        .map(|r| cap(r, MAX_REASON));
    (code, reason)
}

/// The `parent_stream` of a 424 `SYNC_PARENT_PENDING` answer, or `None` for any other 424.
pub fn parent_stream_of(resp: &SyncResponse) -> Option<String> {
    let root = parse_body(&resp.body).filter(Json::is_object)?;
    if text(&root, "code") != Some(sync_streams::PARENT_PENDING_CODE) {
        return None;
    }
    let parent = text(&root, "parent_stream")?;
    let len = parent.encode_utf16().count();
    (len > 0 && len <= sync_streams::MAX_STREAM_LENGTH && !parent.starts_with('!'))
        .then(|| parent.to_owned())
}

/// The vendor table of a queued batch (its `table` field), or `None`.
pub fn table_of(payload: &[u8]) -> Option<String> {
    let root = parse_body(payload)?;
    text(&root, "table").map(|t| cap(t, 128))
}

fn try_expected_seq(resp: &SyncResponse) -> Option<i64> {
    find_expected(&parse_body(&resp.body)?)
}

fn find_expected(e: &Json) -> Option<i64> {
    let Json::Object(m) = e else { return None };
    for (name, v) in m.iter() {
        let is_key = name.eq_ignore_ascii_case("expected_seq") || name == "expectedSeq";
        if is_key
            && let Json::Number(n) = v
            && let Some(x) = n
                .as_raw_str()
                .map(str::to_owned)
                .unwrap_or_else(|| n.to_string())
                .parse::<i64>()
                .ok()
        {
            return Some(x);
        }
        if let Some(nested) = find_expected(v) {
            return Some(nested);
        }
    }
    None
}

fn seconds(ticks: i64) -> i64 {
    ticks / TICKS_PER_SECOND
}

/// Drains the outbox (`BatchUploader`). Used by the single uploader loop.
pub struct BatchUploader {
    outbox: Arc<SqliteOutbox>,
    http: Arc<SyncHttpClient>,
    tokens: Arc<TokenProvider>,
    log: Logger,
    clock: SharedClock,
    parents: ParentPendingTracker,
    backlog: BacklogReporter,
    parallel: usize,
}

impl std::fmt::Debug for BatchUploader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatchUploader").finish_non_exhaustive()
    }
}

impl BatchUploader {
    /// An uploader; `clock` defaults to the system clock and `parent_pending_escalate_minutes`
    /// to the [`crate::config::AgentOptions`] default (15).
    pub fn new(
        outbox: Arc<SqliteOutbox>,
        http: Arc<SyncHttpClient>,
        tokens: Arc<TokenProvider>,
        log: &Logger,
        clock: Option<SharedClock>,
        parent_pending_escalate_minutes: Option<i32>,
    ) -> BatchUploader {
        let minutes = parent_pending_escalate_minutes.unwrap_or_else(|| {
            crate::config::AgentOptions::default().parent_pending_escalate_minutes
        });
        let escalate = Duration::from_secs(u64::try_from(minutes).unwrap_or(0) * 60);
        BatchUploader {
            outbox,
            http,
            tokens,
            log: log.for_category(BATCH_CATEGORY),
            clock: clock.unwrap_or_else(SystemClock::shared),
            parents: ParentPendingTracker::new(escalate),
            backlog: BacklogReporter::default(),
            parallel: 1,
        }
    }

    /// The uploader posting the next batch of up to `streams` (at least 1) streams at the
    /// same time (RT-1318); one stream's batches still go one after the other, in seq order.
    #[must_use]
    pub fn with_parallel_streams(mut self, streams: usize) -> BatchUploader {
        self.parallel = streams.max(1);
        self
    }

    fn now(&self) -> DateTimeOffset {
        to_dto(self.clock.now())
    }

    /// Sends up to `max_batches` queued batches: the live open-check stream first, then the
    /// rest oldest first (RT-1309). A batch that parks its stream does not end the cycle
    /// idle while other streams still have batches: the outbox is peeked again without it.
    pub fn run_cycle(
        &mut self,
        max_batches: i32,
        ctx: &Ctx,
    ) -> Result<UploadCycleResult, UploadError> {
        self.promote_deferred_resnapshot()?;
        let mut skip = self.parents.parked(self.now());
        let result = self.drain(max_batches, &mut skip, ctx)?;
        let now = self.now();
        self.backlog.check(&self.outbox, &skip, now, &self.log)?;
        Ok(result)
    }

    /// Posts one batch; the answer is read by [`Self::answer`]. Takes `&self` so the
    /// batches of different streams can be posted side by side (RT-1318).
    fn post(
        &self,
        item: &OutboxItem,
        bearer: &str,
        ctx: &Ctx,
    ) -> Result<(SyncResponse, Duration), UploadError> {
        self.outbox.mark_attempt(item.id)?;
        let sending = std::time::Instant::now();
        // RT-1318: the batch is handed to the transport now (`t_sent`).
        let body = with_t_sent(&item.payload, hop_clock::now_field());
        let resp = self
            .http
            .send(ctx, Method::Post, BATCHES_PATH, &body, Some(bearer))?;
        Ok((resp, sending.elapsed()))
    }

    /// What the cloud's answer to `item` means for its stream.
    fn answer(
        &mut self,
        item: &OutboxItem,
        resp: &SyncResponse,
        took: Duration,
        ctx: &Ctx,
    ) -> Result<Outcome, UploadError> {
        if resp.is_success() {
            if item.stream == sync_streams::OPEN {
                self.log_open_timing(item, took, resp.reused)?;
            }
            self.outbox.ack(item.id)?;
            self.parent_arrived(&item.stream);
            return Ok(Outcome::Done);
        }
        match resp.status {
            401 => {
                self.tokens.invalidate();
                Ok(Outcome::Retry)
            }
            409 => self.handle_seq_conflict(item, resp),
            403 => {
                self.log.error(
                    "Cloud forbids batch uploads for this agent (HTTP 403); backing off",
                    &[],
                );
                Ok(Outcome::Retry)
            }
            424 => match parent_stream_of(resp) {
                Some(parent) => self.park(item, &parent, ctx),
                None => self.refused(item, resp),
            },
            _ => self.refused(item, resp),
        }
    }

    /// RT-1309: how long a live open-check batch waited in the outbox and how long its POST
    /// took, so a site's end-to-end latency can be read from its log; RT-1318: and whether
    /// the POST reused a warm connection or paid a new TCP+TLS handshake.
    fn log_open_timing(
        &self,
        item: &OutboxItem,
        post: Duration,
        reused: bool,
    ) -> Result<(), UploadError> {
        let queued_ms = self
            .outbox
            .created_utc(item.id)?
            .and_then(|t| DateTimeOffset::parse(&t))
            .map_or(-1, |t| {
                DateTimeOffset::now_utc().unix_millis() - t.unix_millis()
            });
        self.log.info(
            "Batch {Stream}#{Seq} of {Table} applied {QueuedMs} ms after it was queued (POST {PostMs} ms, connection {Connection})",
            &[
                Value::from(&item.stream),
                Value::from(item.seq),
                Value::from(table_of(&item.payload).unwrap_or_else(|| "?".to_owned())),
                Value::from(queued_ms),
                Value::from(i64::try_from(post.as_millis()).unwrap_or(i64::MAX)),
                Value::from(if reused { "reused" } else { "new" }),
            ],
        );
        Ok(())
    }

    /// Any other refusal: retried in place when transient, else quarantined.
    fn refused(&self, item: &OutboxItem, resp: &SyncResponse) -> Result<Outcome, UploadError> {
        if is_transient(resp.status) {
            return Ok(Outcome::Retry);
        }
        let (reject_code, reason) = rejection_of(resp);
        self.quarantine_refused(item, resp.status, reject_code.as_deref(), reason.as_deref())?;
        Ok(Outcome::Done)
    }

    fn quarantine_refused(
        &self,
        item: &OutboxItem,
        status: u16,
        reject_code: Option<&str>,
        reason: Option<&str>,
    ) -> Result<(), UploadError> {
        self.outbox.quarantine(
            item.id,
            &format!(
                "permanent HTTP {status} {}",
                reject_code.unwrap_or("without code")
            ),
            false,
        )?;
        self.log.error(
            "Batch {Stream}#{Seq} of {Table} permanently rejected by the cloud ({Code}: HTTP {Status} {RejectCode}): {Reason}; quarantined",
            &[
                Value::from(&item.stream),
                Value::from(item.seq),
                Value::from(table_of(&item.payload).unwrap_or_else(|| "?".to_owned())),
                Value::from(codes::SYNC_BATCH_QUARANTINED),
                Value::from(i32::from(status)),
                Value::from(reject_code.unwrap_or("-")),
                Value::from(reason.unwrap_or("no reason given")),
            ],
        );
        self.recovery_resnapshot(&format!("batch {}#{} quarantined", item.stream, item.seq))
    }

    fn park(&mut self, item: &OutboxItem, parent: &str, ctx: &Ctx) -> Result<Outcome, UploadError> {
        let now = self.now();
        let episode = self.parents.park(&item.stream, parent, now).clone();
        let waited = seconds(episode.waited_ticks(now));
        let resent = self.outbox.stream_resnapshot_finished_at(parent)?;
        if self.parents.parent_gone(&episode, resent, now)
            || self.parents.skip_orphans_early(&episode, now)
        {
            return self.skip_orphans(item, parent, waited, "was resent in full", ctx);
        }
        if let Some(stall) = self.stalled_resend(&episode, now)? {
            self.log.warn(
                "Stream {Stream} waited {WaitedSeconds} s for parent stream {ParentStream} that capture cannot resend ({Reason}): leaving out the rows without a parent ({Code})",
                &[
                    Value::from(&item.stream),
                    Value::from(waited),
                    Value::from(parent),
                    Value::from(stall),
                    Value::from(codes::SYNC_PARENT_PENDING),
                ],
            );
            return self.skip_orphans(item, parent, waited, "could not be resent", ctx);
        }
        self.log.info(
            "Batch {Stream}#{Seq} waits for parent stream {ParentStream} (attempt {Attempts}, {WaitedSeconds} s); next try at {Next:O}",
            &[
                Value::from(&item.stream),
                Value::from(item.seq),
                Value::from(parent),
                Value::from(episode.attempts),
                Value::from(waited),
                Value::from(to_timestamp(episode.next_attempt)),
            ],
        );
        if !self.parents.escalation_due(&episode, now) {
            return Ok(Outcome::Parked);
        }
        if let Some(e) = self.parents.find_mut(&item.stream) {
            e.escalated_at = Some(now);
        }
        let request = self.outbox.request_stream_resnapshot(
            &[parent, item.stream.as_str()],
            &format!(
                "stream {} waited {waited} s for rows of parent stream {parent}",
                item.stream
            ),
            now,
        )?;
        if let Some(e) = self.parents.find_mut(&item.stream) {
            e.reported = true;
        }
        let resent = if request.added.is_empty() {
            "nothing new, already pending".to_owned()
        } else {
            request.added.join(", ")
        };
        self.log.warn(
            "Stream {Stream} waited {WaitedSeconds} s for parent stream {ParentStream} ({Code}, outcome {Outcome}): resending {Resent}",
            &[
                Value::from(&item.stream),
                Value::from(waited),
                Value::from(parent),
                Value::from(codes::SYNC_PARENT_PENDING),
                Value::from("resnapshot_requested"),
                Value::from(resent),
            ],
        );
        Ok(Outcome::Parked)
    }

    /// After the escalation: why the parent resend cannot finish (full outbox or overdue).
    fn stalled_resend(
        &self,
        episode: &ParentPendingEpisode,
        now: DateTimeOffset,
    ) -> Result<Option<&'static str>, UploadError> {
        if episode.escalated_at.is_none() {
            return Ok(None);
        }
        let full = self.outbox.is_full()?;
        Ok(self.parents.parent_resend_stall(episode, full, now))
    }

    fn skip_orphans(
        &mut self,
        item: &OutboxItem,
        parent: &str,
        waited: i64,
        resend: &str,
        ctx: &Ctx,
    ) -> Result<Outcome, UploadError> {
        let bearer = self.tokens.get_access_token(ctx)?;
        let path = format!("{BATCHES_PATH}?{}", sync_streams::SKIP_ORPHANS_QUERY);
        let body = with_t_sent(&item.payload, hop_clock::now_field());
        let resp = self
            .http
            .send(ctx, Method::Post, &path, &body, Some(&bearer))?;
        if resp.is_success() {
            self.outbox.ack(item.id)?;
            self.parents.resolve(&item.stream);
            let now = self.now();
            self.parents.orphans_skipped(&item.stream, now);
            self.log.warn(
                "Batch {Stream}#{Seq} of {Table} applied without its rows whose parent rows of stream {ParentStream} the store no longer has ({Code}, after {WaitedSeconds} s)",
                &[
                    Value::from(&item.stream),
                    Value::from(item.seq),
                    Value::from(table_of(&item.payload).unwrap_or_else(|| "?".to_owned())),
                    Value::from(parent),
                    Value::from(sync_streams::ORPHANS_SKIPPED_CODE),
                    Value::from(waited),
                ],
            );
            return Ok(Outcome::Done);
        }
        if resp.status == 401 {
            self.tokens.invalidate();
        }
        if resp.status == 401 || is_transient(resp.status) {
            return Ok(Outcome::Retry);
        }
        self.parents.resolve(&item.stream);
        let (reject_code, reason) = rejection_of(&resp);
        if resp.status != 424 {
            self.quarantine_refused(item, resp.status, reject_code.as_deref(), reason.as_deref())?;
            return Ok(Outcome::Done);
        }
        let why = format!(
            "parent stream {parent} {resend} and the rows are still missing after {waited} s: {}",
            reason.as_deref().unwrap_or("-")
        );
        self.quarantine_refused(item, resp.status, Some(PARENT_GONE_CODE), Some(&why))?;
        Ok(Outcome::Done)
    }

    /// A batch of the stream applied: its parent-pending episode, if any, is over.
    fn parent_arrived(&mut self, stream: &str) {
        let Some(episode) = self.parents.resolve(stream) else {
            return;
        };
        let waited = seconds(episode.waited_ticks(self.now()));
        if episode.reported {
            self.log.info(
                "Stream {Stream} received its parent rows from stream {ParentStream} after {WaitedSeconds} s",
                &[
                    Value::from(stream),
                    Value::from(&episode.parent_stream),
                    Value::from(waited),
                ],
            );
            return;
        }
        self.log.warn(
            "Stream {Stream} waited {WaitedSeconds} s for parent stream {ParentStream} ({Code}, outcome {Outcome}) over {Attempts} attempts",
            &[
                Value::from(stream),
                Value::from(waited),
                Value::from(&episode.parent_stream),
                Value::from(codes::SYNC_PARENT_PENDING),
                Value::from("resolved"),
                Value::from(episode.attempts),
            ],
        );
    }

    fn handle_seq_conflict(
        &self,
        item: &OutboxItem,
        resp: &SyncResponse,
    ) -> Result<Outcome, UploadError> {
        let Some(expected) = try_expected_seq(resp) else {
            self.outbox
                .quarantine(item.id, "409 without expected_seq", false)?;
            self.recovery_resnapshot(&format!(
                "batch {}#{} quarantined: 409 without expected_seq",
                item.stream, item.seq
            ))?;
            return Ok(Outcome::Done);
        };
        // Everything below expected_seq is already applied cloud-side.
        self.outbox.ack_below(&item.stream, expected)?;
        let quarantined = self.outbox.is_quarantined(&item.stream, expected)?;
        if self.outbox.has(&item.stream, expected)? && !quarantined {
            self.log.warn(
                "Seq gap on {Stream}: cloud expects {Expected}, had {Seq}; re-aligned",
                &[
                    Value::from(&item.stream),
                    Value::from(expected),
                    Value::from(item.seq),
                ],
            );
            return Ok(Outcome::Done);
        }
        self.heal_gap(item, expected, quarantined)
    }

    /// No sendable batch carries the seq the cloud expects: renumber the stream's queue
    /// from it (AD1).
    fn heal_gap(
        &self,
        item: &OutboxItem,
        expected: i64,
        quarantined: bool,
    ) -> Result<Outcome, UploadError> {
        let cause = if quarantined {
            "was quarantined after a permanent rejection"
        } else {
            "is no longer queued"
        };
        self.log.warn(
            "Stream {Stream} has a seq gap ({Code}): the cloud expects #{Expected}, which {Cause}; renumbering the queued batches from there",
            &[
                Value::from(&item.stream),
                Value::from(codes::SYNC_STREAM_GAP),
                Value::from(expected),
                Value::from(cause),
            ],
        );
        let result = match self.outbox.resequence(&item.stream, expected) {
            Ok(r) => r,
            Err(e @ (OutboxError::Sqlite(_) | OutboxError::Corrupt(_))) => {
                self.log.error_ex(
                    Exception::from_error(&e),
                    "Stream {Stream} could not be renumbered from #{Expected} ({Code}); backing off and retrying",
                    &[
                        Value::from(&item.stream),
                        Value::from(expected),
                        Value::from(codes::SYNC_STREAM_RECOVERY_FAILED),
                    ],
                );
                return Ok(Outcome::Retry);
            }
            Err(e) => return Err(e.into()),
        };
        // A quarantined batch's rows come back through the resnapshot its quarantine requested; lost ones need one now.
        if !quarantined {
            self.recovery_resnapshot(&format!(
                "cloud expects {}#{expected} which is no longer queued",
                item.stream
            ))?;
        }
        self.log.warn(
            "Stream {Stream} recovered ({Code}): {Renumbered} queued batches renumbered from #{Expected}, {SetAside} quarantined set aside",
            &[
                Value::from(&item.stream),
                Value::from(codes::SYNC_STREAM_RECOVERED),
                Value::from(result.renumbered),
                Value::from(expected),
                Value::from(result.set_aside),
            ],
        );
        Ok(Outcome::Repeek)
    }

    /// Requests a resnapshot unless one was requested automatically within the cooldown;
    /// then defers it.
    fn recovery_resnapshot(&self, reason: &str) -> Result<(), UploadError> {
        let now = self.now();
        let last = self.outbox.last_recovery_resnapshot()?;
        match last {
            Some(last) if now.ticks_since(last) < ticks(RECOVERY_RESNAPSHOT_COOLDOWN) => {
                self.outbox.defer_recovery_resnapshot()?;
                let due = last
                    .checked_add(RECOVERY_RESNAPSHOT_COOLDOWN)
                    .unwrap_or(last);
                self.log.warn(
                    "Resnapshot after {Reason} deferred until {Due:O}: the last automatic one was requested at {Last:O}",
                    &[
                        Value::from(reason),
                        Value::from(to_timestamp(due)),
                        Value::from(to_timestamp(last)),
                    ],
                );
            }
            _ => self.outbox.request_recovery_resnapshot(reason, now)?,
        }
        Ok(())
    }

    fn promote_deferred_resnapshot(&self) -> Result<(), UploadError> {
        if !self.outbox.recovery_resnapshot_deferred()? {
            return Ok(());
        }
        let now = self.now();
        if let Some(last) = self.outbox.last_recovery_resnapshot()?
            && now.ticks_since(last) < ticks(RECOVERY_RESNAPSHOT_COOLDOWN)
        {
            return Ok(());
        }
        self.outbox
            .request_recovery_resnapshot("a deferred recovery resnapshot is due", now)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(status: u16, body: &str) -> SyncResponse {
        SyncResponse {
            status,
            body: body.as_bytes().to_vec(),
            reused: false,
        }
    }

    #[test]
    fn rejection_reason_is_read_from_either_envelope_and_capped() {
        assert_eq!(
            rejection_of(&r(400, r#"{"code":"C","detail":"problem detail"}"#)),
            (Some("C".into()), Some("problem detail".into()))
        );
        assert_eq!(
            rejection_of(&r(400, "<html>bad gateway</html>")),
            (None, None)
        );
        assert_eq!(rejection_of(&r(400, "")), (None, None));
        let long = "r".repeat(1000);
        let (_, reason) = rejection_of(&r(400, &format!(r#"{{"message":"{long}"}}"#)));
        assert_eq!(reason.unwrap().len(), MAX_REASON);
    }

    #[test]
    fn only_a_parent_pending_answer_names_a_parent_stream() {
        let p = |b: &str| parent_stream_of(&r(424, b));
        assert_eq!(
            p(r#"{"code":"SYNC_PARENT_PENDING","parent_stream":"Invoice"}"#).as_deref(),
            Some("Invoice")
        );
        assert_eq!(p(r#"{"code":"OTHER","parent_stream":"Invoice"}"#), None);
        assert_eq!(p(r#"{"code":"SYNC_PARENT_PENDING"}"#), None);
        assert_eq!(
            p(r#"{"code":"SYNC_PARENT_PENDING","parent_stream":"!quarantined:Trn"}"#),
            None
        );
        assert_eq!(
            p(&format!(
                r#"{{"code":"SYNC_PARENT_PENDING","parent_stream":"{}"}}"#,
                "s".repeat(65)
            )),
            None
        );
        assert_eq!(p("<html>failed dependency</html>"), None);
        assert_eq!(p(""), None);
    }

    #[test]
    fn expected_seq_is_found_in_either_spelling_and_nested() {
        assert_eq!(
            try_expected_seq(&r(409, r#"{"result":409,"expected_seq":3}"#)),
            Some(3)
        );
        assert_eq!(
            try_expected_seq(&r(409, r#"{"data":{"expectedSeq":7}}"#)),
            Some(7)
        );
        assert_eq!(try_expected_seq(&r(409, r#"{"Expected_Seq":4}"#)), Some(4));
        assert_eq!(try_expected_seq(&r(409, r#"{"expected_seq":"3"}"#)), None);
        assert_eq!(try_expected_seq(&r(409, r#"{"expected_seq":1.5}"#)), None);
        assert_eq!(try_expected_seq(&r(409, "")), None);
    }

    #[test]
    fn table_of_reads_the_batch_table() {
        assert_eq!(
            table_of(br#"{"table":"Invoice"}"#).as_deref(),
            Some("Invoice")
        );
        assert_eq!(table_of(b"[1]"), None);
        assert_eq!(table_of(b"nope"), None);
    }
}
