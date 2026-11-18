//! The connection handle and the state it shares with the connection threads.

// Without sockets (wasm32-unknown-unknown) nothing creates or drives a connection.
#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(dead_code, unused_imports)
)]

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use dl_json::Value;

use crate::protocol::{self, HubMessage};
use crate::{CloseReason, ConnectOptions, Error, Handler};

/// Work for the connection thread.
pub(crate) enum Outgoing {
    /// An encoded message (with its separator).
    Frame(String),
    /// Close the connection.
    Close,
}

/// What an invocation receives: the result, or why there is none.
pub(crate) type Reply = Result<Value, Error>;

pub(crate) struct State {
    /// Queue to the connection thread; `None` once the connection has ended.
    out: Option<Sender<Outgoing>>,
    /// Invocations waiting for their completion, by invocation id.
    pending: HashMap<String, Sender<Reply>>,
    reason: Option<CloseReason>,
}

/// State shared by the handles and the connection threads.
pub(crate) struct Shared {
    /// Handlers by lowercased target.
    handlers: Mutex<HashMap<String, Vec<Handler>>>,
    state: Mutex<State>,
    closed: Condvar,
    next_id: AtomicU64,
}

/// Locks a mutex; a panic while it was held does not leave the protected data
/// inconsistent here (every critical section is a few map operations).
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Targets compare case-insensitively, like ASP.NET Core's `StringComparer.OrdinalIgnoreCase`.
fn handler_key(target: &str) -> String {
    target.to_lowercase()
}

impl Shared {
    pub(crate) fn new(handlers: &[(String, Handler)], out: Sender<Outgoing>) -> Shared {
        let mut map: HashMap<String, Vec<Handler>> = HashMap::new();
        for (target, h) in handlers {
            map.entry(handler_key(target)).or_default().push(h.clone());
        }
        Shared {
            handlers: Mutex::new(map),
            state: Mutex::new(State {
                out: Some(out),
                pending: HashMap::new(),
                reason: None,
            }),
            closed: Condvar::new(),
            next_id: AtomicU64::new(0),
        }
    }

    /// The handlers for `target`, if any.
    pub(crate) fn handlers_for(&self, target: &str) -> Option<Vec<Handler>> {
        lock(&self.handlers)
            .get(&handler_key(target))
            .filter(|v| !v.is_empty())
            .cloned()
    }

    /// Hands a completion to the waiting invocation; unknown ids (late completions after
    /// a timeout) are dropped.
    pub(crate) fn complete(&self, invocation_id: &str, reply: Reply) {
        let waiter = lock(&self.state).pending.remove(invocation_id);
        if let Some(w) = waiter {
            let _ = w.send(reply);
        }
    }

    /// Records why the connection ended, fails pending invocations and wakes
    /// [`HubConnection::wait_closed`]. The first reason wins.
    pub(crate) fn finish(&self, reason: CloseReason) {
        let pending = {
            let mut st = lock(&self.state);
            if st.reason.is_none() {
                st.reason = Some(reason);
            }
            st.out = None;
            std::mem::take(&mut st.pending)
        };
        self.closed.notify_all();
        for (_, w) in pending {
            let _ = w.send(Err(Error::Closed));
        }
        // Handlers often hold a clone of the connection; dropping them breaks the cycle.
        lock(&self.handlers).clear();
    }

    fn request_close(&self) {
        if let Some(out) = &lock(&self.state).out {
            let _ = out.send(Outgoing::Close);
        }
    }

    fn queue(&self, frame: String) -> Result<(), Error> {
        let st = lock(&self.state);
        match &st.out {
            Some(out) if out.send(Outgoing::Frame(frame)).is_ok() => Ok(()),
            _ => Err(Error::Closed),
        }
    }
}

/// Closes the connection when the last [`HubConnection`] clone is dropped.
struct Handle {
    shared: Arc<Shared>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.shared.request_close();
    }
}

/// A connection to an ASP.NET Core SignalR hub (JSON protocol over WebSockets).
///
/// Cheap to clone; clones share the connection, and every method takes `&self` and
/// may be called from any thread, including from inside a handler. One background
/// thread owns the WebSocket (sends queued messages, pings, reads); another runs the
/// handlers one at a time in arrival order, so a slow handler delays other handlers
/// but never pings or completions.
///
/// The connection closes on [`close`](HubConnection::close), when the server or the
/// network ends it, or when the last clone is dropped. A handler that holds a clone
/// keeps the connection open until one of the others happens. There is no automatic
/// reconnect: wait for [`wait_closed`](HubConnection::wait_closed) and connect again
/// (see the crate documentation for a loop).
#[derive(Clone)]
pub struct HubConnection {
    inner: Arc<Handle>,
}

impl fmt::Debug for HubConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HubConnection")
            .field("close_reason", &self.close_reason())
            .finish_non_exhaustive()
    }
}

impl HubConnection {
    pub(crate) fn from_shared(shared: Arc<Shared>) -> HubConnection {
        HubConnection {
            inner: Arc::new(Handle { shared }),
        }
    }

    fn shared(&self) -> &Shared {
        &self.inner.shared
    }

