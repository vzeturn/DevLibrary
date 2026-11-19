//! The running service's own verdict in `status.json` (port of `Config/AgentHealth.cs`),
//! written after every heartbeat attempt, so the install script proves that the service
//! (LocalSystem) works and not only the admin who ran it (`PdaSyncAgent status --wait`).
//! `reenroll_required` stays until a new enroll replaces it.

use super::errors::codes;
use super::paths::AgentPaths;
use super::status::{AgentStatus, ReadError};
use super::time::{SharedClock, Timestamp};
use crate::reporting::log::{Exception, Logger};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// `healthy`.
pub const HEALTHY: &str = "healthy";
/// `degraded`.
pub const DEGRADED: &str = "degraded";
/// `reenroll_required`.
pub const REENROLL_REQUIRED: &str = "reenroll_required";

/// The logger category (`ILogger<AgentHealth>`).
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Config.AgentHealth";

/// Writes the service's health verdict to `status.json`.
#[derive(Clone)]
pub struct AgentHealth {
    paths: AgentPaths,
    clock: SharedClock,
    log: Logger,
    /// Unix seconds of the last heartbeat the cloud accepted (0: none in this run);
    /// shared by clones (B5: the dl-agent watchdog's cloud link).
    heartbeat_ok: Arc<AtomicU64>,
}

impl std::fmt::Debug for AgentHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentHealth")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

/// The .NET exception a failed status read or write is logged as.
pub(crate) fn io_exception(e: &io::Error) -> Exception {
    let ty = if e.kind() == io::ErrorKind::PermissionDenied {
        "System.UnauthorizedAccessException"
    } else {
        "System.IO.IOException"
    };
    Exception::new(ty, e.to_string())
}

pub(crate) fn read_exception(e: &ReadError) -> Exception {
    match e {
        ReadError::Io(io) => io_exception(io),
        ReadError::Json(j) => Exception::new("System.Text.Json.JsonException", j.to_string()),
    }
}

fn heartbeat_failure_hint(code: &str) -> String {
    match code {
        codes::CLOUD_UNREACHABLE => "The heartbeat did not reach the cloud: check this machine's internet access and proxy (the service runs as LocalSystem).".to_owned(),
        codes::REENROLL_REQUIRED => crate::transport::token::REENROLL_HINT.to_owned(),
        _ => format!("The heartbeat failed with {code}; see the agent log (logs/agent-*.log)."),
    }
}

fn capture_hint(code: &str) -> String {
    match code {
        codes::SQL_LOGIN_FAILED => "pda_sync cannot sign in to SQL Server; see the agent log, then run the install script again.".to_owned(),
        codes::SQL_UNREACHABLE => "SQL Server cannot be reached from the service: check the instance name and that SQL Server is running.".to_owned(),
        codes::SQL_SCHEMA_MISMATCH => "SMILE_POS does not match the expected schema; run PdaSyncAgent diag and send the zip to support.".to_owned(),
        codes::OUTBOX_FULL | codes::BLOB_QUEUE_FULL => "The local queue is full because uploads are not getting through; check the cloud connection.".to_owned(),
        _ => format!("Capture reports {code}; see the agent log (logs/agent-*.log)."),
    }
}

