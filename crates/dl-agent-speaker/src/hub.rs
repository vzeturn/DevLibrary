//! The order hub connection (port of the Go `internal/signalrclient` `Run`/`runOnce` and
//! its `NextBackoff`), on the workspace `dl-signalr` client.
//!
//! Each connection negotiates (`POST /hubs/order/negotiate?negotiateVersion=1`), opens
//! the WebSocket with the bearer token as the `Authorization` header (never in the URL),
//! then calls `JoinRvc(rvc)` for every configured RVC. Unlike the Go agent (fire and
//! forget), each join waits for its completion, so a refusal (`FORBIDDEN_RVC`) is seen
//! and reported per RVC. When no RVC could be joined the connection is closed and tried
//! again after [`HubSettings::refused_retry`]. A dropped connection reconnects on the
//! web client's schedule ([`next_backoff`]), with a fresh token each time.
//!
//! No print relay: the speaker module ignores `PrintJob` (ADR 0026, one relay per
//! branch, and `pda-sync` relays), and `ForceUninstall` is logged only (dl-agent owns
//! uninstalling; the cloud sends it only to legacy speaker-device connections).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dl_signalr::{CloseReason, ConnectOptions, HubConnection};
use dl_winsvc::StopSignal;

use crate::decision::{GUEST_ORDER, GUEST_SERVICE, parse_event};
use crate::output::Announcer;

/// A connection that stayed up this long resets the outage clock (Go `healthyAfter`).
pub const HEALTHY_AFTER: Duration = Duration::from_secs(30);
/// How often a live connection checks for the module stopping.
const STOP_POLL: Duration = Duration::from_millis(200);

/// Where the bearer token for the hub and the speaker route comes from.
pub trait TokenSource: Send + Sync {
    /// A current access token.
    ///
    /// # Errors
    /// A text to show (never the token).
    fn token(&self) -> Result<String, String>;

    /// Forgets a cached token the cloud refused (401).
    fn invalidate(&self);
}

/// `NextBackoff`: the web client's reconnect schedule by how long the outage has lasted.
pub fn next_backoff(elapsed: Duration) -> Duration {
    let s = elapsed.as_secs_f64();
    Duration::from_secs(if s < 1.0 {
        0
    } else if s < 10.0 {
        2
    } else if s < 30.0 {
        5
    } else if s < 60.0 {
        10
    } else {
        30
    })
}

/// Connection settings.
#[derive(Debug, Clone)]
pub struct HubSettings {
    /// The order hub URL, e.g. `https://pda.example.com/hubs/order`.
    pub hub_url: String,
    /// The RVCs to join.
    pub rvcs: Vec<i64>,
    /// Limit for each connect step and each `JoinRvc`.
    pub call_timeout: Duration,
    /// Wait before trying again when the cloud refused every RVC.
    pub refused_retry: Duration,
}

impl HubSettings {
    /// Settings with a 30 s call limit and a 5 min retry after a full refusal.
    pub fn new(hub_url: impl Into<String>, rvcs: Vec<i64>) -> HubSettings {
        HubSettings {
            hub_url: hub_url.into(),
            rvcs,
            call_timeout: Duration::from_secs(30),
            refused_retry: Duration::from_secs(300),
        }
    }
}

/// What the link looks like now (module state and the `status` command).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkState {
    /// Whether a connection is up with at least one RVC joined.
    pub connected: bool,
    /// RVCs joined on the current connection.
    pub joined: Vec<i64>,
    /// RVCs the cloud refused, with its message.
    pub refused: Vec<(i64, String)>,
    /// Why the last attempt or connection ended.
    pub last_error: Option<String>,
    /// Connections that joined at least one RVC.
    pub connects: u64,
}

/// How one attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    /// No connection, or it ended.
    Ended(String),
    /// Connected, but every RVC was refused.
    AllRefused,
    /// The module is stopping.
    Stopping,
}

/// The hub link: connects, joins, dispatches guest events to the [`Announcer`].
pub struct HubLink {
    settings: HubSettings,
    tokens: Arc<dyn TokenSource>,
    announcer: Arc<Announcer>,
    state: Mutex<LinkState>,
}

