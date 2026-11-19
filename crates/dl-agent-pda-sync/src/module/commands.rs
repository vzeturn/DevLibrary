//! The hub commands of the module (`module: "pda-sync"`):
//!
//! - `status`: enrolment, the last `status.json` state, queue depths, site event queue;
//! - `selftest`: the module's self-test rows (the same as `dl-agent selftest`);
//! - `capture_dry_run`: one capture pass against `SMILE_POS` into memory, counts and
//!   hashes per stream, nothing queued or uploaded ([`super::dry_run`]). It runs on its
//!   own thread, one at a time, and completes the command when done.
//!
//! Kinds may carry the module prefix (`pda-sync.status`).

use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use dl_agent::command::Command;
use dl_agent::report::Severity;
use dl_agent::supervisor::ModuleContext;
use dl_json::{Map, Value};

use super::{NAME, NOT_ENROLLED, dry_run};
use crate::capture::{CaptureSource, SqlServerCaptureSource, SqlServerSourceOptions};
use crate::config::time::{SystemClock, Timestamp};
use crate::config::{
    AgentIdentity, AgentOptions, AgentPaths, AgentStatus, RuntimeConfig, RuntimeConfigStore,
};
use crate::diagnostics::SiteEventQueueStats;
use crate::hosting::HostContext;
use crate::hosting::host::{AgentServices, site_time_zone};
use crate::outbox::{SqliteBlobOutbox, SqliteOutbox};
use crate::security::SharedSecretStore;
use crate::util::ctx::Ctx;

/// `status`.
pub const STATUS: &str = "status";
/// `selftest`.
pub const SELFTEST: &str = "selftest";
/// `capture_dry_run`.
pub const CAPTURE_DRY_RUN: &str = "capture_dry_run";

/// The queues of an enrolled run.
struct Queues {
    outbox: Arc<SqliteOutbox>,
    blobs: Arc<SqliteBlobOutbox>,
    runtime_config: Arc<RuntimeConfigStore>,
}

/// What the commands read: the folder, the options, and once enrolled the identity and
/// the run's queues.
pub struct Runtime {
    /// The module's data folder.
    pub paths: AgentPaths,
    options: AgentOptions,
    identity: Option<AgentIdentity>,
    secrets: SharedSecretStore,
    queues: Option<Queues>,
    dry: Mutex<Option<(JoinHandle<()>, Ctx)>>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("paths", &self.paths)
            .field("enrolled", &self.identity.is_some())
            .finish_non_exhaustive()
    }
}

impl Runtime {
    /// Before enrolment.
    pub fn waiting(host: &HostContext) -> Runtime {
        Runtime {
            paths: host.paths.clone(),
            options: host.options.clone(),
            identity: None,
            secrets: crate::security::create(&host.paths),
            queues: None,
            dry: Mutex::new(None),
        }
    }

    /// Over a running host's services.
    pub fn enrolled(host: &HostContext, identity: AgentIdentity, s: &AgentServices) -> Runtime {
        Runtime {
            paths: host.paths.clone(),
            options: host.options.clone(),
            identity: Some(identity),
            secrets: s.secrets.clone(),
            queues: Some(Queues {
                outbox: s.outbox.clone(),
                blobs: s.blobs.clone(),
                runtime_config: s.runtime_config.clone(),
            }),
            dry: Mutex::new(None),
        }
    }

    /// Cancels a running dry pass and waits for it.
    pub fn finish(&self, ctx: &ModuleContext) {
        let running = self
            .dry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some((h, c)) = running {
            c.cancel();
            joined(ctx, h);
        }
    }

    /// Answers one command.
    pub fn answer(&self, ctx: &ModuleContext, cmd: &Command) {
        let kind = strip_prefix(&cmd.kind);
        match kind {
            STATUS => {
                let body = Value::Object(self.status());
                ctx.complete(&cmd.id, true, &dl_json::to_string(&body));
            }
            SELFTEST => {
                let checks = super::selftest::checks(ctx);
                let failed = checks
                    .iter()
                    .any(|c| c.status == dl_agent::selftest::Status::Fail);
                let rows = Value::Array(checks.iter().map(|c| c.to_json()).collect());
                ctx.complete(&cmd.id, !failed, &dl_json::to_string(&rows));
            }
            CAPTURE_DRY_RUN => self.start_dry_run(ctx, cmd),
            _ => ctx.complete(
                &cmd.id,
                false,
                &format!("unknown {NAME} command {:?}", cmd.kind),
            ),
        }
    }

    /// The `status` body.
    pub fn status(&self) -> Map {
        let mut m = Map::new();
        m.insert("enrolled", self.identity.is_some());
        if let Some(id) = &self.identity {
            m.insert("agent_id", id.agent_id_text());
            m.insert("tenant_id", id.tenant_id.as_str());
            m.insert("cloud_url", id.cloud_url.as_str());
            m.insert("sql_instance", id.sql_instance.clone().unwrap_or_default());
            m.insert("sql_database", id.sql_database.clone().unwrap_or_default());
        } else {
            m.insert("detail", NOT_ENROLLED);
        }
        match AgentStatus::try_read(&self.paths) {
            Ok(Some(s)) => {
                m.insert("state", s.state.as_str());
                if let Some(c) = &s.error_code {
                    m.insert("error_code", c.as_str());
                }
                if let Some(h) = &s.hint {
                    m.insert("hint", h.as_str());
                }
                m.insert("state_at", s.at_utc.to_stj());
            }
            Ok(None) => {}
            Err(e) => {
                m.insert("state_error", e.to_string());
            }
        }
        for (k, v) in self.metrics() {
            m.insert(k, v);
        }
        m.insert(
            "site_events",
            SiteEventQueueStats::read(&self.paths.events_db()).describe(Timestamp::now_utc()),
        );
        m.insert("version", crate::hosting::agent_version());
        m
    }

