//! The cloud's agent HTTP endpoints:
//!
//! ```text
//! POST /agent/v1/enroll  {code, publicKey, hostInfo} -> 201 {agentId, siteId, tenantId}
//! POST /agent/v1/token   signed request              -> 200 {token, expiresAt, ...}
//! ```
//!
//! A token request is signed with the agent's Ed25519 key over
//! `METHOD|path|ts|nonce|sha256hex(body)`, sent in the `x-agent-*` headers. Codes, keys,
//! signatures and tokens never reach logs or errors.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dl_ed25519::SigningKey;
use dl_http::{Method, Request, Url};
use dl_json::Value;
use dl_time::OffsetDateTime;

use crate::ctx::Ctx;
use crate::json::{self, Obj};
use crate::wire;

/// Request auth headers (packages/control-plane/src/agent-auth.ts).
pub const HEADER_AGENT_ID: &str = "x-agent-id";
/// See [`HEADER_AGENT_ID`].
pub const HEADER_TIMESTAMP: &str = "x-agent-timestamp";
/// See [`HEADER_AGENT_ID`].
pub const HEADER_NONCE: &str = "x-agent-nonce";
/// See [`HEADER_AGENT_ID`].
pub const HEADER_SIGNATURE: &str = "x-agent-signature";

const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// How long before expiry a cached token is replaced (the cloud's TTL is 15 min).
pub const REFRESH_BEFORE: Duration = Duration::from_secs(120);

/// An enroll or token failure. `Display` never includes a response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollError {
    /// 401: a bad or used code at enrollment, a revoked or unknown agent at token.
    Refused,
    /// 429, with the `Retry-After` seconds when given.
    RateLimited {
        /// How long to wait.
        retry_after: Option<Duration>,
    },
    /// Another non-success status.
    Status(u16),
    /// Network or protocol failure (transient).
    Transport(String),
    /// The answer did not have the expected shape.
    BadResponse(String),
}

impl EnrollError {
    /// The `Retry-After` of a 429.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            EnrollError::RateLimited { retry_after } => *retry_after,
            _ => None,
        }
    }
}

impl fmt::Display for EnrollError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnrollError::Refused => f.write_str("cloud answered HTTP 401"),
            EnrollError::RateLimited { .. } => f.write_str("cloud answered HTTP 429"),
            EnrollError::Status(s) => write!(f, "cloud answered HTTP {s}"),
            EnrollError::Transport(m) | EnrollError::BadResponse(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for EnrollError {}

/// What the console shows about the machine (all optional, omitted when empty).
#[derive(Debug, Clone, Default)]
pub struct HostInfo {
    /// Host name.
    pub hostname: String,
    /// `os/arch`, e.g. `windows/amd64`.
    pub os: String,
    /// Agent build.
    pub agent_version: String,
}

/// The identity the cloud assigned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrolled {
    /// Agent id.
    pub agent_id: String,
    /// Site id.
    pub site_id: String,
    /// Tenant id.
    pub tenant_id: String,
}

/// A short-lived agent JWT.
#[derive(Debug, Clone)]
pub struct Token {
    /// The JWT.
    pub value: String,
    /// When it lapses.
    pub expires_at: OffsetDateTime,
}

/// Caps an enroll or token answer (small JSON documents).
const MAX_HTTP_BODY: usize = 1 << 20;

/// The shared HTTP client: HTTP/1.1, OS TLS, `HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY`,
/// 30 s per request, bodies up to [`MAX_HTTP_BODY`].
pub fn new_http_client() -> Arc<dl_http::Client> {
    Arc::new(dl_http::Client::with_options(dl_http::ClientOptions {
        timeout: Some(Duration::from_secs(30)),
        connect_timeout: Some(Duration::from_secs(10)),
        max_body: MAX_HTTP_BODY,
        user_agent: Some(format!("dl-agent/{}", dl_agent::agent_version())),
        ..dl_http::ClientOptions::default()
    }))
}

/// Calls the agent endpoints on one tenant host.
pub struct Client {
    /// e.g. `https://acme.pms.example.com`.
    pub base_url: String,
    /// The HTTP client.
    pub http: Arc<dl_http::Client>,
    /// The agent key.
    pub key: SigningKey,
    /// The clock (tests).
    pub now: fn() -> OffsetDateTime,
}

/// The raw public key, standard base64, as enrollment expects.
pub fn public_key_base64(key: &SigningKey) -> String {
    dl_encoding::base64::STANDARD.encode(key.verifying_key().as_bytes())
}

