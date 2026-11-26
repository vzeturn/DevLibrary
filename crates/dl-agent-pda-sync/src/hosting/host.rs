//! Composition root and service host (port of `Hosting/AgentHost.cs` and
//! `Cli.RunServiceAsync`): builds the components of the running agent, starts each
//! background loop on its own thread, and stops them in reverse order on the service's
//! Stop.
//!
//! The site events (`AddSiteEvents`) and the capture runner are built here; the other
//! hosted loops (commands, order grants, print relay, staff sync, self-update) plug in
//! through [`RuntimeHooks`] ([`hooks`] lists this build's).

use super::cli::{EXIT_FAILED, EXIT_OK, Unhandled, unhandled};
use super::services::{
    BlobUploaderService, CaptureService, HeartbeatService, IdleWait, LoopService, SiteEventsLoop,
    UploaderService, run_resilient,
};
use super::verbs::{HostContext, VerbContext};
use super::{SERVICE_NAME, agent_version};
use crate::capture::error::time_zone_not_found;
use crate::capture::{
    BatchStore, CaptureNudge, CaptureRunnerDependencies, CaptureSource, CaptureState,
    CaptureTimings, OutboxBatchStore, RuntimeConfigSource, SourceFactory, SqlCaptureRunner,
    SqlServerCaptureSource, SqlServerSourceOptions, SqliteCaptureState, StateFactory, UploadWake,
    ZoneFactory,
};
use crate::config::time::{SharedClock, SystemClock, Timestamp, local_offset_minutes};
use crate::config::{
    AgentHealth, AgentIdentity, AgentOptions, AgentPaths, AgentStatus, RuntimeConfigStore, codes,
};
use crate::contracts::catalog::SyncCatalog;
use crate::contracts::time::{DateTimeOffset, TimeZone};
use crate::contracts::wire::StreamLag;
use crate::contracts::wire::site_event_sources;
use crate::outbox::{
    BatchUploader, BlobUploader, CaptureStatus, HeartbeatParts, HeartbeatSender, SqliteBlobOutbox,
    SqliteOutbox,
};
use crate::reporting::agent_logging::{create_service_logger, new_run_id};
use crate::reporting::site_event_store::DEFAULT_CAPACITY;
use crate::reporting::{
    CaptureHealth, CloudSiteEventTransport, Exception, Logger, MemorySiteEventStore, Sink,
    SiteEventFlusher, SiteEventLogBridge, SiteEventReporter, SiteEventService,
    SiteEventServiceParts, SiteEventStore, SiteEvents, SiteHealthMonitor, SqlServerInfo,
    SqlServerInfoSource, SqliteSiteEventStore, Value,
};
use crate::security::{AgentKey, SharedSecretStore};
use crate::transport::http::SERVICE_TIMEOUT;
use crate::transport::{RequestSigner, RetryPolicy, SyncHttpClient, TokenProvider};
use crate::util::console::LineWriter;
use crate::util::ctx::Ctx;
use dl_winsvc::StopSignal;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a stop waits for the loops (`HostOptions.ShutdownTimeout`, 30 s).
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// The hint written when the machine is not enrolled.
pub const NOT_ENROLLED_HINT: &str =
    "This machine is not enrolled. Run: dl-agent pda-enroll --code <code> --url <url>";

/// `ILogger<SiteEventFlusher>`.
pub const FLUSHER_CATEGORY: &str = "MM.PdaOrder.SyncAgent.Reporting.SiteEventFlusher";

/// The command timeout of the start-up version read (SqlClient's default, 30 s).
const SQL_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// The components of the running agent (`AddAgentRuntime`), shared by the loops.
pub struct AgentServices {
    /// Bound options.
    pub options: AgentOptions,
    /// The data folder.
    pub paths: AgentPaths,
    /// The enrolled identity.
    pub identity: AgentIdentity,
    /// This run's id (`agent-<utc>-<hex>`), in every log line.
    pub run_id: String,
    /// `TimeProvider.System`.
    pub clock: SharedClock,
    /// The retry policy every cloud call and loop backoff uses.
    pub retry: RetryPolicy,
    /// The service logger (`logs/agent-.log`, site event bridge from `Warning`).
    pub log: Logger,
    /// The secret store.
    pub secrets: SharedSecretStore,
    /// The agent's signing key.
    pub key: Arc<AgentKey>,
    /// The signed cloud client (2 min timeout).
    pub http: Arc<SyncHttpClient>,
    /// Access tokens.
    pub tokens: Arc<TokenProvider>,
    /// `outbox.db`.
    pub outbox: Arc<SqliteOutbox>,
    /// `blobs.db`.
    pub blobs: Arc<SqliteBlobOutbox>,
    /// Cloud-pushed tunables (`runtime-config.json`).
    pub runtime_config: Arc<RuntimeConfigStore>,
    /// `status.json` writer.
    pub health: Arc<AgentHealth>,
}

