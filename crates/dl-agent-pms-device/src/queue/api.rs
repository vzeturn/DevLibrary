//! `Queue/ApiCommandQueue.cs`: the queue over the API's agent routes. The agent holds no DB
//! credentials; the API derives the workstation from the token, so requests carry none.

use std::sync::Arc;
use std::time::Duration;

use dl_http::{Client, ClientOptions, Request, Response, Url};
use dl_json::{ToJson, Value};

use super::{CommandQueue, DeviceApiError, QueueError, StaleClaim, StaleClaimReason};
use crate::agent::{Clock, TokenFile};
use crate::config::{AgentOptions, DotnetError};
use crate::contracts::{ClaimResponse, DeviceCommand, PendingResult, parse_json, to_dotnet_json};

/// `ApiCommandQueue.AuthScheme`: `Authorization: DeviceAgent <token>`.
pub const AUTH_SCHEME: &str = "DeviceAgent";
/// `ApiCommandQueue.AgentIdHeader`.
pub const AGENT_ID_HEADER: &str = "X-Device-Agent-Id";
/// `ApiCommandQueue.AdapterHeader`: the configured `Agent:Adapter`, sent on every call (the
/// API reports a workstation's encoder as ready only while its agent polls with an adapter
/// other than `none`).
pub const ADAPTER_HEADER: &str = "X-Device-Adapter";
/// The HttpClient timeout of the C# host (`c.Timeout = 15 s`), for every call.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

const JSON_CONTENT_TYPE: &str = "application/json; charset=utf-8";
const MAX_DETAIL: usize = 300;

/// [`CommandQueue`] over `POST {ApiBaseUrl}/device-agent/commands/claim` and
/// `POST {ApiBaseUrl}/device-agent/commands/{id}/result`, through one pooled dl-http client.
pub struct ApiCommandQueue {
    client: Client,
    api_base_url: String,
    agent_id: String,
    adapter: String,
    token: Arc<TokenFile>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for ApiCommandQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiCommandQueue")
            .field("api_base_url", &self.api_base_url)
            .field("agent_id", &self.agent_id)
            .field("adapter", &self.adapter)
            .finish_non_exhaustive()
    }
}

impl ApiCommandQueue {
    /// A queue for `options` (ApiBaseUrl, AgentId, Adapter). Like the .NET HttpClient it
    /// sends no `User-Agent`, times out after [`HTTP_TIMEOUT`] and takes the proxy from
    /// `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY`.
    pub fn new(options: &AgentOptions, token: Arc<TokenFile>, clock: Arc<dyn Clock>) -> Self {
        let client = Client::with_options(ClientOptions {
            timeout: Some(HTTP_TIMEOUT),
            connect_timeout: None,
            user_agent: None,
            ..ClientOptions::default()
        });
        ApiCommandQueue {
            client,
            api_base_url: options.api_base_url.clone(),
            agent_id: options.agent_id.clone(),
            adapter: options.adapter.clone(),
            token,
            clock,
        }
    }

    fn new_request(&self, path: &str, body: Option<Vec<u8>>) -> Result<Request, QueueError> {
        let base = format!("{}/", self.api_base_url.trim_end_matches('/'));
        let url = Url::parse(&base).and_then(|b| b.join(path)).map_err(|e| {
            QueueError::Other(DotnetError::new(
                "System.UriFormatException",
                format!("Invalid URI: {e}"),
            ))
        })?;
        let token = self
            .token
            .read()
            .map_err(|e| QueueError::Other(e.to_dotnet()))?;
        let has_body = body.is_some();
        let mut request = Request::post(url, body.unwrap_or_default());
        let mut headers = vec![
            ("Authorization", format!("{AUTH_SCHEME} {token}")),
            (AGENT_ID_HEADER, self.agent_id.clone()),
            (ADAPTER_HEADER, self.adapter.clone()),
        ];
        if has_body {
            headers.push(("Content-Type", JSON_CONTENT_TYPE.to_owned()));
        }
        for (name, value) in headers {
            // The error names the header, never its value (the token).
            request.headers.append(name, &value).map_err(|_| {
                QueueError::Other(DotnetError::new(
                    "System.FormatException",
                    format!("The format of value for header '{name}' is invalid."),
                ))
            })?;
        }
        Ok(request)
    }

    fn send(&self, request: &Request) -> Result<Response, QueueError> {
        self.client
            .request(request)
            .map_err(|e| transport_error(e, &request.url))
    }
}

