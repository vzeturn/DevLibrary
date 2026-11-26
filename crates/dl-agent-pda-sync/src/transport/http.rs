//! The signed HTTPS client (port of `Transport/SyncHttpClient.cs`): every attempt is
//! re-signed (fresh nonce and timestamp), transient failures retry with backoff and
//! jitter.
//!
//! Over `dl_http` with the OS TLS stack. Settings of the C# handler kept: connect timeout
//! 20 s, the caller's overall timeout (2 min for the service), no `User-Agent`, no
//! decompression. The proxy is the one dl-agent chose for the process ([`proxy`]:
//! `HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY`, then WinHTTP, then the Internet Options), the
//! closest match to the C# handler's system proxy (WPAD/PAC scripts and integrated proxy
//! authentication are not followed). Request bodies are buffered: a file upload is read
//! into memory on every attempt instead of streamed.

use super::Error;
use super::retry::{RetryPolicy, is_transient, parse_retry_after};
use super::signer::{
    NONCE_HEADER, RequestSigner, SIGNATURE_HEADER, SignedHeaders, TIMESTAMP_HEADER,
};
use crate::config::{AgentError, codes};
use crate::reporting::log::{Logger, Value as LogValue};
use crate::util::ctx::Ctx;
use dl_http::{Client, ClientOptions, Method, Proxy, Request, Url};
use dl_json::Value;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// The header a tenant API key travels in (ADR 0032), for the one unsigned call.
pub const API_KEY_HEADER: &str = "X-Api-Key";
/// The logger category (`ILogger<SyncHttpClient>`).
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Transport.SyncHttpClient";
/// `SocketsHttpHandler.ConnectTimeout`.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// The service's `HttpClient.Timeout`.
pub const SERVICE_TIMEOUT: Duration = Duration::from_secs(120);
/// How long an idle keep-alive connection to the cloud is kept for the next request
/// (RT-1318): a store uplink pays ~1.7 s for every new TCP+TLS handshake, so the open-batch,
/// result and heartbeat calls reuse one warm connection across their usual gaps.
pub const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// `application/json; charset=utf-8`.
pub const JSON_CONTENT_TYPE: &str = "application/json; charset=utf-8";

/// A response after retries; `body` is the raw payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncResponse {
    /// The HTTP status code.
    pub status: u16,
    /// The body bytes.
    pub body: Vec<u8>,
    /// Whether the last attempt went over a pooled keep-alive connection (`false`: a new
    /// TCP+TLS connection was opened for it).
    pub reused: bool,
}

impl SyncResponse {
    /// A 2xx status.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The `data` element of the API envelope `{ result, data }`, or the root when absent;
    /// `None` for an empty body. A body that is not JSON fails like `JsonDocument.Parse`.
    pub fn data(&self) -> Result<Option<Value>, Error> {
        if self.body.is_empty() {
            return Ok(None);
        }
        let text = std::str::from_utf8(&self.body).map_err(|e| Error::Unexpected {
            type_name: "System.Text.Json.JsonException",
            message: e.to_string(),
        })?;
        let root = crate::util::stj::parse(text).map_err(|e| Error::Unexpected {
            type_name: "System.Text.Json.JsonException",
            message: e.to_string(),
        })?;
        if let Value::Object(m) = &root
            && let Some((_, d)) = m.iter().rev().find(|(k, _)| k.as_str() == "data")
        {
            return Ok(Some(d.clone()));
        }
        Ok(Some(root))
    }
}

/// The proxy of every PDA cloud call (HTTP, the command WebSocket, the order hub): the
/// one dl-agent chose for this process (`dl_agent::proxy::current`), as the C# handler
/// used the machine's proxy (`WebRequest.GetSystemWebProxy`) rather than only the
/// environment.
pub fn proxy() -> Proxy {
    dl_agent::proxy::current().proxy.clone()
}

/// `SyncHttpClient.CreateHandler`: the client options of every cloud call with an
/// overall `timeout`.
pub fn client_options(timeout: Duration) -> ClientOptions {
    ClientOptions {
        timeout: Some(timeout),
        connect_timeout: Some(CONNECT_TIMEOUT),
        // HttpClient.MaxResponseContentBufferSize.
        max_body: i32::MAX as usize,
        proxy: proxy(),
        user_agent: None,
        max_redirects: 0,
        danger_accept_invalid_certs: false,
        pool_idle_timeout: POOL_IDLE_TIMEOUT,
    }
}

