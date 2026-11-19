//! `HubClient`: the `api/agent/*` routes the read side uses.
//!
//! Every request carries `Authorization: Bearer <token>` and `X-Hotel-Id: <uuid>`
//! (`AgentAuthHandler`), and passes through the [`gate::TokenGate`]. Bodies are the
//! [`crate::wire`] JSON, byte for byte what the .NET agent sends.
//!
//! Older-hub fallbacks kept from .NET: `POST jobs/pull` answered 404/405 falls back to
//! `GET jobs`; a `null` sync-config means both syncs on; a 202 on folio-observations means
//! "held" (the hub keeps the report because the source database is not confirmed yet), which
//! counts as accepted and is never resent.
//!
//! `jobs/ack` is the M2 write side. Not ported yet: `earn`,
//! `earn/walk-in`, `bookings/pending-checkout`, `bookings/pms-cancelled`, `folio-revenue`,
//! `checkout-scan-summary`, `pms/extra-bed-terms`, `pms/special-services`, `update/*`.

pub mod gate;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use dl_http::{ClientOptions, Request, Response, Url};
use dl_uuid::Uuid;

use crate::jobs::result::Ack;
use crate::wire::parse::{self, DecodeError, Job, ObservationResult, SyncConfig};
use crate::wire::{
    AvailabilityRequest, FolioObservation, Heartbeat, RoomInventoryRequest, RoomTypeCatalogRequest,
    observation_batch,
};

pub use gate::TokenGate;

/// `X-MM-Agent-Auth-Error`.
pub const AUTH_ERROR_HEADER: &str = "X-MM-Agent-Auth-Error";
/// The value of [`AUTH_ERROR_HEADER`] that closes the gate.
pub const TOKEN_REJECTED_CODE: &str = "AGENT_TOKEN_REJECTED";
/// `X-MM-Run-Diagnostics` (heartbeat reply).
pub const RUN_DIAGNOSTICS_HEADER: &str = "X-MM-Run-Diagnostics";

/// Why a hub call failed. Messages never carry the token or a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubError {
    /// The hub rejected the token (or the gate is closed); retry after `retry_in`.
    TokenRejected {
        /// The gate's probe interval.
        retry_in: Duration,
    },
    /// A non-success status.
    Status {
        /// The HTTP status.
        code: u16,
        /// The route, e.g. `heartbeat`.
        route: &'static str,
    },
    /// The request timed out (the folio poster halves its batch on this).
    Timeout(&'static str),
    /// Connect, TLS or protocol failure.
    Transport(String),
    /// The body did not decode.
    Decode(String),
}

impl HubError {
    /// Whether this is the token gate (auth) rather than the network or the hub.
    pub fn is_auth(&self) -> bool {
        matches!(self, HubError::TokenRejected { .. })
    }

    /// `FolioObservationPoster.IsTimeout`: a client timeout, or a 408/504 from the hub.
    pub fn is_timeout(&self) -> bool {
        matches!(
            self,
            HubError::Timeout(_)
                | HubError::Status {
                    code: 408 | 504,
                    ..
                }
        )
    }
}

impl fmt::Display for HubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HubError::TokenRejected { retry_in } => write!(
                f,
                "hub rejected the agent token ({TOKEN_REJECTED_CODE}); next probe in {} min",
                retry_in.as_secs() / 60
            ),
            HubError::Status { code, route } => write!(f, "hub {route} answered {code}"),
            HubError::Timeout(route) => write!(f, "hub {route} timed out"),
            HubError::Transport(e) => write!(f, "hub unreachable: {e}"),
            HubError::Decode(e) => write!(f, "hub answer unreadable: {e}"),
        }
    }
}

impl std::error::Error for HubError {}

impl From<DecodeError> for HubError {
    fn from(e: DecodeError) -> HubError {
        HubError::Decode(e.0)
    }
}

/// What the scanners and the sync loop need from the hub. [`HubClient`] is the real one;
/// tests use hand-written fakes.
pub trait HubApi: Send + Sync {
    /// `POST heartbeat`. Returns whether the hub asked for a SMILE diagnostics run.
    fn heartbeat(&self, hb: &Heartbeat) -> Result<bool, HubError>;
    /// `GET sync-config`.
    fn sync_config(&self) -> Result<SyncConfig, HubError>;
    /// `POST jobs/pull?max=` (older hub: `GET jobs?max=`). Leases the jobs.
    fn pull_jobs(&self, max: u32) -> Result<Vec<Job>, HubError>;
    /// `POST availability`.
    fn post_availability(&self, req: &AvailabilityRequest) -> Result<(), HubError>;
    /// `POST room-inventory`.
    fn post_room_inventory(&self, req: &RoomInventoryRequest) -> Result<(), HubError>;
    /// `POST room-types`.
    fn post_room_types(&self, req: &RoomTypeCatalogRequest) -> Result<(), HubError>;
    /// `POST folio-observations`. A held batch (202) returns an empty result.
    fn post_folio_observations(
        &self,
        observations: &[FolioObservation],
        agent_version: &str,
        db_identity_fingerprint: Option<&str>,
    ) -> Result<ObservationResult, HubError>;
    /// `POST jobs/ack` (`EnsureNoContent`).
    fn ack_job(&self, ack: &Ack) -> Result<(), HubError>;
    /// `POST pms/extra-bed-terms`: `false` when the hub predates the endpoint (404/405).
    fn post_extra_bed_terms(&self, _req: &crate::terms::TermsRequest) -> Result<bool, HubError> {
        Ok(false)
    }
    /// `POST pms/special-services`: `false` when the hub predates the endpoint (404/405).
    fn post_special_services(&self, _req: &crate::terms::CatalogRequest) -> Result<bool, HubError> {
        Ok(false)
    }
}