impl std::fmt::Debug for AgentServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentServices")
            .field("paths", &self.paths)
            .field("run_id", &self.run_id)
            .finish_non_exhaustive()
    }
}

impl AgentServices {
    /// Builds the components over the data folder of `host`.
    pub fn build(
        host: &HostContext,
        identity: AgentIdentity,
        run_id: String,
        log: Logger,
        secrets: SharedSecretStore,
    ) -> Result<AgentServices, Unhandled> {
        let clock = SystemClock::shared();
        let retry = RetryPolicy::default();
        let paths = host.paths.clone();
        let options = host.options.clone();
        let (key, _) = AgentKey::load_or_create(&*secrets)?;
        let key = Arc::new(key);
        let signer = RequestSigner::new(key.clone(), clock.clone());
        let http = Arc::new(SyncHttpClient::new(
            &identity.cloud_url,
            SERVICE_TIMEOUT,
            Some(signer),
            retry.clone(),
            &log,
        )?);
        let tokens = Arc::new(TokenProvider::new(
            http.clone(),
            secrets.clone(),
            paths.clone(),
            clock.clone(),
            &log,
        ));
        let outbox = Arc::new(SqliteOutbox::open(
            paths.outbox_db(),
            options.outbox_cap_bytes,
            &log,
        )?);
        outbox.adopt_agent_id(
            &identity.agent_id_text(),
            DateTimeOffset::from_unix_nanos(clock.now().unix_nanos(), 0)
                .unwrap_or(DateTimeOffset::UNIX_EPOCH),
        )?;
        let catalog = SyncCatalog::load().map_err(|e| e.to_string())?;
        let blobs = Arc::new(SqliteBlobOutbox::open(
            paths.blob_queue_db(),
            options.blob_queue_cap_bytes,
            catalog,
            &log,
        )?);
        let runtime_config = Arc::new(RuntimeConfigStore::new(paths.clone(), &log)?);
        let health = Arc::new(AgentHealth::new(paths.clone(), clock.clone(), &log));
        Ok(AgentServices {
            options,
            paths,
            identity,
            run_id,
            clock,
            retry,
            log,
            secrets,
            key,
            http,
            tokens,
            outbox,
            blobs,
            runtime_config,
            health,
        })
    }
}

/// What the capture runner brings (`SqlCaptureRunner` registered as `ICaptureRunner`,
/// `ICaptureStatus`, `ICaptureNudge`).
pub struct CapturePlugin {
    /// The `CaptureService` loop (`runner.RunAsync(store, config, ct)`).
    pub service: Box<dyn LoopService>,
    /// What the heartbeat and the site health monitor report about capture.
    pub status: Arc<dyn CaptureHealth>,
    /// Read-your-writes: an applied order verb makes the open checks due now.
    pub nudge: Option<Arc<dyn CaptureNudge>>,
    /// Wakes the uploader's idle wait when capture queued an order command's rows.
    pub upload_wake: Option<Arc<dyn IdleWait>>,
}

/// Builds the capture runner over the services.
pub type CaptureFactory = fn(&AgentServices) -> Result<CapturePlugin, Unhandled>;

/// What the hosted-loop factories get beyond [`AgentServices`]: the capture status, the
/// site events and the capture nudge (the C# singletons `ICaptureStatus`, `ISiteEvents`,
/// `ICaptureNudge`).
#[derive(Clone)]
pub struct RuntimeParts {
    /// The capture status (the runner, or [`NoCapture`]).
    pub capture: Arc<dyn CaptureStatus>,
    /// The service's site events (`SiteEventReporter`).
    pub site_events: Arc<dyn SiteEvents>,
    /// The capture runner's nudge; `None` without a capture runner.
    pub capture_nudge: Option<Arc<dyn CaptureNudge>>,
}

impl std::fmt::Debug for RuntimeParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeParts")
            .field("capture_nudge", &self.capture_nudge.is_some())
            .finish_non_exhaustive()
    }
}

/// Builds more hosted loops over the services and the runtime parts.
pub type ServicesFactory =
    fn(&AgentServices, &RuntimeParts) -> Result<Vec<Box<dyn LoopService>>, Unhandled>;

/// The parts of the runtime that live in other modules.
#[derive(Default)]
pub struct RuntimeHooks {
    /// The capture runner (`SqlCaptureRunner`, `CaptureService`); `None`: no capture
    /// ([`NoCapture`]).
    pub capture: Option<CaptureFactory>,
    /// Loops registered before capture: `CommandService`, `OrderGrantsService`.
    pub before_capture: Vec<ServicesFactory>,
    /// Loops registered after capture: print relay, staff sync, self-update.
    pub after_capture: Vec<ServicesFactory>,
}

impl std::fmt::Debug for RuntimeHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeHooks")
            .field("capture", &self.capture.is_some())
            .field("before_capture", &self.before_capture.len())
            .field("after_capture", &self.after_capture.len())
            .finish()
    }
}

