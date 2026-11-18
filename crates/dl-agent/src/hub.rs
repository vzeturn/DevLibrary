//! Signed calls to the hub (PROTOCOL.md §1–§2), with fallback hosts and clock-skew repair.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use dl_json::{Value, json};

use crate::command::{Command, CommandResult};
use crate::config::RemoteConfig;
use crate::identity::{Identity, public_key_b64};
use crate::report::ProblemReport;
use crate::{Error, now};

/// Sends one HTTP request. The runtime uses [`HttpTransport`]; tests use a fake.
pub trait Transport: Send + Sync {
    /// Sends `method url` with the headers and body; returns status and body. Errors are
    /// transport failures only (a 4xx/5xx answer is `Ok`).
    fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), Error>;
}

/// [`Transport`] over `dl_http::Client` (not available on `wasm32-unknown-unknown`).
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub struct HttpTransport {
    client: dl_http::Client,
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
impl HttpTransport {
    /// A client with agent timeouts and a 200 MiB body limit (update artifacts).
    pub fn new() -> HttpTransport {
        let options = dl_http::ClientOptions {
            timeout: Some(std::time::Duration::from_secs(120)),
            connect_timeout: Some(std::time::Duration::from_secs(10)),
            max_body: 200 * 1024 * 1024,
            user_agent: Some(format!("dl-agent/{}", crate::agent_version())),
            max_redirects: 3,
            ..dl_http::ClientOptions::default()
        };
        HttpTransport {
            client: dl_http::Client::with_options(options),
        }
    }
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
impl Default for HttpTransport {
    fn default() -> HttpTransport {
        HttpTransport::new()
    }
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
impl Transport for HttpTransport {
    fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), Error> {
        let m = match method {
            "GET" => dl_http::Method::Get,
            "POST" => dl_http::Method::Post,
            other => return Err(Error::Transport(format!("unsupported method {other}"))),
        };
        let url = dl_http::Url::parse(url).map_err(|e| Error::Transport(e.to_string()))?;
        let mut req = dl_http::Request::new(m, url);
        for (k, v) in headers {
            req = req
                .with_header(k, v)
                .map_err(|e| Error::Transport(e.to_string()))?;
        }
        if method == "POST" {
            req = req
                .with_header("Content-Type", "application/json")
                .map_err(|e| Error::Transport(e.to_string()))?
                .with_body(body.to_vec());
        }
        let resp = self
            .client
            .request(&req)
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok((resp.status.as_u16(), resp.body))
    }
}

/// The hub's error code (`{"error":"revoked"}`) of a failed call.
pub fn error_code(e: &Error) -> Option<String> {
    match e {
        Error::Status { body, .. } => dl_json::parse(body)
            .ok()?
            .get("error")?
            .as_str()
            .map(str::to_string),
        _ => None,
    }
}

/// What the hub answered to an enrolment.
#[derive(Debug)]
pub struct Enrolled {
    /// The new identity (save it).
    pub identity: Identity,
    /// Assigned channel.
    pub channel: String,
    /// First configuration.
    pub config: Option<RemoteConfig>,
}

/// What the hub answered to a heartbeat.
#[derive(Clone, Debug, PartialEq)]
pub struct HeartbeatAnswer {
    /// Hub clock.
    pub server_time: u64,
    /// Channel the hub assigns.
    pub channel: String,
    /// Next heartbeat interval.
    pub heartbeat_seconds: u64,
    /// Newer configuration, if any.
    pub config: Option<RemoteConfig>,
    /// Commands to run.
    pub commands: Vec<Command>,
    /// A newer release exists on the agent's channel.
    pub update_check: bool,
}

impl HeartbeatAnswer {
    fn from_json(v: &Value) -> Result<HeartbeatAnswer, Error> {
        let config = match v.get("config") {
            Some(c) if !c.is_null() => Some(RemoteConfig::from_json(c)?),
            _ => None,
        };
        Ok(HeartbeatAnswer {
            server_time: v.get("server_time").and_then(Value::as_u64).unwrap_or(0),
            channel: v
                .get("channel")
                .and_then(Value::as_str)
                .unwrap_or("stable")
                .to_string(),
            heartbeat_seconds: v
                .get("heartbeat_seconds")
                .and_then(Value::as_u64)
                .unwrap_or(60)
                .clamp(10, 3600),
            config,
            commands: v
                .get("commands")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Command::from_json).collect())
                .unwrap_or_default(),
            update_check: v
                .get("update_check")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

fn truncate(s: &[u8]) -> String {
    let text = String::from_utf8_lossy(s);
    text.chars().take(500).collect()
}

fn join(base: &str, path: &str) -> String {
    format!("{}{path}", base.trim_end_matches('/'))
}

/// The path and query a request signature covers.
fn path_and_query(url: &str) -> Result<String, Error> {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let start = after_scheme
        .find('/')
        .ok_or_else(|| Error::Config(format!("URL without path: {url}")))?;
    let pq = &after_scheme[start..];
    Ok(pq.split('#').next().unwrap_or(pq).to_string())
}

/// Calls the hub as one enrolled agent.
pub struct HubClient {
    transport: Box<dyn Transport>,
    identity: Identity,
    preferred: AtomicUsize,
    skew: AtomicI64,
}

impl HubClient {
    /// A client for an enrolled agent.
    pub fn new(transport: Box<dyn Transport>, identity: Identity) -> HubClient {
        HubClient {
            transport,
            identity,
            preferred: AtomicUsize::new(0),
            skew: AtomicI64::new(0),
        }
    }

    /// The identity requests are signed with.
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Hub clock minus local clock, learnt from `clock_skew` answers and heartbeats.
    pub fn skew(&self) -> i64 {
        self.skew.load(Ordering::Relaxed)
    }

    /// Records the hub clock (from a heartbeat answer).
    pub fn observe_server_time(&self, server_time: u64) {
        if server_time > 0 {
            self.skew
                .store(server_time as i64 - now() as i64, Ordering::Relaxed);
        }
    }

    fn hub_now(&self) -> u64 {
        (now() as i64 + self.skew()).max(0) as u64
    }

    fn signed_once(&self, method: &str, url: &str, body: &[u8]) -> Result<(u16, Vec<u8>), Error> {
        let headers = self
            .identity
            .sign(method, &path_and_query(url)?, body, self.hub_now())?;
        self.transport.send(method, url, &headers, body)
    }

    /// One signed call to `url`, retried once after a `clock_skew` answer.
    fn signed_url(&self, method: &str, url: &str, body: &[u8]) -> Result<Vec<u8>, Error> {
        let (mut code, mut resp) = self.signed_once(method, url, body)?;
        if code == 401
            && let Ok(v) = dl_json::parse_slice(&resp)
            && v.get("error").and_then(Value::as_str) == Some("clock_skew")
            && let Some(t) = v.get("server_time").and_then(Value::as_u64)
        {
            self.observe_server_time(t);
            (code, resp) = self.signed_once(method, url, body)?;
        }
        if (200..300).contains(&code) {
            Ok(resp)
        } else {
            Err(Error::Status {
                code,
                body: truncate(&resp),
            })
        }
    }

    /// A signed call to `path` on the first hub that answers; fallback hosts are tried on
    /// transport errors and 5xx answers, and the one that worked is used first next time.
    fn call(&self, method: &str, path: &str, body: &[u8]) -> Result<Vec<u8>, Error> {
        let urls = &self.identity.hub_urls;
        let first = self.preferred.load(Ordering::Relaxed) % urls.len().max(1);
        let mut last = Error::Config("no hub URL".into());
        for i in 0..urls.len() {
            let idx = (first + i) % urls.len();
            match self.signed_url(method, &join(&urls[idx], path), body) {
                Ok(r) => {
                    self.preferred.store(idx, Ordering::Relaxed);
                    return Ok(r);
                }
                Err(
                    e @ (Error::Transport(_)
                    | Error::Status {
                        code: 500..=599, ..
                    }),
                ) => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value, Error> {
        let resp = self.call("POST", path, dl_json::to_string(body).as_bytes())?;
        Ok(dl_json::parse_slice(&resp)?)
    }

    /// `POST /v1/heartbeat`.
    pub fn heartbeat(&self, body: &Value) -> Result<HeartbeatAnswer, Error> {
        let a = HeartbeatAnswer::from_json(&self.post("/v1/heartbeat", body)?)?;
        self.observe_server_time(a.server_time);
        Ok(a)
    }

    /// `POST /v1/reports`; returns how many the hub accepted.
    pub fn reports(&self, reports: &[ProblemReport], hostname: &str) -> Result<u64, Error> {
        let version = crate::agent_version().to_string();
        let items: Vec<Value> = reports
            .iter()
            .map(|r| r.to_json(&version, std::env::consts::OS, hostname))
            .collect();
        let v = self.post("/v1/reports", &json!({"reports": items}))?;
        Ok(v.get("accepted").and_then(Value::as_u64).unwrap_or(0))
    }

    /// `POST /v1/commands/{id}/result`.
    pub fn command_result(&self, r: &CommandResult) -> Result<(), Error> {
        let id = dl_http::encode_path_segment(&r.id);
        self.post(&format!("/v1/commands/{id}/result"), &r.body())
            .map(|_| ())
    }

    /// `GET /v1/releases/{product}/{channel}`: the manifest text, `None` when there is no
    /// release.
    pub fn manifest(&self, product: &str, channel: &str) -> Result<Option<String>, Error> {
        let path = format!(
            "/v1/releases/{}/{}",
            dl_http::encode_path_segment(product),
            dl_http::encode_path_segment(channel)
        );
        match self.call("GET", &path, b"") {
            Ok(b) => String::from_utf8(b)
                .map(Some)
                .map_err(|_| Error::Manifest("manifest is not UTF-8".into())),
            Err(Error::Status { code: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Downloads an artifact. URLs on one of the agent's hubs are signed (the hub serves
    /// artifacts to enrolled agents only); other URLs are fetched as they are. Integrity
    /// comes from the signed manifest either way; `no-cache` keeps a CDN from serving a stale
    /// copy that would then fail the hash check.
    pub fn download(&self, url: &str) -> Result<Vec<u8>, Error> {
        let on_hub = self
            .identity
            .hub_urls
            .iter()
            .any(|h| url.starts_with(&format!("{}/", h.trim_end_matches('/'))));
        if on_hub {
            return self.signed_url("GET", url, b"");
        }
        let headers = [("Cache-Control", "no-cache".to_string())];
        let (code, body) = self.transport.send("GET", url, &headers, b"")?;
        if (200..300).contains(&code) {
            Ok(body)
        } else {
            Err(Error::Status {
                code,
                body: truncate(&body),
            })
        }
    }
}

/// Enrols this machine with a one-time code (`POST /v1/enroll`, unsigned) and returns the
/// identity to save. Hub URLs are tried in order.
pub fn enroll(
    transport: &dyn Transport,
    hub_urls: &[String],
    code: &str,
    modules: &[String],
) -> Result<Enrolled, Error> {
    let key = crate::identity::generate_key()?;
    let body = json!({
        "code": code, "public_key": public_key_b64(&key), "hostname": hostname(),
        "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
        "version": crate::agent_version().to_string(), "modules": modules.to_vec(),
    });
    let bytes = dl_json::to_string(&body);
    let mut last = Error::Config("no hub URL".into());
    for base in hub_urls {
        match transport.send("POST", &join(base, "/v1/enroll"), &[], bytes.as_bytes()) {
            Ok((code, resp)) if (200..300).contains(&code) => {
                let v = dl_json::parse_slice(&resp)?;
                let s = |k: &str| {
                    v.get(k)
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .ok_or_else(|| Error::Json(format!("enroll answer without {k}")))
                };
                let identity = Identity::new(
                    &s("agent_id")?,
                    &s("tenant")?,
                    &s("site")?,
                    hub_urls.to_vec(),
                    key,
                );
                let config = match v.get("config") {
                    Some(c) if c.get("version").and_then(Value::as_u64).unwrap_or(0) > 0 => {
                        Some(RemoteConfig::from_json(c)?)
                    }
                    _ => None,
                };
                return Ok(Enrolled {
                    identity,
                    channel: s("channel").unwrap_or_else(|_| "stable".into()),
                    config,
                });
            }
            Ok((code, resp)) if code < 500 => {
                return Err(Error::Status {
                    code,
                    body: truncate(&resp),
                });
            }
            Ok((code, resp)) => {
                last = Error::Status {
                    code,
                    body: truncate(&resp),
                }
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// This machine's name (`COMPUTERNAME` / `HOSTNAME` / `/etc/hostname`).
pub fn hostname() -> String {
    for var in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(h) = std::env::var(var)
            && !h.trim().is_empty()
        {
            return h.trim().to_string();
        }
    }
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
#[path = "hub_tests.rs"]
pub(crate) mod tests_support;
