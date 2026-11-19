//! Builds and posts one heartbeat (port of `Outbox/HeartbeatSender.cs`); applies any
//! `config` object in the response. After every attempt the service's health (heartbeat
//! answer + capture state) goes to `status.json` through [`AgentHealth`].

use std::sync::Arc;

use super::batch_uploader::to_dto;
use super::batches::SqliteOutbox;
use super::blobs::SqliteBlobOutbox;
use super::error::{OutboxError, UploadError};
use crate::config::time::SharedClock;
use crate::config::{AgentHealth, AgentIdentity, RuntimeConfigStore};
use crate::contracts::WireJson;
use crate::contracts::catalog::SyncCatalog;
use crate::contracts::wire::{StreamLag, StreamResnapshot, SyncHeartbeat, wire_version};
use crate::reporting::{Logger, Value};
use crate::transport::{SyncHttpClient, TokenProvider};
use crate::util::ctx::Ctx;

/// `MM.PdaOrder.SyncAgent.Outbox.HeartbeatSender`: the log category.
pub const HEARTBEAT_CATEGORY: &str = "MM.PdaOrder.SyncAgent.Outbox.HeartbeatSender";

/// The heartbeat route.
pub const HEARTBEAT_PATH: &str = "/sync/v1/heartbeat";

/// What the heartbeat reports about capture (C# `ICaptureStatus`).
pub trait CaptureStatus: Send + Sync {
    /// Whether SQL Server answered the last capture cycle.
    fn sql_reachable(&self) -> bool;
    /// Per-stream lag.
    fn streams(&self) -> Vec<StreamLag>;
    /// Stable code of the failure that stops capture (`SQL_LOGIN_FAILED`, ...).
    fn capture_error(&self) -> Option<String>;
}

/// Sends heartbeats (`HeartbeatSender`).
pub struct HeartbeatSender {
    agent_id: String,
    outbox: Arc<SqliteOutbox>,
    blobs: Arc<SqliteBlobOutbox>,
    capture: Arc<dyn CaptureStatus>,
    http: Arc<SyncHttpClient>,
    tokens: Arc<TokenProvider>,
    config: Arc<RuntimeConfigStore>,
    clock: SharedClock,
    health: Arc<AgentHealth>,
    agent_version: String,
    log: Logger,
}

impl std::fmt::Debug for HeartbeatSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeartbeatSender").finish_non_exhaustive()
    }
}

/// The pieces a [`HeartbeatSender`] reads and writes.
pub struct HeartbeatParts {
    /// The enrolled identity.
    pub identity: AgentIdentity,
    /// The batch queue (depth, finished resnapshots).
    pub outbox: Arc<SqliteOutbox>,
    /// The blob queue (depth).
    pub blobs: Arc<SqliteBlobOutbox>,
    /// Capture state.
    pub capture: Arc<dyn CaptureStatus>,
    /// The signed client.
    pub http: Arc<SyncHttpClient>,
    /// Access tokens.
    pub tokens: Arc<TokenProvider>,
    /// Cloud-pushed tunables.
    pub config: Arc<RuntimeConfigStore>,
    /// The clock.
    pub clock: SharedClock,
    /// `status.json` writer.
    pub health: Arc<AgentHealth>,
    /// `agent_version` (C# `AgentBuild.VersionText` of the assembly).
    pub agent_version: String,
}

impl HeartbeatSender {
    /// A sender over `parts`.
    pub fn new(parts: HeartbeatParts, log: &Logger) -> HeartbeatSender {
        HeartbeatSender {
            agent_id: parts.identity.agent_id.to_string(),
            outbox: parts.outbox,
            blobs: parts.blobs,
            capture: parts.capture,
            http: parts.http,
            tokens: parts.tokens,
            config: parts.config,
            clock: parts.clock,
            health: parts.health,
            agent_version: parts.agent_version,
            log: log.for_category(HEARTBEAT_CATEGORY),
        }
    }

    /// The heartbeat; `resnapshots` are clean stream resnapshots the cloud has not been
    /// told about (ADR 0102), sent only when there are some.
    pub fn build(&self, resnapshots: &[StreamResnapshot]) -> Result<SyncHeartbeat, OutboxError> {
        let catalog = SyncCatalog::load().map_err(|e| OutboxError::Corrupt(e.to_string()))?;
        Ok(SyncHeartbeat {
            agent_id: self.agent_id.clone(),
            at_utc: to_dto(self.clock.now()),
            agent_version: self.agent_version.clone(),
            envelope_version: wire_version::ENVELOPE,
            mapper_version: wire_version::MAPPER,
            catalog_version: catalog.catalog_version,
            sql_reachable: self.capture.sql_reachable(),
            queue_depth: self.outbox.depth()?,
            streams: self.capture.streams(),
            blob_queue_depth: Some(self.blobs.depth()?),
            capture_error: self.capture.capture_error(),
            resnapshots: (!resnapshots.is_empty()).then(|| resnapshots.to_vec()),
        })
    }

    /// Posts one heartbeat: `Ok(false)` when the cloud answered with an error status, an
    /// error when it could not be reached (recorded as degraded first).
    pub fn send(&self, ctx: &Ctx) -> Result<bool, UploadError> {
        // Carried until a heartbeat is accepted; the cloud supersedes the streams' rejected batches from them.
        let resnapshots = self
            .outbox
            .finished_stream_resnapshots(to_dto(self.clock.now()))?;
        let attempt = (|| {
            let bearer = self.tokens.get_access_token(ctx)?;
            let body = self
                .build(&resnapshots)
                .map_err(UploadError::Outbox)?
                .to_json();
            Ok::<_, UploadError>(
                self.http
                    .post_json(ctx, HEARTBEAT_PATH, &body, Some(&bearer))?,
            )
        })();
        let resp = match attempt {
            Ok(r) => r,
            Err(e) => {
                // No answer (cloud unreachable, token refused): recorded as degraded, then the loop's own handling continues.
                if let UploadError::Transport(t) = &e
                    && let Some(code) = t.code()
                {
                    self.health.record(
                        None,
                        Some(code),
                        self.capture.sql_reachable(),
                        self.capture.capture_error().as_deref(),
                    );
                }
                return Err(e);
            }
        };
        self.health.record(
            Some(i32::from(resp.status)),
            None,
            self.capture.sql_reachable(),
            self.capture.capture_error().as_deref(),
        );
        if resp.status == 401 {
            self.tokens.invalidate();
        }
        if !resp.is_success() {
            self.log.warn(
                "Heartbeat rejected with HTTP {Status}",
                &[Value::from(i32::from(resp.status))],
            );
            return Ok(false);
        }
        if !resnapshots.is_empty() {
            self.outbox.mark_stream_resnapshots_reported(&resnapshots)?;
        }
        if let Some(data) = resp.data()?
            && data.is_object()
            && let Some(cfg) = crate::transport::data_member(&data, "config")?
        {
            self.config
                .apply(cfg)
                .map_err(|e| UploadError::Outbox(OutboxError::Io(e)))?;
        }
        Ok(true)
    }
}