/// What a request carries besides the method and path.
enum Payload<'a> {
    Bytes {
        body: &'a [u8],
        content_type: Option<&'a str>,
    },
    File {
        file: &'a Path,
        content_type: &'a str,
    },
}

enum Auth<'a> {
    Signed {
        bearer: Option<&'a str>,
        body_hash: Option<&'a str>,
    },
    ApiKey(&'a str),
}

/// The signed cloud client (`SyncHttpClient`).
pub struct SyncHttpClient {
    client: Arc<Client>,
    base: Url,
    signer: Option<RequestSigner>,
    retry: RetryPolicy,
    log: Logger,
}

impl std::fmt::Debug for SyncHttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncHttpClient")
            .field("base", &self.base.to_string())
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

fn unexpected(type_name: &'static str, message: impl Into<String>) -> Error {
    Error::Unexpected {
        type_name,
        message: message.into(),
    }
}

impl SyncHttpClient {
    /// A client for the cloud at `base_url` (`HttpClient.BaseAddress`) with an overall
    /// per-request `timeout`. `signer` is `None` only for a client that sends
    /// [`put_file_with_api_key`](Self::put_file_with_api_key) and nothing else.
    pub fn new(
        base_url: &str,
        timeout: Duration,
        signer: Option<RequestSigner>,
        retry: RetryPolicy,
        log: &Logger,
    ) -> Result<SyncHttpClient, Error> {
        let client = Arc::new(Client::with_options(client_options(timeout)));
        SyncHttpClient::with_client(client, base_url, signer, retry, log)
    }

    /// [`SyncHttpClient::new`] over an existing `dl_http` client.
    pub fn with_client(
        client: Arc<Client>,
        base_url: &str,
        signer: Option<RequestSigner>,
        retry: RetryPolicy,
        log: &Logger,
    ) -> Result<SyncHttpClient, Error> {
        let base = Url::parse(base_url)
            .map_err(|e| unexpected("System.UriFormatException", format!("Invalid URI: {e}")))?;
        Ok(SyncHttpClient {
            client,
            base,
            signer,
            retry,
            log: log.for_category(CATEGORY),
        })
    }

    /// The base URL.
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// The retry policy.
    pub fn retry(&self) -> &RetryPolicy {
        &self.retry
    }

    /// Connections opened to the cloud since the client was created (RT-1318); a request
    /// on a pooled keep-alive connection does not count.
    pub fn connections_opened(&self) -> u64 {
        self.client.connections_opened()
    }

    fn signer(&self) -> Result<&RequestSigner, Error> {
        self.signer.as_ref().ok_or_else(|| {
            unexpected(
                "System.InvalidOperationException",
                "This client has no agent key; it can only send with an API key.",
            )
        })
    }

    /// `POST` of a JSON text.
    pub fn post_json(
        &self,
        ctx: &Ctx,
        path: &str,
        json: &str,
        bearer: Option<&str>,
    ) -> Result<SyncResponse, Error> {
        self.send(ctx, Method::Post, path, json.as_bytes(), bearer)
    }

    /// `GET`.
    pub fn get(&self, ctx: &Ctx, path: &str, bearer: Option<&str>) -> Result<SyncResponse, Error> {
        self.send(ctx, Method::Get, path, &[], bearer)
    }

    /// The signature headers of a request without a body (the websocket upgrade).
    pub fn sign_only(&self, method: &str, path_and_query: &str) -> Result<SignedHeaders, Error> {
        self.signer()?
            .sign(method, path_and_query, &[])
            .map_err(|e| {
                unexpected(
                    "System.Security.Cryptography.CryptographicException",
                    e.to_string(),
                )
            })
    }