/// The hooks of this build, in the C# registration order. Modules that port a runtime
/// part add it here.
pub fn hooks() -> RuntimeHooks {
    RuntimeHooks {
        capture: Some(sql_capture),
        before_capture: vec![command_services],
        after_capture: vec![crate::printrelay::service::services, staff_services],
    }
}

/// `CommandService` and `OrderGrantsService` with the host's site events and capture
/// nudge.
pub fn command_services(
    s: &AgentServices,
    rt: &RuntimeParts,
) -> Result<Vec<Box<dyn LoopService>>, Unhandled> {
    crate::commands::service::build(
        s,
        &crate::commands::service::CommandExtras {
            events: Some(rt.site_events.clone()),
            nudge: rt.capture_nudge.clone(),
        },
    )
}

/// `AddStaffSync`.
pub fn staff_services(
    s: &AgentServices,
    rt: &RuntimeParts,
) -> Result<Vec<Box<dyn LoopService>>, Unhandled> {
    crate::staff::hosting::services(s, &rt.capture)
}

impl IdleWait for UploadWake {
    fn wait(&self, timeout: Duration, ctx: &Ctx) -> bool {
        UploadWake::wait(self, timeout, ctx);
        !ctx.is_done()
    }
}

/// The site time zone of a capture session: `Agent:SiteTimeZone` when set, else the
/// machine zone.
pub fn site_time_zone(configured: Option<&str>) -> crate::capture::Result<Arc<TimeZone>> {
    match configured.filter(|z| !z.is_empty()) {
        Some(id) => TimeZone::find_system_time_zone_by_id(id)
            .map(Arc::new)
            .ok_or_else(|| time_zone_not_found(id)),
        None => Ok(TimeZone::local()),
    }
}

/// The capture runner of the running agent: SELECT-only reads of the POS database as
/// `pda_sync` (the connection built per session, so a re-enroll's new password is picked
/// up on the next retry), diff state in `capture.db`, blobs to `blobs.db`, and a shared
/// upload wake for read-your-writes.
pub fn sql_capture(s: &AgentServices) -> Result<CapturePlugin, Unhandled> {
    let identity = s.identity.clone();
    let secrets = s.secrets.clone();
    let source: SourceFactory = Arc::new(move || {
        let cfg = crate::config::sql::agent_connection_for(&identity, &*secrets)?;
        let src = SqlServerCaptureSource::new(SqlServerSourceOptions::new(cfg));
        Ok(Arc::new(src) as Arc<dyn CaptureSource>)
    });
    capture_with(s, source)
}

/// [`sql_capture`] over another POS reader (tests).
pub fn capture_with(s: &AgentServices, source: SourceFactory) -> Result<CapturePlugin, Unhandled> {
    let path = s.paths.capture_state_db();
    let state: StateFactory =
        Arc::new(move || Ok(Arc::new(SqliteCaptureState::open(&path)?) as Arc<dyn CaptureState>));
    let zone_id = s.options.site_time_zone.clone();
    let zone: ZoneFactory = Arc::new(move || site_time_zone(zone_id.as_deref()));
    let catalog = SyncCatalog::load().map_err(|e| e.to_string())?;
    let wake = Arc::new(UploadWake::new());
    let runner = SqlCaptureRunner::new(
        CaptureRunnerDependencies {
            source,
            state,
            blobs: s.blobs.clone(),
            catalog: Arc::new(catalog.clone()),
            agent_id: s.identity.agent_id_text(),
            site_time_zone: zone,
            timings: CaptureTimings::default(),
            upload_wake: Some(wake.clone()),
        },
        s.retry.clone(),
        s.clock.clone(),
        &s.log,
    )?;
    let store: Arc<dyn BatchStore> = Arc::new(OutboxBatchStore::new(
        s.outbox.clone(),
        Some(s.clock.clone()),
    ));
    let config: Arc<dyn RuntimeConfigSource> = s.runtime_config.clone();
    Ok(CapturePlugin {
        service: Box::new(CaptureService::new(runner.clone(), store, config)),
        status: Arc::new(runner.clone()),
        nudge: Some(Arc::new(runner)),
        upload_wake: Some(wake),
    })
}

/// Capture status while no capture runner is wired: SQL not reached, no streams.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoCapture;

impl CaptureStatus for NoCapture {
    fn sql_reachable(&self) -> bool {
        false
    }
    fn streams(&self) -> Vec<StreamLag> {
        Vec::new()
    }
    fn capture_error(&self) -> Option<String> {
        None
    }
}

impl CaptureHealth for NoCapture {}

/// Reads the POS database server's version as `pda_sync`, read-only
/// (`AgentSqlServerInfoSource`); the connection is built per read.
pub struct AgentSqlServerInfoSource {
    identity: AgentIdentity,
    secrets: SharedSecretStore,
}

