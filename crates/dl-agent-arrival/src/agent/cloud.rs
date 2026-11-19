//! `CloudLinkWorker.ExchangeAsync`: one sealed POST to the Check-In cloud and its sealed reply.
//!
//! Every body is an `agent.poll` envelope (AgentToCloud) and every reply a
//! `cloud.poll-response` envelope (CloudToAgent) that must fall inside the 2-minute replay
//! window. HTTP 409 means the cloud does not accept this catalog version; HTTP 401 means it
//! refused the agent's key, or its clock when `X-Agent-Refused` says `clock`.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use dl_http::{Client, ClientOptions, Proxy, Request, Scheme, Url};

use crate::agent::clock::Clock;
use crate::agent::health::CloudFailure;
use crate::contracts::messages::{HEADER_REFUSED, REFUSED_CLOCK, TYPE_POLL, TYPE_POLL_RESPONSE};
use crate::contracts::{
    AgentPollRequest, AgentPollResponse, Direction, EnvelopeCipher, ReplayGuard, SyncEnvelope,
};
use crate::error::Error;

/// Long-poll path, relative to `CloudBaseUrl`.
pub const POLL_PATH: &str = "api/agent/poll";
/// Results and change-notice path.
pub const RESULTS_PATH: &str = "api/agent/results";
/// A poll may be held by the cloud; this is the whole-call limit (the .NET client's 60 s).
pub const POLL_TIMEOUT: Duration = Duration::from_secs(60);
/// Limit of one upload attempt.
pub const UPLOAD_TIMEOUT: Duration = Duration::from_secs(15);
/// DNS and TCP connect limit.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// The replay window either side of now (`SyncReplayGuard` of the agent).
pub const REPLAY_WINDOW_MS: i64 = 120_000;
/// Largest reply accepted: a batch of vault puts (6 MiB each, base64) fits.
pub const MAX_REPLY_BYTES: usize = 64 * 1024 * 1024;

/// `CloudBaseUrl` as the base the paths join onto (trailing `/` ensured).
///
/// # Errors
/// [`Error::Config`] unless it is `https://`, or `http://` to a loopback host (a local test
/// cloud).
pub fn check_base_url(raw: &str) -> Result<Url, Error> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(Error::Config("CloudBaseUrl is not set".into()));
    }
    let url = Url::parse(&format!("{trimmed}/"))
        .map_err(|e| Error::Config(format!("CloudBaseUrl: {e}")))?;
    match url.scheme() {
        Scheme::Https => Ok(url),
        Scheme::Http if is_loopback(url.host()) => Ok(url),
        _ => Err(Error::Config(
            "CloudBaseUrl must be an https:// address".into(),
        )),
    }
}

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_matches(|c| c == '[' || c == ']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The text shown (link status, module state) when the cloud refused the agent.
pub fn refused_message(clock: bool, agent_id: &str, kid: &str) -> String {
    if clock {
        "The cloud refused this agent because the date and time of this machine are wrong. Correct the Windows clock."
            .to_string()
    } else {
        format!(
            "The cloud does not accept the key of agent {agent_id} ({kid}): it was deleted or replaced in the platform console. Run the install command shown there again."
        )
    }
}

/// How a failed exchange shows in the module's health.
pub fn classify(e: &Error) -> CloudFailure {
    match e {
        Error::Refused { clock: true } | Error::ClockSkew { .. } => CloudFailure::ClockSkew,
        Error::Refused { clock: false } | Error::Crypto(_) | Error::Json(_) => CloudFailure::Auth,
        _ => CloudFailure::Transport,
    }
}

/// The agent's side of the link: an HTTP client, the sealing key and the replay guard.
pub struct CloudLink {
    http: Client,
    base: Url,
    cipher: EnvelopeCipher,
    replay: ReplayGuard,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for CloudLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudLink")
            .field("base", &self.base.to_string())
            .field("agent_id", &self.cipher.agent_id())
            .finish_non_exhaustive()
    }
}

/// The proxy the cloud is reached through: the dl-agent core's choice (environment, then
/// WinHTTP, then the service account's Internet Options), as the .NET `HttpClient` used the
/// machine's proxy settings rather than only the environment.
pub fn proxy() -> Proxy {
    dl_agent::proxy::current().proxy.clone()
}

/// The HTTP options of the link.
fn client_options(user_agent: &str) -> ClientOptions {
    ClientOptions {
        timeout: Some(POLL_TIMEOUT),
        connect_timeout: Some(CONNECT_TIMEOUT),
        max_body: MAX_REPLY_BYTES,
        proxy: proxy(),
        user_agent: Some(user_agent.to_string()),
        ..ClientOptions::default()
    }
}

impl CloudLink {
    /// A link to `base_url` sealing with `cipher`.
    ///
    /// # Errors
    /// [`Error::Config`] for a base URL [`check_base_url`] refuses.
    pub fn new(
        base_url: &str,
        cipher: EnvelopeCipher,
        clock: Arc<dyn Clock>,
        user_agent: &str,
    ) -> Result<CloudLink, Error> {
        let base = check_base_url(base_url)?;
        let http = Client::with_options(client_options(user_agent));
        Ok(CloudLink {
            http,
            base,
            cipher,
            replay: ReplayGuard::new(REPLAY_WINDOW_MS),
            clock,
        })
    }