    /// Raw bytes (blob content, ADR 0030); the signature's body hash is the content's.
    pub fn put_bytes(
        &self,
        ctx: &Ctx,
        path: &str,
        body: &[u8],
        content_type: &str,
        bearer: Option<&str>,
    ) -> Result<SyncResponse, Error> {
        self.send_with_retry(
            ctx,
            Method::Put,
            path,
            &Payload::Bytes {
                body,
                content_type: Some(content_type),
            },
            &Auth::Signed {
                bearer,
                body_hash: None,
            },
        )
    }

    /// Any method with a JSON body (`Content-Type: application/json; charset=utf-8` when
    /// the body is not empty).
    pub fn send(
        &self,
        ctx: &Ctx,
        method: Method,
        path: &str,
        body: &[u8],
        bearer: Option<&str>,
    ) -> Result<SyncResponse, Error> {
        self.send_with_retry(
            ctx,
            method,
            path,
            &Payload::Bytes {
                body,
                content_type: None,
            },
            &Auth::Signed {
                bearer,
                body_hash: None,
            },
        )
    }

    /// A file from disk (legacy bundle zip, ADR 0095), read again on every attempt. The
    /// signature carries `sha256` (lowercase hex of the file, also named by the path).
    pub fn put_file(
        &self,
        ctx: &Ctx,
        path_and_query: &str,
        file: &Path,
        sha256: &str,
        content_type: &str,
        bearer: Option<&str>,
    ) -> Result<SyncResponse, Error> {
        self.send_with_retry(
            ctx,
            Method::Put,
            path_and_query,
            &Payload::File { file, content_type },
            &Auth::Signed {
                bearer,
                body_hash: Some(sha256),
            },
        )
    }

    /// The file upload of the legacy bundle with a tenant API key instead of the agent's
    /// token and signature (ADR 0096). Same retries.
    pub fn put_file_with_api_key(
        &self,
        ctx: &Ctx,
        path_and_query: &str,
        file: &Path,
        content_type: &str,
        api_key: &str,
    ) -> Result<SyncResponse, Error> {
        self.send_with_retry(
            ctx,
            Method::Put,
            path_and_query,
            &Payload::File { file, content_type },
            &Auth::ApiKey(api_key),
        )
    }

    fn build(
        &self,
        method: Method,
        path: &str,
        payload: &Payload<'_>,
        auth: &Auth<'_>,
    ) -> Result<Request, Error> {
        let url = self
            .base
            .join(path)
            .map_err(|e| unexpected("System.UriFormatException", format!("Invalid URI: {e}")))?;
        let mut req = Request::new(method, url);
        let header_error = |e: dl_http::Error| unexpected("System.FormatException", e.to_string());
        let body: Vec<u8> = match payload {
            Payload::Bytes { body, content_type } => {
                if !body.is_empty() {
                    req = req
                        .with_header("Content-Type", content_type.unwrap_or(JSON_CONTENT_TYPE))
                        .map_err(header_error)?;
                }
                body.to_vec()
            }
            Payload::File { file, content_type } => {
                let bytes = std::fs::read(file).map_err(|e| {
                    let ty = match e.kind() {
                        std::io::ErrorKind::NotFound => "System.IO.FileNotFoundException",
                        std::io::ErrorKind::PermissionDenied => {
                            "System.UnauthorizedAccessException"
                        }
                        _ => "System.IO.IOException",
                    };
                    unexpected(ty, e.to_string())
                })?;
                req = req
                    .with_header("Content-Type", content_type)
                    .map_err(header_error)?;
                bytes
            }
        };
        match auth {
            Auth::Signed { bearer, body_hash } => {
                let signer = self.signer()?;
                let h = match body_hash {
                    Some(hash) => signer.sign_body_hash(method.as_str(), path, hash),
                    None => signer.sign(method.as_str(), path, &body),
                }
                .map_err(|e| {
                    unexpected(
                        "System.Security.Cryptography.CryptographicException",
                        e.to_string(),
                    )
                })?;
                req = req
                    .with_header(TIMESTAMP_HEADER, &h.timestamp)
                    .and_then(|r| r.with_header(NONCE_HEADER, &h.nonce))
                    .and_then(|r| r.with_header(SIGNATURE_HEADER, &h.signature))
                    .map_err(header_error)?;
                if let Some(b) = bearer {
                    req = req
                        .with_header("Authorization", &format!("Bearer {b}"))
                        .map_err(header_error)?;
                }
            }
            Auth::ApiKey(key) => {
                req = req.with_header(API_KEY_HEADER, key).map_err(header_error)?;
            }
        }
        Ok(req.with_body(body))
    }

