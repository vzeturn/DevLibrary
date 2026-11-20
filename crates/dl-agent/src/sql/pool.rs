//! A bounded, self-healing connection pool, generic over the connection so it is testable
//! without a server.
//!
//! * At most `max_connections` sessions per database; callers wait up to `acquire_timeout`
//!   for a free one, then get [`Error::Sql`] instead of piling up.
//! * Idle sessions are reused newest-first; one that sat idle longer than `ping_after_idle`
//!   is pinged before it is handed out, and one older than `max_lifetime` or idle longer
//!   than `idle_timeout` is closed.
//! * A broken session, or one returned inside an open transaction, is closed on return
//!   (closing the session makes the server roll the transaction back).
//! * When connecting fails the pool goes *down*: callers fail fast with the last error
//!   until the backoff (1 s doubling to 60 s) allows one new attempt, so a dead server
//!   costs one connect attempt per backoff step, not one per caller.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use dl_json::{Value, json};

use crate::{Error, now};

/// Opens sessions for a pool.
pub trait Connector: Send + Sync + 'static {
    /// The session type.
    type Conn: Connection;
    /// Opens a new session.
    fn connect(&self) -> Result<Self::Conn, Error>;
    /// How sessions are opened, for stats (`transport` key); `null` by default.
    fn describe(&self) -> Value {
        Value::Null
    }
}

/// What the pool needs from a session.
pub trait Connection: Send + 'static {
    /// A cheap round trip proving the session still works.
    fn ping(&mut self) -> Result<(), Error>;
    /// `true` after an unrecoverable I/O or protocol failure.
    fn is_broken(&self) -> bool;
    /// `true` while a transaction is open.
    fn in_transaction(&self) -> bool;
    /// Sets the per-request timeout (`None` = wait forever).
    fn set_query_timeout(&mut self, timeout: Option<Duration>) -> Result<(), Error>;
    /// Runs a statement written by the agent itself (bundle install: `BEGIN TRANSACTION`,
    /// `CREATE PROCEDURE …`, extended properties), parameters bound as `nvarchar`.
    fn run(&mut self, sql: &str, params: &[(&str, &str)]) -> Result<(), Error> {
        let _ = (sql, params);
        Err(Error::Sql("this connection cannot run statements".into()))
    }
    /// Runs a read-only statement written by the agent itself (survey, schema probe), with
    /// `(name, text)` parameters bound as `nvarchar`, and returns the first result set as
    /// `[{column: value}]`. Values a site or the hub supplies always go in `params`.
    fn query_json(&mut self, sql: &str, params: &[(&str, &str)]) -> Result<Vec<Value>, Error> {
        let _ = (sql, params);
        Err(Error::Sql(
            "this connection cannot run introspection queries".into(),
        ))
    }
    /// The TDS version the server accepted at login, when the session knows it.
    fn tds_version(&self) -> Option<u32> {
        None
    }
    /// Whether the whole session is encrypted (`false`: the login only), when it knows.
    fn encrypted(&self) -> Option<bool> {
        None
    }
}

/// Pool limits and timeouts.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolOptions {
    /// Most sessions open at once.
    pub max_connections: usize,
    /// How long a caller waits for a free session.
    pub acquire_timeout: Duration,
    /// Per-request timeout set on every checkout (`None` = none).
    pub query_timeout: Option<Duration>,
    /// Idle sessions older than this are closed.
    pub idle_timeout: Duration,
    /// Sessions older than this are closed when returned or found idle.
    pub max_lifetime: Duration,
    /// Idle sessions older than this are pinged before reuse.
    pub ping_after_idle: Duration,
    /// First retry delay after a failed connect.
    pub backoff_min: Duration,
    /// Longest retry delay.
    pub backoff_max: Duration,
}

impl Default for PoolOptions {
    fn default() -> PoolOptions {
        PoolOptions {
            max_connections: 4,
            acquire_timeout: Duration::from_secs(10),
            query_timeout: Some(Duration::from_secs(15)),
            idle_timeout: Duration::from_secs(300),
            max_lifetime: Duration::from_secs(1800),
            ping_after_idle: Duration::from_secs(30),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
        }
    }
}

struct Idle<T> {
    conn: T,
    created: Instant,
    returned: Instant,
}

struct Down {
    since: u64,
    error: String,
    backoff: Duration,
    retry_at: Instant,
}

struct State<T> {
    idle: Vec<Idle<T>>,
    open: usize,
    down: Option<Down>,
    opts: PoolOptions,
}