/// `HubBaseAddress.Create`: trims, drops a trailing `/` and a trailing `/api` (enrolment
/// packages carry it), and ends with `/` so `api/agent/...` joins under it.
pub fn base_address(hub_base_url: &str) -> Result<Url, HubError> {
    let mut url = hub_base_url.trim().trim_end_matches('/');
    if url.len() >= 4 && url[url.len() - 4..].eq_ignore_ascii_case("/api") {
        url = &url[..url.len() - 4];
    }
    Url::parse(&format!("{url}/")).map_err(|e| HubError::Transport(format!("bad hub URL: {e}")))
}

/// The real hub over `dl-http`.
pub struct HubClient {
    base: Url,
    token: String,
    hotel_id: String,
    http: dl_http::Client,
    gate: Arc<TokenGate>,
}

impl fmt::Debug for HubClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HubClient")
            .field("base", &self.base.to_string())
            .field("hotel_id", &self.hotel_id)
            .finish_non_exhaustive()
    }
}

impl HubClient {
    /// A client for `hub_base_url` acting as `hotel_id` with `token`.
    pub fn new(
        hub_base_url: &str,
        token: &str,
        hotel_id: Uuid,
        user_agent: &str,
        gate: Arc<TokenGate>,
    ) -> Result<HubClient, HubError> {
        let http = dl_http::Client::with_options(ClientOptions {
            timeout: Some(Duration::from_secs(100)),
            user_agent: Some(user_agent.to_owned()),
            ..ClientOptions::default()
        });
        Ok(HubClient {
            base: base_address(hub_base_url)?,
            token: token.to_owned(),
            hotel_id: hotel_id.to_string(),
            http,
            gate,
        })
    }

    /// The token gate.
    pub fn gate(&self) -> &Arc<TokenGate> {
        &self.gate
    }

    fn send(
        &self,
        route: &'static str,
        path: &str,
        body: Option<String>,
    ) -> Result<Response, HubError> {
        if !self.gate.try_enter() {
            return Err(HubError::TokenRejected {
                retry_in: self.gate.retry_interval(),
            });
        }
        let url = self
            .base
            .join(path)
            .map_err(|e| HubError::Transport(e.to_string()))?;
        let req = match body {
            // `PostAsync(content: null)`: no body, no content type.
            Some(b) if b.is_empty() => Ok(Request::post(url, Vec::new())),
            Some(b) => Request::post(url, b.into_bytes())
                .with_header("Content-Type", "application/json; charset=utf-8"),
            None => Ok(Request::get(url)),
        };
        let req = req
            .and_then(|r| r.with_header("Authorization", &format!("Bearer {}", self.token)))
            .and_then(|r| r.with_header("X-Hotel-Id", &self.hotel_id))
            .map_err(|_| HubError::Transport(format!("{route}: invalid request header")))?;
        let resp = self.http.request(&req).map_err(|e| match e {
            dl_http::Error::Timeout => HubError::Timeout(route),
            e => HubError::Transport(format!("{route}: {e}")),
        })?;
        let code = resp.status.as_u16();
        if code == 401
            && resp
                .headers
                .get_all(AUTH_ERROR_HEADER)
                .any(|v| v.split(',').any(|p| p.trim() == TOKEN_REJECTED_CODE))
        {
            self.gate.record_rejected();
            return Err(HubError::TokenRejected {
                retry_in: self.gate.retry_interval(),
            });
        }
        if resp.status.is_success() {
            self.gate.record_accepted();
        }
        Ok(resp)
    }

    /// A report an older hub may lack: 404/405 is `false`, else `EnsureNoContent`.
    fn post_optional(
        &self,
        route: &'static str,
        path: &str,
        body: String,
    ) -> Result<bool, HubError> {
        let resp = self.send(route, path, Some(body))?;
        if matches!(resp.status.as_u16(), 404 | 405) {
            return Ok(false);
        }
        ensure_ok(&resp, route).map(|()| true)
    }

    fn post_no_content(
        &self,
        route: &'static str,
        path: &str,
        body: String,
    ) -> Result<(), HubError> {
        let resp = self.send(route, path, Some(body))?;
        ensure_ok(&resp, route)
    }
}

/// `EnsureNoContent`: 200 or 204.
fn ensure_ok(resp: &Response, route: &'static str) -> Result<(), HubError> {
    match resp.status.as_u16() {
        200 | 204 => Ok(()),
        code => Err(HubError::Status { code, route }),
    }
}