impl std::fmt::Debug for AgentSqlServerInfoSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSqlServerInfoSource")
            .finish_non_exhaustive()
    }
}

impl AgentSqlServerInfoSource {
    /// A source for the enrolled agent.
    pub fn new(identity: AgentIdentity, secrets: SharedSecretStore) -> AgentSqlServerInfoSource {
        AgentSqlServerInfoSource { identity, secrets }
    }
}

fn tds_exception(e: &dl_tds::Error) -> Box<Exception> {
    match crate::bootstrap::error::tds_failure(e, &[]) {
        Ok(f) => Box::new(f.to_exception()),
        Err(ex) => ex,
    }
}

impl SqlServerInfoSource for AgentSqlServerInfoSource {
    fn read(&self, ctx: &Ctx) -> Result<SqlServerInfo, Box<Exception>> {
        let cfg = crate::config::sql::agent_connection_for(&self.identity, &*self.secrets)
            .map_err(|e| Box::new(Exception::from_agent_error(&e)))?;
        let cancelled = || {
            Box::new(Exception::new(
                "System.OperationCanceledException",
                "The operation was canceled.",
            ))
        };
        if ctx.is_done() {
            return Err(cancelled());
        }
        let mut c = match dl_tds::Client::connect(&cfg) {
            Ok(c) => c,
            Err(_) if ctx.is_done() => return Err(cancelled()),
            Err(e) => return Err(tds_exception(&e)),
        };
        c.take_messages();
        c.set_timeout(Some(SQL_COMMAND_TIMEOUT))
            .map_err(|e| tds_exception(&e))?;
        let info = crate::bootstrap::SqlServerInfo::read(&mut c)
            .map_err(|e| Box::new(e.to_exception()))?;
        c.close();
        Ok(SqlServerInfo {
            product_version: info.product_version,
            edition: info.edition,
            product_level: info.product_level,
        })
    }
}

/// How a host run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostOutcome {
    /// Loops that stopped in time.
    pub stopped: Vec<&'static str>,
    /// Loops still running at the shutdown timeout (left behind, as .NET does).
    pub abandoned: Vec<&'static str>,
}

/// The log category of the .NET host lifetimes (`ConsoleLifetime`,
/// `WindowsServiceLifetime`).
pub const LIFETIME_CATEGORY: &str = "Microsoft.Hosting.Lifetime";

/// The .NET host lifetime whose status lines the host writes when the loops have started
/// (`ApplicationStarted`) and when a stop begins (`ApplicationStopping`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HostLifetime {
    /// No status lines (tests and embedded hosts).
    #[default]
    Silent,
    /// `ConsoleLifetime`: three started lines, then `Application is shutting down...`.
    Console {
        /// `IHostEnvironment.EnvironmentName`.
        environment: String,
        /// `IHostEnvironment.ContentRootPath` (`AppContext.BaseDirectory`).
        content_root: String,
    },
    /// `WindowsServiceLifetime`: one started line, then `Application is shutting down...`.
    WindowsService {
        /// `IHostEnvironment.EnvironmentName`.
        environment: String,
        /// `IHostEnvironment.ContentRootPath` (`AppContext.BaseDirectory`).
        content_root: String,
    },
}

impl HostLifetime {
    /// The lines of `ApplicationStarted`, on [`LIFETIME_CATEGORY`].
    pub fn started(&self, log: &Logger) {
        let log = log.for_category(LIFETIME_CATEGORY);
        match self {
            HostLifetime::Silent => {}
            HostLifetime::Console {
                environment,
                content_root,
            } => {
                log.info("Application started. Press Ctrl+C to shut down.", &[]);
                log.info(
                    "Hosting environment: {EnvName}",
                    &[Value::from(environment.as_str())],
                );
                log.info(
                    "Content root path: {ContentRoot}",
                    &[Value::from(content_root.as_str())],
                );
            }
            HostLifetime::WindowsService {
                environment,
                content_root,
            } => log.info(
                "Application started. Hosting environment: {EnvName}; Content root path: {ContentRoot}",
                &[
                    Value::from(environment.as_str()),
                    Value::from(content_root.as_str()),
                ],
            ),
        }
    }

    /// The line of `ApplicationStopping`, on [`LIFETIME_CATEGORY`].
    pub fn stopping(&self, log: &Logger) {
        if *self != HostLifetime::Silent {
            log.for_category(LIFETIME_CATEGORY)
                .info("Application is shutting down...", &[]);
        }
    }
}

