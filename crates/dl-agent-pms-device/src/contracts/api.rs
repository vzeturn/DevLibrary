//! Wire DTOs of the API (`Queue/ApiContracts.cs`), read with `DeviceJson.Envelope` (unknown
//! members skipped) and written like System.Text.Json (camelCase, `null` members kept).

use dl_json::{ToJson, Value, json};
use dl_time::OffsetDateTime;
use dl_uuid::Uuid;

use super::json::{PayloadError, Reader, to_dotnet_json, wrong_token};
use super::{CommandOutcome, DeviceCommand, DeviceError};
use crate::config::DotnetError;

const CLAIM_RESPONSE: &str = "SmilePms.DeviceAgent.Queue.ClaimResponse";
const RESULT_REQUEST: &str = "SmilePms.DeviceAgent.Queue.ResultRequest";
const PENDING_RESULT: &str = "SmilePms.DeviceAgent.Queue.PendingResult";
const DEVICE_ERROR: &str = "SmilePms.DeviceAgent.Commands.DeviceError";

/// Body of 200 from `POST /device-agent/commands/claim` (contract/claim.response.json).
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimResponse {
    /// `id`; the nil UUID when missing (C# `default(Guid)`).
    pub id: Uuid,
    /// `kind`.
    pub kind: Option<String>,
    /// `payload`: `None` when the member is missing (C# `JsonValueKind.Undefined`),
    /// `Some(Value::Null)` for an explicit `null`.
    pub payload: Option<Value>,
    /// `claimToken`.
    pub claim_token: Uuid,
    /// `attempt`.
    pub attempt: i32,
    /// `leaseSeconds`; older APIs omit it.
    pub lease_seconds: Option<i32>,
}

impl ClaimResponse {
    /// `ClaimResponse.DefaultLeaseSeconds`: the API's lease when `leaseSeconds` is absent.
    pub const DEFAULT_LEASE_SECONDS: i32 = 60;
    /// `ClaimResponse.LeaseMargin` in seconds, taken off the lease.
    pub const LEASE_MARGIN_SECONDS: i64 = 5;

    /// `JsonSerializer.Deserialize<ClaimResponse>(body, DeviceJson.Envelope)`; `Ok(None)`
    /// for a JSON `null` body.
    pub fn from_json(body: &Value) -> Result<Option<ClaimResponse>, PayloadError> {
        if body.is_null() {
            return Ok(None);
        }
        let mut r = Reader::new(body, false);
        let map = r.object(body, CLAIM_RESPONSE)?;
        let mut c = ClaimResponse {
            id: Uuid::NIL,
            kind: None,
            payload: None,
            claim_token: Uuid::NIL,
            attempt: 0,
            lease_seconds: None,
        };
        r.members(
            map,
            CLAIM_RESPONSE,
            &[
                "id",
                "kind",
                "payload",
                "claimToken",
                "attempt",
                "leaseSeconds",
            ],
            |r, i, v| {
                match i {
                    0 => c.id = r.guid(v, CLAIM_RESPONSE)?,
                    1 => c.kind = r.string(v, CLAIM_RESPONSE)?,
                    2 => c.payload = Some(v.clone()),
                    3 => c.claim_token = r.guid(v, CLAIM_RESPONSE)?,
                    4 => c.attempt = r.int32(v, CLAIM_RESPONSE)?,
                    _ => c.lease_seconds = r.int32_opt(v, CLAIM_RESPONSE)?,
                }
                Ok(())
            },
        )?;
        Ok(Some(c))
    }

    /// `ClaimResponse.ToCommand(claimedAt)`: the lease ends `leaseSeconds` (default 60)
    /// less 5 s after `claimed_at`. A missing payload fails like `default(JsonElement).Clone()`.
    pub fn to_command(&self, claimed_at: OffsetDateTime) -> Result<DeviceCommand, DotnetError> {
        let payload = self.payload.clone().ok_or_else(|| {
            DotnetError::new(
                "System.InvalidOperationException",
                "Operation is not valid due to the current state of the object.",
            )
        })?;
        let lease = i64::from(self.lease_seconds.unwrap_or(Self::DEFAULT_LEASE_SECONDS))
            - Self::LEASE_MARGIN_SECONDS;
        let nanos = claimed_at.unix_nanos() + i128::from(lease) * 1_000_000_000;
        let lease_until = OffsetDateTime::from_unix_nanos(nanos)
            .ok()
            .and_then(|t| t.checked_to_offset(claimed_at.offset()))
            .ok_or_else(|| {
                DotnetError::new(
                    "System.ArgumentOutOfRangeException",
                    "The added or subtracted value results in an un-representable DateTime. (Parameter 'value')",
                )
            })?;
        Ok(DeviceCommand {
            id: self.id,
            kind: self.kind.clone().unwrap_or_default(),
            payload,
            claim_token: self.claim_token,
            attempt: self.attempt,
            lease_until,
        })
    }
}

