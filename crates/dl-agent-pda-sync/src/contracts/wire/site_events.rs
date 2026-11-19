//! Site events (`Wire/SiteEvents.cs`, ADR 0092).

use dl_json::Value;
use dl_uuid::Uuid;

use crate::contracts::json::{self, WireJson};
use crate::contracts::time::DateTimeOffset;
use crate::util::stj::{Obj, ReadError, Writer};

/// One thing that happened at a site, reported to the cloud. Never row data.
#[derive(Debug, Clone, PartialEq)]
pub struct SiteEvent {
    /// Made by the sender; the cloud keeps one row per (tenant, event id).
    pub event_id: Uuid,
    /// When, on the sender's clock.
    pub at_utc: DateTimeOffset,
    /// One of [`site_event_sources`].
    pub source: String,
    /// One of [`site_event_levels`].
    pub level: String,
    /// Stable UPPER_SNAKE code.
    pub code: String,
    /// One English line, no secrets.
    pub message: String,
    /// Groups the events of one command run.
    pub run_id: Option<String>,
    /// A small JSON object (counts, names, versions). Never row values.
    pub detail: Option<Value>,
}

impl WireJson for SiteEvent {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("event_id").string(&self.event_id.to_string());
        w.key("at_utc").unescaped(&self.at_utc.to_json_string());
        w.key("source").string(&self.source);
        w.key("level").string(&self.level);
        w.key("code").string(&self.code);
        w.key("message").string(&self.message);
        json::opt_str(w, "run_id", &self.run_id);
        json::opt_value(w, "detail", &self.detail);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SiteEvent {
            event_id: o.guid("event_id")?,
            at_utc: json::dto(&o, "at_utc")?,
            source: o.string("source")?,
            level: o.string("level")?,
            code: o.string("code")?,
            message: o.string("message")?,
            run_id: o.opt_string("run_id")?,
            detail: json::opt_element(&o, "detail"),
        })
    }
}

/// Body of `POST /sync/v1/events`.
#[derive(Debug, Clone, PartialEq)]
pub struct SiteEventBatch {
    /// Agent id.
    pub agent_id: String,
    /// Agent version.
    pub agent_version: String,
    /// Events.
    pub events: Vec<SiteEvent>,
}

impl WireJson for SiteEventBatch {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("agent_id").string(&self.agent_id);
        w.key("agent_version").string(&self.agent_version);
        w.key("events");
        json::write_items(w, &self.events);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SiteEventBatch {
            agent_id: o.string("agent_id")?,
            agent_version: o.string("agent_version")?,
            events: o.list("events", json::item)?,
        })
    }
}

/// Who wrote the event.
pub mod site_event_sources {
    /// The running SyncAgent service.
    pub const AGENT: &str = "agent";
    /// `enroll`.
    pub const ENROLL: &str = "enroll";
    /// `legacy-export`.
    pub const LEGACY_EXPORT: &str = "legacy-export";
    /// `legacy-archive`.
    pub const LEGACY_ARCHIVE: &str = "legacy-archive";
    /// `legacy-upload` (ADR 0095).
    pub const LEGACY_UPLOAD: &str = "legacy-upload";
    /// The cloud side of the agent-driven import.
    pub const LEGACY_IMPORT: &str = "legacy-import";
    /// TenantCli on the operator machine.
    pub const TENANT_CLI: &str = "tenant-cli";
    /// The cloud's batch ingest.
    pub const SYNC_INGEST: &str = "sync-ingest";
    /// The cloud's site command channel.
    pub const SITE_COMMAND: &str = "site-command";
    /// The cloud's readiness gate.
    pub const READINESS: &str = "readiness";
    /// All sources.
    pub const ALL: &[&str] = &[
        AGENT,
        ENROLL,
        LEGACY_EXPORT,
        LEGACY_ARCHIVE,
        LEGACY_UPLOAD,
        LEGACY_IMPORT,
        TENANT_CLI,
        SYNC_INGEST,
        SITE_COMMAND,
        READINESS,
    ];

    /// Ordinal membership.
    pub fn is_known(source: Option<&str>) -> bool {
        source.is_some_and(|s| ALL.contains(&s))
    }
}

/// Event levels.
pub mod site_event_levels {
    /// Info.
    pub const INFO: &str = "info";
    /// Warn.
    pub const WARN: &str = "warn";
    /// Error.
    pub const ERROR: &str = "error";
    /// All levels.
    pub const ALL: &[&str] = &[INFO, WARN, ERROR];

    /// Ordinal membership.
    pub fn is_known(level: Option<&str>) -> bool {
        level.is_some_and(|l| ALL.contains(&l))
    }
}

/// Size limits both sides apply.
pub mod site_event_limits {
    /// Events per request.
    pub const MAX_BATCH: usize = 200;
    /// Longest code.
    pub const MAX_CODE: usize = 64;
    /// Longest run id.
    pub const MAX_RUN_ID: usize = 64;
    /// Longer messages are cut to this length.
    pub const MAX_MESSAGE: usize = 1000;
    /// UTF-8 bytes of a detail; larger becomes `{"truncated":true}`.
    pub const MAX_DETAIL_BYTES: usize = 4096;
}