    /// Queue depths (`outbox_depth`, `outbox_bytes`, `outbox_quarantined`,
    /// `blob_depth`, `blob_bytes`); a read error is a `*_error` metric, never a zero.
    pub fn metrics(&self) -> Vec<(String, Value)> {
        let Some(q) = &self.queues else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut put = |key: &str, r: Result<i64, String>| match r {
            Ok(v) => out.push((key.to_owned(), Value::from(v))),
            Err(e) => out.push((format!("{key}_error"), Value::from(e))),
        };
        put("outbox_depth", q.outbox.depth().map_err(|e| e.to_string()));
        put("outbox_bytes", q.outbox.bytes().map_err(|e| e.to_string()));
        put(
            "outbox_quarantined",
            q.outbox.quarantined_count().map_err(|e| e.to_string()),
        );
        put("blob_depth", q.blobs.depth().map_err(|e| e.to_string()));
        put("blob_bytes", q.blobs.bytes().map_err(|e| e.to_string()));
        out
    }

    fn start_dry_run(&self, ctx: &ModuleContext, cmd: &Command) {
        let Some(identity) = self.identity.clone() else {
            ctx.complete(&cmd.id, false, NOT_ENROLLED);
            return;
        };
        let mut slot = self.dry.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((h, _)) = slot.as_ref()
            && !h.is_finished()
        {
            ctx.complete(&cmd.id, false, "a capture_dry_run is already running");
            return;
        }
        if let Some((h, _)) = slot.take() {
            joined(ctx, h);
        }
        let runtime = self.queues.as_ref().map_or_else(
            || Arc::new(RuntimeConfig::default()),
            |q| q.runtime_config.current(),
        );
        let job = DryRun {
            identity,
            secrets: self.secrets.clone(),
            zone: self.options.site_time_zone.clone(),
            runtime,
        };
        let run_ctx = Ctx::background().child();
        let (mctx, id, c) = (ctx.clone(), cmd.id.clone(), run_ctx.clone());
        let spawned = std::thread::Builder::new()
            .name("pda-sync dry run".into())
            .spawn(move || {
                let (ok, body) = match job.run(&c) {
                    Ok(m) => (true, dl_json::to_string(&Value::Object(m))),
                    Err(e) => (false, e),
                };
                mctx.complete(&id, ok, &body);
            });
        match spawned {
            Ok(h) => *slot = Some((h, run_ctx)),
            Err(e) => ctx.complete(&cmd.id, false, &format!("spawn the dry run: {e}")),
        }
    }
}

/// Joins a finished dry pass; a panic is reported (its command stays unanswered and
/// expires on the hub).
fn joined(ctx: &ModuleContext, h: JoinHandle<()>) {
    if h.join().is_err() {
        ctx.report(
            Severity::Error,
            "PDA_DRY_RUN_PANIC",
            "the capture_dry_run thread panicked",
            Map::new(),
        );
    }
}

/// The inputs of one dry pass, moved to its thread.
struct DryRun {
    identity: AgentIdentity,
    secrets: SharedSecretStore,
    zone: Option<String>,
    runtime: Arc<RuntimeConfig>,
}

impl DryRun {
    fn run(&self, ctx: &Ctx) -> Result<Map, String> {
        let cfg = crate::config::sql::agent_connection_for(&self.identity, &*self.secrets)
            .map_err(|e| format!("[{}] {}", e.code(), e.message()))?;
        let source: Arc<dyn CaptureSource> = Arc::new(SqlServerCaptureSource::new(
            SqlServerSourceOptions::new(cfg),
        ));
        let zone = site_time_zone(self.zone.as_deref()).map_err(|e| e.to_string())?;
        let started = SystemClock::shared().now();
        let mut out = dry_run::run(
            source,
            zone,
            &self.identity.agent_id_text(),
            &self.runtime,
            ctx,
        )
        .map_err(|e| e.to_string())?;
        out.insert("started_at", started.to_stj());
        Ok(out)
    }
}

/// `pda-sync.status` and `pda_sync.status` are `status`.
pub fn strip_prefix(kind: &str) -> &str {
    kind.strip_prefix("pda-sync.")
        .or_else(|| kind.strip_prefix("pda_sync."))
        .unwrap_or(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_may_carry_the_module_prefix() {
        assert_eq!(strip_prefix("pda-sync.status"), STATUS);
        assert_eq!(strip_prefix("pda_sync.capture_dry_run"), CAPTURE_DRY_RUN);
        assert_eq!(strip_prefix("selftest"), SELFTEST);
    }
}
