//! The platform's agent API: a port of `client.go` and the relay calls of `relay.go`, over
//! dl-http (HTTPS through the OS TLS stack, `HTTP(S)_PROXY` honoured).

use std::time::{Duration, Instant};

use crate::profile::CollectProfile;

use dl_http::{ClientOptions, Method, Request, Url};
use dl_json::Value;
use dl_time::OffsetDateTime;
use dl_uuid::Uuid;

use crate::domain::PushItem;
use crate::gojson::{Obj, array};
use crate::relay::wire::{MAX_REQUEST_BODY, Poll, RelayRequest, RelayResult};
use crate::sql::TrnTypeInfo;

const AGENT_API_PREFIX: &str = "/api/v1/agent";
/// Set by a hub that honoured `/poll?wait=` (MMEI RT-0785).
pub const LONG_POLL_HEADER: &str = "X-MMEI-Long-Poll";
const MAX_ATTEMPTS: u32 = 5;
const BASE_DELAY: Duration = Duration::from_millis(200);
const MAX_DELAY: Duration = Duration::from_secs(5);
const JITTER: [f64; 5] = [1.0, 0.75, 0.9, 0.6, 0.85];
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
const RELAY_TIMEOUT: Duration = Duration::from_secs(30);

/// Errors talking to the platform.
#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    /// The key now belongs to an agent on another machine: stop for good.
    DeviceReplaced,
    /// The deadline passed before a call could start or finish.
    Deadline,
    /// Anything else, with a message like the Go agent's.
    Other(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::DeviceReplaced => f.write_str(
                "agent: this agent has been replaced by a deployment on another machine",
            ),
            ApiError::Deadline => f.write_str("context deadline exceeded"),
            ApiError::Other(m) => f.write_str(m),
        }
    }
}

/// The Hub-controlled settings `GET /config` returns (the fields the agent uses).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HubConfig {
    /// Business date the collector starts from (none: the platform's default).
    pub collect_from: Option<OffsetDateTime>,
    /// Which SMILE sources to read (`fo`, `pos`, both).
    pub collect_sources: String,
    /// Fold tax into the service charge when recomputing (legacy sites).
    pub recalc_tax_into_sc: bool,
    /// Per-tenant collect choices (`collect_profile`); every key defaults to the old behaviour.
    pub collect_profile: CollectProfile,
}

/// One per-item outcome of `/invoices:push`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PushResult {
    /// The pushed item's source reference.
    pub source_ref: String,
    /// `accepted`, `duplicate`, `error`, ...
    pub outcome: String,
    /// The platform's explanation, if any.
    pub message: String,
}

/// A write-back command from `/poll`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WritebackCommand {
    /// Command id, acknowledged once applied.
    pub id: Uuid,
    /// Command type.
    pub kind: String,
    /// The bill the invoice belongs to.
    pub source_ref: String,
    /// Invoice number to write back.
    pub invoice_number: String,
    /// Invoice series (ký hiệu).
    pub ky_hieu: String,
    /// Tax authority code (mã CQT).
    pub ma_cqt: String,
    /// When the invoice was issued.
    pub issued_at: Option<OffsetDateTime>,
}

/// One captured log line sent with a heartbeat.
#[derive(Debug, Clone, PartialEq)]
pub struct HeartbeatLog {
    /// When the line was logged.
    pub ts: OffsetDateTime,
    /// `INFO`, `WARN` or `ERROR`.
    pub level: String,
    /// The message with its fields.
    pub msg: String,
}

/// The `/heartbeat` body.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Heartbeat {
    /// `dl-agent-<version>`.
    pub agent_version: String,
    /// The machine's host name.
    pub device_name: String,
    /// Invoices read in the last cycle.
    pub read: i64,
    /// Invoices pushed in the last cycle.
    pub pushed: i64,
    /// Invoices the platform answered with an error in the last cycle.
    pub errored: i64,
    /// The last cycle's error, empty when it went well.
    pub last_error: String,
    /// Recent log lines.
    pub logs: Vec<HeartbeatLog>,
}

/// `retry.Delay` with the client's deterministic jitter.
pub fn backoff(attempt: u32) -> Duration {
    let shifted = BASE_DELAY.checked_mul(1u32 << (attempt.saturating_sub(1)).min(30));
    let d = match shifted {
        Some(d) if !d.is_zero() && d <= MAX_DELAY => d,
        _ => MAX_DELAY,
    };
    let f = JITTER[(attempt as usize) % JITTER.len()];
    Duration::from_nanos((d.as_nanos() as f64 * f) as u64)
}