fn ensure_success(resp: &Response, route: &'static str) -> Result<(), HubError> {
    if resp.status.is_success() {
        Ok(())
    } else {
        Err(HubError::Status {
            code: resp.status.as_u16(),
            route,
        })
    }
}

impl HubApi for HubClient {
    fn ack_job(&self, ack: &Ack) -> Result<(), HubError> {
        self.post_no_content("ack", "api/agent/jobs/ack", ack.to_j().to_json())
    }

    fn heartbeat(&self, hb: &Heartbeat) -> Result<bool, HubError> {
        let resp = self.send(
            "heartbeat",
            "api/agent/heartbeat",
            Some(hb.to_j().to_json()),
        )?;
        ensure_ok(&resp, "heartbeat")?;
        Ok(resp
            .headers
            .get_all(RUN_DIAGNOSTICS_HEADER)
            .any(|v| v.trim() == "1"))
    }

    fn sync_config(&self) -> Result<SyncConfig, HubError> {
        let resp = self.send("sync-config", "api/agent/sync-config", None)?;
        ensure_success(&resp, "sync-config")?;
        let body = resp.body.trim_ascii();
        if body.is_empty() || body == b"null" {
            return Ok(SyncConfig {
                room_inventory_sync_enabled: true,
                availability_sync_enabled: true,
                ..SyncConfig::default()
            });
        }
        Ok(parse::sync_config(body)?)
    }

    fn pull_jobs(&self, max: u32) -> Result<Vec<Job>, HubError> {
        let resp = self.send(
            "jobs/pull",
            &format!("api/agent/jobs/pull?max={max}"),
            Some(String::new()),
        )?;
        let resp = match resp.status.as_u16() {
            404 | 405 => {
                let legacy = self.send("jobs", &format!("api/agent/jobs?max={max}"), None)?;
                ensure_success(&legacy, "jobs")?;
                legacy
            }
            _ => {
                ensure_success(&resp, "jobs/pull")?;
                resp
            }
        };
        let body = resp.body.trim_ascii();
        if body.is_empty() || body == b"null" {
            return Ok(Vec::new());
        }
        Ok(parse::jobs(body)?)
    }

    fn post_extra_bed_terms(&self, req: &crate::terms::TermsRequest) -> Result<bool, HubError> {
        self.post_optional(
            "pms/extra-bed-terms",
            "api/agent/pms/extra-bed-terms",
            req.to_j().to_json(),
        )
    }

    fn post_special_services(&self, req: &crate::terms::CatalogRequest) -> Result<bool, HubError> {
        self.post_optional(
            "pms/special-services",
            "api/agent/pms/special-services",
            req.to_j().to_json(),
        )
    }

    fn post_availability(&self, req: &AvailabilityRequest) -> Result<(), HubError> {
        self.post_no_content(
            "availability",
            "api/agent/availability",
            req.to_j().to_json(),
        )
    }

    fn post_room_inventory(&self, req: &RoomInventoryRequest) -> Result<(), HubError> {
        self.post_no_content(
            "room-inventory",
            "api/agent/room-inventory",
            req.to_j().to_json(),
        )
    }

    fn post_room_types(&self, req: &RoomTypeCatalogRequest) -> Result<(), HubError> {
        self.post_no_content("room-types", "api/agent/room-types", req.to_j().to_json())
    }

    fn post_folio_observations(
        &self,
        observations: &[FolioObservation],
        agent_version: &str,
        db_identity_fingerprint: Option<&str>,
    ) -> Result<ObservationResult, HubError> {
        let body =
            observation_batch(observations, agent_version, db_identity_fingerprint).to_json();
        let resp = self.send(
            "folio-observations",
            "api/agent/folio-observations",
            Some(body),
        )?;
        if resp.status.as_u16() == 202 {
            crate::log::info(
                "hub held folio-observations: source SMILE database not confirmed yet; not resending",
                &[("count", crate::log::V::I(observations.len() as i64))],
            );
            return Ok(ObservationResult::default());
        }
        ensure_success(&resp, "folio-observations")?;
        Ok(parse::observation_result(&resp.body)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_address_drops_api_suffix_and_adds_slash() {
        for (input, want) in [
            ("https://hub.example.test", "https://hub.example.test/"),
            (
                " https://hub.example.test/api/ ",
                "https://hub.example.test/",
            ),
            ("https://hub.example.test/API", "https://hub.example.test/"),
            (
                "https://hub.example.test/x/api",
                "https://hub.example.test/x/",
            ),
        ] {
            assert_eq!(base_address(input).unwrap().to_string(), want, "{input}");
        }
    }

    #[test]
    fn errors_never_echo_bodies() {
        let e = HubError::Status {
            code: 500,
            route: "heartbeat",
        };
        assert_eq!(e.to_string(), "hub heartbeat answered 500");
        assert!(
            HubError::TokenRejected {
                retry_in: Duration::from_secs(900)
            }
            .is_auth()
        );
    }
}
