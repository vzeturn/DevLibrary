//! The SyncAgent print relay endpoint (port of `PrintRelay/PrintRelayService.cs` and the
//! `AddPrintRelay` registrations of `AgentHost.cs`, ADR 0098).
//!
//! Off when `Agent:PrintRelay:Enabled` is false or its safety gate is invalid. Otherwise:
//! discover the local print queues (self-setup), recover the journal, then keep one order
//! hub connection with the agent's sync token, joined to the relay of every branch of the
//! tenant. The cloud closes the connection when the token expires; the service
//! reconnects with a fresh one and joins again.

use super::discovery::{
    CloudPrintQueueReporter, PrintQueueDiscovery, WindowsPrintQueueSource, WindowsRegistry,
    adapter_networks, host_networks,
};
use super::engine::{PrintRelayEngine, PrintRelayEngineParts, PrintRelayJob, PrintRelayLink};
use super::journal::PrintJournal;
use super::policy::PrinterTargetPolicy;
use super::raw_tcp::{RawPrinter, RawTcpPrinter};
use super::spooler::{SpoolerPrinter, SpoolerQueues, WindowsSpoolerPrinter};
use crate::config::time::SharedClock;
use crate::config::{AgentIdentity, AgentOptions, AgentPaths};
use crate::contracts::json::dto;
use crate::contracts::wire::site_event_levels::{ERROR, INFO, WARN};
use crate::hosting::cli::Unhandled;
use crate::hosting::host::{AgentServices, RuntimeParts};
use crate::hosting::services::{LoopService, PeriodicTicks, ServiceError};
use crate::reporting::{Exception, Logger, SiteEvents, Value};
use crate::transport::{RetryPolicy, TokenProvider};
use crate::util::ctx::Ctx;
use crate::util::stj::{self, Obj, ReadError};
use dl_json::{Map, Value as JsonValue};
use dl_signalr::{CloseReason, ConnectOptions, HubConnection};
use std::sync::Arc;
use std::time::Duration;

/// `ILogger<PrintRelayService>`.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.PrintRelay.PrintRelayService";
/// The order hub's path on the cloud.
pub const HUB_PATH: &str = "/hubs/order";
/// How long the branch list and each join may take (C#: no limit but the stop).
pub const HUB_CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// How often a connection checks for the service stopping.
const STOP_POLL: Duration = Duration::from_millis(200);

/// What the relay service needs (`PrintRelayDependencies`).
pub struct PrintRelayDependencies {
    /// Bound options (`Agent:PrintRelay`).
    pub options: AgentOptions,
    /// The data folder (default journal folder).
    pub paths: AgentPaths,
    /// The enrolled identity (cloud URL).
    pub identity: AgentIdentity,
    /// The sync token for the hub.
    pub tokens: Arc<TokenProvider>,
    /// Site events.
    pub events: Arc<dyn SiteEvents>,
    /// Queue discovery and its report to the cloud.
    pub discovery: Arc<PrintQueueDiscovery>,
    /// Raw TCP printing.
    pub printer: Arc<dyn RawPrinter>,
    /// The clock.
    pub clock: SharedClock,
    /// The service logger.
    pub log: Logger,
    /// Windows spooler printing.
    pub spooler: Arc<dyn SpoolerPrinter>,
    /// The reported USB/WSD queues.
    pub spooler_queues: Arc<SpoolerQueues>,
}

/// The relay loop (`PrintRelayService`, a resilient service named `PrintRelay`).
pub struct PrintRelayService {
    deps: Arc<PrintRelayDependencies>,
    retry: RetryPolicy,
    log: Logger,
    engine: Option<PrintRelayEngine>,
    ack_loop: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for PrintRelayService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrintRelayService").finish_non_exhaustive()
    }
}

/// The hub URL of a cloud URL (`new Uri(new Uri(cloudUrl), "/hubs/order")`): the
/// scheme and authority of the cloud, path `/hubs/order`.
pub fn hub_url(cloud_url: &str) -> String {
    let trimmed = cloud_url.trim();
    let authority_end = trimmed
        .find("://")
        .map(|i| i + 3)
        .and_then(|start| {
            trimmed[start..]
                .find(['/', '?', '#'])
                .map(|j| start + j)
                .or(Some(trimmed.len()))
        })
        .unwrap_or(trimmed.len());
    format!("{}{HUB_PATH}", &trimmed[..authority_end])
}

