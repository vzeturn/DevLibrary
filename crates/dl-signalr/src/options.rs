use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use dl_json::Value;

use crate::Error;

/// An event handler: receives the arguments of a server-to-client invocation.
pub type Handler = Arc<dyn Fn(&[Value]) + Send + Sync + 'static>;

/// Settings for [`HubConnection::connect`](crate::HubConnection::connect).
///
/// ```
/// use std::time::Duration;
///
/// let mut options = dl_signalr::ConnectOptions::new("https://pos.example.com/hubs/order");
/// options.access_token = Some("eyJhbGciOi...".into());
/// options.timeout = Duration::from_secs(10);
/// options.on("OrderPlaced", |args| println!("order {args:?}"));
/// assert_eq!(options.handlers.len(), 1);
/// ```
#[derive(Clone)]
pub struct ConnectOptions {
    /// The hub URL, e.g. `https://host/hubs/chat` (`http`, `https`, `ws` or `wss`; a
    /// query string is kept).
    pub url: String,
    /// `true` (default): `POST {url}/negotiate?negotiateVersion=1` first, follow its
    /// redirects, and connect with `?id=<connectionToken>`. `false`: connect the
    /// WebSocket directly (the JavaScript client's `skipNegotiation`).
    pub negotiate: bool,
    /// Sent as `Authorization: Bearer` on negotiate and on the WebSocket upgrade, and as
    /// the URL-encoded `access_token` query parameter of the WebSocket URL.
    pub access_token: Option<String>,
    /// Extra header fields for the negotiate request and the WebSocket upgrade.
    pub headers: Vec<(String, String)>,
    /// Limit for each step of connecting: every negotiate request, the WebSocket
    /// upgrade, and the handshake response. Default 15 s.
    pub timeout: Duration,
    /// A ping is sent when nothing else was sent for this long. Default 15 s (the
    /// server's `ClientTimeoutInterval` is 30 s by default).
    pub keep_alive_interval: Duration,
    /// The connection fails with [`Error::ServerTimeout`] when nothing arrives from the
    /// server for this long. Default 30 s (twice the server's `KeepAliveInterval`).
    pub server_timeout: Duration,
    /// Largest message accepted from the server, in bytes. Default 16 MiB.
    pub max_message_size: usize,
    /// The HTTP client (proxy and TLS settings). `None` uses `dl_http::Client::new()`,
    /// which reads the proxy from the environment.
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    pub http: Option<Arc<dl_http::Client>>,
    /// Handlers registered before connecting, as `(target, handler)`. They see
    /// invocations the server sends right after the handshake (from
    /// `OnConnectedAsync`). Prefer [`ConnectOptions::on`] to fill it.
    pub handlers: Vec<(String, Handler)>,
}

impl ConnectOptions {
    /// Options for `url` with the defaults.
    pub fn new(url: impl Into<String>) -> ConnectOptions {
        ConnectOptions {
            url: url.into(),
            negotiate: true,
            access_token: None,
            headers: Vec::new(),
            timeout: Duration::from_secs(15),
            keep_alive_interval: Duration::from_secs(15),
            server_timeout: Duration::from_secs(30),
            max_message_size: 16 << 20,
            #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
            http: None,
            handlers: Vec::new(),
        }
    }

    /// Registers a handler for invocations of `target` (compared case-insensitively,
    /// as ASP.NET Core does). Several handlers for one target all run, in order.
    pub fn on(
        &mut self,
        target: &str,
        handler: impl Fn(&[Value]) + Send + Sync + 'static,
    ) -> &mut ConnectOptions {
        self.handlers.push((target.to_owned(), Arc::new(handler)));
        self
    }

    /// Checks the durations and limits.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.timeout.is_zero() {
            return Err(Error::InvalidOptions("timeout must be positive"));
        }
        if self.keep_alive_interval.is_zero() {
            return Err(Error::InvalidOptions(
                "keep_alive_interval must be positive",
            ));
        }
        if self.server_timeout.is_zero() {
            return Err(Error::InvalidOptions("server_timeout must be positive"));
        }
        if self.max_message_size == 0 {
            return Err(Error::InvalidOptions("max_message_size must be positive"));
        }
        Ok(())
    }
}

impl Default for ConnectOptions {
    fn default() -> ConnectOptions {
        ConnectOptions::new(String::new())
    }
}

impl fmt::Debug for ConnectOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let targets: Vec<&str> = self.handlers.iter().map(|(t, _)| t.as_str()).collect();
        let header_names: Vec<&str> = self.headers.iter().map(|(n, _)| n.as_str()).collect();
        f.debug_struct("ConnectOptions")
            .field("url", &self.url)
            .field("negotiate", &self.negotiate)
            .field(
                "access_token",
                &self.access_token.as_ref().map(|_| "<redacted>"),
            )
            .field("headers", &header_names)
            .field("timeout", &self.timeout)
            .field("keep_alive_interval", &self.keep_alive_interval)
            .field("server_timeout", &self.server_timeout)
            .field("max_message_size", &self.max_message_size)
            .field("handlers", &targets)
            .finish_non_exhaustive()
    }
}

/// Why a connection ended; see [`HubConnection::wait_closed`](crate::HubConnection::wait_closed).
#[derive(Debug, Clone)]
pub enum CloseReason {
    /// [`HubConnection::close`](crate::HubConnection::close) was called or every handle
    /// was dropped.
    Local,
    /// The server sent a close message.
    Server {
        /// The error the server gave, if it closed because of one.
        error: Option<String>,
        /// Whether the server allows reconnecting.
        allow_reconnect: bool,
    },
    /// The transport failed: a network error, [`Error::ServerTimeout`], a WebSocket
    /// closed without a close message ([`Error::WebSocketClosed`]), or a malformed
    /// message.
    Transport(Error),
}

impl fmt::Display for CloseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CloseReason::Local => f.write_str("closed locally"),
            CloseReason::Server {
                error,
                allow_reconnect,
            } => {
                f.write_str("closed by the server")?;
                if let Some(e) = error {
                    write!(f, ": {e}")?;
                }
                if *allow_reconnect {
                    f.write_str(" (reconnect allowed)")?;
                }
                Ok(())
            }
            CloseReason::Transport(e) => write!(f, "connection lost: {e}"),
        }
    }
}