/// `METHOD|path|ts|nonce|sha256hex(body)`, path being the request target as sent.
pub fn canonical_request(method: &str, path: &str, ts: &str, nonce: &str, body: &[u8]) -> String {
    format!(
        "{}|{path}|{ts}|{nonce}|{}",
        method.to_uppercase(),
        crate::sha256_hex(body)
    )
}

/// The `x-agent-*` headers for one request, with a fresh 24-byte nonce.
pub fn sign_request(
    key: &SigningKey,
    agent_id: &str,
    method: &str,
    path: &str,
    body: &[u8],
    now: OffsetDateTime,
) -> Result<Vec<(&'static str, String)>, EnrollError> {
    let nonce: [u8; 24] =
        dl_rand::secure_bytes().map_err(|e| EnrollError::Transport(format!("nonce: {e}")))?;
    Ok(sign_request_with_nonce(
        key, agent_id, method, path, body, now, &nonce,
    ))
}

/// [`sign_request`] with a given nonce (tests compare it with the Go agent).
pub fn sign_request_with_nonce(
    key: &SigningKey,
    agent_id: &str,
    method: &str,
    path: &str,
    body: &[u8],
    now: OffsetDateTime,
    nonce: &[u8],
) -> Vec<(&'static str, String)> {
    let nonce = dl_encoding::base64::URL_SAFE_NO_PAD.encode(nonce);
    let ts = (now.unix_nanos().div_euclid(1_000_000_000)).to_string();
    let sig = key.sign(canonical_request(method, path, &ts, &nonce, body).as_bytes());
    vec![
        (HEADER_AGENT_ID, agent_id.to_owned()),
        (HEADER_TIMESTAMP, ts),
        (HEADER_NONCE, nonce),
        (HEADER_SIGNATURE, dl_encoding::base64::STANDARD.encode(sig)),
    ]
}

/// The request target (path and query) of `url`, like Go's `URL.RequestURI`.
pub fn request_uri(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = &rest[..rest.find('#').unwrap_or(rest.len())];
    match rest.find(['/', '?']) {
        Some(i) if rest.as_bytes()[i] == b'/' => rest[i..].to_owned(),
        Some(i) => format!("/{}", &rest[i..]),
        None => "/".to_owned(),
    }
}

fn str_field(v: &Value, name: &str) -> Result<String, EnrollError> {
    let Value::Object(o) = v else {
        return Err(EnrollError::BadResponse(
            "decode response: not an object".into(),
        ));
    };
    match json::field_ci(o, name) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(EnrollError::BadResponse(format!(
            "decode response: {name} is not a string"
        ))),
    }
}