/// Reads the `PrintJob` argument (`PrintRelayJobMessage`, camelCase, case-insensitive,
/// as the SignalR JSON protocol binds it).
pub fn parse_job(args: &[JsonValue]) -> Result<PrintRelayJob, ReadError> {
    let v = args
        .first()
        .ok_or_else(|| ReadError::new("$", "The PrintJob invocation has no argument."))?;
    let o = Obj::require(v, stj::WEB, "$")?;
    let job_id = match o.get_non_null("job_id") {
        None => dl_uuid::Uuid::NIL,
        Some(_) => o.guid("job_id")?,
    };
    let payload = match o.opt_string("payload")? {
        None => Vec::new(),
        Some(b64) => dl_encoding::base64::STANDARD.decode(&b64).map_err(|_| {
            ReadError::new(
                &o.path_of("payload"),
                "The JSON value is not in a supported Base64 format.",
            )
        })?,
    };
    let expires_at = match o.get_non_null("expires_at") {
        None => crate::contracts::time::DateTimeOffset::MIN,
        Some(_) => dto(&o, "expires_at")?,
    };
    Ok(PrintRelayJob {
        job_id,
        rvc_no: o.int("rvc_no")?,
        host: o.opt_string("host")?.unwrap_or_default(),
        port: o.int("port")?,
        payload,
        expires_at,
        attempts: o.int("attempts")?,
    })
}

/// Starts one order hub connection of `cloud_url` (`HubConnection.StartAsync`: WebSockets
/// only, negotiation skipped, `AccessTokenProvider` = the agent's cached sync token);
/// `handlers` registers the client methods. A failure is the exception the .NET client
/// throws ([`connect_exception`]). As in C#, a hub that refuses the token (401 on the
/// upgrade) does not drop the cached token: the next attempt sends the same one.
pub fn start_hub(
    cloud_url: &str,
    tokens: &TokenProvider,
    handlers: impl FnOnce(&mut ConnectOptions),
    ctx: &Ctx,
) -> Result<HubConnection, Box<Exception>> {
    let token = tokens
        .get_access_token(ctx)
        .map_err(|e| Box::new(e.to_exception()))?;
    let mut options = ConnectOptions::new(hub_url(cloud_url));
    options.negotiate = false;
    options.http = Some(Arc::new(
        dl_http::Client::with_options(hub_client_options()),
    ));
    // The .NET client (not in a browser) sends the token only as the upgrade's
    // `Authorization` header; `ConnectOptions::access_token` would also put it in the
    // URL query.
    options
        .headers
        .push(("Authorization".to_owned(), format!("Bearer {token}")));
    handlers(&mut options);
    HubConnection::connect(&options).map_err(|err| {
        if is_unauthorized(&err) {
            tokens.invalidate();
        }
        Box::new(connect_exception(&err))
    })
}

/// The HTTP client options of the order hub connection: the signalr defaults with the
/// proxy of every other PDA cloud call ([`crate::transport::http::proxy`]).
pub fn hub_client_options() -> dl_http::ClientOptions {
    dl_http::ClientOptions {
        proxy: crate::transport::http::proxy(),
        ..dl_http::ClientOptions::default()
    }
}

/// Acks through the live hub connection (`HubLink`).
struct HubLink(HubConnection);

impl PrintRelayLink for HubLink {
    fn ack(
        &self,
        job_id: dl_uuid::Uuid,
        ok: bool,
        error: Option<&str>,
        retryable: bool,
        ctx: &Ctx,
    ) -> Result<bool, Box<Exception>> {
        if ctx.is_done() {
            return Err(Box::new(Exception::new(
                "System.OperationCanceledException",
                "The operation was canceled.",
            )));
        }
        let args = vec![
            JsonValue::from(job_id.to_string()),
            JsonValue::from(ok),
            error.map_or(JsonValue::Null, JsonValue::from),
            JsonValue::from(retryable),
        ];
        let timeout = ctx.remaining().unwrap_or(super::engine::ACK_CALL_TIMEOUT);
        match self.0.invoke("AckPrintJob", args, Some(timeout)) {
            Ok(v) => Ok(v.as_bool().unwrap_or(false)),
            Err(e) => Err(Box::new(hub_exception(&e))),
        }
    }
}

/// `System.Net.WebSockets.WebSocketException`.
pub const WEB_SOCKET_EXCEPTION: &str = "System.Net.WebSockets.WebSocketException";
/// `Microsoft.AspNetCore.SignalR.HubException`.
pub const HUB_EXCEPTION: &str = "Microsoft.AspNetCore.SignalR.HubException";
/// `E_FAIL`: the `HResult` of every `WebSocketException` the client throws, shown by
/// its `ToString()` (`WebSocketException (0x80004005): ...`).
const E_FAIL: u32 = 0x8000_4005;

