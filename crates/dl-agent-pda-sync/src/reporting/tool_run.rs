//! One run of a verb (port of `Reporting/ToolRun.cs`): its JSON-lines log
//! (`logs/tools-.log`), its run id, and its site events (`<VERB>_STARTED`, stage and
//! progress events, `<VERB>_FINISHED` or `<VERB>_FAILED`). Events are queued durably and
//! flushed best-effort (at most [`ToolRunOptions::flush_budget`] each, paused for a minute
//! after a failed flush). Nothing here changes the verb's exit code or fails the verb.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use dl_json::Map;

use super::agent_logging::{
    ToolLogTarget, create_tool_logger, fallback_targets, new_run_id, resolve_tool_target,
    tools_log_file,
};
use super::log::{Exception, Level, Logger, Value};
use super::run_stages::RunStages;
use super::site_event_bridge::SiteEventLogBridge;
use super::site_event_store::{
    DEFAULT_CAPACITY, MemorySiteEventStore, SiteEventStore, SqliteSiteEventStore,
};
use super::site_events::{
    CloudSiteEventTransport, SiteEventFlusher, SiteEventReporter, SiteEventTransport, SiteEvents,
    normalize_code,
};
use crate::config::time::{SharedClock, SystemClock};
use crate::config::{AgentIdentity, AgentPaths};
use crate::contracts::wire::site_event_levels::{ERROR, INFO, WARN};
use crate::security::{AgentKey, names};
use crate::transport::{RequestSigner, RetryPolicy, SyncHttpClient, TokenProvider};
use crate::util::ctx::Ctx;

/// `ILogger<ToolRun>`.
pub const TOOL_RUN_CATEGORY: &str = "MM.PdaOrder.SyncAgent.Reporting.ToolRun";
/// How long flushes pause after a failed one.
pub const FLUSH_PAUSE: Duration = Duration::from_secs(60);
/// `Cli.ExitFailed`.
pub const EXIT_FAILED: i32 = 1;

/// Builds the transport of a run; `None` while there is none (not enrolled).
pub type TransportFactory =
    Arc<dyn Fn(&ToolRun) -> Option<Arc<dyn SiteEventTransport>> + Send + Sync>;

/// Where console lines go.
pub type Console = Arc<Mutex<dyn Write + Send>>;

/// How a run is set up (`ToolRunOptions`).
#[derive(Clone)]
pub struct ToolRunOptions {
    /// One of `site_event_sources`.
    pub source: String,
    /// The verb as typed (`enroll`, `legacy-export`): the run id and event prefix.
    pub verb: String,
    /// The code of `<VERB>_FAILED` when the run ends non-zero without a recorded refusal.
    pub failure_code: String,
    /// The agent data directory: first choice for the log and the queue.
    pub data_dir: Option<String>,
    /// The verb's output or input path: a `logs` folder beside it is the second choice.
    pub output_path: Option<String>,
    /// Console output (stdout).
    pub out: Console,
    /// The clock.
    pub clock: SharedClock,
    /// The temp folder.
    pub temp_dir: PathBuf,
    /// Upper bound of one flush (5 s).
    pub flush_budget: Duration,
    /// The exit code when the verb stops on an unexpected error (`Cli.ExitFailed`).
    pub unexpected_exit_code: i32,
    /// Called before each flush until it returns a transport; default: the enrolled
    /// identity in `data_dir`.
    pub transport: Option<TransportFactory>,
    /// `HeartbeatSender.AgentVersion` for the default transport.
    pub agent_version: String,
}

