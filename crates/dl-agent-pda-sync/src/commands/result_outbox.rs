//! Durable delivery of command results (RT-1309). A result is never only "sent": it is
//! kept here until the cloud acknowledged it over HTTP (`POST /sync/v1/commands/result`),
//! so a result written to a dead or replaced link (blue-green deploy, half-open socket) is
//! posted again, with backoff, and is flushed first when the link comes back. The cloud
//! stores a result idempotently per command id, so a result delivered over both the
//! socket and HTTP, or posted twice, is harmless.
//!
//! RT-1318: a result written to the live command socket is the fast path; its HTTP post
//! waits [`SOCKET_CONFIRM_WAIT`] first and is skipped when the cloud confirms the socket
//! delivery with a `{"result_ack":"<command_id>"}` frame. A failed socket write leaves
//! the result due at once.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dl_uuid::Uuid;

use super::channel::{ChannelError, RESULT_PATH};
use crate::contracts::WireJson;
use crate::contracts::wire::SyncCommandResult;
use crate::reporting::{Logger, Value};
use crate::transport::{RetryPolicy, SyncHttpClient, TokenProvider};
use crate::util::ctx::Ctx;

/// Results kept while the cloud cannot be reached; the oldest is dropped (and logged) past it.
pub const MAX_PENDING: usize = 500;
/// How often the poster beside the socket loop looks for due results.
pub const POST_BESIDE_POLL: Duration = Duration::from_millis(50);
/// How long a result written to the command socket waits before its HTTP post (RT-1318):
/// the socket normally delivers it in milliseconds, the post is the fallback for a socket
/// that died with the frame in flight.
pub const SOCKET_CONFIRM_WAIT: Duration = Duration::from_secs(3);
/// The field of the cloud's frame confirming a result it stored from the socket.
pub const RESULT_ACK_FIELD: &str = "result_ack";
/// The longest wait between two posts of the same result.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// What the cloud said to a posted result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostOutcome {
    /// Stored (2xx).
    Acked,
    /// Refused for good (4xx other than 401/408/429): posting again cannot help.
    Refused(u16),
    /// Not stored yet (5xx, 401, 408, 429): post again later.
    Retry(u16),
}

/// Where a result is posted (the cloud's result endpoint; a fake in tests).
pub trait ResultPoster: Send + Sync {
    /// Posts `result`; an error is a transport failure (post again later).
    fn post(&self, result: &SyncCommandResult, ctx: &Ctx) -> Result<PostOutcome, ChannelError>;
}

/// Posts to `POST /sync/v1/commands/result` with the signed client.
pub struct HttpResultPoster {
    http: Arc<SyncHttpClient>,
    tokens: Arc<TokenProvider>,
}

impl HttpResultPoster {
    /// A poster over the channel's client and tokens.
    pub fn new(http: Arc<SyncHttpClient>, tokens: Arc<TokenProvider>) -> HttpResultPoster {
        HttpResultPoster { http, tokens }
    }
}

impl ResultPoster for HttpResultPoster {
    fn post(&self, result: &SyncCommandResult, ctx: &Ctx) -> Result<PostOutcome, ChannelError> {
        let bearer = self.tokens.get_access_token(ctx)?;
        let resp = self
            .http
            .post_json(ctx, RESULT_PATH, &result.to_json(), Some(&bearer))?;
        if resp.status == 401 {
            self.tokens.invalidate();
        }
        Ok(classify(resp.status))
    }
}

/// The command id of a `{"result_ack":"<command_id>"}` frame, `None` for any other frame.
pub fn parse_result_ack(text: &str) -> Option<Uuid> {
    if !text.contains(RESULT_ACK_FIELD) {
        return None;
    }
    dl_json::parse(text)
        .ok()?
        .get(RESULT_ACK_FIELD)?
        .as_str()?
        .parse()
        .ok()
}

/// How an HTTP status answers a posted result.
pub fn classify(status: u16) -> PostOutcome {
    match status {
        200..=299 => PostOutcome::Acked,
        401 | 408 | 429 => PostOutcome::Retry(status),
        400..=499 => PostOutcome::Refused(status),
        _ => PostOutcome::Retry(status),
    }
}

struct Entry {
    result: SyncCommandResult,
    attempts: u32,
    next_at: Instant,
}

/// Results waiting for the cloud's acknowledgement.
pub struct ResultOutbox {
    poster: Arc<dyn ResultPoster>,
    retry: RetryPolicy,
    log: Logger,
    pending: Mutex<VecDeque<Entry>>,
    socket_wait: Duration,
}

impl ResultOutbox {
    /// An empty outbox posting through `poster`.
    pub fn new(poster: Arc<dyn ResultPoster>, retry: RetryPolicy, log: &Logger) -> ResultOutbox {
        ResultOutbox {
            poster,
            retry,
            log: log.clone(),
            pending: Mutex::new(VecDeque::new()),
            socket_wait: SOCKET_CONFIRM_WAIT,
        }
    }

