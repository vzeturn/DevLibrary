//! Connecting to `ws://` and `wss://` URLs through dl-http (proxies, TLS, timeouts).

use std::time::Duration;

use dl_http::{Client, Method, Request, Response, Upgraded, Url};

use crate::Error;
use crate::handshake::{check_response, generate_key};
use crate::socket::{Config, Role, WebSocket};

/// Options for [`connect_with`].
#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    /// Extra request header fields, e.g. `("Authorization", "Bearer ...")`. The handshake
    /// fields (`Upgrade`, `Connection`, `Sec-WebSocket-*`) are set by this crate and may
    /// not be given here.
    pub headers: Vec<(String, String)>,
    /// Subprotocols to offer, most preferred first.
    pub protocols: Vec<String>,
    /// Deadline for connecting and the handshake; `None` uses the client's timeout.
    pub timeout: Option<Duration>,
    /// Limits for the connection.
    pub config: Config,
}

/// Maps `ws`/`wss` to `http`/`https`; `http(s)` URLs pass through.
fn http_url(url: &str) -> Result<Url, Error> {
    let lower = url
        .get(..6)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let mapped = if lower.starts_with("wss://") {
        format!("https://{}", &url[6..])
    } else if lower.starts_with("ws://") {
        format!("http://{}", &url[5..])
    } else {
        url.to_owned()
    };
    Ok(Url::parse(&mapped)?)
}

/// Connects with a default [`Client`] (proxy from the environment, 30 s handshake
/// deadline).
///
/// # Errors
/// As [`connect_with`].
pub fn connect(url: &str) -> Result<(WebSocket<Upgraded>, Response), Error> {
    connect_with(&Client::new(), url, &ConnectOptions::default())
}

/// Connects to `url` (`ws://`, `wss://`, `http://` or `https://`) and runs the opening
/// handshake. Returns the connection and the `101` response.
///
/// # Errors
/// [`Error::Http`] when connecting fails, [`Error::Rejected`] for a non-`101` answer,
/// [`Error::Handshake`] for an invalid `101` response, [`Error::InvalidInput`] for a
/// reserved header in `options`.
pub fn connect_with(
    client: &Client,
    url: &str,
    options: &ConnectOptions,
) -> Result<(WebSocket<Upgraded>, Response), Error> {
    let url = http_url(url)?;
    let key = generate_key()?;
    let mut req = Request::new(Method::Get, url)
        .with_header("Upgrade", "websocket")?
        .with_header("Connection", "Upgrade")?
        .with_header("Sec-WebSocket-Key", &key)?
        .with_header("Sec-WebSocket-Version", "13")?;
    if !options.protocols.is_empty() {
        req = req.with_header("Sec-WebSocket-Protocol", &options.protocols.join(", "))?;
    }
    for (name, value) in &options.headers {
        let n = name.to_ascii_lowercase();
        if n == "upgrade" || n == "connection" || n.starts_with("sec-websocket-") {
            return Err(Error::InvalidInput("handshake header set by the caller"));
        }
        req = req.with_header(name, value)?;
    }
    if let Some(t) = options.timeout {
        req = req.with_timeout(t);
    }
    let (resp, conn) = client.upgrade(&req)?;
    let Some(conn) = conn else {
        return Err(Error::Rejected {
            status: resp.status.as_u16(),
            body: String::from_utf8_lossy(&resp.body).into_owned(),
        });
    };
    let fields: Vec<(&str, &str)> = resp.headers.iter().collect();
    let protocol = check_response(&key, &options.protocols, &fields)?;
    let mut ws = WebSocket::from_raw(conn, Role::Client, options.config);
    ws.set_protocol(protocol);
    Ok((ws, resp))
}

impl WebSocket<Upgraded> {
    /// Sets the socket read timeout; see [`Upgraded::set_read_timeout`].
    ///
    /// # Errors
    /// The OS error, e.g. for a zero duration.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<(), Error> {
        Ok(self.get_ref().set_read_timeout(timeout)?)
    }

    /// Sets the socket write timeout. A write that times out ends the connection.
    ///
    /// # Errors
    /// The OS error, e.g. for a zero duration.
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> Result<(), Error> {
        Ok(self.get_ref().set_write_timeout(timeout)?)
    }

    /// A handle that shuts the socket down from another thread, so a blocked
    /// [`WebSocket::read`] returns.
    ///
    /// # Errors
    /// The OS error from duplicating the socket handle.
    pub fn shutdown_handle(&self) -> Result<dl_http::ShutdownHandle, Error> {
        Ok(self.get_ref().shutdown_handle()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_schemes() {
        assert_eq!(
            http_url("ws://h:81/hubs?id=1").unwrap().to_string(),
            "http://h:81/hubs?id=1"
        );
        assert_eq!(http_url("WSS://h/x").unwrap().to_string(), "https://h/x");
        assert_eq!(http_url("https://h/").unwrap().to_string(), "https://h/");
        assert!(http_url("ftp://h/").is_err());
    }
}
