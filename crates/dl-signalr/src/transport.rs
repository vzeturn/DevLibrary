//! Connecting (negotiate, WebSocket, handshake) and the connection threads.

use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use dl_http::{Client, Request, Upgraded};
use dl_json::Value;
use dl_ws::{CloseFrame, Message, WebSocket};

use crate::connection::{HubConnection, Outgoing, Shared};
use crate::negotiate::{self, NegotiateResponse};
use crate::protocol::{self, HubMessage, RecordReader};
use crate::{CloseReason, ConnectOptions, Error, Handler};

/// How long a socket read waits before the connection thread sends queued messages.
/// Bounds the latency of [`HubConnection::invoke`] and `send`.
const POLL: Duration = Duration::from_millis(20);

/// Negotiate redirects followed before giving up (the JavaScript client's limit).
const MAX_REDIRECTS: usize = 100;

/// How long a closing connection waits for the server's close frame.
const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// The completion error ASP.NET Core clients send when a handler gives no result.
const NO_RESULT: &str = "Client didn't provide a result.";

type Event = (Vec<Handler>, Vec<Value>);

pub(crate) fn connect(options: &ConnectOptions) -> Result<HubConnection, Error> {
    options.validate()?;
    let http = options
        .http
        .clone()
        .unwrap_or_else(|| Arc::new(Client::new()));
    let (url, token) = if options.negotiate {
        negotiate(&http, options)?
    } else {
        let token = options.access_token.clone();
        (
            negotiate::websocket_url(&options.url, None, token.as_deref())?,
            token,
        )
    };
    let mut headers = options.headers.clone();
    if let Some(t) = &token {
        headers.push(("Authorization".to_owned(), format!("Bearer {t}")));
    }
    let ws_options = dl_ws::ConnectOptions {
        headers,
        protocols: Vec::new(),
        timeout: Some(options.timeout),
        config: dl_ws::Config {
            max_message_size: options.max_message_size,
            max_frame_size: options.max_message_size,
        },
    };
    let (mut ws, _) = dl_ws::connect_with(&http, &url, &ws_options)?;
    let mut records = RecordReader::new(options.max_message_size);
    let started = handshake(&mut ws, &mut records, options.timeout).and_then(|()| {
        ws.set_read_timeout(Some(POLL))?;
        ws.set_write_timeout(Some(options.server_timeout))?;
        Ok(())
    });
    if let Err(e) = started {
        let _ = ws.get_mut().shutdown();
        return Err(e);
    }

    let (out_tx, out_rx) = mpsc::channel();
    let shared = Arc::new(Shared::new(&options.handlers, out_tx));
    let (events_tx, events_rx) = mpsc::channel::<Event>();
    thread::Builder::new()
        .name("signalr-events".into())
        .spawn(move || dispatch(&events_rx))?;
    let io = Io {
        ws,
        records,
        shared: shared.clone(),
        out: out_rx,
        events: events_tx,
        keep_alive: options.keep_alive_interval,
        server_timeout: options.server_timeout,
        last_sent: Instant::now(),
        last_received: Instant::now(),
    };
    // On failure the closure, and with it the socket and the event sender, is dropped.
    thread::Builder::new()
        .name("signalr-io".into())
        .spawn(move || io.run())?;
    Ok(HubConnection::from_shared(shared))
}

/// Runs negotiate (following redirects); returns the WebSocket URL and the access
/// token in effect.
fn negotiate(http: &Client, options: &ConnectOptions) -> Result<(String, Option<String>), Error> {
    let mut url = options.url.clone();
    let mut token = options.access_token.clone();
    for _ in 0..=MAX_REDIRECTS {
        let target = dl_http::Url::parse(&negotiate::negotiate_url(&url)?)
            .map_err(|e| Error::InvalidUrl(e.to_string()))?;
        let mut req = Request::post(target, Vec::new()).with_timeout(options.timeout);
        for (name, value) in &options.headers {
            req = req.with_header(name, value)?;
        }
        if let Some(t) = &token {
            req = req.with_header("Authorization", &format!("Bearer {t}"))?;
        }
        let resp = http.request(&req)?;
        if resp.status.as_u16() != 200 {
            return Err(Error::NegotiateRejected {
                status: resp.status.as_u16(),
                body: String::from_utf8_lossy(&resp.body).into_owned(),
            });
        }
        match negotiate::parse_negotiate_response(&resp.body)? {
            NegotiateResponse::Connect {
                connection_token, ..
            } => {
                let ws = negotiate::websocket_url(&url, Some(&connection_token), token.as_deref())?;
                return Ok((ws, token));
            }
            NegotiateResponse::Redirect {
                url: next,
                access_token,
            } => {
                url = next;
                if access_token.is_some() {
                    token = access_token;
                }
            }
        }
    }
    Err(Error::Negotiate(format!(
        "more than {MAX_REDIRECTS} redirects"
    )))
}

fn closed_by_peer(frame: Option<CloseFrame>) -> Error {
    match frame {
        Some(c) => Error::WebSocketClosed {
            code: Some(c.code),
            reason: c.reason,
        },
        None => Error::WebSocketClosed {
            code: None,
            reason: String::new(),
        },
    }
}

