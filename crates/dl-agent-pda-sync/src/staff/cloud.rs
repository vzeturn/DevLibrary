//! The two staff calls of the cloud (port of `Staff/StaffCloudClient.cs`, ADR 0101).

use super::StaffError;
use crate::contracts::WireJson;
use crate::contracts::staff;
use crate::contracts::wire::{StaffSnapshot, staff_sync_routes};
use crate::reporting::scrubber;
use crate::transport::{self, SyncHttpClient, SyncResponse, TokenProvider};
use crate::util::ctx::Ctx;
use std::sync::Arc;

/// How a staff call ended (`StaffCloudStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaffCloudStatus {
    /// 2xx.
    Ok,
    /// HTTP 409: the snapshot was made with a PIN key the cloud no longer uses.
    KeyStale,
    /// HTTP 400: the cloud refused the snapshot's shape.
    Rejected,
    /// HTTP 401/403 even after one token refresh (revoked agent, tenant not in
    /// site_sync, fenced).
    Unauthorized,
    /// HTTP 404: the cloud predates the staff sync.
    NotSupported,
    /// Unreachable, timed out, 5xx, or a malformed answer.
    Failed,
}

impl StaffCloudStatus {
    /// The C# enum member name (what the log lines show).
    pub fn name(self) -> &'static str {
        match self {
            StaffCloudStatus::Ok => "Ok",
            StaffCloudStatus::KeyStale => "KeyStale",
            StaffCloudStatus::Rejected => "Rejected",
            StaffCloudStatus::Unauthorized => "Unauthorized",
            StaffCloudStatus::NotSupported => "NotSupported",
            StaffCloudStatus::Failed => "Failed",
        }
    }
}

impl std::fmt::Display for StaffCloudStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The tenant's PIN key `K_t` and its fingerprint; never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct StaffPinKey {
    /// The 32 key bytes.
    pub key: Vec<u8>,
    /// `StaffPin.KeyId(key)`.
    pub key_id: String,
}

impl std::fmt::Debug for StaffPinKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaffPinKey")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

/// The answer of a staff call (`StaffCloudResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaffCloudResult {
    /// How it ended.
    pub status: StaffCloudStatus,
    /// The HTTP status, 0 when none was received.
    pub http_status: u16,
    /// The key, for a successful key fetch.
    pub pin_key: Option<StaffPinKey>,
}

impl StaffCloudResult {
    /// A result without a key.
    pub fn new(status: StaffCloudStatus, http_status: u16) -> StaffCloudResult {
        StaffCloudResult {
            status,
            http_status,
            pin_key: None,
        }
    }
}

/// The two staff calls of the cloud (`IStaffCloud`). An error is what the C# code lets
/// escape (cancellation, an unexpected exception); a coded failure is a
/// [`StaffCloudStatus::Failed`] result.
pub trait StaffCloud: Send + Sync {
    /// `GET /sync/v1/staff/pin-key`.
    fn get_pin_key(&self, ctx: &Ctx) -> Result<StaffCloudResult, StaffError>;
    /// `POST /sync/v1/staff`.
    fn send(&self, snapshot: &StaffSnapshot, ctx: &Ctx) -> Result<StaffCloudResult, StaffError>;
}

/// Signed and authorised like the heartbeat; refreshes the token once on 401/403
/// (`StaffCloudClient`).
pub struct StaffCloudClient {
    http: Arc<SyncHttpClient>,
    tokens: Arc<TokenProvider>,
}

impl std::fmt::Debug for StaffCloudClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaffCloudClient").finish_non_exhaustive()
    }
}

impl StaffCloudClient {
    /// A client over the runtime's signed client and token provider.
    pub fn new(http: Arc<SyncHttpClient>, tokens: Arc<TokenProvider>) -> StaffCloudClient {
        StaffCloudClient { http, tokens }
    }