impl std::fmt::Debug for HubLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubLink")
            .field("settings", &self.settings)
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl HubLink {
    /// A link that announces through `announcer`.
    pub fn new(
        settings: HubSettings,
        tokens: Arc<dyn TokenSource>,
        announcer: Arc<Announcer>,
    ) -> HubLink {
        HubLink {
            settings,
            tokens,
            announcer,
            state: Mutex::new(LinkState::default()),
        }
    }

    /// A copy of the current state.
    pub fn state(&self) -> LinkState {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn update(&self, f: impl FnOnce(&mut LinkState)) {
        f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner));
    }

    /// Connects and reconnects until `stop` (Go `Client.Run`).
    pub fn run(&self, stop: &StopSignal) {
        let mut outage_start: Option<Instant> = None;
        while !stop.is_stopped() {
            let attempt = Instant::now();
            let outcome = self.run_once(stop);
            if outcome == Outcome::Stopping || stop.is_stopped() {
                break;
            }
            if attempt.elapsed() >= HEALTHY_AFTER {
                outage_start = None;
            }
            let outage = *outage_start.get_or_insert(attempt);
            let wait = match &outcome {
                Outcome::AllRefused => self.settings.refused_retry,
                _ => next_backoff(outage.elapsed()),
            };
            let reason = match &outcome {
                Outcome::Ended(r) => r.clone(),
                _ => "every RVC was refused".to_owned(),
            };
            dl_log::warn!("speaker: hub connection ended, reconnecting"; reason = %reason, wait_s = wait.as_secs());
            self.update(|s| {
                s.connected = false;
                s.joined.clear();
                s.last_error = Some(reason);
            });
            if stop.wait_timeout(wait) {
                break;
            }
        }
        self.update(|s| {
            s.connected = false;
            s.joined.clear();
        });
    }

    fn options(&self, token: &str) -> ConnectOptions {
        let mut o = ConnectOptions::new(self.settings.hub_url.clone());
        o.negotiate = true;
        o.timeout = self.settings.call_timeout;
        o.headers
            .push(("Authorization".to_owned(), format!("Bearer {token}")));
        for target in [GUEST_ORDER, GUEST_SERVICE] {
            let announcer = self.announcer.clone();
            o.on(target, move |args| {
                if let Some(ev) = parse_event(target, args) {
                    announcer.announce(&ev);
                }
            });
        }
        o.on("ForceUninstall", |_| {
            dl_log::warn!("speaker: the cloud sent ForceUninstall; ignored (dl-agent owns uninstalling, disable modules.speaker instead)");
        });
        o
    }

    /// One connection until it ends (Go `runOnce`).
    fn run_once(&self, stop: &StopSignal) -> Outcome {
        let token = match self.tokens.token() {
            Ok(t) => t,
            Err(e) => return Outcome::Ended(format!("no access token: {e}")),
        };
        let hub = match HubConnection::connect(&self.options(&token)) {
            Ok(h) => h,
            Err(e) => {
                if is_unauthorized(&e) {
                    self.tokens.invalidate();
                }
                return Outcome::Ended(format!("connect: {e}"));
            }
        };
        let mut joined = Vec::new();
        let mut refused = Vec::new();
        for &rvc in &self.settings.rvcs {
            match hub.invoke(
                "JoinRvc",
                vec![dl_json::Value::from(rvc)],
                Some(self.settings.call_timeout),
            ) {
                Ok(_) => joined.push(rvc),
                Err(dl_signalr::Error::Hub { message, .. }) => {
                    dl_log::warn!("speaker: the cloud refused JoinRvc"; rvc = rvc, error = %message);
                    refused.push((rvc, message));
                }
                Err(e) => {
                    hub.close();
                    return Outcome::Ended(format!("JoinRvc({rvc}): {e}"));
                }
            }
        }
        let none_joined = joined.is_empty();
        self.update(|s| {
            s.refused = refused;
            if !none_joined {
                s.connected = true;
                s.connects += 1;
                s.last_error = None;
            }
            s.joined = joined.clone();
        });
        if none_joined {
            hub.close();
            return Outcome::AllRefused;
        }
        dl_log::info!("speaker: joined the order hub"; rvcs = ?joined);
        let outcome = loop {
            if stop.is_stopped() {
                break Outcome::Stopping;
            }
            if let Some(r) = hub.wait_closed_timeout(STOP_POLL) {
                break Outcome::Ended(close_text(&r));
            }
        };
        hub.close();
        outcome
    }
}

/// A refused token: on negotiate or on the upgrade.
fn is_unauthorized(e: &dl_signalr::Error) -> bool {
    match e {
        dl_signalr::Error::NegotiateRejected { status, .. } => *status == 401,
        dl_signalr::Error::WebSocket(w) => {
            matches!(&**w, dl_ws::Error::Rejected { status: 401, .. })
        }
        _ => false,
    }
}

fn close_text(reason: &CloseReason) -> String {
    match reason {
        CloseReason::Server {
            error: Some(error), ..
        } => format!("the cloud closed the connection: {error}"),
        CloseReason::Server { error: None, .. } => "closed by the cloud".to_owned(),
        CloseReason::Local => "closed".to_owned(),
        CloseReason::Transport(e) => e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_follows_the_web_client() {
        for (secs, want) in [
            (0, 0),
            (1, 2),
            (9, 2),
            (10, 5),
            (29, 5),
            (30, 10),
            (59, 10),
            (60, 30),
            (600, 30),
        ] {
            assert_eq!(
                next_backoff(Duration::from_secs(secs)),
                Duration::from_secs(want),
                "{secs}"
            );
        }
        assert_eq!(next_backoff(Duration::from_millis(999)), Duration::ZERO);
    }
}
