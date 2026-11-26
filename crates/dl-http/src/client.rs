//! Blocking HTTP/1.1 client with keep-alive pooling, deadlines, proxies and TLS.

use crate::body::ChunkedDecoder;
use crate::cancel::Cancel;
use crate::headers::validate_value;
use crate::message::{
    BodyFraming, MAX_HEAD_SIZE, find_head_end, keep_alive, parse_response_head, response_framing,
};
use crate::upgraded::Upgraded;
use crate::url::Scheme;
use crate::{Error, Method, Proxy, Request, Response, Url, percent_decode};
use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

/// Idle connections kept per (scheme, host, port, proxy).
const MAX_IDLE_PER_HOST: usize = 4;
/// Default for [`ClientOptions::pool_idle_timeout`].
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Bytes requested from the socket per read.
const READ_CHUNK: usize = 16 * 1024;
/// Smallest socket timeout set (a zero timeout means "block forever" to the OS).
const MIN_IO_TIMEOUT: Duration = Duration::from_millis(1);

/// Client settings. Construct with struct update syntax:
///
/// ```
/// use std::time::Duration;
/// let options = dl_http::ClientOptions {
///     timeout: Some(Duration::from_secs(10)),
///     max_redirects: 5,
///     ..Default::default()
/// };
/// let client = dl_http::Client::with_options(options);
/// assert_eq!(client.options().max_redirects, 5);
/// ```
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// Deadline for a whole call to [`Client::request`]: DNS, connect, TLS, sending, and
    /// reading the full response, across redirects and the stale-connection retry.
    /// `None` waits forever. Default 30 s. [`Request::timeout`] overrides it.
    pub timeout: Option<Duration>,
    /// Limit for DNS resolution and for each TCP connect attempt (also bounded by
    /// `timeout`). Default 10 s.
    pub connect_timeout: Option<Duration>,
    /// Largest response body accepted, in bytes. Default 10 MiB.
    pub max_body: usize,
    /// Proxy selection. Default [`Proxy::FromEnv`].
    pub proxy: Proxy,
    /// `User-Agent` sent unless the request sets one. Default `dl-http/<version>`.
    pub user_agent: Option<String>,
    /// Redirects (301, 302, 303, 307, 308) followed for `GET` and `HEAD`. Default 0:
    /// redirects are returned as responses. Other methods are never redirected.
    pub max_redirects: u32,
    /// Accept any TLS certificate. The connection is encrypted but not authenticated;
    /// use only for tests or known self-signed hosts.
    pub danger_accept_invalid_certs: bool,
    /// How long an idle keep-alive connection stays in the pool before it is closed
    /// instead of reused. Default 60 s.
    pub pool_idle_timeout: Duration,
}

impl Default for ClientOptions {
    fn default() -> ClientOptions {
        ClientOptions {
            timeout: Some(Duration::from_secs(30)),
            connect_timeout: Some(Duration::from_secs(10)),
            max_body: 10 * 1024 * 1024,
            proxy: Proxy::FromEnv,
            user_agent: Some(concat!("dl-http/", env!("CARGO_PKG_VERSION")).to_owned()),
            max_redirects: 0,
            danger_accept_invalid_certs: false,
            pool_idle_timeout: IDLE_TIMEOUT,
        }
    }
}

/// A blocking HTTP/1.1 client.
///
/// Keeps up to 4 idle keep-alive connections per destination for
/// [`ClientOptions::pool_idle_timeout`] (60 s by default). A `Client` is
/// `Send + Sync`; share one (by reference or `Arc`) instead of creating one per request.
pub struct Client {
    options: ClientOptions,
    pool: Mutex<HashMap<PoolKey, Vec<Conn>>>,
    opened: AtomicU64,
}

