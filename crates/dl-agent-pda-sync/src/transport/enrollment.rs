//! Redeems the one-time enrollment code (port of `Transport/EnrollmentClient.cs`); the
//! request is signed with the very key being enrolled (proof of possession).

use super::Error;
use super::element;
use super::http::SyncHttpClient;
use crate::config::{AgentError, codes};
use crate::reporting::log::Logger;
use crate::security::AgentKey;
use crate::util::ctx::Ctx;
use crate::util::stj::quote;

/// The route of the redemption.
pub const REDEEM_PATH: &str = "/sync/v1/enroll/redeem";
/// The logger category (`ILogger<EnrollmentClient>`).
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Transport.EnrollmentClient";

/// What the cloud returns for a redeemed code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemResult {
    /// The agent id the cloud assigned.
    pub agent_id: dl_uuid::Uuid,
    /// The long-lived agent token.
    pub agent_token: String,
    /// The tenant (`JsonElement.ToString()` of `tenantId`, empty when absent).
    pub tenant_id: String,
}

/// Redeems enrollment codes (`EnrollmentClient`).
pub struct EnrollmentClient<'a> {
    http: &'a SyncHttpClient,
    key: &'a AgentKey,
    log: Logger,
}

impl std::fmt::Debug for EnrollmentClient<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrollmentClient").finish_non_exhaustive()
    }
}

/// `Guid.Parse`: surrounding white space allowed; `D`, `N`, `B` forms.
fn parse_guid(s: Option<String>) -> Result<dl_uuid::Uuid, Error> {
    let Some(s) = s else {
        return Err(Error::Unexpected {
            type_name: "System.ArgumentNullException",
            message: "Value cannot be null. (Parameter 'input')".to_owned(),
        });
    };
    let t = s.trim();
    let ok = matches!(t.len(), 32 | 36 | 38) && !t.to_ascii_lowercase().starts_with("urn:");
    ok.then(|| dl_uuid::Uuid::parse(t).ok())
        .flatten()
        .ok_or_else(|| Error::Unexpected {
            type_name: "System.FormatException",
            message: format!("Unrecognized Guid format. Value: '{t}'"),
        })
}

impl<'a> EnrollmentClient<'a> {
    /// A client sending with `http`, proving possession of `key`.
    pub fn new(http: &'a SyncHttpClient, key: &'a AgentKey, log: &Logger) -> EnrollmentClient<'a> {
        EnrollmentClient {
            http,
            key,
            log: log.for_category(CATEGORY),
        }
    }

    /// The request body: `{"code":…,"publicKey":…,"machineId":…,"agentVersion":…}`.
    pub fn body(&self, code: &str, machine_id: &str, agent_version: &str) -> String {
        format!(
            "{{\"code\":{},\"publicKey\":{},\"machineId\":{},\"agentVersion\":{}}}",
            quote(code),
            quote(&self.key.public_key_base64()),
            quote(machine_id),
            quote(agent_version)
        )
    }

    /// Redeems `code` for this machine.
    pub fn redeem(
        &self,
        ctx: &Ctx,
        code: &str,
        machine_id: &str,
        agent_version: &str,
    ) -> Result<RedeemResult, Error> {
        let body = self.body(code, machine_id, agent_version);
        let resp = self.http.post_json(ctx, REDEEM_PATH, &body, None)?;
        if matches!(resp.status, 400 | 401 | 403 | 404 | 409 | 410) {
            return Err(AgentError::new(
                codes::ENROLL_CODE_INVALID,
                format!(
                    "The enrollment code was refused (HTTP {}). Codes are single-use and expire after 24 h; create a new one in the web console.",
                    resp.status
                ),
            )
            .into());
        }
        if !resp.is_success() {
            return Err(AgentError::new(
                codes::ENROLL_FAILED,
                format!("Enrollment failed with HTTP {}.", resp.status),
            )
            .into());
        }
        let data = resp.data()?.ok_or_else(|| {
            AgentError::new(codes::ENROLL_FAILED, "Enrollment response had no data.")
        })?;
        let agent_id = parse_guid(element::string(element::get(&data, "agentId")?)?)?;
        let agent_token =
            element::string(element::get(&data, "agentToken")?)?.ok_or_else(|| {
                Error::Unexpected {
                    type_name: "System.ArgumentNullException",
                    message: "Value cannot be null. (Parameter 's')".to_owned(),
                }
            })?;
        let tenant_id = element::try_get(&data, "tenantId")?
            .map(element::to_string)
            .unwrap_or_default();
        let result = RedeemResult {
            agent_id,
            agent_token,
            tenant_id,
        };
        self.log
            .info("Enrolled as agent {AgentId}", &[result.agent_id.into()]);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{RetryPolicy, SyncHttpClient};
    use std::time::Duration;

    /// Expected: `JsonSerializer.Serialize(new { code, publicKey, machineId, agentVersion })`
    /// and `new { agentToken }` on .NET 10 (`sa-core\b2b`, "REDEEMBODY", "TOKENBODY").
    #[test]
    fn bodies_match_system_text_json() {
        let seed: Vec<u8> = (1..=32).collect();
        let key = AgentKey::from_seed(&seed).unwrap();
        let log = Logger::none();
        let http = SyncHttpClient::new(
            "http://127.0.0.1:9/",
            Duration::from_secs(1),
            None,
            RetryPolicy::default(),
            &log,
        )
        .unwrap();
        let c = EnrollmentClient::new(&http, &key, &log);
        assert_eq!(
            c.body("CODE-1234", "PC-Ơ", "3.30.0"),
            r#"{"code":"CODE-1234","publicKey":"ebVWLo/mVPlAeLES6KmLp5AfhTrmlb7X4OORC60ElmQ=","machineId":"PC-\u01A0","agentVersion":"3.30.0"}"#
        );
        assert_eq!(
            format!("{{\"agentToken\":{}}}", quote("tok+/<ơ>\"")),
            r#"{"agentToken":"tok\u002B/\u003C\u01A1\u003E\u0022"}"#
        );
    }

    #[test]
    fn guids_parse_like_dotnet() {
        let id = "5f0c1b9e-0000-4000-8000-000000000001";
        assert_eq!(parse_guid(Some(format!(" {id} "))).unwrap().to_string(), id);
        assert_eq!(
            parse_guid(Some("5F0C1B9E000040008000000000000001".into()))
                .unwrap()
                .to_string(),
            id
        );
        assert!(matches!(
            parse_guid(None),
            Err(Error::Unexpected {
                type_name: "System.ArgumentNullException",
                ..
            })
        ));
        assert!(matches!(
            parse_guid(Some("x".into())),
            Err(Error::Unexpected {
                type_name: "System.FormatException",
                ..
            })
        ));
    }
}
