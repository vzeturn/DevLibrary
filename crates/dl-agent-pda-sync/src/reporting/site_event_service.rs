//! The service's site events (port of `Reporting/SiteEventService.cs`): `AGENT_STARTED`
//! (version, POS database server), health transitions every 15 s, log-bridge summaries, a
//! flush every 15 s and right after an error event, and `AGENT_STOPPING` with a last
//! bounded flush. Every failure here is logged and retried; capture, upload and replay
//! never wait for it.
//!
//! Not here (hosting's part): `WindowsServiceRecovery.TryConfigure` at start when running
//! as a Windows service.

use std::sync::Arc;
use std::time::Duration;

use dl_json::Map;

use super::log::{Exception, Level, Logger, Value};
use super::site_event_bridge::SiteEventLogBridge;
use super::site_events::{SiteEventFlusher, SiteEventReporter, SiteEventTransport, SiteEvents};
use super::site_health::SiteHealthMonitor;
use crate::contracts::wire::site_event_levels::INFO;
use crate::util::ctx::Ctx;

/// `ILogger<SiteEventService>`.
pub const SERVICE_CATEGORY: &str = "MM.PdaOrder.SyncAgent.Reporting.SiteEventService";
/// The start event.
pub const AGENT_STARTED: &str = "AGENT_STARTED";
/// The stop event.
pub const AGENT_STOPPING: &str = "AGENT_STOPPING";
/// Health check and flush interval.
pub const INTERVAL: Duration = Duration::from_secs(15);
/// Budget of the last flush at stop.
pub const STOP_FLUSH_BUDGET: Duration = Duration::from_secs(5);

/// The POS database server's version (`SqlServerInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlServerInfo {
    /// `SERVERPROPERTY('ProductVersion')`.
    pub product_version: String,
    /// `SERVERPROPERTY('Edition')`.
    pub edition: String,
    /// `SERVERPROPERTY('ProductLevel')`.
    pub product_level: String,
}

/// Reads the POS database server's version as the runtime login, read-only
/// (`ISqlServerInfoSource`). The error is the failure as a logged exception (its `sql`
/// facts set for a SQL Server error).
pub trait SqlServerInfoSource: Send + Sync {
    /// Reads the version.
    fn read(&self, ctx: &Ctx) -> Result<SqlServerInfo, Box<Exception>>;
}

/// The parts of the service.
pub struct SiteEventServiceParts {
    /// The reporter (its flush requests wake the loop).
    pub events: Arc<SiteEventReporter>,
    /// The log bridge (attached at start).
    pub bridge: Arc<SiteEventLogBridge>,
    /// The flusher.
    pub flusher: Arc<SiteEventFlusher>,
    /// The cloud transport.
    pub transport: Arc<dyn SiteEventTransport>,
    /// The health monitor.
    pub monitor: Arc<SiteHealthMonitor>,
    /// The POS server version reader.
    pub sql_info: Arc<dyn SqlServerInfoSource>,
    /// `HeartbeatSender.AgentVersion`.
    pub agent_version: String,
}

/// The service (`SiteEventService`): [`Self::run`] on its own thread until stopping, then
/// [`Self::stop`].
pub struct SiteEventService {
    p: SiteEventServiceParts,
    log: Logger,
}

impl std::fmt::Debug for SiteEventService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteEventService").finish_non_exhaustive()
    }
}

fn version_detail(v: &str) -> Map {
    let mut m = Map::new();
    m.insert("version", v);
    m
}

impl SiteEventService {
    /// A service logging under [`SERVICE_CATEGORY`].
    pub fn new(parts: SiteEventServiceParts, log: &Logger) -> SiteEventService {
        SiteEventService {
            p: parts,
            log: log.for_category(SERVICE_CATEGORY),
        }
    }

    /// `ExecuteAsync`: attaches the bridge, reports `AGENT_STARTED`, then checks health and
    /// flushes every [`INTERVAL`] (or at once after an error event) until `ctx` ends.
    pub fn run(&self, ctx: &Ctx) {
        self.p.bridge.attach(self.p.events.clone());
        self.report_started(ctx);
        while !ctx.is_done() {
            self.run_once();
            self.flush(ctx);
            if !self.wait(ctx) {
                return;
            }
        }
    }

    /// `StopAsync` after [`Self::run`] returned: reports `AGENT_STOPPING`, drains the
    /// bridge and flushes for at most [`STOP_FLUSH_BUDGET`] (`ctx` is the host's stop token).
    pub fn stop(&self, ctx: &Ctx) {
        self.p.events.report(
            INFO,
            AGENT_STOPPING,
            "The agent service is stopping",
            Some(version_detail(&self.p.agent_version)),
        );
        self.p.bridge.drain();
        self.flush(&ctx.with_timeout(STOP_FLUSH_BUDGET));
    }

    /// One health check and one summary pass.
    pub fn run_once(&self) {
        if let Err(ex) = self.p.monitor.check() {
            self.log.warn_ex(
                *ex,
                "Site health check failed; retrying in {Seconds} s",
                &[Value::from(15)],
            );
        }
        self.p.bridge.tick();
    }

    fn report_started(&self, ctx: &Ctx) {
        let v = &self.p.agent_version;
        let mut detail = version_detail(v);
        match self.p.sql_info.read(ctx) {
            Ok(info) => {
                detail.insert("sql_version", info.product_version);
                detail.insert("sql_edition", info.edition);
                detail.insert("sql_level", info.product_level);
            }
            Err(ex) => {
                // Not fatal: capture reports the database state on its own.
                let why = match &ex.sql {
                    Some(sql) => format!("SQL error {}", sql.number()),
                    None => ex
                        .type_name
                        .rsplit('.')
                        .next()
                        .unwrap_or(&ex.type_name)
                        .to_owned(),
                };
                detail.insert("sql_error", why);
                self.log.write(
                    Level::Debug,
                    Some(*ex),
                    "POS database version not read at start",
                    &[],
                );
            }
        }
        self.p.events.report(
            INFO,
            AGENT_STARTED,
            &format!("Agent {v} started"),
            Some(detail),
        );
    }

    fn flush(&self, ctx: &Ctx) {
        match self.p.flusher.flush(&*self.p.transport, ctx) {
            Ok(_) => {}
            Err(e) if e.is_cancelled() && ctx.is_done() => {
                self.log.write(
                    Level::Debug,
                    Some(e.to_exception()),
                    "Site event flush cancelled; the events stay queued",
                    &[],
                );
            }
            Err(e) => {
                self.log.warn_ex(
                    e.to_exception(),
                    "Site event flush failed; the events stay queued and are retried in {Seconds} s",
                    &[Value::from(15)],
                );
            }
        }
    }

    /// Waits for the interval or an error event; false when the service is stopping.
    fn wait(&self, ctx: &Ctx) -> bool {
        self.p.events.wait_flush_request(INTERVAL, ctx);
        // Coalesce: every error event since the last flush is covered by the next flush.
        let coalesced = usize::from(self.p.events.take_flush_request());
        if coalesced > 1 {
            self.log.debug(
                "{Count} flush requests coalesced into one flush",
                &[Value::from(coalesced)],
            );
        }
        !ctx.is_done()
    }
}