fn is_device_replaced(status: u16, body: &[u8]) -> bool {
    status == 401
        && dl_json::parse_slice(body)
            .ok()
            .and_then(|v| {
                v.get("error")?
                    .get("code")?
                    .as_str()
                    .map(|c| c == "device_replaced")
            })
            .unwrap_or(false)
}

fn retryable(status: u16) -> bool {
    status == 429 || status >= 500
}

fn parse_time(v: Option<&Value>) -> Option<OffsetDateTime> {
    OffsetDateTime::parse_rfc3339(v?.as_str()?).ok()
}

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The agent's platform client.
pub struct Platform {
    http: dl_http::Client,
    base: String,
    api_key: String,
    device_id: Uuid,
    sleep: fn(Duration),
}

impl Platform {
    /// `NewClient`: the public platform URL, with the agent-API prefix appended unless it
    /// is already there.
    pub fn new(base_url: &str, api_key: &str, device_id: Uuid) -> Platform {
        let mut base = base_url.trim_end_matches('/').to_owned();
        if !base.ends_with(AGENT_API_PREFIX) {
            base.push_str(AGENT_API_PREFIX);
        }
        let http = dl_http::Client::with_options(ClientOptions {
            timeout: Some(Duration::from_secs(30)),
            max_body: 32 << 20,
            ..Default::default()
        });
        Platform {
            http,
            base,
            api_key: api_key.to_owned(),
            device_id,
            sleep: std::thread::sleep,
        }
    }

    /// Replaces the retry sleep (tests).
    pub fn set_sleep(&mut self, sleep: fn(Duration)) {
        self.sleep = sleep;
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&str>,
        timeout: Duration,
    ) -> Result<Request, ApiError> {
        let url = Url::parse(&format!("{}{path}", self.base))
            .map_err(|e| ApiError::Other(format!("agent: build request: {e}")))?;
        let mut req = Request::new(method, url)
            .with_header("Authorization", &format!("Bearer {}", self.api_key))
            .and_then(|r| r.with_header("X-Device-ID", &self.device_id.to_string()))
            .map_err(|e| ApiError::Other(format!("agent: build request: {e}")))?;
        if let Some(b) = body {
            req = req
                .with_header("Content-Type", "application/json")
                .map_err(|e| ApiError::Other(format!("agent: build request: {e}")))?
                .with_body(b.as_bytes().to_vec());
        }
        Ok(req.with_timeout(timeout))
    }