fn hub_exception(e: &dl_signalr::Error) -> Exception {
    match e {
        // The .NET client's HubException carries the server's text as its message.
        dl_signalr::Error::Hub { message, .. } => Exception::new(HUB_EXCEPTION, message.clone()),
        dl_signalr::Error::Timeout => Exception::new(
            "System.OperationCanceledException",
            "The operation was canceled.",
        ),
        other => Exception::from_error(other),
    }
}

/// What `HubConnection.StartAsync` throws for a failed start (WebSockets transport,
/// negotiation skipped), as far as the .NET text is certain:
/// - upgrade answered with another status: `ClientWebSocket` throws `WebSocketException`
///   `The server returned status code '404' when status code '101' was expected.`;
/// - connect failed (DNS, refused, TLS): `WebSocketException`
///   `Unable to connect to the remote server` with the `HttpRequestException` inside;
/// - an invalid upgrade answer or a socket failure before the hub handshake: a
///   `WebSocketException` (the .NET text varies; the DevLibrary text is kept);
/// - the server's handshake error: `HubException`
///   `Unable to complete handshake with the server due to an error: <error>`.
///
/// Anything else keeps the generic mapping.
pub fn connect_exception(e: &dl_signalr::Error) -> Exception {
    let ws = |message: String| {
        let mut ex = Exception::new(WEB_SOCKET_EXCEPTION, message);
        ex.hresult = Some(E_FAIL);
        ex
    };
    match e {
        dl_signalr::Error::WebSocket(w) => match &**w {
            dl_ws::Error::Rejected { status, .. } => ws(format!(
                "The server returned status code '{status}' when status code '101' was expected."
            )),
            dl_ws::Error::Http(h) => ws("Unable to connect to the remote server".to_owned())
                .with_inner(Exception::new(
                    "System.Net.Http.HttpRequestException",
                    h.to_string(),
                )),
            // The socket failed after the upgrade or the upgrade answer is invalid: a
            // WebSocketException, but the .NET text varies; the DevLibrary text is kept.
            dl_ws::Error::Io(_) | dl_ws::Error::Handshake(_) | dl_ws::Error::ConnectionReset => {
                ws(w.to_string())
            }
            _ => hub_exception(e),
        },
        dl_signalr::Error::Handshake(error) => Exception::new(
            HUB_EXCEPTION,
            format!("Unable to complete handshake with the server due to an error: {error}"),
        ),
        _ => hub_exception(e),
    }
}

/// Whether a connect failure makes the C# relay drop the cached token: only an
/// `HttpRequestException` with status 401, which the WebSockets transport without
/// negotiation never throws (a refused upgrade is a `WebSocketException`). So, as in C#,
/// a 401 from the hub keeps the token and the next attempt reuses it (no token
/// exchange per retry); only a refused negotiate (not used here) would count.
fn is_unauthorized(e: &dl_signalr::Error) -> bool {
    matches!(e, dl_signalr::Error::NegotiateRejected { status: 401, .. })
}

/// Why a closed connection ended, as the C# `Closed` handler words it.
fn close_text(reason: &CloseReason) -> String {
    match reason {
        CloseReason::Server {
            error: Some(error), ..
        } => format!("The server closed the connection with the following error: {error}"),
        CloseReason::Server { error: None, .. } | CloseReason::Local => {
            "closed by the cloud".to_owned()
        }
        CloseReason::Transport(e) => e.to_string(),
    }
}

/// How the last connection went (the C# `onUp` captures).
#[derive(Debug, Default)]
struct LinkState {
    failures: u32,
    was_up: bool,
    last_failure: Option<String>,
}

impl PrintRelayService {
    /// The service over `deps`.
    pub fn new(deps: PrintRelayDependencies, retry: RetryPolicy) -> PrintRelayService {
        let log = deps.log.for_category(CATEGORY);
        PrintRelayService {
            deps: Arc::new(deps),
            retry,
            log,
            engine: None,
            ack_loop: None,
        }
    }

    fn connect_hub(
        &self,
        engine: &PrintRelayEngine,
        ctx: &Ctx,
    ) -> Result<HubConnection, Box<Exception>> {
        let (e, log) = (engine.clone(), self.log.clone());
        start_hub(
            &self.deps.identity.cloud_url,
            &self.deps.tokens,
            move |options| {
                options.on("PrintJob", move |args| match parse_job(args) {
                    Ok(job) => e.accept(&job),
                    Err(err) => log.warn_ex(
                        Exception::new("System.Text.Json.JsonException", err.to_string()),
                        "Print relay job could not be read; it waits in the cloud",
                        &[],
                    ),
                });
            },
            ctx,
        )
    }