/// Body of `POST /device-agent/commands/{id}/result` (contract/result.request.json).
#[derive(Debug, Clone, PartialEq)]
pub struct ResultRequest {
    /// `claimToken`.
    pub claim_token: Uuid,
    /// `outcome`: `done` or `failed`.
    pub outcome: String,
    /// `result` (JSON `null` when absent).
    pub result: Option<Value>,
    /// `error`.
    pub error: Option<DeviceError>,
}

impl ResultRequest {
    /// `ResultRequest.From(claimToken, outcome)`.
    pub fn from_outcome(claim_token: Uuid, outcome: &CommandOutcome) -> ResultRequest {
        ResultRequest {
            claim_token,
            outcome: if outcome.ok() { "done" } else { "failed" }.to_owned(),
            result: outcome.result.clone(),
            error: outcome.error.clone(),
        }
    }

    fn decode(r: &mut Reader<'_>, v: &Value) -> Result<Option<ResultRequest>, PayloadError> {
        r.nested(v, RESULT_REQUEST, |r, map| {
            let mut o = ResultRequest {
                claim_token: Uuid::NIL,
                outcome: String::new(),
                result: None,
                error: None,
            };
            r.members(
                map,
                RESULT_REQUEST,
                &["claimToken", "outcome", "result", "error"],
                |r, i, v| {
                    match i {
                        0 => o.claim_token = r.guid(v, RESULT_REQUEST)?,
                        1 => o.outcome = r.string(v, RESULT_REQUEST)?.unwrap_or_default(),
                        2 => o.result = (!v.is_null()).then(|| v.clone()),
                        _ => {
                            o.error = r.nested(v, DEVICE_ERROR, |r, map| {
                                let mut e = DeviceError {
                                    code: String::new(),
                                    message: String::new(),
                                };
                                r.members(map, DEVICE_ERROR, &["code", "message"], |r, i, v| {
                                    let s = r.string(v, DEVICE_ERROR)?.unwrap_or_default();
                                    if i == 0 {
                                        e.code = s
                                    } else {
                                        e.message = s
                                    }
                                    Ok(())
                                })?;
                                Ok(e)
                            })?
                        }
                    }
                    Ok(())
                },
            )?;
            Ok(o)
        })
    }
}

impl ToJson for DeviceError {
    fn to_json(&self) -> Value {
        json!({"code": self.code, "message": self.message})
    }
}

impl ToJson for ResultRequest {
    fn to_json(&self) -> Value {
        json!({
            "claimToken": self.claim_token.to_string(),
            "outcome": self.outcome,
            "result": self.result.clone().unwrap_or(Value::Null),
            "error": self.error.as_ref().map_or(Value::Null, ToJson::to_json),
        })
    }
}

/// A result the agent owes the API, written to the outbox before the first send.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingResult {
    /// `commandId`.
    pub command_id: Uuid,
    /// `kind`.
    pub kind: String,
    /// `leaseUntil`: when the claim lapses, on the agent's clock.
    pub lease_until: OffsetDateTime,
    /// `body`: what is posted to the result route.
    pub body: ResultRequest,
}

impl PendingResult {
    /// `PendingResult.From(command, outcome)`.
    pub fn from_command(command: &DeviceCommand, outcome: &CommandOutcome) -> PendingResult {
        PendingResult {
            command_id: command.id,
            kind: command.kind.clone(),
            lease_until: command.lease_until,
            body: ResultRequest::from_outcome(command.claim_token, outcome),
        }
    }

    /// Reads an outbox file body (`DeviceJson.Envelope`). Unlike C#, a missing or `null`
    /// `body` is an error (C# would load it and fail later when sending).
    pub fn from_json(v: &Value) -> Result<PendingResult, PayloadError> {
        let mut r = Reader::new(v, false);
        let map = r.object(v, PENDING_RESULT)?;
        let mut command_id = Uuid::NIL;
        let mut kind = String::new();
        // C# default(DateTimeOffset) is 0001-01-01; a missing leaseUntil reads as long expired.
        let mut lease_until = None;
        let mut body = None;
        r.members(
            map,
            PENDING_RESULT,
            &["commandId", "kind", "leaseUntil", "body"],
            |r, i, v| {
                match i {
                    0 => command_id = r.guid(v, PENDING_RESULT)?,
                    1 => kind = r.string(v, PENDING_RESULT)?.unwrap_or_default(),
                    2 => {
                        lease_until = match v.as_str().map(OffsetDateTime::parse_rfc3339) {
                            Some(Ok(t)) => Some(t),
                            Some(Err(_)) => {
                                return Err(r.convert_error(PENDING_RESULT).with_inner(
                                    DotnetError::new(
                                        "System.FormatException",
                                        "The JSON value is not in a supported DateTimeOffset format.",
                                    ),
                                ));
                            }
                            None => {
                                return Err(r
                                    .convert_error(PENDING_RESULT)
                                    .with_inner(wrong_token(v, "a string")));
                            }
                        }
                    }
                    _ => body = ResultRequest::decode(r, v)?,
                }
                Ok(())
            },
        )?;
        let lease_until = match lease_until {
            Some(t) => t,
            None => min_date_time_offset()?,
        };
        let body = body.ok_or_else(|| PayloadError::new("outbox entry has no body"))?;
        Ok(PendingResult {
            command_id,
            kind,
            lease_until,
            body,
        })
    }
}