    /// Negotiates (unless [`ConnectOptions::negotiate`] is `false`), opens the
    /// WebSocket, runs the handshake and starts the connection threads.
    ///
    /// # Errors
    /// [`Error::InvalidUrl`], [`Error::InvalidOptions`], [`Error::NegotiateRejected`],
    /// [`Error::Negotiate`], [`Error::Http`], [`Error::WebSocket`] (including
    /// `dl_ws::Error::Rejected` for a refused upgrade), [`Error::Handshake`],
    /// [`Error::Timeout`] when the handshake response does not arrive in time, and
    /// [`Error::Unsupported`] on `wasm32-unknown-unknown`.
    pub fn connect(options: &ConnectOptions) -> Result<HubConnection, Error> {
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            crate::transport::connect(options)
        }
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        {
            options.validate()?;
            Err(Error::Unsupported)
        }
    }

    /// Registers a handler for invocations of `target` (case-insensitive). Several
    /// handlers for one target all run, in registration order. Invocations that arrive
    /// before the handler is registered are dropped; register in
    /// [`ConnectOptions::on`] to see messages sent right after the handshake.
    ///
    /// When the server asks for a result (a client result invocation with an
    /// invocation id), the handlers run and the server is told that the client
    /// provides no result.
    pub fn on(&self, target: &str, handler: impl Fn(&[Value]) + Send + Sync + 'static) {
        let mut handlers = lock(&self.shared().handlers);
        if lock(&self.shared().state).reason.is_some() {
            return; // ended: the handler would never run
        }
        handlers
            .entry(handler_key(target))
            .or_default()
            .push(Arc::new(handler));
    }

    /// Calls the hub method `target` and waits for its completion. `timeout: None`
    /// waits until the completion arrives or the connection ends.
    ///
    /// Returns the method's result, `Value::Null` for a `void` method. Numbers keep
    /// their exact text (see [`dl_json::Number::as_raw_str`]).
    ///
    /// # Errors
    /// [`Error::Hub`] when the method failed on the server, [`Error::Timeout`],
    /// [`Error::Closed`] when the connection ended before the completion arrived (the
    /// server may or may not have run the method), [`Error::StreamingNotSupported`]
    /// when the server answers with a stream.
    pub fn invoke(
        &self,
        target: &str,
        arguments: Vec<Value>,
        timeout: Option<Duration>,
    ) -> Result<Value, Error> {
        let shared = self.shared();
        let id = shared.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let frame = protocol::encode(&HubMessage::Invocation {
            invocation_id: Some(id.clone()),
            target: target.to_owned(),
            arguments,
        });
        let (tx, rx) = mpsc::channel();
        {
            let mut st = lock(&shared.state);
            let queued = st
                .out
                .as_ref()
                .is_some_and(|out| out.send(Outgoing::Frame(frame)).is_ok());
            if !queued {
                return Err(Error::Closed);
            }
            // Inserted before the lock is released, so the completion cannot be missed.
            st.pending.insert(id.clone(), tx);
        }
        let reply = match timeout {
            None => rx.recv().unwrap_or(Err(Error::Closed)),
            Some(t) => match rx.recv_timeout(t) {
                Ok(r) => r,
                Err(RecvTimeoutError::Disconnected) => Err(Error::Closed),
                Err(RecvTimeoutError::Timeout) => {
                    lock(&shared.state).pending.remove(&id);
                    // The completion may have arrived between the timeout and the removal.
                    rx.try_recv().unwrap_or(Err(Error::Timeout))
                }
            },
        };
        reply.map_err(|e| match e {
            Error::Hub { message, .. } => Error::Hub {
                target: target.to_owned(),
                message,
            },
            e => e,
        })
    }

    /// Calls the hub method `target` without waiting (no invocation id, so the server
    /// sends no completion and does not report errors).
    ///
    /// # Errors
    /// [`Error::Closed`] when the connection has ended. A queued message is lost if the
    /// connection ends before it is written.
    pub fn send(&self, target: &str, arguments: Vec<Value>) -> Result<(), Error> {
        self.shared()
            .queue(protocol::encode(&HubMessage::Invocation {
                invocation_id: None,
                target: target.to_owned(),
                arguments,
            }))
    }

    /// Closes the connection (messages queued before are sent first) and waits until
    /// it has ended. Pending invocations fail with [`Error::Closed`]. Does nothing when
    /// the connection has already ended. Safe to call from a handler.
    pub fn close(&self) {
        self.shared().request_close();
        self.wait_closed();
    }

    /// Blocks until the connection has ended and returns why. Handlers for messages
    /// received before the end may still be running.
    pub fn wait_closed(&self) -> CloseReason {
        let shared = self.shared();
        let mut st = lock(&shared.state);
        loop {
            if let Some(r) = &st.reason {
                return r.clone();
            }
            st = shared
                .closed
                .wait(st)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Like [`wait_closed`](HubConnection::wait_closed) but gives up after `timeout`
    /// and returns `None` if the connection is still open.
    pub fn wait_closed_timeout(&self, timeout: Duration) -> Option<CloseReason> {
        let shared = self.shared();
        let start = Instant::now();
        let mut st = lock(&shared.state);
        loop {
            if let Some(r) = &st.reason {
                return Some(r.clone());
            }
            let left = timeout.checked_sub(start.elapsed())?;
            if left.is_zero() {
                return None;
            }
            st = shared
                .closed
                .wait_timeout(st, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Why the connection ended, or `None` while it is open.
    pub fn close_reason(&self) -> Option<CloseReason> {
        lock(&self.shared().state).reason.clone()
    }
}