/// `IHostEnvironment.EnvironmentName` of `HostApplicationBuilder`: the host
/// configuration key `environment` from the command line (`--environment X`,
/// `--environment=X`, `/environment X`, `environment=X`; the last wins, key
/// case-insensitive), else `DOTNET_ENVIRONMENT`, else `Production`.
pub fn environment_name(args: &[String], dotnet_environment: Option<&str>) -> String {
    let mut found: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let (key, inline) = match arg.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (arg, None),
        };
        let (bare, prefixed) = match key.strip_prefix("--").or_else(|| key.strip_prefix('/')) {
            Some(k) => (k, true),
            None => (key, false),
        };
        if bare.eq_ignore_ascii_case("environment") {
            match inline {
                Some(v) => found = Some(v.to_owned()),
                None if prefixed => {
                    if let Some(v) = args.get(i + 1) {
                        found = Some(v.clone());
                        i += 1;
                    }
                }
                None => {}
            }
        } else if prefixed && inline.is_none() {
            // `--key value`: the value is consumed with its key.
            i += 1;
        }
        i += 1;
    }
    found
        .or_else(|| dotnet_environment.map(str::to_owned))
        .unwrap_or_else(|| "Production".to_owned())
}

/// `AppContext.BaseDirectory`: the executable's folder with a trailing separator.
pub fn content_root() -> String {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_default();
    let mut s = dir.to_string_lossy().into_owned();
    if !s.ends_with(std::path::MAIN_SEPARATOR) {
        s.push(std::path::MAIN_SEPARATOR);
    }
    s
}

/// The background loops of one run.
pub struct AgentHost {
    services: Vec<Box<dyn LoopService>>,
    retry: RetryPolicy,
    log: Logger,
    shutdown_timeout: Duration,
    lifetime: HostLifetime,
}

impl std::fmt::Debug for AgentHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<_> = self.services.iter().map(|s| s.name()).collect();
        f.debug_struct("AgentHost")
            .field("services", &names)
            .finish_non_exhaustive()
    }
}

impl AgentHost {
    /// A host for `services`, started in order and stopped in reverse order.
    pub fn new(services: Vec<Box<dyn LoopService>>, retry: RetryPolicy, log: Logger) -> AgentHost {
        AgentHost {
            services,
            retry,
            log,
            shutdown_timeout: SHUTDOWN_TIMEOUT,
            lifetime: HostLifetime::Silent,
        }
    }

    /// Sets the lifetime whose status lines [`run`](Self::run) writes.
    pub fn with_lifetime(mut self, lifetime: HostLifetime) -> AgentHost {
        self.lifetime = lifetime;
        self
    }

    /// Sets how long [`run`](Self::run) waits for the loops after a stop.
    pub fn with_shutdown_timeout(mut self, timeout: Duration) -> AgentHost {
        self.shutdown_timeout = timeout;
        self
    }

    /// The loops' names in start order.
    pub fn service_names(&self) -> Vec<&'static str> {
        self.services.iter().map(|s| s.name()).collect()
    }

    /// Starts every loop, waits for `stop`, then stops the loops one at a time in reverse
    /// start order (the .NET host's `StopAsync`: each loop is cancelled and awaited
    /// before the previous one is), all within the shutdown timeout.
    pub fn run(self, stop: &StopSignal) -> Result<HostOutcome, Unhandled> {
        let root = Ctx::background().child();
        let mut threads = Vec::new();
        for mut svc in self.services {
            let name = svc.name();
            let c = root.child();
            let (retry, log, run_ctx) = (self.retry.clone(), self.log.clone(), c.clone());
            let spawned = std::thread::Builder::new()
                .name(name.to_owned())
                .spawn(move || run_resilient(&mut *svc, &retry, &log, &run_ctx));
            match spawned {
                Ok(h) => threads.push((name, c, h)),
                Err(e) => {
                    root.cancel();
                    return Err(Box::new(e));
                }
            }
        }
        // `ApplicationStarted`: every hosted service has been started.
        self.lifetime.started(&self.log);
        stop.wait();
        self.lifetime.stopping(&self.log);
        let deadline = Instant::now() + self.shutdown_timeout;
        let mut outcome = HostOutcome {
            stopped: Vec::new(),
            abandoned: Vec::new(),
        };
        for (name, c, h) in threads.into_iter().rev() {
            c.cancel();
            while !h.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if h.is_finished() {
                let _ = h.join();
                outcome.stopped.push(name);
            } else {
                // Past the timeout the rest are only cancelled, as .NET does.
                root.cancel();
                outcome.abandoned.push(name);
            }
        }
        root.cancel();
        Ok(outcome)
    }
}

/// The site events of one run (`AddSiteEvents`): the reporter every part reports through,
/// and the `SiteEventService` loop when the run has a log bridge.
struct SiteEventsRuntime {
    reporter: Arc<SiteEventReporter>,
    service: Option<SiteEventService>,
}

fn open_site_event_queue(s: &AgentServices) -> Arc<dyn SiteEventStore> {
    let path = s.paths.events_db();
    match SqliteSiteEventStore::try_open(&path, DEFAULT_CAPACITY) {
        Ok(q) => Arc::new(q),
        Err(error) => {
            s.log.warn(
                "Site event queue {Path} could not be opened ({Error}); events are kept in memory until the agent restarts",
                &[Value::from(path.display().to_string()), Value::from(error)],
            );
            Arc::new(MemorySiteEventStore::default())
        }
    }
}

