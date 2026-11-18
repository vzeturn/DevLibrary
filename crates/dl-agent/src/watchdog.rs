//! External watchdog (PROTOCOL.md §6.3). The running agent writes `heartbeat.json` once a
//! minute; the `watchdog` sub-command, started every few minutes by a scheduled task (Windows)
//! or a systemd timer, looks at the service and that file and decides one action: start a
//! stopped service, kill one stuck starting or stopping, or restart one whose loop hung or
//! whose hub link went stale while the hub is reachable. Restarts back off from 1 h to 12 h.
//! It stays out of updates on probation (the agent rolls those back itself) and out of
//! disabled or missing services.

use std::path::Path;

use dl_json::{Value, json};

use crate::Error;
use crate::report::Spool;
use crate::store;

/// File the agent writes its liveness to.
pub const BEAT_FILE: &str = "heartbeat.json";
/// How often the agent rewrites it.
pub const BEAT_EVERY_SECONDS: u64 = 60;
/// A loop or hub beat older than this is stale.
pub const STALE_SECONDS: u64 = 20 * 60;
/// A service starting or stopping for this long is stuck.
pub const STUCK_SECONDS: u64 = 10 * 60;
/// First wait after a watchdog restart before the next one.
pub const MIN_BACKOFF_SECONDS: u64 = 3600;
/// Longest wait between watchdog restarts.
pub const MAX_BACKOFF_SECONDS: u64 = 12 * 3600;
/// The watchdog's own state.
pub const STATE_FILE: &str = "watchdog.json";
/// Reports the watchdog leaves for the agent to send (it has no hub session of its own).
pub const REPORTS_FILE: &str = "watchdog-reports.json";

/// What the agent last wrote to [`BEAT_FILE`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Beat {
    /// Time of the last loop iteration.
    pub at: u64,
    /// Time of the last successful heartbeat to the hub (0: none since start).
    pub hub_at: u64,
    /// Version of the running binary.
    pub version: String,
    /// Process id, so a stuck process can be killed.
    pub pid: u32,
}

impl Beat {
    fn to_json(&self) -> Value {
        json!({"at": self.at, "hub_at": self.hub_at, "version": self.version, "pid": u64::from(self.pid)})
    }

    fn from_json(v: &Value) -> Option<Beat> {
        Some(Beat {
            at: v.get("at")?.as_u64()?,
            hub_at: v.get("hub_at").and_then(Value::as_u64).unwrap_or(0),
            version: v
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            pid: u32::try_from(v.get("pid").and_then(Value::as_u64).unwrap_or(0)).unwrap_or(0),
        })
    }
}

/// Writes the beat (atomically).
pub fn write_beat(dir: &Path, beat: &Beat) -> Result<(), Error> {
    store::write_json(&dir.join(BEAT_FILE), &beat.to_json())
}

/// The last beat, `None` when missing or unreadable.
pub fn read_beat(dir: &Path) -> Option<Beat> {
    store::read_json(&dir.join(BEAT_FILE))
        .ok()
        .flatten()
        .as_ref()
        .and_then(Beat::from_json)
}

/// The service as the service manager sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    /// Not installed.
    Missing,
    /// Installed but disabled: someone turned it off on purpose.
    Disabled,
    /// Not running.
    Stopped,
    /// Starting or stopping.
    Pending,
    /// Running.
    Running,
}

/// What the watchdog does this run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing, with the reason.
    Nothing(String),
    /// Start the stopped service.
    Start,
    /// Kill the stuck process, then start the service.
    Kill(u32),
    /// Restart the running service, with the reason.
    Restart(String),
}

/// The watchdog's memory between runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// First run that saw the service starting or stopping.
    pub pending_since: Option<u64>,
    /// Wait after the next restart.
    pub backoff: u64,
    /// No restart before this time.
    pub not_before: u64,
}

impl State {
    /// Loads [`STATE_FILE`] (missing or unreadable → fresh).
    pub fn load(dir: &Path) -> State {
        let v = store::read_json(&dir.join(STATE_FILE))
            .ok()
            .flatten()
            .unwrap_or(Value::Null);
        let n = |k: &str| v.get(k).and_then(Value::as_u64);
        State {
            pending_since: n("pending_since"),
            backoff: n("backoff").unwrap_or(0),
            not_before: n("not_before").unwrap_or(0),
        }
    }

    /// Saves [`STATE_FILE`].
    pub fn save(&self, dir: &Path) -> Result<(), Error> {
        let pending = self.pending_since.map_or(Value::Null, Value::from);
        store::write_json(
            &dir.join(STATE_FILE),
            &json!({"pending_since": pending, "backoff": self.backoff, "not_before": self.not_before}),
        )
    }
}

/// Decides this run's action and updates `state`. `hub_reachable` is asked only when the
/// answer depends on it.
pub fn decide(
    state: &mut State,
    now: u64,
    service: Service,
    beat: Option<&Beat>,
    update_pending: bool,
    hub_reachable: impl FnOnce() -> bool,
) -> Action {
    if service != Service::Pending {
        state.pending_since = None;
    }
    match service {
        Service::Missing => Action::Nothing("service is not installed".into()),
        Service::Disabled => Action::Nothing("service is disabled".into()),
        Service::Stopped => Action::Start,
        Service::Pending => {
            let since = *state.pending_since.get_or_insert(now);
            if now.saturating_sub(since) < STUCK_SECONDS {
                return Action::Nothing("service is starting or stopping".into());
            }
            match beat.map(|b| b.pid).filter(|p| *p != 0) {
                Some(pid) => {
                    state.pending_since = None;
                    Action::Kill(pid)
                }
                None => Action::Nothing("service is stuck but its process id is unknown".into()),
            }
        }
        Service::Running => {
            if update_pending {
                return Action::Nothing("an update is on probation".into());
            }
            let Some(b) = beat else {
                return Action::Nothing("no heartbeat file yet".into());
            };
            let reason = if now.saturating_sub(b.at) >= STALE_SECONDS {
                format!("agent loop silent for {} min", (now - b.at) / 60)
            } else if now.saturating_sub(b.hub_at) >= STALE_SECONDS && hub_reachable() {
                format!(
                    "no hub heartbeat for {} min while the hub answers",
                    now.saturating_sub(b.hub_at) / 60
                )
            } else {
                state.backoff = MIN_BACKOFF_SECONDS;
                return Action::Nothing("healthy".into());
            };
            if now < state.not_before {
                return Action::Nothing(format!(
                    "{reason}; next restart allowed in {} min",
                    (state.not_before - now) / 60
                ));
            }
            let wait = state
                .backoff
                .clamp(MIN_BACKOFF_SECONDS, MAX_BACKOFF_SECONDS);
            state.not_before = now + wait;
            state.backoff = (wait * 2).min(MAX_BACKOFF_SECONDS);
            Action::Restart(reason)
        }
    }
}

/// Moves the watchdog's reports into the agent's spool; the file is removed once they are in.
pub fn drain_reports(dir: &Path, into: &Spool) -> Result<usize, Error> {
    let path = dir.join(REPORTS_FILE);
    if !path.exists() {
        return Ok(0);
    }
    let from = Spool::open(path.clone());
    let items = from.peek(usize::MAX);
    for r in &items {
        into.record(r.clone())?;
    }
    crate::error::io("remove watchdog reports", std::fs::remove_file(&path))?;
    Ok(items.len())
}

#[cfg(test)]
#[path = "watchdog_tests.rs"]
mod tests;