impl ToolRunOptions {
    /// Options with the C# defaults.
    pub fn new(source: &str, verb: &str, failure_code: &str) -> ToolRunOptions {
        ToolRunOptions {
            source: source.to_owned(),
            verb: verb.to_owned(),
            failure_code: failure_code.to_owned(),
            data_dir: None,
            output_path: None,
            out: Arc::new(Mutex::new(std::io::stdout())),
            clock: SystemClock::shared(),
            temp_dir: std::env::temp_dir(),
            flush_budget: Duration::from_secs(5),
            unexpected_exit_code: EXIT_FAILED,
            transport: None,
            agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

#[derive(Default)]
struct RunState {
    transport: Option<Arc<dyn SiteEventTransport>>,
    pause_until: Option<i128>,
    failure: Option<String>,
    unexpected: Option<Exception>,
    completed: bool,
}

/// One run of a verb (`ToolRun`).
pub struct ToolRun {
    o: ToolRunOptions,
    run_id: String,
    prefix: String,
    target: Option<ToolLogTarget>,
    logger: Logger,
    log: Logger,
    bridge: Arc<SiteEventLogBridge>,
    events: Arc<SiteEventReporter>,
    flusher: SiteEventFlusher,
    started: Instant,
    state: Mutex<RunState>,
}

impl std::fmt::Debug for ToolRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRun")
            .field("run_id", &self.run_id)
            .finish_non_exhaustive()
    }
}

fn short_type(full: &str) -> &str {
    full.rsplit('.').next().unwrap_or(full)
}

impl ToolRun {
    /// Sets up a run: log target, logger with the bridge, durable queue (or memory),
    /// reporter; adopts events stranded by earlier runs. An unknown source fails
    /// (`ArgumentException`).
    pub fn begin(o: ToolRunOptions) -> Result<ToolRun, Box<Exception>> {
        let run_id = new_run_id(&o.verb, o.clock.now());
        let mut problems = Vec::new();
        let target = resolve_tool_target(
            o.data_dir.as_deref(),
            o.output_path.as_deref(),
            &o.temp_dir,
            &mut problems,
        );
        let identity = read_identity(o.data_dir.as_deref(), &mut problems);
        let bridge = Arc::new(SiteEventLogBridge::new(o.clock.clone()));
        let logger = create_tool_logger(
            target.as_ref(),
            &o.source,
            &run_id,
            identity.as_ref(),
            Some(bridge.clone()),
            o.clock.clone(),
        );
        let mut queue_error = None;
        let durable = match &target {
            Some(t) => match SqliteSiteEventStore::try_open(&t.events_db, DEFAULT_CAPACITY) {
                Ok(s) => Some(Arc::new(s)),
                Err(e) => {
                    queue_error = Some(e);
                    None
                }
            },
            None => None,
        };
        let queue: Arc<dyn SiteEventStore> = match &durable {
            Some(d) => d.clone(),
            None => Arc::new(MemorySiteEventStore::default()),
        };
        let events = Arc::new(
            SiteEventReporter::new(queue, &o.source, Some(&run_id), &logger, o.clock.clone())
                .map_err(|m| Box::new(Exception::new("System.ArgumentException", m)))?,
        );
        bridge.attach(events.clone());
        let log = logger.for_category(TOOL_RUN_CATEGORY);
        let flusher = SiteEventFlusher::for_reporter(events.clone(), o.clock.clone(), &log);
        let run = ToolRun {
            prefix: normalize_code(Some(&o.verb)),
            run_id,
            target,
            logger,
            log,
            bridge,
            events,
            flusher,
            started: Instant::now(),
            state: Mutex::default(),
            o,
        };
        for p in problems {
            run.log
                .info("Tools run setup: {Problem}", &[Value::from(p)]);
        }
        match (&run.target, &durable) {
            (None, _) => run.events.report(
                WARN,
                "TOOLS_LOG_UNAVAILABLE",
                "No writable folder for the tools log; this run is not logged to a file",
                None,
            ),
            (Some(t), None) => run.log.warn(
                "Site event queue {Path} could not be opened ({Error}); events of this run are kept in memory",
                &[
                    Value::from(t.events_db.display().to_string()),
                    Value::from(queue_error),
                ],
            ),
            (Some(t), Some(active)) if t.kind == "data" => run.adopt_stranded_events(active),
            _ => {}
        }
        Ok(run)
    }

    /// Events an earlier run left beside its output or in the temp folder move into the
    /// agent's own queue, so they reach the cloud with this run's first flush.
    fn adopt_stranded_events(&self, active: &SqliteSiteEventStore) {
        for t in fallback_targets(self.o.output_path.as_deref(), &self.o.temp_dir) {
            let path = t.events_db.display().to_string();
            let (moved, error) = active.adopt_from(&t.events_db);
            if moved > 0 {
                self.log.info(
                    "Adopted {Count} site event(s) queued by an earlier run in {Path}",
                    &[Value::from(moved), Value::from(path.clone())],
                );
            }
            if let Some(e) = error {
                self.log.info(
                    "Site events queued by an earlier run in {Path} were left there ({Error})",
                    &[Value::from(path), Value::from(e)],
                );
            }
        }
    }