#[derive(Default)]
struct Counters {
    connects: AtomicU64,
    connect_failures: AtomicU64,
    acquires: AtomicU64,
    waits: AtomicU64,
    timeouts: AtomicU64,
    discarded: AtomicU64,
}

/// A pool of sessions to one database.
pub struct Pool<C: Connector> {
    name: String,
    connector: C,
    state: Mutex<State<C::Conn>>,
    freed: Condvar,
    counters: Counters,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl<C: Connector> Pool<C> {
    /// An empty pool; sessions open on first use.
    pub fn new(name: &str, connector: C, opts: PoolOptions) -> Pool<C> {
        Pool {
            name: name.into(),
            connector,
            state: Mutex::new(State {
                idle: Vec::new(),
                open: 0,
                down: None,
                opts,
            }),
            freed: Condvar::new(),
            counters: Counters::default(),
        }
    }

    /// The database name the pool was registered under.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The current options.
    pub fn options(&self) -> PoolOptions {
        lock(&self.state).opts.clone()
    }

    /// Replaces the options; sessions above the new maximum close as they come back.
    pub fn set_options(&self, opts: PoolOptions) {
        lock(&self.state).opts = opts;
        self.freed.notify_all();
    }

    /// Takes a session, opening one when none is idle and the pool has room.
    ///
    /// # Errors
    /// [`Error::Sql`] when the database is down (fail fast until the next retry), the
    /// connect fails, or no session frees up within `acquire_timeout`.
    pub fn get(&self) -> Result<Pooled<'_, C>, Error> {
        self.counters.acquires.fetch_add(1, Ordering::Relaxed);
        let mut state = lock(&self.state);
        let deadline = Instant::now() + state.opts.acquire_timeout;
        let mut waited = false;
        loop {
            if let Some(idle) = self.take_idle(&mut state) {
                let opts = state.opts.clone();
                drop(state);
                match self.checkout(idle, &opts) {
                    Some(conn) => return Ok(conn),
                    None => {
                        state = lock(&self.state);
                        continue;
                    }
                }
            }
            if state.open < state.opts.max_connections {
                if let Some(down) = &mut state.down {
                    if Instant::now() < down.retry_at {
                        return Err(self.down_error(&down.error));
                    }
                    // This caller probes; the others keep failing fast meanwhile.
                    down.retry_at = Instant::now() + down.backoff;
                }
                state.open += 1;
                let opts = state.opts.clone();
                drop(state);
                return self.open_new(&opts);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.counters.timeouts.fetch_add(1, Ordering::Relaxed);
                let max = state.opts.max_connections;
                return Err(Error::Sql(format!(
                    "{}: all {max} connections busy for {:?}",
                    self.name, state.opts.acquire_timeout
                )));
            }
            if !waited {
                waited = true;
                self.counters.waits.fetch_add(1, Ordering::Relaxed);
            }
            state = self
                .freed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Runs `f` on a session and returns it to the pool.
    ///
    /// # Errors
    /// The acquire error, or whatever `f` returns.
    pub fn with<T>(&self, f: impl FnOnce(&mut C::Conn) -> Result<T, Error>) -> Result<T, Error> {
        let mut conn = self.get()?;
        f(&mut conn)
    }

    /// Opens or reuses a session and pings it: the health check behind `sql.ping`.
    ///
    /// # Errors
    /// The acquire or ping error.
    pub fn check(&self) -> Result<Duration, Error> {
        let started = Instant::now();
        self.with(|c| c.ping())?;
        Ok(started.elapsed())
    }

    /// `Some((since_unix, error))` while connects are failing.
    pub fn down(&self) -> Option<(u64, String)> {
        lock(&self.state)
            .down
            .as_ref()
            .map(|d| (d.since, d.error.clone()))
    }

    /// Counters and gauges for heartbeats and `sql.status`.
    pub fn stats(&self) -> Value {
        let state = lock(&self.state);
        let c = &self.counters;
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        json!({
            "open": state.open as u64,
            "idle": state.idle.len() as u64,
            "in_use": (state.open - state.idle.len()) as u64,
            "max": state.opts.max_connections as u64,
            "connects": get(&c.connects),
            "connect_failures": get(&c.connect_failures),
            "acquires": get(&c.acquires),
            "waits": get(&c.waits),
            "timeouts": get(&c.timeouts),
            "discarded": get(&c.discarded),
            "down_since": state.down.as_ref().map(|d| d.since),
            "last_error": state.down.as_ref().map(|d| d.error.clone()),
            "transport": self.connector.describe(),
        })
    }

    /// Closes idle sessions past their idle or lifetime limit.
    pub fn reap(&self) {
        let mut state = lock(&self.state);
        let (idle_timeout, lifetime) = (state.opts.idle_timeout, state.opts.max_lifetime);
        let before = state.idle.len();
        state
            .idle
            .retain(|i| i.returned.elapsed() < idle_timeout && i.created.elapsed() < lifetime);
        let closed = before - state.idle.len();
        state.open -= closed;
        self.counters
            .discarded
            .fetch_add(closed as u64, Ordering::Relaxed);
        if closed > 0 {
            self.freed.notify_all();
        }
    }

    fn down_error(&self, error: &str) -> Error {
        Error::Sql(format!("{}: database unavailable: {error}", self.name))
    }

    fn take_idle(&self, state: &mut State<C::Conn>) -> Option<Idle<C::Conn>> {
        while let Some(idle) = state.idle.pop() {
            let expired = idle.returned.elapsed() >= state.opts.idle_timeout
                || idle.created.elapsed() >= state.opts.max_lifetime;
            if !expired {
                return Some(idle);
            }
            state.open -= 1;
            self.counters.discarded.fetch_add(1, Ordering::Relaxed);
        }
        None
    }

    fn checkout(&self, mut idle: Idle<C::Conn>, opts: &PoolOptions) -> Option<Pooled<'_, C>> {
        let stale = idle.returned.elapsed() >= opts.ping_after_idle;
        let ok = !idle.conn.is_broken()
            && (!stale || idle.conn.ping().is_ok())
            && idle.conn.set_query_timeout(opts.query_timeout).is_ok();
        if ok {
            return Some(Pooled {
                pool: self,
                conn: Some(idle.conn),
                created: idle.created,
            });
        }
        self.discard();
        None
    }

    fn open_new(&self, opts: &PoolOptions) -> Result<Pooled<'_, C>, Error> {
        let opened = self.connector.connect().and_then(|mut c| {
            c.set_query_timeout(opts.query_timeout)?;
            Ok(c)
        });
        match opened {
            Ok(conn) => {
                self.counters.connects.fetch_add(1, Ordering::Relaxed);
                lock(&self.state).down = None;
                Ok(Pooled {
                    pool: self,
                    conn: Some(conn),
                    created: Instant::now(),
                })
            }
            Err(e) => {
                self.counters
                    .connect_failures
                    .fetch_add(1, Ordering::Relaxed);
                let error = match e {
                    Error::Sql(m) => m,
                    other => other.to_string(),
                };
                let mut state = lock(&self.state);
                state.open -= 1;
                let backoff = match &state.down {
                    Some(d) => (d.backoff * 2).min(opts.backoff_max),
                    None => opts.backoff_min,
                };
                let since = state.down.as_ref().map_or_else(now, |d| d.since);
                state.down = Some(Down {
                    since,
                    error: error.clone(),
                    backoff,
                    retry_at: Instant::now() + backoff,
                });
                drop(state);
                self.freed.notify_one();
                Err(Error::Sql(format!(
                    "{}: connect failed: {error}",
                    self.name
                )))
            }
        }
    }