    /// The enrolled agent id.
    pub fn agent_id(&self) -> &str {
        self.cipher.agent_id()
    }

    /// The key id of the agent secret.
    pub fn kid(&self) -> &str {
        self.cipher.kid()
    }

    /// The refusal text for this agent.
    pub fn refused_text(&self, clock: bool) -> String {
        refused_message(clock, self.agent_id(), self.kid())
    }

    /// Seals `body`, posts it to `path` and opens the reply.
    ///
    /// # Errors
    /// [`Error::CatalogRejected`] (409), [`Error::Refused`] (401), [`Error::ClockSkew`] for a
    /// reply stamped outside the window, [`Error::Crypto`] / [`Error::Json`] for a reply that
    /// does not open or parse, [`Error::Transport`] for everything else.
    pub fn exchange(
        &self,
        path: &str,
        body: &AgentPollRequest,
        timeout: Duration,
    ) -> Result<AgentPollResponse, Error> {
        let sealed = self.cipher.seal(
            Direction::AgentToCloud,
            TYPE_POLL,
            &body.to_json_string(),
            self.clock.now_ms(),
        )?;
        let url = self
            .base
            .join(path)
            .map_err(|e| Error::Transport(format!("cloud URL: {e}")))?;
        let request = Request::post(url, sealed.to_json_string().into_bytes())
            .with_header("Content-Type", "application/json; charset=utf-8")
            .map_err(|e| Error::Transport(e.to_string()))?
            .with_timeout(timeout);
        let response = self
            .http
            .request(&request)
            .map_err(|e| Error::Transport(e.to_string()))?;
        let status = response.status.as_u16();
        match status {
            409 => return Err(Error::CatalogRejected),
            401 => {
                let clock = response.headers.get(HEADER_REFUSED).is_some_and(|v| {
                    v.split(',')
                        .any(|t| t.trim().eq_ignore_ascii_case(REFUSED_CLOCK))
                });
                return Err(Error::Refused { clock });
            }
            200..=299 => {}
            other => return Err(Error::Transport(format!("the cloud answered HTTP {other}"))),
        }
        if response.body.is_empty() {
            return Err(Error::Transport("Empty reply from cloud.".into()));
        }
        let text = std::str::from_utf8(&response.body)
            .map_err(|_| Error::Json("cloud reply is not UTF-8".into()))?;
        let envelope = SyncEnvelope::parse(text)?;
        let plain = self
            .cipher
            .open(Direction::CloudToAgent, TYPE_POLL_RESPONSE, &envelope)?;
        let now = self.clock.now_ms();
        let drift = envelope.timestamp.saturating_sub(now);
        if drift.saturating_abs() > REPLAY_WINDOW_MS {
            return Err(Error::ClockSkew {
                seconds: drift / 1000,
            });
        }
        if !self.replay.try_accept(&envelope, now) {
            return Err(Error::Crypto("Stale or replayed reply from cloud.".into()));
        }
        AgentPollResponse::from_json(&plain).map_err(Error::Json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_link_uses_the_core_proxy_choice() {
        let o = client_options("dl-agent/test");
        assert_eq!(o.proxy, dl_agent::proxy::current().proxy);
        assert_ne!(o.proxy, Proxy::FromEnv);
        assert_eq!(o.user_agent.as_deref(), Some("dl-agent/test"));
        assert_eq!(o.timeout, Some(POLL_TIMEOUT));
    }

    #[test]
    fn base_url_needs_https_except_on_loopback() {
        assert_eq!(
            check_base_url("https://arrival.example/")
                .expect("https")
                .to_string(),
            "https://arrival.example/"
        );
        assert_eq!(
            check_base_url("https://arrival.example/tenant")
                .expect("path")
                .to_string(),
            "https://arrival.example/tenant/"
        );
        assert!(check_base_url("http://127.0.0.1:8080").is_ok());
        assert!(check_base_url("http://localhost:8080").is_ok());
        assert!(check_base_url("http://arrival.example").is_err());
        assert!(check_base_url("").is_err());
    }

    #[test]
    fn failures_map_to_the_right_health_kind() {
        assert_eq!(
            classify(&Error::Refused { clock: true }),
            CloudFailure::ClockSkew
        );
        assert_eq!(
            classify(&Error::ClockSkew { seconds: 300 }),
            CloudFailure::ClockSkew
        );
        assert_eq!(
            classify(&Error::Refused { clock: false }),
            CloudFailure::Auth
        );
        assert_eq!(classify(&Error::Crypto("tag".into())), CloudFailure::Auth);
        assert_eq!(
            classify(&Error::Transport("refused".into())),
            CloudFailure::Transport
        );
        assert_eq!(classify(&Error::CatalogRejected), CloudFailure::Transport);
        assert!(refused_message(false, "a1", "k1").contains("a1 (k1)"));
    }
}