/// Sends the handshake request and waits for the response. Messages that arrive in
/// the same frame after the response stay in `records`.
fn handshake(
    ws: &mut WebSocket<Upgraded>,
    records: &mut RecordReader,
    timeout: Duration,
) -> Result<(), Error> {
    ws.send_text(protocol::HANDSHAKE_REQUEST)?;
    let start = Instant::now();
    loop {
        if let Some(record) = records.next_record() {
            return protocol::parse_handshake_response(record);
        }
        let left = timeout.saturating_sub(start.elapsed());
        if left.is_zero() {
            return Err(Error::Timeout);
        }
        ws.set_read_timeout(Some(left))?;
        match ws.read() {
            Ok(Message::Text(t)) => records.push(t.as_bytes())?,
            Ok(Message::Binary(b)) => records.push(&b)?,
            Ok(Message::Ping(_) | Message::Pong(_)) => {}
            Ok(Message::Close(c)) => return Err(closed_by_peer(c)),
            Err(e) if e.is_timeout() => return Err(Error::Timeout),
            Err(e) => return Err(e.into()),
        }
    }
}

/// Runs handlers one at a time until the connection thread drops its sender. A
/// panicking handler does not stop the others.
fn dispatch(events: &Receiver<Event>) {
    for (handlers, args) in events {
        for h in &handlers {
            let _ = panic::catch_unwind(AssertUnwindSafe(|| h(&args)));
        }
    }
}

/// The connection thread: owns the WebSocket.
struct Io {
    ws: WebSocket<Upgraded>,
    records: RecordReader,
    shared: Arc<Shared>,
    out: Receiver<Outgoing>,
    events: Sender<Event>,
    keep_alive: Duration,
    server_timeout: Duration,
    last_sent: Instant,
    last_received: Instant,
}

impl Io {
    fn run(mut self) {
        let reason = self.session();
        if !matches!(reason, CloseReason::Transport(_)) {
            self.close_gracefully();
        }
        let _ = self.ws.get_mut().shutdown();
        // Let the dispatcher finish the queued events and exit.
        drop(self.events);
        self.shared.finish(reason);
    }

    /// Sends a close frame and waits briefly for the server's.
    fn close_gracefully(&mut self) {
        if self.ws.close(Some(CloseFrame::normal())).is_err() {
            return;
        }
        let start = Instant::now();
        while start.elapsed() < CLOSE_GRACE {
            match self.ws.read() {
                Ok(Message::Close(_)) => return,
                Ok(_) => {}
                Err(e) if e.is_timeout() => {}
                Err(_) => return,
            }
        }
    }

    fn send(&mut self, frame: &str) -> Result<(), Error> {
        self.ws.send_text(frame)?;
        self.last_sent = Instant::now();
        Ok(())
    }

    fn session(&mut self) -> CloseReason {
        loop {
            while let Some(record) = self.records.next_record() {
                if record.trim_ascii().is_empty() {
                    continue;
                }
                let message = match protocol::parse(record) {
                    Ok(m) => m,
                    Err(e) => return CloseReason::Transport(e),
                };
                if let Some(reason) = self.handle(message) {
                    return reason;
                }
            }
            loop {
                match self.out.try_recv() {
                    Ok(Outgoing::Frame(f)) => {
                        if let Err(e) = self.send(&f) {
                            return CloseReason::Transport(e);
                        }
                    }
                    Ok(Outgoing::Close) | Err(TryRecvError::Disconnected) => {
                        return CloseReason::Local;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
            if self.last_sent.elapsed() >= self.keep_alive
                && let Err(e) = self.send(protocol::PING)
            {
                return CloseReason::Transport(e);
            }
            if self.last_received.elapsed() >= self.server_timeout {
                return CloseReason::Transport(Error::ServerTimeout);
            }
            let data = match self.ws.read() {
                Ok(Message::Text(t)) => t.into_bytes(),
                Ok(Message::Binary(b)) => b,
                Ok(Message::Ping(_) | Message::Pong(_)) => {
                    self.last_received = Instant::now();
                    continue;
                }
                Ok(Message::Close(c)) => return CloseReason::Transport(closed_by_peer(c)),
                Err(e) if e.is_timeout() => continue,
                Err(e) => return CloseReason::Transport(e.into()),
            };
            self.last_received = Instant::now();
            if let Err(e) = self.records.push(&data) {
                return CloseReason::Transport(e);
            }
        }
    }

    /// Acts on one message; returns a reason when it ends the connection.
    fn handle(&mut self, message: HubMessage) -> Option<CloseReason> {
        match message {
            HubMessage::Invocation {
                invocation_id,
                target,
                arguments,
            } => {
                if let Some(handlers) = self.shared.handlers_for(&target) {
                    let _ = self.events.send((handlers, arguments));
                }
                if let Some(id) = invocation_id {
                    let reply = protocol::encode(&HubMessage::Completion {
                        invocation_id: id,
                        result: None,
                        error: Some(NO_RESULT.to_owned()),
                    });
                    if let Err(e) = self.send(&reply) {
                        return Some(CloseReason::Transport(e));
                    }
                }
            }
            HubMessage::Completion {
                invocation_id,
                result,
                error,
            } => {
                let reply = match error {
                    Some(message) => Err(Error::Hub {
                        target: String::new(),
                        message,
                    }),
                    None => Ok(result.unwrap_or(Value::Null)),
                };
                self.shared.complete(&invocation_id, reply);
            }
            HubMessage::StreamItem { invocation_id, .. } => {
                self.shared
                    .complete(&invocation_id, Err(Error::StreamingNotSupported));
            }
            HubMessage::Close {
                error,
                allow_reconnect,
            } => {
                return Some(CloseReason::Server {
                    error,
                    allow_reconnect,
                });
            }
            HubMessage::Ping | HubMessage::Other { .. } => {}
        }
        None
    }
}