impl Default for Client {
    fn default() -> Client {
        Client::new()
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PoolKey {
    scheme: Scheme,
    host: String,
    port: u16,
    /// Proxy host, port and `Proxy-Authorization` value.
    proxy: Option<(String, u16, Option<String>)>,
}

struct ProxyRoute {
    url: Url,
    authorization: Option<String>,
}

struct Route {
    proxy: Option<ProxyRoute>,
    key: PoolKey,
}

/// What happened before an exchange failed; decides whether a retry is safe.
#[derive(Default)]
struct Progress {
    request_written: bool,
    response_started: bool,
}

pub(crate) enum Transport {
    Plain(TcpStream),
    Tls(Box<dl_tls::TlsStream<TcpStream>>),
}

impl Transport {
    pub(crate) fn tcp(&self) -> &TcpStream {
        match self {
            Transport::Plain(s) => s,
            Transport::Tls(t) => t.get_ref(),
        }
    }
}

impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Transport::Plain(s) => s.read(buf),
            Transport::Tls(t) => t.read(buf),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Transport::Plain(s) => s.write(buf),
            Transport::Tls(t) => t.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Transport::Plain(s) => s.flush(),
            Transport::Tls(t) => t.flush(),
        }
    }
}

/// Time left before `deadline` (at least 1 ms), `None` for no deadline.
fn remaining(deadline: Option<Instant>) -> Result<Option<Duration>, Error> {
    match deadline {
        None => Ok(None),
        Some(d) => {
            let left = d.saturating_duration_since(Instant::now());
            if left.is_zero() {
                Err(Error::Timeout)
            } else {
                Ok(Some(left.max(MIN_IO_TIMEOUT)))
            }
        }
    }
}

fn min_timeout(a: Option<Duration>, b: Option<Duration>) -> Option<Duration> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn io_error(e: io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => Error::Timeout,
        _ => Error::Io(e),
    }
}

fn tls_error(e: dl_tls::Error) -> Error {
    match e {
        dl_tls::Error::Io(io) => io_error(io),
        e => Error::Tls(e),
    }
}

/// Errors that mean a pooled connection was closed by the server while idle.
fn is_stale_error(e: &Error) -> bool {
    matches!(
        e,
        Error::Io(io) if matches!(
            io.kind(),
            io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::UnexpectedEof
                | io::ErrorKind::NotConnected
        )
    )
}

fn closed_before_response() -> Error {
    Error::Io(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "connection closed before the response",
    ))
}

fn truncated(reason: &'static str) -> Error {
    Error::InvalidResponse { reason }
}

/// A connection plus bytes read from it but not consumed yet.
struct Conn {
    io: Transport,
    buf: Vec<u8>,
    idle_since: Instant,
}

impl Conn {
    fn new(io: Transport) -> Conn {
        Conn {
            io,
            buf: Vec::new(),
            idle_since: Instant::now(),
        }
    }

    /// Whether an idle connection still looks usable: not closed by the peer and, for
    /// plain TCP, without unexpected bytes waiting.
    fn is_alive(&self) -> bool {
        let tcp = self.io.tcp();
        if tcp.set_nonblocking(true).is_err() {
            return false;
        }
        let mut probe = [0u8; 1];
        let peeked = tcp.peek(&mut probe);
        if tcp.set_nonblocking(false).is_err() {
            return false;
        }
        match peeked {
            Ok(0) => false,
            // TLS may legitimately receive records (session tickets) while idle.
            Ok(_) => matches!(self.io, Transport::Tls(_)),
            Err(e) => e.kind() == io::ErrorKind::WouldBlock,
        }
    }