impl PendingResult {
    /// The outbox file text, as `JsonSerializer.SerializeToUtf8Bytes(result,
    /// DeviceJson.Envelope)` writes it: [`to_dotnet_json`] escaping, except `leaseUntil`,
    /// which System.Text.Json formats itself without the encoder (its `+` stays as is).
    pub fn to_outbox_json(&self) -> String {
        let mut s = String::from("{\"commandId\":");
        s.push_str(&to_dotnet_json(&Value::from(self.command_id.to_string())));
        s.push_str(",\"kind\":");
        s.push_str(&to_dotnet_json(&Value::from(self.kind.as_str())));
        s.push_str(",\"leaseUntil\":\"");
        s.push_str(&format_date_time_offset(self.lease_until));
        s.push_str("\",\"body\":");
        s.push_str(&to_dotnet_json(&self.body.to_json()));
        s.push('}');
        s
    }
}

impl ToJson for PendingResult {
    fn to_json(&self) -> Value {
        json!({
            "commandId": self.command_id.to_string(),
            "kind": self.kind,
            "leaseUntil": format_date_time_offset(self.lease_until),
            "body": self.body.to_json(),
        })
    }
}

/// `default(DateTimeOffset)`: 0001-01-01T00:00:00+00:00.
fn min_date_time_offset() -> Result<OffsetDateTime, PayloadError> {
    // 0001-01-01 is 719 162 days before the Unix epoch.
    OffsetDateTime::from_unix_nanos(-719_162_i128 * 86_400 * 1_000_000_000)
        .map_err(|e| PayloadError::new(e.to_string()))
}

/// System.Text.Json's `DateTimeOffset` form: `yyyy-MM-ddTHH:mm:ss[.fffffff]` (trailing
/// zeros of the 100 ns fraction trimmed) and the offset as `+HH:MM`.
pub fn format_date_time_offset(t: OffsetDateTime) -> String {
    let mut s = super::json::format_wall_clock(t.datetime());
    let ticks = t.nanosecond() / 100;
    if ticks > 0 {
        let frac = format!("{ticks:07}");
        s.push('.');
        s.push_str(frac.trim_end_matches('0'));
    }
    let secs = t.offset().whole_seconds();
    let sign = if secs < 0 { '-' } else { '+' };
    let m = secs.unsigned_abs() / 60;
    s.push_str(&format!("{sign}{:02}:{:02}", m / 60, m % 60));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbox_text_escapes_strings_but_not_the_lease_time() {
        // .NET writes the DateTimeOffset without the encoder (the e2e outbox comparison).
        let pending = PendingResult {
            command_id: Uuid::parse("6f1c2b0e-8d1a-4c55-9a51-3f7d2b9e0a11").unwrap(),
            kind: "lock.issue".into(),
            lease_until: OffsetDateTime::parse_rfc3339("2026-10-14T14:00:05.5+07:00").unwrap(),
            body: ResultRequest {
                claim_token: Uuid::parse("0b8f6c1e-2c3d-4e5f-8a9b-1c2d3e4f5a6b").unwrap(),
                outcome: "failed".into(),
                result: None,
                error: Some(DeviceError {
                    code: "DEVICE_NOT_CONFIGURED".into(),
                    message: "room's".into(),
                }),
            },
        };
        assert_eq!(
            pending.to_outbox_json(),
            r#"{"commandId":"6f1c2b0e-8d1a-4c55-9a51-3f7d2b9e0a11","kind":"lock.issue","leaseUntil":"2026-10-14T14:00:05.5+07:00","body":{"claimToken":"0b8f6c1e-2c3d-4e5f-8a9b-1c2d3e4f5a6b","outcome":"failed","result":null,"error":{"code":"DEVICE_NOT_CONFIGURED","message":"room\u0027s"}}}"#
        );
    }
}