    /// One hub connection until it closes; returns why it ended (`RunConnectionAsync`).
    fn run_connection(
        &self,
        engine: &PrintRelayEngine,
        ctx: &Ctx,
        state: &mut LinkState,
    ) -> String {
        let hub = match self.connect_hub(engine, ctx) {
            Ok(h) => h,
            Err(ex) => {
                let reason = ex.message.clone();
                self.log.warn_ex(*ex, "Print relay connection failed", &[]);
                return reason;
            }
        };
        let joined = (|| -> Result<Vec<i64>, dl_signalr::Error> {
            let branches = hub.invoke("PrintRelayBranches", vec![], Some(HUB_CALL_TIMEOUT))?;
            let rvcs: Vec<i64> = match &branches {
                JsonValue::Array(items) => items.iter().filter_map(JsonValue::as_i64).collect(),
                _ => Vec::new(),
            };
            for rvc in &rvcs {
                hub.invoke(
                    "JoinPrintRelay",
                    vec![JsonValue::from(*rvc)],
                    Some(HUB_CALL_TIMEOUT),
                )?;
            }
            Ok(rvcs)
        })();
        let reason = match joined {
            Err(err) => {
                let ex = hub_exception(&err);
                let reason = ex.message.clone();
                self.log.warn_ex(ex, "Print relay connection failed", &[]);
                reason
            }
            Ok(rvcs) => {
                engine.connect(Some(Arc::new(HubLink(hub.clone()))));
                state.was_up = true;
                state.failures = 0;
                state.last_failure = None;
                let mut detail = Map::new();
                detail.insert(
                    "rvcs",
                    JsonValue::Array(rvcs.iter().map(|r| JsonValue::from(*r)).collect()),
                );
                self.deps.events.report(
                    INFO,
                    "PRINT_RELAY_UP",
                    &format!("The print relay is connected for {} branch(es)", rvcs.len()),
                    Some(detail),
                );
                loop {
                    if ctx.is_done() {
                        break "stopping".to_owned();
                    }
                    if let Some(r) = hub.wait_closed_timeout(STOP_POLL) {
                        break close_text(&r);
                    }
                }
            }
        };
        engine.connect(None);
        hub.close();
        reason
    }

    fn start_engine(
        &mut self,
        policy: PrinterTargetPolicy,
        ctx: &Ctx,
    ) -> Result<PrintRelayEngine, ServiceError> {
        if let Some(e) = &self.engine {
            return Ok(e.clone());
        }
        let o = &self.deps.options.print_relay;
        let dir = match o.queue_dir.as_deref() {
            Some(d) if !d.is_empty() => std::path::PathBuf::from(d),
            _ => self.deps.paths.data_dir().join("print-queue"),
        };
        let journal = PrintJournal::new(dir, &self.deps.log)?;
        let engine = PrintRelayEngine::new(
            PrintRelayEngineParts {
                journal,
                policy,
                printer: self.deps.printer.clone(),
                events: self.deps.events.clone(),
                clock: self.deps.clock.clone(),
                spooler: self.deps.spooler.clone(),
                spooler_queues: self.deps.spooler_queues.clone(),
            },
            &self.deps.log,
        );
        // As C#: the engine is kept even when recovery fails (the restart then skips it).
        self.engine = Some(engine.clone());
        engine.recover()?;
        let (e, c) = (engine.clone(), ctx.clone());
        self.ack_loop = Some(
            std::thread::Builder::new()
                .name("PrintRelayAcks".to_owned())
                .spawn(move || e.run_ack_loop(&c))?,
        );
        Ok(engine)
    }

    fn rediscover(&self, ctx: &Ctx) -> Option<std::thread::JoinHandle<()>> {
        let o = &self.deps.options.print_relay;
        if !o.discover_queues {
            return None;
        }
        let minutes = u64::try_from(o.discovery_minutes.max(5)).unwrap_or(5);
        let (discovery, c) = (self.deps.discovery.clone(), ctx.clone());
        std::thread::Builder::new()
            .name("PrintRelayDiscovery".to_owned())
            .spawn(move || {
                let mut timer = PeriodicTicks::new(Duration::from_secs(minutes * 60));
                while timer.wait(&c) {
                    if discovery.run(&c).is_err() {
                        return;
                    }
                }
            })
            .ok()
    }
}