impl Client {
    fn post(
        &self,
        ctx: &Ctx,
        path: &str,
        body: Vec<u8>,
        headers: &[(&'static str, String)],
        want: u16,
    ) -> Result<Value, EnrollError> {
        if let Some(d) = ctx.err() {
            return Err(EnrollError::Transport(format!("request: {d}")));
        }
        let url = Url::parse(&format!("{}{path}", self.base_url))
            .map_err(|e| EnrollError::Transport(format!("cloud url: {e}")))?;
        let mut req = Request::new(Method::Post, url).with_body(body);
        let bad = |e: dl_http::Error| EnrollError::Transport(format!("build request: {e}"));
        for (k, v) in headers {
            req = req.with_header(k, v).map_err(bad)?;
        }
        req = req
            .with_header("Content-Type", "application/json")
            .map_err(bad)?
            .with_header("Accept", "application/json")
            .map_err(bad)?;
        if let Some(left) = ctx.remaining() {
            req = req.with_timeout(left.min(Duration::from_secs(30)));
        }
        let resp = self
            .http
            .request(&req)
            .map_err(|e| EnrollError::Transport(format!("request: {e}")))?;
        let status = resp.status.as_u16();
        if status != want {
            return Err(match status {
                401 => EnrollError::Refused,
                429 => EnrollError::RateLimited {
                    retry_after: resp
                        .headers
                        .get("Retry-After")
                        .and_then(|s| s.trim().parse::<u64>().ok())
                        .filter(|s| *s > 0)
                        .map(Duration::from_secs),
                },
                s => EnrollError::Status(s),
            });
        }
        let body = &resp.body[..resp.body.len().min(MAX_RESPONSE_BYTES)];
        json::parse(body).map_err(|e| EnrollError::BadResponse(format!("decode response: {e}")))
    }

    /// Redeems a one-time code for this agent's public key.
    pub fn enroll(&self, ctx: &Ctx, code: &str, host: &HostInfo) -> Result<Enrolled, EnrollError> {
        let mut hi = Obj::new();
        if !host.hostname.is_empty() {
            hi = hi.str("hostname", &host.hostname);
        }
        if !host.os.is_empty() {
            hi = hi.str("os", &host.os);
        }
        if !host.agent_version.is_empty() {
            hi = hi.str("agentVersion", &host.agent_version);
        }
        let body = Obj::new()
            .str("code", code.trim())
            .str("publicKey", &public_key_base64(&self.key))
            .raw("hostInfo", &hi.finish())
            .finish();
        let v = self.post(ctx, wire::ENROLL_PATH, body.into_bytes(), &[], 201)?;
        let out = Enrolled {
            agent_id: str_field(&v, "agentId")?,
            site_id: str_field(&v, "siteId")?,
            tenant_id: str_field(&v, "tenantId")?,
        };
        if out.agent_id.is_empty() || out.site_id.is_empty() {
            return Err(EnrollError::BadResponse(
                "enroll: response is missing agentId or siteId".into(),
            ));
        }
        Ok(out)
    }

    /// Exchanges a signed request for a JWT.
    pub fn token(&self, ctx: &Ctx, agent_id: &str) -> Result<Token, EnrollError> {
        let body = b"{}".to_vec();
        let target = request_uri(&format!("{}{}", self.base_url, wire::TOKEN_PATH));
        let headers = sign_request(&self.key, agent_id, "POST", &target, &body, (self.now)())?;
        let v = self.post(ctx, wire::TOKEN_PATH, body, &headers, 200)?;
        let token = str_field(&v, "token")?;
        let exp = OffsetDateTime::parse_rfc3339(&str_field(&v, "expiresAt")?);
        match exp {
            Ok(expires_at) if !token.is_empty() => Ok(Token {
                value: token,
                expires_at,
            }),
            _ => Err(EnrollError::BadResponse(
                "token: response is missing token or expiresAt".into(),
            )),
        }
    }
}

/// Hands out a valid JWT, fetching a new one before the cached one expires.
pub trait TokenProvider: Send + Sync {
    /// A token valid for at least [`REFRESH_BEFORE`].
    fn token(&self, ctx: &Ctx) -> Result<String, EnrollError>;
    /// Drops the cached token (the stream refused it).
    fn invalidate(&self);
}

/// The [`TokenProvider`] over [`Client::token`].
pub struct TokenSource {
    client: Arc<Client>,
    agent_id: String,
    tok: Mutex<Option<Token>>,
}

impl TokenSource {
    /// A source for `agent_id`.
    pub fn new(client: Arc<Client>, agent_id: &str) -> TokenSource {
        TokenSource {
            client,
            agent_id: agent_id.to_owned(),
            tok: Mutex::new(None),
        }
    }
}

impl TokenProvider for TokenSource {
    fn token(&self, ctx: &Ctx) -> Result<String, EnrollError> {
        let mut tok = self.tok.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(t) = &*tok {
            let now = (self.client.now)().unix_nanos();
            let refresh = i128::try_from(REFRESH_BEFORE.as_nanos()).unwrap_or(i128::MAX);
            if now + refresh < t.expires_at.unix_nanos() {
                return Ok(t.value.clone());
            }
        }
        let t = self.client.token(ctx, &self.agent_id)?;
        let v = t.value.clone();
        *tok = Some(t);
        Ok(v)
    }

    fn invalidate(&self) {
        *self.tok.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_request_and_request_uri() {
        assert_eq!(
            canonical_request("post", "/agent/v1/token", "1700000000", "n", b"{}"),
            "POST|/agent/v1/token|1700000000|n|44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
        assert_eq!(
            request_uri("https://h:1/base/agent/v1/token"),
            "/base/agent/v1/token"
        );
        assert_eq!(request_uri("https://h"), "/");
        assert_eq!(request_uri("https://h?x=1"), "/?x=1");
    }

    #[test]
    fn signed_headers_verify() {
        let key = SigningKey::from_seed(&[1u8; 32]);
        let now = OffsetDateTime::parse_rfc3339("2026-11-15T00:00:00Z").unwrap();
        let h = sign_request_with_nonce(
            &key,
            "agent-1",
            "POST",
            "/agent/v1/token",
            b"{}",
            now,
            &[0u8; 24],
        );
        assert_eq!(h[0], (HEADER_AGENT_ID, "agent-1".to_owned()));
        assert_eq!(h[1].1, "1794700800");
        assert_eq!(h[2].1, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        let sig = dl_encoding::base64::STANDARD.decode(&h[3].1).unwrap();
        let msg = canonical_request("POST", "/agent/v1/token", &h[1].1, &h[2].1, b"{}");
        assert!(dl_ed25519::verify(
            key.verifying_key().as_bytes(),
            msg.as_bytes(),
            &sig
        ));
    }
}
