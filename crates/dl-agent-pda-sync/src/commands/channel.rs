//! The cloud command channel (`CommandChannel.cs`): receives commands over a WebSocket
//! (falling back to long-poll after repeated failures) and acks the result. A terminal
//! result (applied/rejected) is cached per process so a redelivery gets it back; across
//! restarts the executor's command log answers instead. An expired command is rejected
//! unexecuted unless it already ran, and a command written for a newer contract than this
//! agent knows is rejected (ADR 0029). A failed/unknown outcome is not cached: the cloud
//! retries it. A command addressed to another tenant than the enrolled one is rejected
//! unexecuted, and each result carries how long the store took (ADR 0100).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dl_json::Value as Json;
use dl_uuid::Uuid;

use super::error::CommandError;
use super::executor::{CommandExecutor, utc_now};
use super::result_outbox::{HttpResultPoster, ResultOutbox, ResultPoster};
use crate::config::time::SharedClock;
use crate::config::{AgentError, AgentOptions, codes};
use crate::contracts::WireJson;
use crate::contracts::wire::{
    CommandStatus, SyncCommand, SyncCommandResult, site_command_contract,
};
use crate::reporting::{Exception, Level, Logger, Value};
use crate::transport::signer::{NONCE_HEADER, SIGNATURE_HEADER, TIMESTAMP_HEADER};
use crate::transport::{RetryPolicy, SyncHttpClient, TokenProvider};
use crate::util::ctx::Ctx;

/// The WebSocket of the command channel.
pub const WEB_SOCKET_PATH: &str = "/sync/v1/commands/ws";
/// The long-poll fallback.
pub const POLL_PATH: &str = "/sync/v1/commands/poll?wait=25";
/// Where a polled command's result is acked.
pub const RESULT_PATH: &str = "/sync/v1/commands/result";
/// Results remembered for redeliveries.
pub const MAX_REMEMBERED: usize = 2000;
/// Category of the channel's log lines.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Commands.CommandChannel";
/// `ClientWebSocketOptions.KeepAliveInterval`.
pub const KEEP_ALIVE: Duration = Duration::from_secs(20);
/// A link up this long that then drops is reconnected at once.
pub const STABLE_LINK: Duration = Duration::from_secs(5);
/// How often a blocked WebSocket read wakes up to check for a stop.
const READ_POLL: Duration = Duration::from_secs(1);

/// Failures in a row after one more failed attempt (`NextFailureCount`). A link that
/// came up and later dropped starts a fresh count, so only consecutive failures to connect
/// move the channel to long-poll.
pub fn next_failure_count(failures: i32, link_was_up: bool) -> i32 {
    if link_was_up {
        1
    } else {
        failures.saturating_add(1)
    }
}

/// Why a channel attempt ended.
#[derive(Debug)]
pub enum ChannelError {
    /// The WebSocket failed (`WebSocketException`, `IOException`).
    WebSocket(dl_ws::Error),
    /// A cloud call failed (`HttpRequestException`, `AgentException`).
    Transport(crate::transport::Error),
    /// A coded failure (`AgentException`).
    Agent(AgentError),
    /// The context ended.
    Cancelled,
}

impl ChannelError {
    /// Whether the C# loop catches this failure and reconnects (the rest escape to the
    /// resilient service, which restarts the loop).
    fn is_link_failure(&self) -> bool {
        match self {
            ChannelError::WebSocket(_) | ChannelError::Agent(_) => true,
            ChannelError::Transport(e) => e.agent().is_some(),
            ChannelError::Cancelled => false,
        }
    }

    pub(super) fn to_exception(&self) -> Exception {
        match self {
            ChannelError::WebSocket(e) => {
                Exception::new("System.Net.WebSockets.WebSocketException", e.to_string())
            }
            ChannelError::Transport(e) => e.to_exception(),
            ChannelError::Agent(e) => Exception::from_agent_error(e),
            ChannelError::Cancelled => Exception::new(
                "System.OperationCanceledException",
                "The operation was canceled.",
            ),
        }
    }
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelError::WebSocket(e) => e.fmt(f),
            ChannelError::Transport(e) => e.fmt(f),
            ChannelError::Agent(e) => e.fmt(f),
            ChannelError::Cancelled => f.write_str("The operation was canceled."),
        }
    }
}

