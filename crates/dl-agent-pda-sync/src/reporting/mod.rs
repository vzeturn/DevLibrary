//! Logging, site events, health and tool runs (port of `Reporting/`).
//!
//! Ported so far: the structured logger ([`log`]), Serilog message templates
//! ([`template`]), the JSON-lines line format ([`format`], `JsonLogFormatter.cs`), the
//! rolling log file ([`file_sink`]), `AgentLogging.cs` ([`agent_logging`]),
//! `SecretScrubber.cs` ([`scrubber`]), `SqlErrorDetail.cs` ([`sql_error`]) and
//! `RunStages.cs` ([`run_stages`]), the site event queue ([`site_event_store`]), reporter,
//! transport and flusher ([`site_events`]), the log bridge ([`site_event_bridge`], a
//! [`log::Sink`]), `SiteHealthMonitor.cs` ([`site_health`]), `SiteEventService.cs`
//! ([`site_event_service`]) and `ToolRun.cs` ([`tool_run`]).

pub mod agent_logging;
pub mod file_sink;
pub mod format;
pub mod log;
pub mod run_stages;
pub mod scrubber;
pub mod site_event_bridge;
pub mod site_event_service;
pub mod site_event_store;
pub mod site_events;
pub mod site_health;
pub mod sql_error;
pub mod template;
pub mod tool_run;

pub use agent_logging::ToolLogTarget;
pub use log::{Event, Exception, Level, Logger, LoggerConfig, MemorySink, Property, Sink, Value};
pub use run_stages::{NullRunStages, RunStages};
pub use scrubber::{MASK, register as register_secret, scrub};
pub use site_event_bridge::SiteEventLogBridge;
pub use site_event_service::{
    SiteEventService, SiteEventServiceParts, SqlServerInfo, SqlServerInfoSource,
};
pub use site_event_store::{
    MemorySiteEventStore, SiteEventQueueStats, SiteEventStore, SqliteSiteEventStore,
};
pub use site_events::{
    CloudSiteEventTransport, SiteEventFlushResult, SiteEventFlusher, SiteEventReporter,
    SiteEventSendResult, SiteEventSendStatus, SiteEventTransport, SiteEvents,
};
pub use site_health::{CaptureHealth, SiteHealthMonitor};
pub use sql_error::SqlFailure;
pub use tool_run::{ToolRun, ToolRunOptions};