    fn send_with_retry(
        &self,
        ctx: &Ctx,
        method: Method,
        path: &str,
        payload: &Payload<'_>,
        auth: &Auth<'_>,
    ) -> Result<SyncResponse, Error> {
        let max = self.retry.max_attempts;
        let mut last: Option<String> = None;
        let mut attempt = 1;
        while attempt <= max {
            if ctx.is_done() {
                return Err(Error::Cancelled);
            }
            let mut retry_after = None;
            let mut req = self.build(method, path, payload, auth)?;
            let overall = req.timeout.or(self.client.options().timeout);
            if let Some(left) = ctx.remaining() {
                req.timeout = Some(overall.map_or(left, |t| t.min(left)));
            }
            match self.client.request(&req) {
                Ok(resp) => {
                    if !resp.reused {
                        self.log.info(
                            "Opened a new connection to the cloud for {Method} {Path} ({Opened} opened since start)",
                            &[
                                method.as_str().into(),
                                path.into(),
                                LogValue::Int(
                                    i64::try_from(self.client.connections_opened())
                                        .unwrap_or(i64::MAX),
                                ),
                            ],
                        );
                    }
                    let status = resp.status.as_u16();
                    if !is_transient(status) {
                        return Ok(SyncResponse {
                            status,
                            body: resp.body,
                            reused: resp.reused,
                        });
                    }
                    retry_after = parse_retry_after(resp.headers.get("retry-after"));
                    last = Some(format!("Transient HTTP {status} from {path}"));
                    if attempt == max {
                        return Ok(SyncResponse {
                            status,
                            body: resp.body,
                            reused: resp.reused,
                        });
                    }
                }
                Err(e) => {
                    if ctx.is_done() {
                        return Err(Error::Cancelled);
                    }
                    match e {
                        dl_http::Error::InvalidUrl { .. } => {
                            return Err(unexpected("System.UriFormatException", e.to_string()));
                        }
                        dl_http::Error::InvalidHeader { .. } => {
                            return Err(unexpected("System.FormatException", e.to_string()));
                        }
                        dl_http::Error::Timeout => {
                            let secs = overall.map_or(0, |t| t.as_secs());
                            last = Some(format!(
                                "The request was canceled due to the configured HttpClient.Timeout of {secs} seconds elapsing."
                            ));
                        }
                        other => last = Some(other.to_string()),
                    }
                }
            }
            if attempt == max {
                break;
            }
            let delay = self.retry.delay_for(attempt, retry_after);
            self.log.warn(
                "Request {Method} {Path} failed (attempt {Attempt}/{Max}): {Reason}; retrying in {Delay}",
                &[
                    method.as_str().into(),
                    path.into(),
                    LogValue::Int(i64::from(attempt)),
                    LogValue::Int(i64::from(max)),
                    last.clone().into(),
                    delay.into(),
                ],
            );
            if !(self.retry.sleep)(delay, ctx) {
                return Err(Error::Cancelled);
            }
            attempt += 1;
        }
        let reason = last.unwrap_or_default();
        let message = format!("Cloud unreachable after {max} attempts: {reason}");
        Err(Error::Agent(if reason.is_empty() {
            AgentError::new(codes::CLOUD_UNREACHABLE, message)
        } else {
            AgentError::with_source(codes::CLOUD_UNREACHABLE, message, RequestFailure(reason))
        }))
    }
}

/// The last attempt's failure (`HttpRequestException`), the source of `CLOUD_UNREACHABLE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestFailure(pub String);

impl std::fmt::Display for RequestFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RequestFailure {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_calls_use_the_proxy_dl_agent_chose() {
        let o = client_options(Duration::from_secs(5));
        assert_eq!(o.proxy, dl_agent::proxy::current().proxy);
        assert_eq!(o.timeout, Some(Duration::from_secs(5)));
        assert_eq!(o.connect_timeout, Some(CONNECT_TIMEOUT));
    }
}