    /// Reads more bytes into `buf`; returns how many (0 at end of stream).
    fn fill(&mut self, deadline: Option<Instant>) -> Result<usize, Error> {
        let t = remaining(deadline)?;
        self.io.tcp().set_read_timeout(t).map_err(Error::Io)?;
        let old = self.buf.len();
        self.buf.resize(old + READ_CHUNK, 0);
        let result = loop {
            match self.io.read(&mut self.buf[old..]) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                r => break r,
            }
        };
        match result {
            Ok(n) => {
                self.buf.truncate(old + n);
                Ok(n)
            }
            Err(e) => {
                self.buf.truncate(old);
                Err(io_error(e))
            }
        }
    }

    /// Writes all of `data`, renewing the write timeout before every write call so the
    /// deadline holds for slow peers.
    fn write_all(&mut self, mut data: &[u8], deadline: Option<Instant>) -> Result<(), Error> {
        while !data.is_empty() {
            let t = remaining(deadline)?;
            self.io.tcp().set_write_timeout(t).map_err(Error::Io)?;
            match self.io.write(data) {
                Ok(0) => return Err(Error::Io(io::ErrorKind::WriteZero.into())),
                Ok(n) => data = &data[n..],
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(io_error(e)),
            }
        }
        let t = remaining(deadline)?;
        self.io.tcp().set_write_timeout(t).map_err(Error::Io)?;
        self.io.flush().map_err(io_error)
    }

    /// Reads one response head; leftover bytes stay in `buf`.
    fn read_head(
        &mut self,
        deadline: Option<Instant>,
        progress: &mut Progress,
    ) -> Result<Response, Error> {
        let mut scanned: usize = 0;
        loop {
            if !self.buf.is_empty() {
                let from = scanned.saturating_sub(3);
                if find_head_end(&self.buf, from).is_some() || self.buf.len() > MAX_HEAD_SIZE {
                    if let Some((resp, used)) = parse_response_head(&self.buf)? {
                        self.buf.drain(..used);
                        return Ok(resp);
                    }
                } else {
                    crate::message::check_head_prefix(&self.buf)?;
                }
            }
            scanned = self.buf.len();
            match self.fill(deadline)? {
                0 if self.buf.is_empty() && !progress.response_started => {
                    return Err(closed_before_response());
                }
                0 => return Err(truncated("connection closed inside the response head")),
                _ => progress.response_started = true,
            }
        }
    }

    fn read_length(
        &mut self,
        len: u64,
        body: &mut Vec<u8>,
        deadline: Option<Instant>,
    ) -> Result<(), Error> {
        let mut left = len;
        body.reserve(usize::try_from(len.min(1 << 20)).unwrap_or(0));
        loop {
            let take = usize::try_from(left).map_or(self.buf.len(), |l| l.min(self.buf.len()));
            body.extend(self.buf.drain(..take));
            left -= take as u64;
            if left == 0 {
                return Ok(());
            }
            if self.fill(deadline)? == 0 {
                return Err(truncated("connection closed before the end of the body"));
            }
        }
    }

    fn read_chunked(
        &mut self,
        body: &mut Vec<u8>,
        max_body: usize,
        deadline: Option<Instant>,
    ) -> Result<crate::Headers, Error> {
        let mut decoder = ChunkedDecoder::new();
        loop {
            if !self.buf.is_empty() {
                let used = decoder.decode(&self.buf, body)?;
                self.buf.drain(..used);
                if body.len() > max_body {
                    return Err(Error::BodyTooLarge { limit: max_body });
                }
            }
            if decoder.is_done() {
                return Ok(decoder.into_trailers());
            }
            if self.fill(deadline)? == 0 {
                return Err(truncated(
                    "connection closed before the end of the chunked body",
                ));
            }
        }
    }

    fn read_to_close(
        &mut self,
        body: &mut Vec<u8>,
        max_body: usize,
        deadline: Option<Instant>,
    ) -> Result<(), Error> {
        loop {
            body.append(&mut self.buf);
            if body.len() > max_body {
                return Err(Error::BodyTooLarge { limit: max_body });
            }
            match self.fill(deadline) {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                // A TLS peer that closes TCP without close_notify.
                Err(Error::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }
}

/// Resolves `host`, bounded by `limit` (the resolver runs on a helper thread because the
/// OS resolver cannot be interrupted).
fn resolve(host: &str, port: u16, limit: Option<Duration>) -> Result<Vec<SocketAddr>, Error> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let (tx, rx) = mpsc::channel();
    let name = host.to_owned();
    std::thread::Builder::new()
        .name("dl-http-dns".to_owned())
        .spawn(move || {
            let result = (name.as_str(), port)
                .to_socket_addrs()
                .map(|addrs| addrs.collect::<Vec<_>>());
            let _ = tx.send(result);
        })
        .map_err(Error::Io)?;
    let result = match limit {
        Some(t) => rx.recv_timeout(t).map_err(|e| match e {
            mpsc::RecvTimeoutError::Timeout => Error::Timeout,
            mpsc::RecvTimeoutError::Disconnected => {
                Error::Io(io::Error::other("DNS resolver thread failed"))
            }
        })?,
        None => rx
            .recv()
            .map_err(|_| Error::Io(io::Error::other("DNS resolver thread failed")))?,
    };
    result.map_err(Error::Io)
}

/// Connects to the first reachable address of `host:port`, IPv4 and IPv6 in resolver
/// order.
fn connect_tcp(
    host: &str,
    port: u16,
    connect_timeout: Option<Duration>,
    deadline: Option<Instant>,
) -> Result<TcpStream, Error> {
    let addrs = resolve(
        host,
        port,
        min_timeout(connect_timeout, remaining(deadline)?),
    )?;
    let mut last: Option<io::Error> = None;
    for addr in addrs {
        let limit = min_timeout(connect_timeout, remaining(deadline)?);
        let attempt = match limit {
            Some(t) => TcpStream::connect_timeout(&addr, t),
            None => TcpStream::connect(addr),
        };
        match attempt {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => io_error(e),
        None => Error::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "host name resolved to no addresses",
        )),
    })
}