impl LoopService for PrintRelayService {
    fn name(&self) -> &'static str {
        "PrintRelay"
    }

    fn run_loop(&mut self, ctx: &Ctx) -> Result<(), ServiceError> {
        let options = self.deps.options.print_relay.clone();
        if !options.enabled {
            self.deps.events.report(
                INFO,
                "PRINT_RELAY_DISABLED",
                "The print relay is turned off on this server (Agent:PrintRelay:Enabled=false)",
                None,
            );
            return Ok(());
        }
        let queues = if options.discover_queues {
            match self.deps.discovery.run(ctx) {
                Ok(q) => q,
                Err(_) => return Ok(()),
            }
        } else {
            Vec::new()
        };
        let auto = adapter_networks(&WindowsRegistry)
            .into_iter()
            .chain(host_networks(&queues));
        let policy = match PrinterTargetPolicy::create(&options, auto, None) {
            Ok(p) => p,
            Err(error) => {
                self.deps.events.report(
                    ERROR,
                    "PRINT_RELAY_CONFIG_INVALID",
                    &format!(
                        "The print relay stays off: {error}. Fix Agent:PrintRelay in appsettings.site.json and restart the service"
                    ),
                    None,
                );
                return Ok(());
            }
        };
        let engine = self.start_engine(policy, ctx)?;
        let rediscovery = self.rediscover(ctx);
        let mut state = LinkState::default();
        while !ctx.is_done() {
            let reason = self.run_connection(&engine, ctx, &mut state);
            if ctx.is_done() {
                break;
            }
            engine.connect(None);
            state.failures += 1;
            // A link that was up and then dropped is usually a cloud redeploy: retry at once
            // and only warn when that reconnect fails too (as the command channel does,
            // CW1). A new failure reason while down is still reported.
            if state.was_up {
                self.log.info(
                    "Print relay dropped ({Reason}); reconnecting",
                    &[Value::from(reason.as_str())],
                );
            } else if state.last_failure.as_deref() != Some(reason.as_str()) {
                self.deps.events.report(
                    WARN,
                    "PRINT_RELAY_DOWN",
                    &format!(
                        "The print relay is disconnected from the cloud ({reason}); jobs wait in the cloud and print once it reconnects"
                    ),
                    None,
                );
            }
            state.last_failure = if state.was_up { None } else { Some(reason) };
            state.was_up = false;
            let delay = self.retry.delay_for(state.failures.min(8), None);
            if !(self.retry.sleep)(delay, ctx) {
                break;
            }
        }
        engine.connect(None);
        if let Some(r) = rediscovery {
            let _ = r.join();
        }
        if ctx.is_done()
            && let Some(a) = self.ack_loop.take()
        {
            // Stopping: pending acks stay in the journal and are sent after the next start.
            let _ = a.join();
        }
        Ok(())
    }
}

/// `AddPrintRelay`: queue discovery, the journaled relay and its hub connection, as a
/// [`RuntimeHooks::after_capture`](crate::hosting::host::RuntimeHooks) factory.
pub fn services(
    s: &AgentServices,
    rt: &RuntimeParts,
) -> Result<Vec<Box<dyn LoopService>>, Unhandled> {
    let spooler_queues = Arc::new(SpoolerQueues::new());
    let discovery = Arc::new(PrintQueueDiscovery::new(
        Arc::new(WindowsPrintQueueSource::default()),
        Arc::new(CloudPrintQueueReporter::new(
            s.http.clone(),
            s.tokens.clone(),
        )),
        rt.site_events.clone(),
        s.identity.agent_id,
        &s.log,
        Some(spooler_queues.clone()),
    ));
    let deps = PrintRelayDependencies {
        options: s.options.clone(),
        paths: s.paths.clone(),
        identity: s.identity.clone(),
        tokens: s.tokens.clone(),
        events: rt.site_events.clone(),
        discovery,
        printer: Arc::new(RawTcpPrinter),
        clock: s.clock.clone(),
        log: s.log.clone(),
        spooler: Arc::new(WindowsSpoolerPrinter),
        spooler_queues,
    };
    Ok(vec![Box::new(PrintRelayService::new(
        deps,
        s.retry.clone(),
    ))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_order_hub_uses_the_cloud_calls_proxy() {
        let o = hub_client_options();
        assert_eq!(o.proxy, crate::transport::http::proxy());
        assert_eq!(o.timeout, dl_http::ClientOptions::default().timeout);
    }
}