    /// The whole run: [`Self::begin`], [`Self::start`], the verb, [`Self::complete`]. An
    /// unexpected error of the verb is logged, reported as `<VERB>_FAILED` with the
    /// failure code, and returned.
    pub fn execute(
        o: ToolRunOptions,
        start_detail: Option<Map>,
        verb: impl FnOnce(&ToolRun) -> Result<i32, Box<Exception>>,
    ) -> Result<i32, Box<Exception>> {
        let unexpected_exit = o.unexpected_exit_code;
        let run = ToolRun::begin(o)?;
        run.start(start_detail);
        match verb(&run) {
            Ok(exit) => Ok(run.complete(exit)),
            Err(error) => {
                run.log.error_ex(
                    (*error).clone(),
                    "{Verb} stopped on an unexpected {ExceptionType}",
                    &[
                        Value::from(&run.o.verb),
                        Value::from(short_type(&error.type_name)),
                    ],
                );
                run.lock().unexpected = Some((*error).clone());
                run.complete(unexpected_exit);
                Err(error)
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, RunState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The run id.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Event code prefix: `ENROLL`, `LEGACY_EXPORT`, `LEGACY_ARCHIVE`.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Where the log and the queue live; `None` when no folder was writable.
    pub fn target(&self) -> Option<&ToolLogTarget> {
        self.target.as_ref()
    }

    /// The run's root logger (for the workflow classes: `for_category` it).
    pub fn logger(&self) -> &Logger {
        &self.logger
    }

    /// The reporter.
    pub fn events(&self) -> &Arc<SiteEventReporter> {
        &self.events
    }

    /// Reports `<VERB>_STARTED` and tries a first flush.
    pub fn start(&self, detail: Option<Map>) {
        self.log.info(
            "{Verb} run {RunId} started",
            &[Value::from(&self.o.verb), Value::from(&self.run_id)],
        );
        self.stage(
            INFO,
            &format!("{}_STARTED", self.prefix),
            &format!("{} started", self.o.verb),
            detail,
        );
    }

    /// Sets the FAILED code without another event.
    pub fn record_failure(&self, code: &str) {
        self.lock().failure = Some(code.to_owned());
    }

    /// The identity in the data directory changed: the next flush builds a new transport.
    pub fn identity_changed(&self) {
        let mut st = self.lock();
        st.transport = None;
        st.pause_until = None;
    }

    /// Reports `<VERB>_FINISHED` (exit 0) or `<VERB>_FAILED`, flushes, and prints the log
    /// path and the run id. Returns `exit_code` unchanged.
    pub fn complete(&self, exit_code: i32) -> i32 {
        let (failure, unexpected) = {
            let mut st = self.lock();
            if st.completed {
                return exit_code;
            }
            st.completed = true;
            (st.failure.clone(), st.unexpected.clone())
        };
        let ms = i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let mut detail = Map::new();
        if exit_code == 0 {
            detail.insert("exit_code", 0);
            detail.insert("duration_ms", ms);
            self.events.report(
                INFO,
                &format!("{}_FINISHED", self.prefix),
                &format!("{} finished", self.o.verb),
                Some(detail),
            );
        } else {
            let code = failure.unwrap_or_else(|| self.o.failure_code.clone());
            detail.insert("code", code.as_str());
            detail.insert("exit_code", exit_code);
            detail.insert("duration_ms", ms);
            let mut message = format!("{} failed with {code} (exit {exit_code})", self.o.verb);
            if let Some(ex) = unexpected {
                // One FAILED event that names the exception, not a separate event.
                detail.insert("exception_type", ex.type_name.as_str());
                message = format!(
                    "{} stopped on an unexpected {}: {}",
                    self.o.verb,
                    short_type(&ex.type_name),
                    ex.message
                );
            }
            self.events.report(
                ERROR,
                &format!("{}_FAILED", self.prefix),
                &message,
                Some(detail),
            );
        }
        self.bridge.drain();
        self.flush(true);
        self.print_footer();
        exit_code
    }

    fn print(&self, text: &str) {
        // `TextWriter.WriteLine`: the platform newline (`\r\n` on Windows), as every other
        // console line of the verbs.
        let mut out = self.o.out.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = write!(out, "{text}{}", crate::util::stj::NEWLINE);
        let _ = out.flush();
    }

    fn print_footer(&self) {
        let file = match &self.target {
            None => "(no writable log folder)".to_owned(),
            Some(t) => tools_log_file(t).display().to_string(),
        };
        self.print(&format!("log: {file} (run {})", self.run_id));
        let queue = self.events.queue();
        let depth = match queue.depth() {
            Ok(d) => d,
            Err(e) => {
                self.log.warn_ex(
                    e.to_exception(),
                    "Site event queue depth could not be read",
                    &[],
                );
                return;
            }
        };
        if depth == 0 || self.has_identity() {
            return;
        }
        let place = queue.location().map_or_else(
            || "memory (lost when this program exits)".to_owned(),
            |p| p.display().to_string(),
        );
        let sent_by = if self.target.as_ref().is_some_and(|t| t.kind == "data") {
            "they are sent by the next successful enroll or when the agent service starts"
        } else {
            "this folder is not the agent data folder, so keep it beside the bundle: a later run of this tool on the enrolled agent machine, given the same path, sends them"
        };
        self.print(&format!(
            "site events: {depth} queued in {place}; {sent_by}."
        ));
    }

    fn has_identity(&self) -> bool {
        self.o
            .data_dir
            .as_deref()
            .is_some_and(|d| AgentPaths::new(d).identity_file().is_file())
    }

    fn flush(&self, final_flush: bool) {
        let now = self.o.clock.now().unix_nanos();
        if !final_flush && self.lock().pause_until.is_some_and(|until| now < until) {
            return;
        }
        let Some(transport) = self.transport() else {
            return;
        };
        let pause = now + FLUSH_PAUSE.as_nanos() as i128;
        let ctx = Ctx::background().with_timeout(self.o.flush_budget);
        match self.flusher.flush(&*transport, &ctx) {
            Ok(r) => {
                self.lock().pause_until = (!r.drained()).then_some(pause);
                if let Some(s) = r.stopped_by {
                    self.log.debug(
                        "Site events not all sent ({Status}); they stay queued",
                        &[Value::from(s.name())],
                    );
                }
            }
            Err(e) if e.is_cancelled() && ctx.is_done() => {
                self.lock().pause_until = Some(pause);
                self.log.write(
                    Level::Debug,
                    Some(e.to_exception()),
                    "Site event flush took longer than {Seconds} s; the events stay queued",
                    &[Value::from(self.o.flush_budget.as_secs())],
                );
            }
            Err(e) => {
                self.lock().pause_until = Some(pause);
                self.log.warn_ex(
                    e.to_exception(),
                    "Site event flush failed; the events stay queued",
                    &[],
                );
            }
        }
    }

    fn transport(&self) -> Option<Arc<dyn SiteEventTransport>> {
        if let Some(t) = self.lock().transport.clone() {
            return Some(t);
        }
        let made = match &self.o.transport {
            Some(factory) => Ok(factory(self)),
            None => self.default_transport(),
        };
        match made {
            Ok(t) => {
                self.lock().transport.clone_from(&t);
                t
            }
            Err(ex) => {
                self.log.warn_ex(
                    *ex,
                    "The agent identity or key could not be read; site events stay queued",
                    &[],
                );
                None
            }
        }
    }

    /// The enrolled identity and its stored key; `None` before enrollment (never creates
    /// a key).
    fn default_transport(&self) -> Result<Option<Arc<dyn SiteEventTransport>>, Box<Exception>> {
        let Some(dir) = self.o.data_dir.as_deref() else {
            return Ok(None);
        };
        let paths = AgentPaths::new(dir);
        let fail = |type_name: &str, m: String| Box::new(Exception::new(type_name, m));
        let identity = match AgentIdentity::try_load(&paths) {
            Ok(Some(i)) => i,
            Ok(None) => return Ok(None),
            Err(e) => return Err(fail("System.IO.IOException", e.to_string())),
        };
        let secrets = crate::security::create(&paths);
        let seed = match secrets.load(names::SIGNING_SEED) {
            Ok(Some(s)) => s,
            Ok(None) => return Ok(None),
            Err(e) => {
                return Err(fail(
                    "System.Security.Cryptography.CryptographicException",
                    e.to_string(),
                ));
            }
        };
        let key = AgentKey::from_seed(&seed).map_err(|e| {
            fail(
                "System.Security.Cryptography.CryptographicException",
                e.to_string(),
            )
        })?;
        let http = SyncHttpClient::new(
            &identity.cloud_url,
            self.o.flush_budget,
            Some(RequestSigner::new(Arc::new(key), self.o.clock.clone())),
            RetryPolicy::with_attempts(1),
            &self.logger,
        )
        .map_err(|e| Box::new(e.to_exception()))?;
        let http = Arc::new(http);
        let tokens = Arc::new(TokenProvider::new(
            http.clone(),
            secrets,
            paths,
            self.o.clock.clone(),
            &self.logger,
        ));
        Ok(Some(Arc::new(CloudSiteEventTransport::new(
            http,
            tokens,
            &identity.agent_id_text(),
            &self.o.agent_version,
        ))))
    }
}

impl RunStages for ToolRun {
    fn line(&self, text: &str) {
        self.print(text);
        self.log.info("{ConsoleLine}", &[Value::from(text)]);
    }

    fn stage(&self, level: &str, code: &str, message: &str, detail: Option<Map>) {
        self.events.report(level, code, message, detail);
        self.flush(false);
    }

    fn progress(&self, level: &str, code: &str, message: &str, detail: Option<Map>) {
        self.events.report(level, code, message, detail);
    }

    fn fail(&self, code: &str, message: &str, level: &str) {
        self.lock().failure = Some(code.to_owned());
        self.events.report(level, code, message, None);
    }
}

fn read_identity(data_dir: Option<&str>, problems: &mut Vec<String>) -> Option<AgentIdentity> {
    let dir = data_dir?;
    match AgentIdentity::try_load(&AgentPaths::new(dir)) {
        Ok(i) => i,
        Err(e) => {
            problems.push(format!("agent identity unreadable: {e}"));
            None
        }
    }
}