fn basic_auth(url: &Url) -> Result<Option<String>, Error> {
    if url.username().is_empty() && url.password().is_none() {
        return Ok(None);
    }
    let mut raw = percent_decode(url.username())?;
    raw.push(b':');
    raw.extend(percent_decode(url.password().unwrap_or(""))?);
    Ok(Some(format!(
        "Basic {}",
        dl_encoding::base64::STANDARD.encode(raw)
    )))
}

fn is_followed_redirect(code: u16) -> bool {
    matches!(code, 301 | 302 | 303 | 307 | 308)
}

impl Client {
    /// A client with [`ClientOptions::default`].
    pub fn new() -> Client {
        Client::with_options(ClientOptions::default())
    }

    /// A client with the given options.
    pub fn with_options(options: ClientOptions) -> Client {
        Client {
            options,
            pool: Mutex::new(HashMap::new()),
            opened: AtomicU64::new(0),
        }
    }

    /// The options in use.
    pub fn options(&self) -> &ClientOptions {
        &self.options
    }

    /// Connections this client opened (TCP, plus TLS for `https`) since it was created;
    /// a request on a pooled connection does not count.
    pub fn connections_opened(&self) -> u64 {
        self.opened.load(Ordering::Relaxed)
    }

    /// Sends a `GET` request.
    ///
    /// # Errors
    /// See [`Client::request`].
    pub fn get(&self, url: &str) -> Result<Response, Error> {
        self.request(&Request::get(Url::parse(url)?))
    }

    /// Sends a `POST` request with `Content-Type: application/json`.
    ///
    /// # Errors
    /// See [`Client::request`].
    pub fn post_json(&self, url: &str, body: &[u8]) -> Result<Response, Error> {
        let request = Request::post(Url::parse(url)?, body.to_vec())
            .with_header("Content-Type", "application/json")?;
        self.request(&request)
    }