impl std::error::Error for ChannelError {}

impl From<dl_ws::Error> for ChannelError {
    fn from(e: dl_ws::Error) -> Self {
        ChannelError::WebSocket(e)
    }
}

impl From<crate::transport::Error> for ChannelError {
    fn from(e: crate::transport::Error) -> Self {
        match e {
            crate::transport::Error::Cancelled => ChannelError::Cancelled,
            other => ChannelError::Transport(other),
        }
    }
}

#[derive(Default)]
struct Remembered {
    done: HashMap<Uuid, SyncCommandResult>,
    order: VecDeque<Uuid>,
}

/// The parts of a [`CommandChannel`].
pub struct CommandChannelParts {
    /// The signed cloud client (its base URL is the cloud's).
    pub http: Arc<SyncHttpClient>,
    /// Access tokens.
    pub tokens: Arc<TokenProvider>,
    /// Runs the commands.
    pub executor: Arc<dyn CommandExecutor>,
    /// Backoff between reconnects.
    pub retry: RetryPolicy,
    /// `Agent:WebSocketFailuresBeforePoll`.
    pub options: AgentOptions,
    /// `TimeProvider`.
    pub clock: SharedClock,
    /// The enrolled tenant; a command for another tenant is refused.
    pub tenant_id: Option<String>,
}

/// The command channel.
pub struct CommandChannel {
    parts: CommandChannelParts,
    log: Logger,
    remembered: Mutex<Remembered>,
    outbox: ResultOutbox,
}

impl CommandChannel {
    /// A channel over `parts`; `log` is the root logger.
    pub fn new(parts: CommandChannelParts, log: &Logger) -> CommandChannel {
        let poster = Arc::new(HttpResultPoster::new(
            parts.http.clone(),
            parts.tokens.clone(),
        ));
        CommandChannel::with_poster(parts, log, poster)
    }

    /// A channel whose results are posted through `poster` (tests).
    pub fn with_poster(
        parts: CommandChannelParts,
        log: &Logger,
        poster: Arc<dyn ResultPoster>,
    ) -> CommandChannel {
        let outbox = ResultOutbox::new(poster, parts.retry.clone(), log);
        CommandChannel {
            parts,
            log: log.for_category(CATEGORY),
            remembered: Mutex::new(Remembered::default()),
            outbox,
        }
    }

    /// Results the cloud has not acknowledged yet.
    pub fn pending_results(&self) -> Vec<Uuid> {
        self.outbox.pending_ids()
    }

    /// Answers `command` (a redelivery gets the stored result, never a second run) and
    /// keeps the answer until the cloud acknowledged it. `None` when `ctx` ended.
    pub fn answer(&self, command: &SyncCommand, ctx: &Ctx) -> Option<SyncCommandResult> {
        let result = self.handle(command, ctx)?;
        self.outbox.enqueue(&result);
        Some(result)
    }

    /// Posts the results still waiting (all of them when `force`).
    pub fn flush_results(&self, ctx: &Ctx, force: bool) -> Result<(), ChannelError> {
        self.outbox.flush(ctx, force)
    }