impl AgentHealth {
    /// A health writer for `paths`; `log` is the agent's root logger.
    pub fn new(paths: AgentPaths, clock: SharedClock, log: &Logger) -> AgentHealth {
        AgentHealth {
            paths,
            clock,
            log: log.for_category(CATEGORY),
            heartbeat_ok: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Unix seconds (system clock) of the last heartbeat the cloud accepted with a 2xx in
    /// this run; 0 before the first one. It never moves backwards.
    pub fn last_heartbeat_ok(&self) -> u64 {
        self.heartbeat_ok.load(Ordering::SeqCst)
    }

    /// `healthy` only when the cloud took the heartbeat (2xx) and capture reaches
    /// SMILE_POS without an error; else `degraded` with the first problem: the heartbeat
    /// (`heartbeat_failure`, or `HEARTBEAT_HTTP_<n>`), then the capture error, then a
    /// database capture has not reached yet.
    pub fn evaluate(
        heartbeat_http: Option<i32>,
        heartbeat_failure: Option<&str>,
        sql_reachable: bool,
        capture_error: Option<&str>,
        at: Timestamp,
    ) -> AgentStatus {
        if let Some(code) = heartbeat_failure {
            return AgentStatus::new(
                DEGRADED,
                Some(code),
                Some(&heartbeat_failure_hint(code)),
                at,
            );
        }
        match heartbeat_http {
            Some(http) if (200..=299).contains(&http) => {}
            other => {
                let status = other.map_or_else(|| "none".to_owned(), |h| h.to_string());
                return AgentStatus::new(
                    DEGRADED,
                    Some(&format!("HEARTBEAT_HTTP_{status}")),
                    Some(&format!(
                        "The cloud answered the heartbeat with HTTP {status}; see the agent log (logs/agent-*.log)."
                    )),
                    at,
                );
            }
        }
        if let Some(code) = capture_error {
            return AgentStatus::new(DEGRADED, Some(code), Some(&capture_hint(code)), at);
        }
        if !sql_reachable {
            return AgentStatus::new(
                DEGRADED,
                Some(codes::SQL_UNREACHABLE),
                Some(
                    "Capture has not reached the POS database yet; it retries on its own. If this stays, run PdaSyncAgent diag.",
                ),
                at,
            );
        }
        AgentStatus::new(HEALTHY, None, None, at)
    }

    /// Writes the verdict of one heartbeat attempt at the clock's time; failures are
    /// logged, never returned (the heartbeat goes on).
    pub fn record(
        &self,
        heartbeat_http: Option<i32>,
        heartbeat_failure: Option<&str>,
        sql_reachable: bool,
        capture_error: Option<&str>,
    ) {
        if heartbeat_failure.is_none() && heartbeat_http.is_some_and(|h| (200..=299).contains(&h)) {
            let at = u64::try_from(self.clock.now().unix_seconds()).unwrap_or(0);
            self.heartbeat_ok.fetch_max(at, Ordering::SeqCst);
        }
        self.record_status(AgentHealth::evaluate(
            heartbeat_http,
            heartbeat_failure,
            sql_reachable,
            capture_error,
            self.clock.now(),
        ));
    }

    /// Writes `next` unless the stored state is `reenroll_required`.
    pub fn record_status(&self, next: AgentStatus) {
        let status_file = self.paths.status_file().to_string_lossy().into_owned();
        let current = match AgentStatus::try_read(&self.paths) {
            Ok(c) => c,
            Err(e) => {
                // An unreadable file is replaced below: the new verdict is what the installer must see.
                self.log.warn_ex(
                    read_exception(&e),
                    "Agent status {Path} could not be read; it is written again",
                    &[status_file.as_str().into()],
                );
                None
            }
        };
        if current.is_some_and(|c| c.state == REENROLL_REQUIRED) {
            self.log.debug(
                "Agent status stays {State} until the agent is enrolled again",
                &[REENROLL_REQUIRED.into()],
            );
            return;
        }
        if let Err(e) = AgentStatus::write(&self.paths, &next) {
            self.log.warn_ex(
                io_exception(&e),
                "Agent status {State} {Code} could not be written to {Path}",
                &[
                    next.state.as_str().into(),
                    next.error_code.clone().into(),
                    status_file.into(),
                ],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::time::{Clock, ManualClock};
    use crate::reporting::log::{Level, LoggerConfig, MemorySink};
    use std::sync::Arc;

    fn at() -> Timestamp {
        Timestamp::parse_iso("2026-11-14T08:00:00Z").unwrap()
    }

    struct Env {
        dir: std::path::PathBuf,
        paths: AgentPaths,
        clock: Arc<ManualClock>,
        sink: Arc<MemorySink>,
        log: Logger,
    }

    impl Env {
        fn new() -> Env {
            let dir = std::env::temp_dir()
                .join("pda-sync-agent-tests")
                .join(dl_uuid::Uuid::new_v4().simple().to_string());
            let paths = AgentPaths::new(&dir);
            paths.ensure_created().unwrap();
            let sink = Arc::new(MemorySink::default());
            let log = Logger::new(LoggerConfig {
                minimum: Level::Verbose,
                sinks: vec![(sink.clone(), Level::Verbose)],
                ..LoggerConfig::default()
            });
            Env {
                dir,
                paths,
                clock: Arc::new(ManualClock::new(
                    Timestamp::parse_iso("2026-10-24T08:00:00Z").unwrap(),
                )),
                sink,
                log,
            }
        }
        fn health(&self) -> AgentHealth {
            AgentHealth::new(self.paths.clone(), self.clock.clone(), &self.log)
        }
        fn warnings(&self) -> Vec<String> {
            self.sink.messages_at(Level::Warning)
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn healthy_needs_a_2xx_heartbeat_and_a_reachable_database_without_capture_error() {
        for http in [200, 204] {
            assert_eq!(
                AgentHealth::evaluate(Some(http), None, true, None, at()),
                AgentStatus::new(HEALTHY, None, None, at())
            );
        }
    }

    #[test]
    fn a_refused_heartbeat_is_degraded_with_its_http_status() {
        for (http, code) in [(401, "HEARTBEAT_HTTP_401"), (503, "HEARTBEAT_HTTP_503")] {
            let st = AgentHealth::evaluate(Some(http), None, true, Some("SQL_LOGIN_FAILED"), at());
            assert_eq!(st.state, DEGRADED);
            assert_eq!(st.error_code.as_deref(), Some(code));
            assert!(st.hint.unwrap().contains(&format!("HTTP {http}")));
        }
        let st = AgentHealth::evaluate(None, None, true, None, at());
        assert_eq!(st.error_code.as_deref(), Some("HEARTBEAT_HTTP_none"));
    }

    #[test]
    fn a_heartbeat_without_an_answer_is_degraded_with_its_code_and_a_proxy_hint() {
        let st = AgentHealth::evaluate(None, Some(codes::CLOUD_UNREACHABLE), true, None, at());
        assert_eq!(st.state, DEGRADED);
        assert_eq!(st.error_code.as_deref(), Some(codes::CLOUD_UNREACHABLE));
        assert!(st.hint.unwrap().contains("LocalSystem"));
    }

    #[test]
    fn a_capture_error_is_degraded_with_that_code() {
        let st = AgentHealth::evaluate(Some(200), None, true, Some(codes::SQL_LOGIN_FAILED), at());
        assert_eq!(
            (st.state.as_str(), st.error_code.as_deref()),
            (DEGRADED, Some(codes::SQL_LOGIN_FAILED))
        );
        assert!(st.hint.unwrap().contains("pda_sync"));
    }

    #[test]
    fn a_database_capture_has_not_reached_yet_is_degraded() {
        let st = AgentHealth::evaluate(Some(200), None, false, None, at());
        assert_eq!(
            (st.state.as_str(), st.error_code.as_deref()),
            (DEGRADED, Some(codes::SQL_UNREACHABLE))
        );
        assert!(st.hint.unwrap().contains("diag"));
    }

    #[test]
    fn record_replaces_the_enrolled_state_with_the_verdict_at_the_clock_time() {
        let env = Env::new();
        AgentStatus::write(&env.paths, &AgentStatus::new("enrolled", None, None, at())).unwrap();
        env.health().record(Some(200), None, true, None);
        assert_eq!(
            AgentStatus::try_read(&env.paths).unwrap(),
            Some(AgentStatus::new(HEALTHY, None, None, env.clock.now()))
        );
    }

    #[test]
    fn the_last_accepted_heartbeat_is_unix_seconds_and_only_moves_forward() {
        let env = Env::new();
        let health = env.health();
        assert_eq!(health.last_heartbeat_ok(), 0, "none before the first 2xx");
        health.record(None, Some(codes::CLOUD_UNREACHABLE), true, None);
        health.record(Some(500), None, true, None);
        assert_eq!(health.last_heartbeat_ok(), 0, "failures do not count");

        // 2026-10-24T08:00:00Z, in seconds (not milliseconds, not local time).
        health.record(Some(204), None, false, Some("SQL_LOGIN_FAILED"));
        assert_eq!(health.last_heartbeat_ok(), 1_792_828_800);
        assert_eq!(
            env.health().last_heartbeat_ok(),
            0,
            "a new writer starts over"
        );
        let shared = health.clone();
        env.clock.advance(std::time::Duration::from_secs(15));
        shared.record(Some(200), None, true, None);
        assert_eq!(health.last_heartbeat_ok(), 1_792_828_815, "clones share it");
        env.clock
            .set(Timestamp::parse_iso("2026-10-24T07:00:00Z").unwrap());
        health.record(Some(200), None, true, None);
        assert_eq!(health.last_heartbeat_ok(), 1_792_828_815, "never backwards");
    }

    #[test]
    fn record_keeps_reenroll_required_until_a_new_enroll() {
        let env = Env::new();
        let sticky = AgentStatus::new(
            REENROLL_REQUIRED,
            Some(codes::REENROLL_REQUIRED),
            Some("run the install script again"),
            at(),
        );
        AgentStatus::write(&env.paths, &sticky).unwrap();
        env.health().record(Some(200), None, true, None);
        assert_eq!(AgentStatus::try_read(&env.paths).unwrap(), Some(sticky));
    }

    #[test]
    fn record_replaces_an_unreadable_status_file_and_logs_it() {
        let env = Env::new();
        std::fs::write(env.paths.status_file(), "{ not json").unwrap();
        env.health().record(Some(200), None, true, None);
        assert_eq!(
            AgentStatus::try_read(&env.paths).unwrap().unwrap().state,
            HEALTHY
        );
        let w = env.warnings();
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("could not be read"), "{w:?}");
    }

    #[test]
    fn record_logs_a_write_failure_and_does_not_throw() {
        let env = Env::new();
        std::fs::create_dir_all(env.paths.status_file()).unwrap();
        env.health()
            .record(None, Some(codes::CLOUD_UNREACHABLE), true, None);
        let w = env.warnings();
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("could not be written"), "{w:?}");
    }
}
