//! The speaker's credential: the PDA identity dl-agent already enrolled for `pda-sync`.
//!
//! The Go speaker read a 365-day speaker-device JWT from its `config.json` (minted by
//! the PDA web console for a device, revocable per device). dl-agent does not keep a
//! second, long-lived bearer secret in a file: the module reads the `pda-sync` module's
//! `agent.json` (cloud URL) and its secret store (Ed25519 key + agent token), and asks
//! the cloud for a short (15 min) access token through the same `/sync/v1/token`
//! exchange `pda-sync` uses (`TokenProvider`). The token is cached and refreshed ahead of
//! expiry, and forgotten on a 401.
//!
//! Consequence: the cloud sees a **SyncAgent** principal, not a speaker device. Today
//! the PDA cloud refuses `JoinRvc` for SyncAgent (`FORBIDDEN_RVC`, `OrderHub.cs`) and the
//! sound-mode route is `ClerkOrSpeakerDevice` only. The cloud-side change this needs is in
//! the crate README ("Cloud change needed"); until it ships the module stays degraded
//! with that reason and plays nothing.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dl_agent_pda_sync::config::{AgentIdentity, AgentPaths, SystemClock};
use dl_agent_pda_sync::reporting::log::Logger;
use dl_agent_pda_sync::security::{AgentKey, names};
use dl_agent_pda_sync::transport::{RequestSigner, RetryPolicy, SyncHttpClient, TokenProvider};
use dl_agent_pda_sync::util::ctx::Ctx;

use crate::hub::TokenSource;

/// The `pda-sync` module's data folder next to this module's
/// (`<data>/modules/speaker` -> `<data>/modules/pda-sync`).
pub fn pda_sync_dir(speaker_dir: &Path) -> PathBuf {
    speaker_dir
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(dl_agent_pda_sync::module::NAME)
}

/// Limit for one token exchange.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

/// Access tokens of the enrolled PDA agent.
pub struct EnrolledTokens {
    provider: TokenProvider,
}

impl std::fmt::Debug for EnrolledTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrolledTokens").finish_non_exhaustive()
    }
}

/// What [`EnrolledTokens::load`] found.
#[derive(Debug)]
pub enum Enrolment {
    /// Enrolled: the identity and its token source.
    Ready(AgentIdentity, Box<EnrolledTokens>),
    /// `pda-sync` has not been enrolled on this machine (no `agent.json` or no key).
    NotEnrolled,
}

impl EnrolledTokens {
    /// Reads the enrolment in `pda_dir`. Never creates a key or writes anything.
    ///
    /// # Errors
    /// `agent.json` or the secret store exists but cannot be read.
    pub fn load(pda_dir: &Path) -> Result<Enrolment, String> {
        let paths = AgentPaths::new(pda_dir);
        let identity = match AgentIdentity::try_load(&paths) {
            Ok(Some(i)) => i,
            Ok(None) => return Ok(Enrolment::NotEnrolled),
            Err(e) => return Err(format!("pda-sync agent.json unreadable: {e}")),
        };
        let secrets = dl_agent_pda_sync::security::create(&paths);
        let seed = match secrets.load(names::SIGNING_SEED) {
            Ok(Some(s)) => s,
            Ok(None) => return Ok(Enrolment::NotEnrolled),
            Err(e) => return Err(format!("pda-sync key unreadable: {e}")),
        };
        let key = AgentKey::from_seed(&seed).map_err(|e| format!("pda-sync key: {e}"))?;
        let log = Logger::none();
        let clock = SystemClock::shared();
        let http = SyncHttpClient::new(
            &identity.cloud_url,
            EXCHANGE_TIMEOUT,
            Some(RequestSigner::new(Arc::new(key), clock.clone())),
            RetryPolicy::with_attempts(1),
            &log,
        )
        .map_err(|e| format!("PDA cloud client: {e}"))?;
        let provider = TokenProvider::new(Arc::new(http), secrets, paths, clock, &log);
        Ok(Enrolment::Ready(
            identity,
            Box::new(EnrolledTokens { provider }),
        ))
    }
}

impl TokenSource for EnrolledTokens {
    fn token(&self) -> Result<String, String> {
        self.provider
            .get_access_token(&Ctx::background().with_timeout(EXCHANGE_TIMEOUT))
            .map_err(|e| e.to_string())
    }

    fn invalidate(&self) {
        self.provider.invalidate();
    }
}

/// The cloud's scheme and authority (`https://pda.example.com`) from its URL.
pub fn api_base(cloud_url: &str) -> String {
    let hub = dl_agent_pda_sync::printrelay::service::hub_url(cloud_url);
    hub.strip_suffix(dl_agent_pda_sync::printrelay::service::HUB_PATH)
        .unwrap_or(&hub)
        .to_owned()
}

/// The order hub URL of the cloud (`<authority>/hubs/order`, as `pda-sync`'s relay).
pub fn hub_url(cloud_url: &str) -> String {
    dl_agent_pda_sync::printrelay::service::hub_url(cloud_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_folders() {
        assert_eq!(
            api_base("https://pda.example.com/app/"),
            "https://pda.example.com"
        );
        assert_eq!(
            hub_url("https://pda.example.com/app/"),
            "https://pda.example.com/hubs/order"
        );
        assert_eq!(
            pda_sync_dir(Path::new("/data/modules/speaker")),
            Path::new("/data/modules/pda-sync")
        );
    }

    #[test]
    fn a_machine_without_enrolment_is_not_enrolled() {
        let dir = std::env::temp_dir().join(format!("speaker-auth-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(matches!(
            EnrolledTokens::load(&dir),
            Ok(Enrolment::NotEnrolled)
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