fn site_events_reporter(
    s: &AgentServices,
    queue: Arc<dyn SiteEventStore>,
) -> Result<Arc<SiteEventReporter>, Unhandled> {
    Ok(Arc::new(SiteEventReporter::new(
        queue,
        site_event_sources::AGENT,
        Some(&s.run_id),
        &s.log,
        s.clock.clone(),
    )?))
}

fn build_site_events(
    s: &AgentServices,
    bridge: Option<&Arc<SiteEventLogBridge>>,
    capture: &Arc<dyn CaptureHealth>,
) -> Result<SiteEventsRuntime, Unhandled> {
    let Some(bridge) = bridge else {
        // No log bridge (tests): events are queued in memory and never flushed.
        let queue = Arc::new(MemorySiteEventStore::default());
        return Ok(SiteEventsRuntime {
            reporter: site_events_reporter(s, queue)?,
            service: None,
        });
    };
    let reporter = site_events_reporter(s, open_site_event_queue(s))?;
    let flusher = Arc::new(SiteEventFlusher::for_reporter(
        reporter.clone(),
        s.clock.clone(),
        &s.log.for_category(FLUSHER_CATEGORY),
    ));
    let transport = Arc::new(CloudSiteEventTransport::new(
        s.http.clone(),
        s.tokens.clone(),
        &s.identity.agent_id_text(),
        &agent_version(),
    ));
    let outbox = s.outbox.clone();
    let tokens = s.tokens.clone();
    let monitor = Arc::new(SiteHealthMonitor::new(
        reporter.clone(),
        capture.clone(),
        Arc::new(move || {
            outbox
                .depth()
                .map_err(|e| Box::new(crate::capture::sinks_bridge::outbox_error(e).to_exception()))
        }),
        Arc::new(move || tokens.refresh_failure()),
        s.options.upload_backlog_alert_batches,
    ));
    let service = SiteEventService::new(
        SiteEventServiceParts {
            events: reporter.clone(),
            bridge: bridge.clone(),
            flusher,
            transport,
            monitor,
            sql_info: Arc::new(AgentSqlServerInfoSource::new(
                s.identity.clone(),
                s.secrets.clone(),
            )),
            agent_version: agent_version(),
        },
        &s.log,
    );
    Ok(SiteEventsRuntime {
        reporter,
        service: Some(service),
    })
}

/// Builds the loops of the running agent in the C# registration order: site events
/// (when `bridge` is given: the service log's bridge; started first, stopped last),
/// uploader, blob uploader, heartbeat, the loops before capture, capture, the loops
/// after. Without a bridge the site events are kept in memory and no `SiteEvents` loop
/// runs.
pub fn build_host(
    services: &AgentServices,
    hooks: &RuntimeHooks,
    bridge: Option<&Arc<SiteEventLogBridge>>,
) -> Result<AgentHost, Unhandled> {
    let capture = match hooks.capture {
        Some(f) => Some(f(services)?),
        None => None,
    };
    let health: Arc<dyn CaptureHealth> = capture.as_ref().map_or_else(
        || Arc::new(NoCapture) as Arc<dyn CaptureHealth>,
        |c| c.status.clone(),
    );
    let status: Arc<dyn CaptureStatus> = health.clone();
    let site = build_site_events(services, bridge, &health)?;
    let parts = RuntimeParts {
        capture: status.clone(),
        site_events: site.reporter.clone(),
        capture_nudge: capture.as_ref().and_then(|c| c.nudge.clone()),
    };
    let mut loops: Vec<Box<dyn LoopService>> = Vec::new();
    if let Some(service) = site.service {
        loops.push(Box::new(SiteEventsLoop::new(service, &services.log)));
    }
    let o = &services.options;
    loops.push(Box::new(UploaderService {
        uploader: BatchUploader::new(
            services.outbox.clone(),
            services.http.clone(),
            services.tokens.clone(),
            &services.log,
            Some(services.clock.clone()),
            Some(o.parent_pending_escalate_minutes),
        )
        .with_parallel_streams(usize::try_from(o.upload_parallel_streams).unwrap_or(1)),
        batches_per_cycle: o.upload_batches_per_cycle,
        retry: services.retry.clone(),
        wake: capture.as_ref().and_then(|c| c.upload_wake.clone()),
    }));
    loops.push(Box::new(BlobUploaderService {
        uploader: BlobUploader::new(
            services.blobs.clone(),
            &services.identity,
            services.http.clone(),
            services.tokens.clone(),
            &services.log,
        ),
        blobs_per_cycle: o.upload_blobs_per_cycle,
        retry: services.retry.clone(),
    }));
    let sender = HeartbeatSender::new(
        HeartbeatParts {
            identity: services.identity.clone(),
            outbox: services.outbox.clone(),
            blobs: services.blobs.clone(),
            capture: status.clone(),
            http: services.http.clone(),
            tokens: services.tokens.clone(),
            config: services.runtime_config.clone(),
            clock: services.clock.clone(),
            health: services.health.clone(),
            agent_version: agent_version(),
        },
        &services.log,
    );
    loops.push(Box::new(HeartbeatService {
        sender: Arc::new(sender),
        seconds: o.heartbeat_seconds,
    }));
    for f in &hooks.before_capture {
        loops.extend(f(services, &parts)?);
    }
    if let Some(c) = capture {
        loops.push(c.service);
    }
    for f in &hooks.after_capture {
        loops.extend(f(services, &parts)?);
    }
    Ok(AgentHost::new(
        loops,
        services.retry.clone(),
        services.log.clone(),
    ))
}