    /// `do`: up to five attempts on network errors, 429 and 5xx.
    fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&str>,
        deadline: Instant,
    ) -> Result<Vec<u8>, ApiError> {
        self.call_resp(method, path, body, deadline).map(|r| r.body)
    }

    /// `call` returning the whole response (headers too).
    fn call_resp(
        &self,
        method: Method,
        path: &str,
        body: Option<&str>,
        deadline: Instant,
    ) -> Result<dl_http::Response, ApiError> {
        let m = if method == Method::Get { "GET" } else { "POST" };
        let mut last = ApiError::Deadline;
        for attempt in 1..=MAX_ATTEMPTS {
            let now = Instant::now();
            if now >= deadline {
                return Err(ApiError::Deadline);
            }
            let left = (deadline - now).min(Duration::from_secs(30));
            let req = self.request(method, path, body, left)?;
            match self.http.request(&req) {
                Err(e) => {
                    if attempt == MAX_ATTEMPTS || !self.wait(attempt, deadline) {
                        return Err(ApiError::Other(format!(
                            "agent: request failed after {attempt} attempt(s): {e}"
                        )));
                    }
                }
                Ok(resp) => {
                    let code = resp.status.as_u16();
                    if (200..300).contains(&code) {
                        return Ok(resp);
                    }
                    let text = String::from_utf8_lossy(&resp.body).into_owned();
                    if !retryable(code) {
                        if is_device_replaced(code, &resp.body) {
                            return Err(ApiError::DeviceReplaced);
                        }
                        return Err(ApiError::Other(format!(
                            "agent: {m} {path}: status {code}: {text}"
                        )));
                    }
                    last = ApiError::Other(format!("agent: {m} {path}: status {code}: {text}"));
                    if attempt == MAX_ATTEMPTS || !self.wait(attempt, deadline) {
                        return Err(last);
                    }
                }
            }
        }
        Err(last)
    }

    fn wait(&self, attempt: u32, deadline: Instant) -> bool {
        (self.sleep)(backoff(attempt));
        Instant::now() < deadline
    }

    fn parse(body: &[u8], what: &str) -> Result<Value, ApiError> {
        dl_json::parse_slice(body)
            .map_err(|e| ApiError::Other(format!("agent: decode {what} response: {e}")))
    }

    /// `GET /config`.
    pub fn config(&self, deadline: Instant) -> Result<HubConfig, ApiError> {
        let body = self.call(Method::Get, "/config", None, deadline)?;
        let v = Self::parse(&body, "config")?;
        Ok(HubConfig {
            collect_from: parse_time(v.get("collect_from")),
            collect_sources: s(&v, "collect_sources"),
            recalc_tax_into_sc: v
                .get("recalc_tax_into_sc")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            collect_profile: CollectProfile::parse(v.get("collect_profile")),
        })
    }

    /// `POST /invoices:push`.
    pub fn push(&self, items: &[PushItem], deadline: Instant) -> Result<Vec<PushResult>, ApiError> {
        let raw: Vec<String> = items.iter().map(PushItem::to_json).collect();
        self.push_raw(&raw, deadline)
    }

    /// `POST /invoices:push` with items already serialised (the outbox stores the bytes).
    pub fn push_raw(
        &self,
        items: &[String],
        deadline: Instant,
    ) -> Result<Vec<PushResult>, ApiError> {
        let mut body = String::new();
        Obj::new(&mut body)
            .raw("items", |o| array(o, Some(items), |o, it| o.push_str(it)))
            .end();
        let resp = self.call(Method::Post, "/invoices:push", Some(&body), deadline)?;
        let v = Self::parse(&resp, "push")?;
        Ok(v.get("results")
            .and_then(Value::as_array)
            .map(|rs| {
                rs.iter()
                    .map(|r| PushResult {
                        source_ref: s(r, "source_ref"),
                        outcome: s(r, "outcome"),
                        message: s(r, "message"),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// `POST /trn-types`.
    pub fn publish_trn_types(
        &self,
        types: &[TrnTypeInfo],
        deadline: Instant,
    ) -> Result<(), ApiError> {
        let mut body = String::new();
        Obj::new(&mut body)
            .raw("trn_types", |o| {
                array(o, Some(types), |o, t| {
                    Obj::new(o)
                        .str("trn_type", &t.trn_type)
                        .str_omit("description", &t.description)
                        .end();
                })
            })
            .end();
        self.call(Method::Post, "/trn-types", Some(&body), deadline)
            .map(|_| ())
    }

    /// `POST /poll`.
    pub fn poll(&self, deadline: Instant) -> Result<Vec<WritebackCommand>, ApiError> {
        let resp = self.call(Method::Post, "/poll", None, deadline)?;
        Self::parse_commands(&resp)
    }

    /// `POST /poll?wait=N` (MMEI RT-0785): the hub holds the request up to `wait` while
    /// no command is pending. The flag is true when the hub honoured the wait (its answer
    /// carries `X-MMEI-Long-Poll`); a hub built before it answers at once without it.
    pub fn poll_wait(
        &self,
        wait: Duration,
        deadline: Instant,
    ) -> Result<(Vec<WritebackCommand>, bool), ApiError> {
        let path = format!("/poll?wait={}", wait.as_secs());
        let resp = self.call_resp(Method::Post, &path, None, deadline)?;
        let long_poll = resp.headers.get(LONG_POLL_HEADER).is_some();
        Ok((Self::parse_commands(&resp.body)?, long_poll))
    }

    fn parse_commands(resp: &[u8]) -> Result<Vec<WritebackCommand>, ApiError> {
        let v = Self::parse(resp, "poll")?;
        let mut out = Vec::new();
        for c in v
            .get("commands")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let id = s(c, "command_id").parse::<Uuid>().map_err(|e| {
                ApiError::Other(format!("agent: decode poll response: command_id: {e:?}"))
            })?;
            out.push(WritebackCommand {
                id,
                kind: s(c, "type"),
                source_ref: s(c, "source_ref"),
                invoice_number: s(c, "invoice_number"),
                ky_hieu: s(c, "ky_hieu"),
                ma_cqt: s(c, "ma_cqt"),
                issued_at: parse_time(c.get("issued_at")),
            });
        }
        Ok(out)
    }

    /// `POST /writeback:ack`.
    pub fn ack(&self, ids: &[Uuid], deadline: Instant) -> Result<(), ApiError> {
        let mut body = String::new();
        Obj::new(&mut body)
            .raw("command_ids", |o| {
                array(o, Some(ids), |o, id| {
                    crate::gojson::string(o, &id.to_string())
                })
            })
            .end();
        self.call(Method::Post, "/writeback:ack", Some(&body), deadline)
            .map(|_| ())
    }

    /// `POST /heartbeat`: one attempt, 10 s, no retry.
    pub fn heartbeat(&self, hb: &Heartbeat) -> Result<(), ApiError> {
        let mut body = String::new();
        Obj::new(&mut body)
            .str("agent_version", &hb.agent_version)
            .str("device_name", &hb.device_name)
            .raw("cycle", |o| {
                Obj::new(o)
                    .int("read", hb.read)
                    .int("pushed", hb.pushed)
                    .int("errored", hb.errored)
                    .end();
            })
            .str("last_error", &hb.last_error)
            .raw("logs", |o| {
                let logs = if hb.logs.is_empty() {
                    None
                } else {
                    Some(hb.logs.as_slice())
                };
                array(o, logs, |o, l| {
                    Obj::new(o)
                        .str("ts", &l.ts.to_rfc3339())
                        .str("level", &l.level)
                        .str("msg", &l.msg)
                        .end();
                })
            })
            .end();
        let req = self.request(Method::Post, "/heartbeat", Some(&body), HEARTBEAT_TIMEOUT)?;
        let resp = self
            .http
            .request(&req)
            .map_err(|e| ApiError::Other(format!("agent: heartbeat request failed: {e}")))?;
        if !resp.is_success() {
            return Err(ApiError::Other(format!(
                "agent: heartbeat: status {}: {}",
                resp.status.as_u16(),
                String::from_utf8_lossy(&resp.body)
            )));
        }
        Ok(())
    }

    /// `relayCall`: one POST without retry (the relay loop is its own retry). Returns the
    /// status and at most `2 * MAX_REQUEST_BODY` bytes of the body.
    fn relay_call(&self, path: &str, payload: Option<&str>) -> Result<(u16, Vec<u8>), ApiError> {
        let req = self.request(Method::Post, path, payload, RELAY_TIMEOUT)?;
        let resp = self
            .http
            .request(&req)
            .map_err(|e| ApiError::Other(format!("agent: relay request failed: {e}")))?;
        let mut body = resp.body;
        body.truncate(MAX_REQUEST_BODY * 2);
        Ok((resp.status.as_u16(), body))
    }

    /// `RelayNext`: one long poll of the hub for a relayed call.
    pub fn relay_next(&self) -> Result<Poll, ApiError> {
        let (code, body) = self.relay_call("/relay/next", None)?;
        match code {
            204 => Ok(Poll::Empty),
            200 => RelayRequest::from_json(&body)
                .map(Poll::Call)
                .map_err(|e| ApiError::Other(format!("agent: decode relay request: {e}"))),
            404 | 405 => Ok(Poll::Unsupported),
            _ if is_device_replaced(code, &body) => Err(ApiError::DeviceReplaced),
            _ => Err(ApiError::Other(format!("agent: relay poll: status {code}"))),
        }
    }

    /// `RelayResult`: posts the answer of call `id`. 410 (the hub's caller gave up) is fine.
    pub fn relay_result(&self, id: &str, res: &RelayResult) -> Result<(), ApiError> {
        let path = format!("/relay/{}/result", dl_http::encode_path_segment(id));
        let (code, _) = self.relay_call(&path, Some(&res.to_json()))?;
        if code == 204 || code == 410 {
            return Ok(());
        }
        Err(ApiError::Other(format!(
            "agent: relay result: status {code}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_matches_go() {
        // retry.Delay(200ms << (n-1), cap 5s) × jitter[n % 5].
        let ms = |n| backoff(n).as_millis();
        assert_eq!(
            [ms(1), ms(2), ms(3), ms(4), ms(5), ms(6)],
            [150, 360, 480, 1360, 3200, 3750]
        );
    }

    #[test]
    fn device_replaced_needs_401_and_code() {
        assert!(is_device_replaced(
            401,
            br#"{"error":{"code":"device_replaced"}}"#
        ));
        assert!(!is_device_replaced(
            401,
            br#"{"error":{"code":"invalid_key"}}"#
        ));
        assert!(!is_device_replaced(
            403,
            br#"{"error":{"code":"device_replaced"}}"#
        ));
        assert!(!is_device_replaced(401, b"not json"));
    }

    #[test]
    fn base_url_gets_the_prefix_once() {
        let id = Uuid::NIL;
        assert_eq!(
            Platform::new("https://h:5080/", "k", id).base,
            "https://h:5080/api/v1/agent"
        );
        assert_eq!(
            Platform::new("https://h/api/v1/agent", "k", id).base,
            "https://h/api/v1/agent"
        );
    }
}
