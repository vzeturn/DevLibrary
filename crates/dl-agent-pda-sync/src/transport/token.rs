//! Exchanges the long-lived agent token for a 15 min JWT and refreshes it ahead of expiry
//! (port of `Transport/TokenProvider.cs`).

use super::Error;
use super::element;
use super::http::SyncHttpClient;
use crate::config::time::{SharedClock, Timestamp};
use crate::config::{AgentError, AgentPaths, AgentStatus, codes};
use crate::reporting::log::Logger;
use crate::reporting::scrubber;
use crate::security::{SharedSecretStore, names};
use crate::util::ctx::Ctx;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// What the operator does when the agent must be enrolled again.
pub const REENROLL_HINT: &str = "Create a new enrollment code in the web console and run: dl-agent pda-enroll --code <code> --url <url> (the stored key is reused).";
/// The route of the token exchange.
pub const TOKEN_PATH: &str = "/sync/v1/token";
/// The logger category (`ILogger<TokenProvider>`).
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Transport.TokenProvider";
const SKEW: Duration = Duration::from_secs(60);

#[derive(Default)]
struct State {
    access: Option<String>,
    expires: Timestamp,
    refresh_failure: Option<String>,
}

/// Caches the access token (`TokenProvider`).
pub struct TokenProvider {
    http: Arc<SyncHttpClient>,
    secrets: SharedSecretStore,
    paths: AgentPaths,
    clock: SharedClock,
    log: Logger,
    state: Mutex<State>,
    /// Serialises exchanges (`SemaphoreSlim(1, 1)`).
    gate: Mutex<()>,
}

impl std::fmt::Debug for TokenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenProvider")
            .field("refresh_failure", &self.refresh_failure())
            .finish_non_exhaustive()
    }
}

impl TokenProvider {
    /// A provider over `http`, reading the agent token from `secrets`.
    pub fn new(
        http: Arc<SyncHttpClient>,
        secrets: SharedSecretStore,
        paths: AgentPaths,
        clock: SharedClock,
        log: &Logger,
    ) -> TokenProvider {
        TokenProvider {
            http,
            secrets,
            paths,
            clock,
            log: log.for_category(CATEGORY),
            state: Mutex::new(State::default()),
            gate: Mutex::new(()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Forgets the cached access token (after a 401).
    pub fn invalidate(&self) {
        self.state().access = None;
    }

    /// Code of the last failed exchange (`REENROLL_REQUIRED`, `CLOUD_UNREACHABLE`); `None`
    /// once one succeeds.
    pub fn refresh_failure(&self) -> Option<String> {
        self.state().refresh_failure.clone()
    }

    fn cached(&self) -> Option<String> {
        let st = self.state();
        match &st.access {
            Some(a) if self.clock.now().unix_nanos() < st.expires.sub(SKEW).unix_nanos() => {
                Some(a.clone())
            }
            _ => None,
        }
    }

    /// The access token, exchanged when missing or within 60 s of expiry.
    pub fn get_access_token(&self, ctx: &Ctx) -> Result<String, Error> {
        if let Some(a) = self.cached() {
            return Ok(a);
        }
        if ctx.is_done() {
            return Err(Error::Cancelled);
        }
        let _gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(a) = self.cached() {
            return Ok(a);
        }
        match self.exchange(ctx) {
            Ok(t) => {
                self.state().refresh_failure = None;
                Ok(t)
            }
            Err(e) => {
                if let Some(code) = e.code() {
                    self.state().refresh_failure = Some(code.to_owned());
                }
                Err(e)
            }
        }
    }

    fn exchange(&self, ctx: &Ctx) -> Result<String, Error> {
        let raw = match self.secrets.load(names::AGENT_TOKEN) {
            Ok(Some(r)) => r,
            Ok(None) => return Err(self.reenroll("No stored agent token.")?),
            Err(e) => {
                return Err(match e.agent_error() {
                    Some(a) => Error::Agent(a.clone()),
                    None => Error::Unexpected {
                        type_name: "System.IO.IOException",
                        message: e.to_string(),
                    },
                });
            }
        };
        let token = String::from_utf8_lossy(&raw).into_owned();
        scrubber::register(Some(&token));
        let body = format!("{{\"agentToken\":{}}}", crate::util::stj::quote(&token));
        let resp = self.http.post_json(ctx, TOKEN_PATH, &body, None)?;
        if matches!(resp.status, 401 | 403 | 404) {
            return Err(self.reenroll(&format!(
                "Cloud rejected the agent token (HTTP {}); the agent was revoked or the enrollment expired.",
                resp.status
            ))?);
        }
        if !resp.is_success() {
            return Err(AgentError::new(
                codes::CLOUD_UNREACHABLE,
                format!("Token exchange failed with HTTP {}.", resp.status),
            )
            .into());
        }
        let data = resp.data()?.ok_or_else(|| {
            AgentError::new(codes::CLOUD_UNREACHABLE, "Token response had no data.")
        })?;
        let access = element::string(element::get(&data, "accessToken")?)?.ok_or_else(|| {
            AgentError::new(
                codes::CLOUD_UNREACHABLE,
                "Token response had no accessToken.",
            )
        })?;
        self.state().access = Some(access.clone());
        scrubber::register(Some(&access));
        let seconds = match element::try_get(&data, "expiresIn")? {
            Some(e) => element::int32(e)?,
            None => 900,
        };
        let now = self.clock.now();
        self.state().expires = if seconds >= 0 {
            now.add(Duration::from_secs(u64::from(seconds.unsigned_abs())))
        } else {
            now.sub(Duration::from_secs(u64::from(seconds.unsigned_abs())))
        };
        self.log.debug(
            "Access token refreshed, valid for {Seconds}s",
            &[seconds.to_string().into()],
        );
        Ok(access)
    }

    /// Records `reenroll_required` in `status.json`, logs the reason and returns the
    /// failure to raise.
    fn reenroll(&self, reason: &str) -> Result<Error, Error> {
        AgentStatus::write(
            &self.paths,
            &AgentStatus::new(
                "reenroll_required",
                Some(codes::REENROLL_REQUIRED),
                Some(REENROLL_HINT),
                Timestamp::now_utc(),
            ),
        )
        .map_err(|e| Error::Unexpected {
            type_name: "System.IO.IOException",
            message: e.to_string(),
        })?;
        self.log
            .error("{Reason} {Hint}", &[reason.into(), REENROLL_HINT.into()]);
        Ok(Error::Agent(AgentError::new(
            codes::REENROLL_REQUIRED,
            format!("{reason} {REENROLL_HINT}"),
        )))
    }
}