/// Records `not_enrolled` in `status.json` and says how to enroll; exit 1.
pub fn refuse_not_enrolled(paths: &AgentPaths, stderr: &dyn LineWriter) -> i32 {
    let status = AgentStatus::new(
        "not_enrolled",
        Some(codes::NOT_ENROLLED),
        Some(NOT_ENROLLED_HINT),
        Timestamp::now_utc(),
    );
    if let Err(e) = AgentStatus::write(paths, &status) {
        return unhandled(stderr, &e);
    }
    stderr.write_line(&format!("[{}] {NOT_ENROLLED_HINT}", codes::NOT_ENROLLED));
    EXIT_FAILED
}

/// The `run` verb (`Cli.RunServiceAsync`) with this build's [`hooks`].
pub fn run_verb(ctx: &VerbContext<'_>) -> i32 {
    match ctx.host {
        Some(host) => {
            let dotnet_env = std::env::var("DOTNET_ENVIRONMENT").ok();
            let env = environment_name(ctx.args, dotnet_env.as_deref());
            run_service_in(host, ctx.stderr, &hooks(), &env)
        }
        None => super::verbs::not_available(ctx),
    }
}

/// Runs the service host: as the Windows service `PdaSyncAgent` when started by the
/// service control manager, else in the console until the process is ended. The
/// environment name comes from the process command line and `DOTNET_ENVIRONMENT`.
pub fn run_service(host: &HostContext, stderr: &dyn LineWriter, hooks: &RuntimeHooks) -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dotnet_env = std::env::var("DOTNET_ENVIRONMENT").ok();
    let env = environment_name(&args, dotnet_env.as_deref());
    run_service_in(host, stderr, hooks, &env)
}