    /// Runs until `ctx` ends (`RunAsync`). A failure the C# loop does not catch is
    /// returned (the resilient service restarts the loop).
    pub fn run(&self, ctx: &Ctx) -> Result<(), ChannelError> {
        let before_poll = self.parts.options.web_socket_failures_before_poll;
        let mut failures: i32 = 0;
        let mut poll_cycles = 0;
        while !ctx.is_done() {
            // Results the last link could not deliver go out first (RT-1309).
            if let Err(ChannelError::Cancelled) = self.outbox.flush(ctx, false) {
                return Ok(());
            }
            let mut link_was_up = false;
            let attempt_started = Instant::now();
            let attempt = if failures < before_poll {
                self.run_web_socket(ctx, &mut link_was_up).map(|()| {
                    failures = 0;
                })
            } else {
                self.poll_once(ctx).map(|()| {
                    poll_cycles += 1;
                    if poll_cycles >= 5 {
                        poll_cycles = 0;
                        failures = before_poll - 1;
                    }
                })
            };
            match attempt {
                Ok(()) => {}
                Err(_) if ctx.is_done() => return Ok(()),
                Err(ChannelError::Cancelled) => return Ok(()),
                Err(e) if e.is_link_failure() => {
                    failures = next_failure_count(failures, link_was_up);
                    // The dead link's results are posted before anything else.
                    if let Err(ChannelError::Cancelled) = self.outbox.flush(ctx, true) {
                        return Ok(());
                    }
                    let up_for = link_was_up.then(|| attempt_started.elapsed());
                    let delay = reconnect_delay(&self.parts.retry, failures, up_for);
                    if failures == 1 {
                        self.log.write(
                            Level::Information,
                            Some(e.to_exception()),
                            "Command channel dropped; reconnecting in {Delay}",
                            &[Value::time_span(delay)],
                        );
                    } else {
                        self.log.warn_ex(
                            e.to_exception(),
                            "Command channel failed ({Failures} in a row); retrying in {Delay}",
                            &[Value::from(failures), Value::time_span(delay)],
                        );
                    }
                    if !(self.parts.retry.sleep)(delay, ctx) {
                        return Ok(());
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// The `ws`/`wss` URL of the command socket.
    pub fn web_socket_url(&self) -> String {
        web_socket_url(&self.parts.http.base_url().to_string())
    }

    fn run_web_socket(&self, ctx: &Ctx, link_was_up: &mut bool) -> Result<(), ChannelError> {
        let bearer = self.parts.tokens.get_access_token(ctx)?;
        let h = self.parts.http.sign_only("GET", WEB_SOCKET_PATH)?;
        let options = dl_ws::ConnectOptions {
            headers: vec![
                ("Authorization".into(), format!("Bearer {bearer}")),
                (TIMESTAMP_HEADER.into(), h.timestamp),
                (NONCE_HEADER.into(), h.nonce),
                (SIGNATURE_HEADER.into(), h.signature),
            ],
            timeout: Some(Duration::from_secs(100)),
            ..Default::default()
        };
        let client = dl_http::Client::with_options(crate::transport::http::client_options(
            crate::transport::http::SERVICE_TIMEOUT,
        ));
        let (mut ws, _resp) = dl_ws::connect_with(&client, &self.web_socket_url(), &options)?;
        self.log.info("Command channel connected", &[]);
        *link_was_up = true;
        ws.set_read_timeout(Some(READ_POLL))?;
        // Pending results first: the answers the previous link lost go out before new work.
        self.outbox.flush(ctx, true)?;
        let mut last_sent = Instant::now();
        loop {
            if ctx.is_done() {
                let _ = ws.close(Some(dl_ws::CloseFrame::normal()));
                return Err(ChannelError::Cancelled);
            }
            let text = match ws.read() {
                Ok(dl_ws::Message::Text(t)) => t,
                Ok(dl_ws::Message::Binary(b)) => String::from_utf8_lossy(&b).into_owned(),
                Ok(dl_ws::Message::Close(_)) => {
                    // The peer's close frame was echoed: the session ended cleanly.
                    return Ok(());
                }
                Ok(_) => continue,
                Err(e) if e.is_timeout() => {
                    self.outbox.flush(ctx, false)?;
                    if last_sent.elapsed() >= KEEP_ALIVE {
                        ws.pong(&[])?;
                        last_sent = Instant::now();
                    }
                    continue;
                }
                Err(dl_ws::Error::Closed) => return Ok(()),
                Err(e) => return Err(e.into()),
            };
            let Ok(command) = SyncCommand::from_json(&text) else {
                // RT-1309: an unread command with an id still gets a terminal answer.
                if let Some(result) = dl_json::parse(&text).ok().and_then(|v| self.unreadable(&v)) {
                    self.outbox.enqueue(&result);
                    let sent = ws.send_text(&result.to_json());
                    self.outbox.flush(ctx, false)?;
                    sent?;
                    last_sent = Instant::now();
                }
                continue;
            };
            let Some(result) = self.answer(&command, ctx) else {
                return Err(ChannelError::Cancelled);
            };
            // Fast path over the socket; the HTTP post is the acknowledged one. A dead
            // socket must not lose the result: it is posted before the link is dropped.
            let sent = ws.send_text(&result.to_json());
            self.outbox.flush(ctx, false)?;
            sent?;
            last_sent = Instant::now();
        }
    }

    /// One long-poll round (`PollOnceAsync`).
    pub fn poll_once(&self, ctx: &Ctx) -> Result<(), ChannelError> {
        let bearer = self.parts.tokens.get_access_token(ctx)?;
        let resp = self.parts.http.get(ctx, POLL_PATH, Some(&bearer))?;
        if resp.status == 401 {
            self.parts.tokens.invalidate();
            return Ok(());
        }
        if !resp.is_success() {
            return Err(ChannelError::Agent(AgentError::new(
                codes::CLOUD_UNREACHABLE,
                format!("Command poll failed with HTTP {}.", resp.status),
            )));
        }
        let Some(Json::Array(list)) = resp.data()? else {
            return Ok(());
        };
        for item in list {
            let Ok(command) = SyncCommand::from_value(&item) else {
                if let Some(result) = self.unreadable(&item) {
                    self.outbox.enqueue(&result);
                    self.outbox.flush(ctx, false)?;
                }
                continue;
            };
            if self.answer(&command, ctx).is_none() {
                return Err(ChannelError::Cancelled);
            }
            self.outbox.flush(ctx, false)?;
        }
        self.outbox.flush(ctx, false)?;
        Ok(())
    }

    /// The answer to a command this agent cannot read (RT-1309): rejected as
    /// `invalid_payload` when its `command_id` can still be read, so the cloud gets a
    /// terminal answer instead of letting the lease expire; `None` (logged) otherwise.
    pub fn unreadable(&self, frame: &Json) -> Option<SyncCommandResult> {
        let Some(id) = frame
            .get("command_id")
            .and_then(Json::as_str)
            .and_then(|s| s.parse::<Uuid>().ok())
        else {
            self.log.warn(
                "Ignoring a command frame without a readable command_id",
                &[],
            );
            return None;
        };
        self.log.warn(
            "Command {Id} could not be read by this agent; it is rejected",
            &[Value::from(id)],
        );
        Some(SyncCommandResult {
            command_id: id,
            status: CommandStatus::Rejected,
            reason: Some(format!(
                "{}: the command could not be read by this agent",
                super::executor::reasons::INVALID_PAYLOAD
            )),
            result: None,
            at_utc: utc_now(&self.parts.clock),
            exec_ms: None,
        })
    }

    /// Decides one command (`HandleAsync`); `None` when `ctx` ended while it ran (the C#
    /// `OperationCanceledException`, nothing is acked).
    pub fn handle(&self, command: &SyncCommand, ctx: &Ctx) -> Option<SyncCommandResult> {
        if let Some(stored) = self.lock().done.get(&command.command_id) {
            return Some(stored.clone());
        }
        let result = self.decide(command, ctx)?;
        if matches!(
            result.status,
            CommandStatus::Applied | CommandStatus::Rejected
        ) {
            self.remember(&result);
        }
        Some(result)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Remembered> {
        self.remembered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn decide(&self, command: &SyncCommand, ctx: &Ctx) -> Option<SyncCommandResult> {
        if command.contract_version > site_command_contract::VERSION {
            self.log.warn(
                "Command {Id} needs contract v{Version}; this agent knows v{Known}",
                &[
                    Value::from(command.command_id),
                    Value::from(command.contract_version),
                    Value::from(site_command_contract::VERSION),
                ],
            );
            return Some(self.refuse(command, "contract_version_unsupported"));
        }
        // Fail closed: an enrolled tenant that is not the command's (or not a tenant id at
        // all) refuses the command.
        if let (Some(target), Some(tenant)) = (command.tenant_id, &self.parts.tenant_id) {
            let matches = tenant
                .trim()
                .parse::<Uuid>()
                .is_ok_and(|enrolled| enrolled == target);
            if !matches {
                self.log.error(
                    "Command {Id} is addressed to tenant {Target}; this agent is enrolled for {Enrolled}",
                    &[
                        Value::from(command.command_id),
                        Value::from(target),
                        Value::from(tenant.as_str()),
                    ],
                );
                return Some(self.refuse(command, "tenant_mismatch"));
            }
        }
        if let Some(exp) = command.expires_at_utc
            && exp <= utc_now(&self.parts.clock)
        {
            return self.recorded_or_expired(command, ctx);
        }
        self.execute_safely(command, ctx)
    }

    fn recorded_or_expired(&self, command: &SyncCommand, ctx: &Ctx) -> Option<SyncCommandResult> {
        match self.parts.executor.find_recorded(command.command_id, ctx) {
            Ok(Some(r)) => Some(r),
            Ok(None) => Some(self.refuse(command, "expired")),
            Err(CommandError::Cancelled) => None,
            Err(e) => {
                self.log.error_ex(
                    e.to_exception(),
                    "Command {CommandId} expired and its log could not be read; reporting unknown outcome ({Code})",
                    &[
                        Value::from(command.command_id),
                        Value::from(codes::COMMAND_REPLAY_FAILED),
                    ],
                );
                Some(self.unknown(command, &e))
            }
        }
    }

    fn execute_safely(&self, command: &SyncCommand, ctx: &Ctx) -> Option<SyncCommandResult> {
        let started = Instant::now();
        match self.parts.executor.execute(command, ctx) {
            Ok(mut result) => {
                let elapsed = started.elapsed();
                if elapsed > site_command_contract::SLOW_COMMAND {
                    self.log.warn(
                        "Command {CommandId} ({Type}) took {ElapsedMs} ms against the store (budget {BudgetMs} ms)",
                        &[
                            Value::from(command.command_id),
                            Value::from(command.type_.as_str()),
                            Value::from(millis(elapsed)),
                            Value::from(millis(site_command_contract::SLOW_COMMAND)),
                        ],
                    );
                }
                result.exec_ms = Some(millis(elapsed));
                Some(result)
            }
            Err(CommandError::Cancelled) if ctx.is_done() => None,
            Err(e) => {
                // Outcome unknown: never blindly retried (spec section 5); a redelivery
                // consults the command log first.
                self.log.error_ex(
                    e.to_exception(),
                    "Command {CommandId} threw; reporting unknown outcome ({Code})",
                    &[
                        Value::from(command.command_id),
                        Value::from(codes::COMMAND_REPLAY_FAILED),
                    ],
                );
                Some(self.unknown(command, &e))
            }
        }
    }

    fn refuse(&self, command: &SyncCommand, reason: &str) -> SyncCommandResult {
        SyncCommandResult {
            command_id: command.command_id,
            status: CommandStatus::Rejected,
            reason: Some(reason.to_owned()),
            result: None,
            at_utc: utc_now(&self.parts.clock),
            exec_ms: None,
        }
    }

    /// `ex.GetType().Name` as the reason.
    fn unknown(&self, command: &SyncCommand, e: &CommandError) -> SyncCommandResult {
        let full = e.to_exception().type_name;
        let short = full.rsplit('.').next().unwrap_or(&full).to_owned();
        SyncCommandResult {
            command_id: command.command_id,
            status: CommandStatus::Unknown,
            reason: Some(short),
            result: None,
            at_utc: utc_now(&self.parts.clock),
            exec_ms: None,
        }
    }

    fn remember(&self, result: &SyncCommandResult) {
        let mut r = self.lock();
        r.done.insert(result.command_id, result.clone());
        r.order.push_back(result.command_id);
        while r.order.len() > MAX_REMEMBERED {
            if let Some(old) = r.order.pop_front() {
                r.done.remove(&old);
            }
        }
    }
}

/// How long to wait before reconnecting after a failed attempt. A link that was up for at
/// least [`STABLE_LINK`] and dropped reconnects at once (no fixed wait); a link that never
/// came up, or died at once, backs off.
pub fn reconnect_delay(retry: &RetryPolicy, failures: i32, up_for: Option<Duration>) -> Duration {
    if up_for.is_some_and(|d| d >= STABLE_LINK) {
        return Duration::ZERO;
    }
    retry.delay_for(u32::try_from(failures.clamp(0, 8)).unwrap_or(8), None)
}

/// `new UriBuilder(new Uri(baseUri, WebSocketPath)) { Scheme = https ? "wss" : "ws" }`:
/// the cloud's scheme and authority with the socket path.
pub fn web_socket_url(base: &str) -> String {
    let (scheme, rest) = base.split_once("://").unwrap_or(("http", base));
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let ws = if scheme.eq_ignore_ascii_case("https") {
        "wss"
    } else {
        "ws"
    };
    format!("{ws}://{authority}{WEB_SOCKET_PATH}")
}

/// `(long)elapsed.TotalMilliseconds`.
fn millis(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}