    /// The outbox with another wait between a socket write and the HTTP fallback (tests).
    #[must_use]
    pub fn with_socket_wait(mut self, wait: Duration) -> ResultOutbox {
        self.socket_wait = wait;
        self
    }

    /// [`enqueue`](Self::enqueue) for a result about to be written to the command socket:
    /// its HTTP post waits [`SOCKET_CONFIRM_WAIT`] for the cloud's confirmation instead of
    /// going out at once. When the write fails the link's forced flush posts it at once.
    pub fn enqueue_for_socket(&self, result: &SyncCommandResult) {
        self.enqueue_due(result, Instant::now() + self.socket_wait);
    }

    /// The cloud confirmed it stored the result from the socket: nothing to post. Returns
    /// whether the result was still waiting.
    pub fn confirm(&self, id: Uuid) -> bool {
        let mut q = self.lock();
        let before = q.len();
        q.retain(|e| e.result.command_id != id);
        q.len() != before
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Entry>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps `result` until acknowledged; the same command id again replaces the entry
    /// (a redelivery re-answers) and is due at once.
    pub fn enqueue(&self, result: &SyncCommandResult) {
        self.enqueue_due(result, Instant::now());
    }

    fn enqueue_due(&self, result: &SyncCommandResult, next_at: Instant) {
        let mut q = self.lock();
        q.retain(|e| e.result.command_id != result.command_id);
        q.push_back(Entry {
            result: result.clone(),
            attempts: 0,
            next_at,
        });
        while q.len() > MAX_PENDING {
            if let Some(old) = q.pop_front() {
                self.log.warn(
                    "Result of command {Id} dropped: {Max} results are waiting for the cloud",
                    &[
                        Value::from(old.result.command_id),
                        Value::from(MAX_PENDING as i64),
                    ],
                );
            }
        }
    }

    /// Ids of the results still waiting.
    pub fn pending_ids(&self) -> Vec<Uuid> {
        self.lock().iter().map(|e| e.result.command_id).collect()
    }

    /// Results still waiting.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether nothing waits.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Posts the due results (all of them when `force`) oldest first. A failure is logged
    /// and the result stays with a longer backoff; posting stops at the first transport
    /// failure (the cloud is unreachable). Only a cancelled `ctx` is an error.
    pub fn flush(&self, ctx: &Ctx, force: bool) -> Result<(), ChannelError> {
        let ids: Vec<Uuid> = {
            let now = Instant::now();
            self.lock()
                .iter()
                .filter(|e| force || e.next_at <= now)
                .map(|e| e.result.command_id)
                .collect()
        };
        for id in ids {
            let Some(result) = self
                .lock()
                .iter()
                .find(|e| e.result.command_id == id)
                .map(|e| e.result.clone())
            else {
                continue;
            };
            match self.poster.post(&result, ctx) {
                Ok(PostOutcome::Acked) => self.remove(id),
                Ok(PostOutcome::Refused(status)) => {
                    self.log.warn(
                        "Cloud refused the result of command {Id} with HTTP {Status}; it is dropped",
                        &[Value::from(id), Value::from(status)],
                    );
                    self.remove(id);
                }
                Ok(PostOutcome::Retry(status)) => {
                    self.log.warn(
                        "Result of command {Id} not stored (HTTP {Status}); it is posted again",
                        &[Value::from(id), Value::from(status)],
                    );
                    self.backoff(id);
                }
                Err(ChannelError::Cancelled) => return Err(ChannelError::Cancelled),
                Err(e) => {
                    self.log.warn_ex(
                        e.to_exception(),
                        "Result of command {Id} could not be posted; it is posted again",
                        &[Value::from(id)],
                    );
                    self.backoff(id);
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Runs `work` while a scoped thread posts the due results every
    /// [`POST_BESIDE_POLL`] (RT-1309): the socket loop answers the next command at once
    /// instead of waiting for the previous result's HTTP round trip.
    pub fn post_beside<R>(&self, ctx: &Ctx, work: impl FnOnce() -> R) -> R {
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                while !stop.load(Ordering::SeqCst) && !ctx.is_done() {
                    if let Err(ChannelError::Cancelled) = self.flush(ctx, false) {
                        return;
                    }
                    std::thread::sleep(POST_BESIDE_POLL);
                }
            });
            let out = work();
            stop.store(true, Ordering::SeqCst);
            out
        })
    }

    fn remove(&self, id: Uuid) {
        self.lock().retain(|e| e.result.command_id != id);
    }

    fn backoff(&self, id: Uuid) {
        let mut q = self.lock();
        if let Some(e) = q.iter_mut().find(|e| e.result.command_id == id) {
            e.attempts = e.attempts.saturating_add(1);
            let delay = self
                .retry
                .delay_for(e.attempts.min(8), None)
                .min(MAX_BACKOFF);
            e.next_at = Instant::now() + delay;
        }
    }
}