/// [`run_service`] in the host environment `environment` (`IHostEnvironment.EnvironmentName`).
pub fn run_service_in(
    host: &HostContext,
    stderr: &dyn LineWriter,
    hooks: &RuntimeHooks,
    environment: &str,
) -> i32 {
    let identity = match AgentIdentity::try_load(&host.paths) {
        Ok(Some(id)) => id,
        Ok(None) => return refuse_not_enrolled(&host.paths, stderr),
        Err(e) => return unhandled(stderr, &e),
    };
    // `TimeZoneInfo.Local` for a site without Agent:SiteTimeZone: the machine's current
    // offset (no daylight-saving rules; the sites' zones have none).
    TimeZone::set_local(TimeZone::fixed("Local", i32::from(local_offset_minutes())));
    let clock = SystemClock::shared();
    // The service log (logs/agent-.log, JSON lines); its Warning+ lines also become site
    // events through the bridge.
    let run_id = new_run_id("agent", clock.now());
    let bridge = Arc::new(SiteEventLogBridge::new(clock.clone()));
    let log = create_service_logger(
        &host.paths,
        &run_id,
        &identity,
        Some(bridge.clone() as Arc<dyn Sink>),
    );
    let secrets = crate::security::create(&host.paths);
    let built = AgentServices::build(host, identity.clone(), run_id, log.clone(), secrets)
        .and_then(|s| build_host(&s, hooks, Some(&bridge)));
    let agent_host = match built {
        Ok(h) => h,
        Err(e) => return unhandled(stderr, &e),
    };
    log.for_category("Agent").info(
        "PdaSyncAgent {Version} starting for agent {AgentId}",
        &[Value::from(agent_version()), Value::from(identity.agent_id)],
    );
    let (environment, content_root) = (environment.to_owned(), content_root());
    let result = if dl_winsvc::is_service_process().unwrap_or(false) {
        let agent_host = agent_host.with_lifetime(HostLifetime::WindowsService {
            environment,
            content_root,
        });
        dl_winsvc::run_service(SERVICE_NAME, move |stop| {
            let _ = agent_host.run(&stop);
        })
        .map_err(|e| Box::new(e) as Unhandled)
    } else {
        // Console: runs until the process is ended (no Ctrl+C hook without a DevLibrary API).
        agent_host
            .with_lifetime(HostLifetime::Console {
                environment,
                content_root,
            })
            .run(&StopSignal::new())
            .map(|_| ())
    };
    match result {
        Ok(()) => EXIT_OK,
        Err(e) => unhandled(stderr, &e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosting::services::FnService;
    use crate::reporting::{Level, LoggerConfig, MemorySink};

    fn memory_log() -> (Arc<MemorySink>, Logger) {
        let sink = Arc::new(MemorySink::default());
        let log = Logger::new(LoggerConfig {
            sinks: vec![(sink.clone(), Level::Verbose)],
            ..LoggerConfig::default()
        });
        (sink, log)
    }

    fn text(v: Option<&Value>) -> Option<String> {
        v.and_then(Value::as_str).map(str::to_owned)
    }

    #[test]
    fn console_lifetime_writes_the_three_dotnet_started_lines_and_the_stopping_line() {
        let (sink, log) = memory_log();
        let lifetime = HostLifetime::Console {
            environment: "Production".to_owned(),
            content_root: r"C:\Program Files\PdaSyncAgent\".to_owned(),
        };
        lifetime.started(&log);
        lifetime.stopping(&log);
        assert_eq!(
            sink.messages_at(Level::Information),
            [
                "Application started. Press Ctrl+C to shut down.",
                "Hosting environment: Production",
                r"Content root path: C:\Program Files\PdaSyncAgent\",
                "Application is shutting down...",
            ]
        );
        let events = sink.events();
        assert!(events.iter().all(|e| {
            text(e.property(crate::reporting::log::SOURCE_CONTEXT)).as_deref()
                == Some(LIFETIME_CATEGORY)
        }));
        assert_eq!(
            text(events[1].property("EnvName")).as_deref(),
            Some("Production")
        );
        assert_eq!(
            text(events[2].property("ContentRoot")).as_deref(),
            Some(r"C:\Program Files\PdaSyncAgent\")
        );
    }

    #[test]
    fn windows_service_lifetime_writes_one_started_line() {
        let (sink, log) = memory_log();
        let lifetime = HostLifetime::WindowsService {
            environment: "Production".to_owned(),
            content_root: r"C:\PdaSyncAgent\".to_owned(),
        };
        lifetime.started(&log);
        lifetime.stopping(&log);
        assert_eq!(
            sink.messages_at(Level::Information),
            [
                r"Application started. Hosting environment: Production; Content root path: C:\PdaSyncAgent\",
                "Application is shutting down...",
            ]
        );
    }

    #[test]
    fn silent_lifetime_writes_nothing_and_the_host_logs_started_after_the_loops_start() {
        let (sink, log) = memory_log();
        HostLifetime::Silent.started(&log);
        HostLifetime::Silent.stopping(&log);
        assert!(sink.events().is_empty());

        let loop_log = log.clone();
        let svc = FnService::new("Uploader", move |c: &Ctx| {
            loop_log.info("loop running", &[]);
            while !c.is_done() {
                c.sleep(Duration::from_millis(5));
            }
            Ok(())
        });
        let stop = StopSignal::new();
        let s2 = stop.clone();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            s2.stop();
        });
        let host = AgentHost::new(vec![Box::new(svc)], RetryPolicy::default(), log).with_lifetime(
            HostLifetime::Console {
                environment: "Staging".to_owned(),
                content_root: "/opt/agent/".to_owned(),
            },
        );
        let outcome = host.run(&stop).unwrap();
        stopper.join().unwrap();
        assert_eq!(outcome.stopped, ["Uploader"]);
        let messages = sink.messages_at(Level::Information);
        let started = messages
            .iter()
            .position(|m| m == "Application started. Press Ctrl+C to shut down.")
            .unwrap();
        let stopping = messages
            .iter()
            .position(|m| m == "Application is shutting down...")
            .unwrap();
        assert!(started < stopping);
        assert!(messages.contains(&"Hosting environment: Staging".to_owned()));
    }

    #[test]
    fn environment_name_follows_the_dotnet_host_configuration() {
        let a = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(environment_name(&a(&["run"]), None), "Production");
        assert_eq!(environment_name(&a(&[]), Some("Staging")), "Staging");
        assert_eq!(
            environment_name(&a(&["run", "--environment", "Dev"]), Some("Staging")),
            "Dev"
        );
        assert_eq!(environment_name(&a(&["--Environment=Dev"]), None), "Dev");
        assert_eq!(environment_name(&a(&["/environment", "Dev"]), None), "Dev");
        assert_eq!(environment_name(&a(&["environment=Dev"]), None), "Dev");
        // A value of another key is not read as a key; a trailing key without value is
        // ignored.
        assert_eq!(
            environment_name(&a(&["--data-dir", "environment=x", "--environment"]), None),
            "Production"
        );
    }

    #[test]
    fn content_root_ends_with_a_separator() {
        assert!(content_root().ends_with(std::path::MAIN_SEPARATOR));
    }
}