/// The failure as `SocketsHttpHandler` reports it. A refused or unreachable connect is
/// `HttpRequestException("<OS text> (host:port)")` with the `SocketException` inside, as
/// in .NET; other failures keep the dl-http text.
pub(crate) fn transport_error(e: dl_http::Error, url: &Url) -> QueueError {
    match e {
        dl_http::Error::Timeout => QueueError::Timeout,
        dl_http::Error::Io(io) if is_connect_failure(&io) => {
            let text = os_text(&io);
            QueueError::Transport(super::TransportError {
                message: format!("{text} ({}:{})", url.host(), url.port()),
                socket: io.raw_os_error().map(|code| (code, text)),
            })
        }
        e => QueueError::Transport(e.to_string().into()),
    }
}

fn is_connect_failure(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::{
        AddrNotAvailable, ConnectionRefused, HostUnreachable, NetworkUnreachable,
    };
    matches!(
        e.kind(),
        ConnectionRefused | HostUnreachable | NetworkUnreachable | AddrNotAvailable
    )
}

/// The OS message without Rust's ` (os error N)` suffix: the `FormatMessage` / `strerror`
/// text .NET puts in `SocketException.Message`.
fn os_text(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(i) if s.ends_with(')') => s[..i].to_owned(),
        _ => s,
    }
}

impl CommandQueue for ApiCommandQueue {
    fn claim(&self) -> Result<Option<DeviceCommand>, QueueError> {
        let request = self.new_request("device-agent/commands/claim", None)?;
        // The lease is measured from before the request went out, so network time only
        // shortens it.
        let sent_at = self.clock.now();
        let response = self.send(&request)?;
        match response.status.as_u16() {
            204 => return Ok(None),
            200 => {}
            _ => return Err(api_error(&response, "claim")),
        }
        let body = parse_body(&response.body)?;
        let claim = ClaimResponse::from_json(&body)
            .map_err(|e| QueueError::Other(e.to_dotnet()))?
            .ok_or_else(|| {
                QueueError::Api(DeviceApiError::new(
                    200,
                    "Device agent API claim returned an empty body",
                ))
            })?;
        claim
            .to_command(sent_at)
            .map(Some)
            .map_err(QueueError::Other)
    }

    fn send_result(&self, result: &PendingResult) -> Result<(), QueueError> {
        let body = to_dotnet_json(&result.body.to_json()).into_bytes();
        let path = format!("device-agent/commands/{}/result", result.command_id);
        let request = self.new_request(&path, Some(body))?;
        let response = self.send(&request)?;
        let stale = |reason| {
            Err(QueueError::StaleClaim(StaleClaim {
                command_id: result.command_id,
                reason,
            }))
        };
        match response.status.as_u16() {
            s if (200..300).contains(&s) => Ok(()),
            404 => stale(StaleClaimReason::Missing),
            // The domain code starts the problem detail (the API's error filter sets `code`
            // from the status).
            409 if read_detail(&response).starts_with("DEVICE_OUTCOME_CONFLICT") => {
                stale(StaleClaimReason::OutcomeConflict)
            }
            409 => stale(StaleClaimReason::NotHeld),
            _ => Err(api_error(&response, "result")),
        }
    }
}

fn json_exception(message: &str) -> DotnetError {
    DotnetError::new("System.Text.Json.JsonException", message)
}

/// `ReadFromJsonAsync`: the body as JSON, or the JsonException.
fn parse_body(body: &[u8]) -> Result<Value, QueueError> {
    if body.is_empty() {
        return Err(QueueError::Other(json_exception(
            "The input does not contain any JSON tokens. Expected the input to start with a valid JSON token, while the input is empty. Path: $ | LineNumber: 0 | BytePositionInLine: 0.",
        )));
    }
    parse_json(body).map_err(|e| QueueError::Other(json_exception(&e.to_string())))
}

fn api_error(response: &Response, what: &str) -> QueueError {
    let status = response.status.as_u16();
    let detail = read_detail(response);
    QueueError::Api(DeviceApiError::new(
        status,
        format!("Device agent API {what} returned {status}: {detail}"),
    ))
}

/// Problem-details text (`detail`, else `title`, else `message`), else the raw body,
/// trimmed and capped at 300 UTF-16 units.
fn read_detail(response: &Response) -> String {
    let raw = String::from_utf8_lossy(&response.body);
    let mut text: &str = &raw;
    let parsed = parse_json(raw.as_bytes()).ok();
    if let Some(Value::Object(map)) = &parsed {
        for key in ["detail", "title", "message"] {
            if let Some(Value::String(s)) = map.get(key) {
                text = s;
                break;
            }
        }
    }
    let text = text.trim_matches(char::is_whitespace);
    let mut units = 0;
    for (i, c) in text.char_indices() {
        units += c.len_utf16();
        if units > MAX_DETAIL {
            return text[..i].to_owned();
        }
    }
    text.to_owned()
}