    /// Sends a request and reads the whole response.
    ///
    /// Any status code is a successful result; check [`Response::status`]. A pooled
    /// connection that turns out to be closed before any response byte arrives is retried
    /// once on a new connection when the method is idempotent or the request was not
    /// completely sent (so the server cannot have processed it).
    ///
    /// # Errors
    /// [`Error::Timeout`] when the deadline passes, [`Error::Io`] for network failures,
    /// [`Error::Tls`] for TLS failures, [`Error::InvalidResponse`] for malformed responses,
    /// [`Error::BodyTooLarge`], [`Error::TooManyRedirects`], [`Error::ProxyConnect`], and
    /// [`Error::InvalidHeader`] for an invalid `user_agent` option.
    pub fn request(&self, request: &Request) -> Result<Response, Error> {
        self.request_inner(request, None)
    }

    /// [`Client::request`] that another thread can end early with [`Cancel::cancel`]: the
    /// socket of the exchange is shut down and the call returns [`Error::Cancelled`].
    ///
    /// # Errors
    /// Those of [`Client::request`], and [`Error::Cancelled`].
    pub fn request_with_cancel(
        &self,
        request: &Request,
        cancel: &Cancel,
    ) -> Result<Response, Error> {
        self.request_inner(request, Some(cancel))
    }

    fn request_inner(&self, request: &Request, cancel: Option<&Cancel>) -> Result<Response, Error> {
        let timeout = request.timeout.or(self.options.timeout);
        let deadline = timeout.and_then(|t| Instant::now().checked_add(t));
        let follow =
            self.options.max_redirects > 0 && matches!(request.method, Method::Get | Method::Head);
        let mut redirected: Option<Request> = None;
        let mut count = 0;
        loop {
            let current = redirected.as_ref().unwrap_or(request);
            let response = self.send(current, deadline, cancel)?;
            if !follow || !is_followed_redirect(response.status.as_u16()) {
                return Ok(response);
            }
            let Some(location) = response.headers.get("location") else {
                return Ok(response);
            };
            if count >= self.options.max_redirects {
                return Err(Error::TooManyRedirects {
                    max: self.options.max_redirects,
                });
            }
            count += 1;
            let next = current
                .url
                .join(location)
                .map_err(|_| Error::InvalidResponse {
                    reason: "invalid Location header",
                })?;
            let mut next_request = current.clone();
            if !next.same_origin(&current.url) {
                // Credentials stay with the origin they were meant for.
                next_request.headers.remove("authorization");
                next_request.headers.remove("cookie");
            }
            next_request.url = next;
            redirected = Some(next_request);
        }
    }

    fn route(&self, url: &Url) -> Result<Route, Error> {
        let proxy = match self.options.proxy.resolve(url)? {
            Some(p) => Some(ProxyRoute {
                authorization: basic_auth(&p)?,
                url: p,
            }),
            None => None,
        };
        let key = PoolKey {
            scheme: url.scheme(),
            host: url.host().to_owned(),
            port: url.port(),
            proxy: proxy.as_ref().map(|p| {
                (
                    p.url.host().to_owned(),
                    p.url.port(),
                    p.authorization.clone(),
                )
            }),
        };
        Ok(Route { proxy, key })
    }