    fn discard(&self) {
        lock(&self.state).open -= 1;
        self.counters.discarded.fetch_add(1, Ordering::Relaxed);
        self.freed.notify_one();
    }

    fn give_back(&self, conn: C::Conn, created: Instant) {
        let mut state = lock(&self.state);
        let keep = !conn.is_broken()
            && !conn.in_transaction()
            && created.elapsed() < state.opts.max_lifetime
            && state.open <= state.opts.max_connections;
        if keep {
            state.idle.push(Idle {
                conn,
                created,
                returned: Instant::now(),
            });
        } else {
            state.open -= 1;
            self.counters.discarded.fetch_add(1, Ordering::Relaxed);
        }
        drop(state);
        self.freed.notify_one();
    }
}

/// A checked-out session; goes back to the pool when dropped.
pub struct Pooled<'a, C: Connector> {
    pool: &'a Pool<C>,
    conn: Option<C::Conn>,
    created: Instant,
}

impl<C: Connector> core::ops::Deref for Pooled<'_, C> {
    type Target = C::Conn;
    fn deref(&self) -> &C::Conn {
        self.conn.as_ref().expect("session present until drop")
    }
}

impl<C: Connector> core::ops::DerefMut for Pooled<'_, C> {
    fn deref_mut(&mut self) -> &mut C::Conn {
        self.conn.as_mut().expect("session present until drop")
    }
}

impl<C: Connector> Drop for Pooled<'_, C> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool.give_back(conn, self.created);
        }
    }
}
