//! The provider call itself: one HTTPS request that follows no redirect (a redirect could
//! leave the allowlist), behind a trait so the worker can be tested with a hand-written fake.

use std::time::Duration;

use dl_http::{ClientOptions, Method, Request, Url};

use super::wire::MAX_RESPONSE_BODY;

/// One checked provider call.
pub struct ProviderCall {
    /// `POST` or `GET`.
    pub method: Method,
    /// An allowlisted URL.
    pub url: Url,
    /// Only the forwarded headers.
    pub headers: Vec<(String, String)>,
    /// Request body.
    pub body: Vec<u8>,
    /// Deadline for the whole call.
    pub timeout: Duration,
}

/// The provider's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderAnswer {
    /// Status code (a redirect is returned as is).
    pub status: u16,
    /// `Content-Type`, when the provider sent one.
    pub content_type: Option<String>,
    /// Body, at most [`MAX_RESPONSE_BODY`] bytes.
    pub body: Vec<u8>,
}

/// Why a provider call produced no answer.
#[derive(Debug, Clone, PartialEq)]
pub enum CallError {
    /// Network, TLS or timeout failure; the reason carries no URL.
    Failed(String),
    /// The answer is larger than [`MAX_RESPONSE_BODY`].
    TooLarge,
}

/// Performs provider calls.
pub trait Transport: Send {
    /// Makes one call.
    fn call(&self, call: &ProviderCall) -> Result<ProviderAnswer, CallError>;
}

/// The real transport over dl-http: no redirects, answers capped at 8 MiB, proxy from the
/// environment like Go's default transport.
pub struct HttpTransport {
    client: dl_http::Client,
}

impl HttpTransport {
    /// A client for relayed calls.
    pub fn new() -> HttpTransport {
        HttpTransport {
            client: dl_http::Client::with_options(ClientOptions {
                max_body: MAX_RESPONSE_BODY,
                max_redirects: 0,
                ..Default::default()
            }),
        }
    }
}

impl Default for HttpTransport {
    fn default() -> HttpTransport {
        HttpTransport::new()
    }
}

impl Transport for HttpTransport {
    fn call(&self, call: &ProviderCall) -> Result<ProviderAnswer, CallError> {
        let mut req = Request::new(call.method, call.url.clone())
            .with_body(call.body.clone())
            .with_timeout(call.timeout);
        for (name, value) in &call.headers {
            req = req
                .with_header(name, value)
                .map_err(|e| CallError::Failed(e.to_string()))?;
        }
        match self.client.request(&req) {
            Ok(resp) => Ok(ProviderAnswer {
                status: resp.status.as_u16(),
                content_type: resp
                    .headers
                    .get("Content-Type")
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned),
                body: resp.body,
            }),
            Err(dl_http::Error::BodyTooLarge { .. }) => Err(CallError::TooLarge),
            Err(e) => Err(CallError::Failed(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    /// A one-shot plain-HTTP server answering `response`; returns its base URL.
    fn serve_once(response: &'static [u8]) -> (String, std::thread::JoinHandle<Vec<u8>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = vec![0u8; 8192];
            let mut got = Vec::new();
            let body_len = |got: &[u8]| -> Option<usize> {
                let end = got.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
                let head = String::from_utf8_lossy(&got[..end]).to_ascii_lowercase();
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .map_or(0, |v| v.trim().parse().unwrap_or(0));
                Some(end + len)
            };
            while body_len(&got).is_none_or(|want| got.len() < want) {
                let n = s.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
            }
            s.write_all(response).unwrap();
            got
        });
        (base, h)
    }

    #[test]
    fn a_redirect_is_returned_not_followed() {
        let (base, server) = serve_once(
            b"HTTP/1.1 302 Found\r\nLocation: http://192.168.1.10/admin\r\nContent-Length: 0\r\n\r\n",
        );
        let call = ProviderCall {
            method: Method::Get,
            url: Url::parse(&format!("{base}/x")).unwrap(),
            headers: vec![("SOAPAction".into(), "urn:a".into())],
            body: Vec::new(),
            timeout: Duration::from_secs(5),
        };
        let answer = HttpTransport::new().call(&call).unwrap();
        assert_eq!(answer.status, 302);
        let head = String::from_utf8(server.join().unwrap()).unwrap();
        assert!(head.contains("SOAPAction: urn:a"), "{head}");
    }

    #[test]
    fn content_type_is_kept() {
        let (base, server) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/xml; charset=utf-8\r\nContent-Length: 4\r\n\r\n<a/>",
        );
        let call = ProviderCall {
            method: Method::Post,
            url: Url::parse(&format!("{base}/x")).unwrap(),
            headers: Vec::new(),
            body: b"<q/>".to_vec(),
            timeout: Duration::from_secs(5),
        };
        let answer = HttpTransport::new().call(&call).unwrap();
        server.join().unwrap();
        assert_eq!(answer.status, 200);
        assert_eq!(
            answer.content_type.as_deref(),
            Some("text/xml; charset=utf-8")
        );
        assert_eq!(answer.body, b"<a/>");
    }
}