    /// One request/response exchange, with the stale keep-alive retry.
    fn send(
        &self,
        request: &Request,
        deadline: Option<Instant>,
        cancel: Option<&Cancel>,
    ) -> Result<Response, Error> {
        let user_agent = match &self.options.user_agent {
            Some(ua) => Some(validate_value(ua)?),
            None => None,
        };
        let route = self.route(&request.url)?;
        let mut retried = false;
        loop {
            if let Some(c) = cancel {
                c.check()?;
            }
            let pooled = if retried {
                None
            } else {
                self.checkout(&route.key)
            };
            let reused = pooled.is_some();
            let mut conn = match pooled {
                Some(c) => c,
                None => self.connect(&request.url, &route, user_agent, deadline)?,
            };
            let watch = match cancel {
                Some(c) => Some(c.watch(conn.io.tcp())?),
                None => None,
            };
            let mut progress = Progress::default();
            let result = self.exchange(
                &mut conn,
                request,
                &route,
                user_agent,
                deadline,
                &mut progress,
            );
            drop(watch);
            let cancelled = cancel.is_some_and(Cancel::is_cancelled);
            match result {
                Ok((mut response, reusable)) => {
                    response.reused = reused;
                    if reusable && !cancelled {
                        self.checkin(route.key.clone(), conn);
                    }
                    return Ok(response);
                }
                Err(_) if cancelled => return Err(Error::Cancelled),
                Err(e)
                    if reused
                        && !progress.response_started
                        && (request.method.is_idempotent() || !progress.request_written)
                        && is_stale_error(&e) =>
                {
                    retried = true;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Writes the request head and body.
    fn write_request(
        conn: &mut Conn,
        request: &Request,
        route: &Route,
        user_agent: Option<&str>,
        deadline: Option<Instant>,
    ) -> Result<(), Error> {
        let absolute = route.proxy.is_some() && request.url.scheme() == Scheme::Http;
        let mut extra: Vec<(&str, &str)> = Vec::with_capacity(2);
        if let Some(ua) = user_agent {
            extra.push(("User-Agent", ua));
        }
        if absolute
            && let Some(auth) = route
                .proxy
                .as_ref()
                .and_then(|p| p.authorization.as_deref())
        {
            extra.push(("Proxy-Authorization", auth));
        }
        let mut head = Vec::with_capacity(512);
        request.write_head(&mut head, absolute, &extra);
        if request.body.len() <= 16 * 1024 {
            head.extend_from_slice(&request.body);
            conn.write_all(&head, deadline)?;
        } else {
            conn.write_all(&head, deadline)?;
            conn.write_all(&request.body, deadline)?;
        }
        Ok(())
    }

    fn exchange(
        &self,
        conn: &mut Conn,
        request: &Request,
        route: &Route,
        user_agent: Option<&str>,
        deadline: Option<Instant>,
        progress: &mut Progress,
    ) -> Result<(Response, bool), Error> {
        Self::write_request(conn, request, route, user_agent, deadline)?;
        progress.request_written = true;

        let response = loop {
            let r = conn.read_head(deadline, progress)?;
            match r.status.as_u16() {
                101 => return Err(truncated("unexpected 101 Switching Protocols")),
                100..=199 => continue,
                _ => break r,
            }
        };
        self.read_body(conn, request, response, deadline)
    }

    /// Reads the body of `response`; returns it with whether the connection is reusable.
    fn read_body(
        &self,
        conn: &mut Conn,
        request: &Request,
        mut response: Response,
        deadline: Option<Instant>,
    ) -> Result<(Response, bool), Error> {
        let max_body = self.options.max_body;
        let mut reusable =
            keep_alive(&response) && !request.headers.has_token("connection", "close");
        let mut body = Vec::new();
        match response_framing(request.method, &response)? {
            BodyFraming::Empty => {}
            BodyFraming::Length(n) => {
                if n > max_body as u64 {
                    return Err(Error::BodyTooLarge { limit: max_body });
                }
                conn.read_length(n, &mut body, deadline)?;
            }
            BodyFraming::Chunked => {
                response.trailers = conn.read_chunked(&mut body, max_body, deadline)?;
            }
            BodyFraming::Close => {
                conn.read_to_close(&mut body, max_body, deadline)?;
                reusable = false;
            }
        }
        response.body = body;
        // Bytes nobody asked for mean the framing is off; do not reuse the connection.
        if !conn.buf.is_empty() {
            reusable = false;
        }
        Ok((response, reusable))
    }

    fn connect(
        &self,
        target: &Url,
        route: &Route,
        user_agent: Option<&str>,
        deadline: Option<Instant>,
    ) -> Result<Conn, Error> {
        let (host, port) = match &route.proxy {
            Some(p) => (p.url.host(), p.url.port()),
            None => (target.host(), target.port()),
        };
        let tcp = connect_tcp(host, port, self.options.connect_timeout, deadline)?;
        self.opened.fetch_add(1, Ordering::Relaxed);
        let _ = tcp.set_nodelay(true);
        let mut conn = Conn::new(Transport::Plain(tcp));
        if target.scheme() == Scheme::Http {
            return Ok(conn);
        }
        if let Some(p) = &route.proxy {
            Self::tunnel(&mut conn, target, p, user_agent, deadline)?;
        }
        let Transport::Plain(tcp) = conn.io else {
            // `conn` was created as plain TCP above.
            return Err(Error::Io(io::Error::other("unexpected transport")));
        };
        let t = remaining(deadline)?;
        tcp.set_read_timeout(t).map_err(Error::Io)?;
        tcp.set_write_timeout(t).map_err(Error::Io)?;
        let mut config = dl_tls::Config::new(target.host()).alpn(&[b"http/1.1".as_slice()]);
        if self.options.danger_accept_invalid_certs {
            config = config.verify(dl_tls::Verify::None);
        }
        let tls = dl_tls::TlsStream::connect(&config, tcp).map_err(tls_error)?;
        Ok(Conn::new(Transport::Tls(Box::new(tls))))
    }

    /// Opens a `CONNECT` tunnel through an HTTP proxy.
    fn tunnel(
        conn: &mut Conn,
        target: &Url,
        proxy: &ProxyRoute,
        user_agent: Option<&str>,
        deadline: Option<Instant>,
    ) -> Result<(), Error> {
        let authority = target.host_port();
        let mut head = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
        if let Some(auth) = &proxy.authorization {
            head.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
        }
        if let Some(ua) = user_agent {
            head.push_str(&format!("User-Agent: {ua}\r\n"));
        }
        head.push_str("\r\n");
        conn.write_all(head.as_bytes(), deadline)?;
        let mut progress = Progress::default();
        let response = conn.read_head(deadline, &mut progress)?;
        if !response.status.is_success() {
            return Err(Error::ProxyConnect {
                status: response.status.as_u16(),
            });
        }
        if !conn.buf.is_empty() {
            return Err(truncated("proxy sent data before the TLS handshake"));
        }
        Ok(())
    }

    fn lock_pool(&self) -> std::sync::MutexGuard<'_, HashMap<PoolKey, Vec<Conn>>> {
        // A panic while holding the lock cannot leave the map inconsistent.
        self.pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn checkout(&self, key: &PoolKey) -> Option<Conn> {
        let mut pool = self.lock_pool();
        let list = pool.get_mut(key)?;
        let mut found = None;
        while let Some(conn) = list.pop() {
            if conn.idle_since.elapsed() < self.options.pool_idle_timeout && conn.is_alive() {
                found = Some(conn);
                break;
            }
        }
        if list.is_empty() {
            pool.remove(key);
        }
        found
    }

    fn checkin(&self, key: PoolKey, mut conn: Conn) {
        conn.idle_since = Instant::now();
        let idle = self.options.pool_idle_timeout;
        let mut pool = self.lock_pool();
        pool.retain(|_, list| {
            list.retain(|c| c.idle_since.elapsed() < idle);
            !list.is_empty()
        });
        let list = pool.entry(key).or_default();
        if list.len() >= MAX_IDLE_PER_HOST {
            list.remove(0);
        }
        list.push(conn);
    }

    /// Closes all idle connections.
    pub fn close_idle(&self) {
        self.lock_pool().clear();
    }

    /// Sends `request` on a new connection and takes the connection over when the server
    /// answers `101 Switching Protocols` (WebSocket and other `Upgrade` protocols).
    ///
    /// The caller sets the `Connection: Upgrade` and `Upgrade` fields. The deadline
    /// ([`Request::timeout`] or [`ClientOptions::timeout`]) covers connecting and the
    /// response head; the returned [`Upgraded`] has no read or write timeout. Any other
    /// final status is read in full and returned with `None`, so the caller can report
    /// it (a `401` from a hub, for instance). Redirects are not followed.
    ///
    /// # Errors
    /// As [`Client::request`].
    pub fn upgrade(&self, request: &Request) -> Result<(Response, Option<Upgraded>), Error> {
        let user_agent = match &self.options.user_agent {
            Some(ua) => Some(validate_value(ua)?),
            None => None,
        };
        let timeout = request.timeout.or(self.options.timeout);
        let deadline = timeout.and_then(|t| Instant::now().checked_add(t));
        let route = self.route(&request.url)?;
        let mut conn = self.connect(&request.url, &route, user_agent, deadline)?;
        Self::write_request(&mut conn, request, &route, user_agent, deadline)?;
        let mut progress = Progress {
            request_written: true,
            ..Progress::default()
        };
        let response = loop {
            let r = conn.read_head(deadline, &mut progress)?;
            match r.status.as_u16() {
                101 => break r,
                100..=199 => continue,
                _ => {
                    let (response, _) = self.read_body(&mut conn, request, r, deadline)?;
                    return Ok((response, None));
                }
            }
        };
        let tcp = conn.io.tcp();
        tcp.set_read_timeout(None).map_err(Error::Io)?;
        tcp.set_write_timeout(None).map_err(Error::Io)?;
        let Conn { io, buf, .. } = conn;
        Ok((response, Some(Upgraded::new(io, buf))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadlines() {
        assert!(remaining(None).unwrap().is_none());
        let past = Instant::now();
        std::thread::sleep(Duration::from_millis(2));
        assert!(matches!(remaining(Some(past)), Err(Error::Timeout)));
        let left = remaining(Some(Instant::now() + Duration::from_secs(5)))
            .unwrap()
            .unwrap();
        assert!(left <= Duration::from_secs(5) && left > Duration::from_secs(4));
        assert_eq!(min_timeout(None, None), None);
        assert_eq!(
            min_timeout(Some(Duration::from_secs(1)), None),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            min_timeout(Some(Duration::from_secs(3)), Some(Duration::from_secs(2))),
            Some(Duration::from_secs(2))
        );
    }

    #[test]
    fn proxy_basic_auth() {
        let u = Url::parse("http://Aladdin:open%20sesame@proxy:3128").unwrap();
        assert_eq!(
            basic_auth(&u).unwrap().as_deref(),
            Some("Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==")
        );
        let u = Url::parse("http://user@proxy:3128").unwrap();
        assert_eq!(basic_auth(&u).unwrap().as_deref(), Some("Basic dXNlcjo="));
        assert_eq!(
            basic_auth(&Url::parse("http://proxy:3128").unwrap()).unwrap(),
            None
        );
    }

    #[test]
    fn error_mapping() {
        assert!(matches!(
            io_error(io::ErrorKind::TimedOut.into()),
            Error::Timeout
        ));
        assert!(matches!(
            io_error(io::ErrorKind::WouldBlock.into()),
            Error::Timeout
        ));
        assert!(matches!(
            tls_error(dl_tls::Error::Io(io::ErrorKind::TimedOut.into())),
            Error::Timeout
        ));
        assert!(matches!(
            tls_error(dl_tls::Error::Unsupported("x")),
            Error::Tls(dl_tls::Error::Unsupported(_))
        ));
        assert!(is_stale_error(&Error::Io(
            io::ErrorKind::ConnectionReset.into()
        )));
        assert!(is_stale_error(&closed_before_response()));
        assert!(!is_stale_error(&Error::Timeout));
        assert!(!is_stale_error(&truncated("x")));
    }

    #[test]
    fn client_is_send_sync() {
        fn check<T: Send + Sync>() {}
        check::<Client>();
    }
}