    fn call(
        &self,
        ctx: &Ctx,
        send: &dyn Fn(&str) -> Result<SyncResponse, transport::Error>,
        ok: &dyn Fn(&SyncResponse) -> StaffCloudResult,
    ) -> Result<StaffCloudResult, StaffError> {
        for attempt in 1..=2 {
            let resp = match self
                .tokens
                .get_access_token(ctx)
                .and_then(|bearer| send(&bearer))
            {
                Ok(r) => r,
                // catch (AgentException): a coded failure is a failed call.
                Err(transport::Error::Agent(_)) => {
                    return Ok(StaffCloudResult::new(StaffCloudStatus::Failed, 0));
                }
                Err(e) => return Err(Box::new(e)),
            };
            let status = resp.status;
            if resp.is_success() {
                return Ok(ok(&resp));
            }
            let s = match status {
                401 | 403 => {
                    self.tokens.invalidate();
                    if attempt == 1 {
                        continue;
                    }
                    StaffCloudStatus::Unauthorized
                }
                409 => StaffCloudStatus::KeyStale,
                400 => StaffCloudStatus::Rejected,
                404 => StaffCloudStatus::NotSupported,
                _ => StaffCloudStatus::Failed,
            };
            return Ok(StaffCloudResult::new(s, status));
        }
        Ok(StaffCloudResult::new(StaffCloudStatus::Unauthorized, 401))
    }
}

impl StaffCloud for StaffCloudClient {
    fn get_pin_key(&self, ctx: &Ctx) -> Result<StaffCloudResult, StaffError> {
        self.call(
            ctx,
            &|bearer| self.http.get(ctx, staff_sync_routes::PIN_KEY, Some(bearer)),
            &parse_key,
        )
    }

    fn send(&self, snapshot: &StaffSnapshot, ctx: &Ctx) -> Result<StaffCloudResult, StaffError> {
        let body = snapshot.to_json();
        self.call(
            ctx,
            &|bearer| {
                self.http
                    .post_json(ctx, staff_sync_routes::SNAPSHOT, &body, Some(bearer))
            },
            &|_| StaffCloudResult::new(StaffCloudStatus::Ok, 200),
        )
    }
}

/// `StaffCloudClient.ParseKey`: the key must be 32 bytes and match the fingerprint the
/// cloud sent with it, or it is not used. A malformed answer is a failed call (the
/// caller logs the status, never the body: it may hold the key).
pub fn parse_key(resp: &SyncResponse) -> StaffCloudResult {
    let failed = || StaffCloudResult::new(StaffCloudStatus::Failed, resp.status);
    let parsed = (|| -> Result<Option<(Vec<u8>, String)>, transport::Error> {
        let Some(data) = resp.data()? else {
            return Ok(None);
        };
        let b64 = transport::element::string(transport::element::get(&data, "key")?)?;
        let key_id = transport::element::string(transport::element::get(&data, "key_id")?)?;
        let (Some(b64), Some(key_id)) = (b64, key_id) else {
            return Ok(None);
        };
        scrubber::register(Some(&b64));
        match dl_encoding::base64::STANDARD.decode(&b64) {
            Ok(key) => Ok(Some((key, key_id))),
            // FormatException
            Err(_) => Ok(None),
        }
    })();
    match parsed {
        Ok(Some((key, key_id)))
            if key.len() == staff::KEY_BYTES && staff::key_id(&key) == key_id =>
        {
            StaffCloudResult {
                status: StaffCloudStatus::Ok,
                http_status: resp.status,
                pin_key: Some(StaffPinKey { key, key_id }),
            }
        }
        _ => failed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::staff::pin_hasher::tests::{golden_key, golden_key_id};

    fn answer(json: &str) -> SyncResponse {
        SyncResponse {
            status: 200,
            body: json.as_bytes().to_vec(),
        }
    }

    #[test]
    fn the_pin_key_answer_is_used_only_when_it_is_32_bytes_and_matches_its_fingerprint() {
        let b64 = dl_encoding::base64::STANDARD.encode(golden_key());
        let id = golden_key_id();
        let ok = parse_key(&answer(&format!(
            r#"{{"result":200,"data":{{"key":"{b64}","key_id":"{id}"}}}}"#
        )));
        assert_eq!(ok.status, StaffCloudStatus::Ok);
        let k = ok.pin_key.unwrap();
        assert_eq!(k.key, golden_key());
        assert_eq!(k.key_id, id);

        let short = dl_encoding::base64::STANDARD.encode([0u8; 16]);
        for json in [
            format!(r#"{{"data":{{"key":"{b64}","key_id":"0000000000000000"}}}}"#),
            format!(r#"{{"data":{{"key":"{short}","key_id":"x"}}}}"#),
            r#"{"data":{"key":"not base64!","key_id":"x"}}"#.to_owned(),
            r#"{"data":{}}"#.to_owned(),
            "not json".to_owned(),
        ] {
            assert_eq!(
                parse_key(&answer(&json)).status,
                StaffCloudStatus::Failed,
                "{json}"
            );
        }
    }
}
