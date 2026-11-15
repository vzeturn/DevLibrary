//! The logger, its builder and the global logger.

use std::cell::Cell;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use dl_json::Value;
use dl_time::{Offset, OffsetDateTime};

use crate::filter::directive_for;
use crate::record::{Context, Field, Key};
use crate::{Error, Filter, Level, LevelFilter, Record, Sink};

type Clock = Box<dyn Fn() -> OffsetDateTime + Send + Sync>;
type ErrorHook = Box<dyn Fn(&Error) + Send + Sync>;

/// Filters records and sends them to sinks.
///
/// Build one with [`Logger::builder`]. A `Logger` is a plain value: pass it (or a clone;
/// clones share everything) to the code that logs, with `info!(logger: log, …)`, or
/// install it once as the global logger with [`init`] so the macros find it without the
/// `logger:` argument.
///
/// ```
/// use dl_log::{Filter, Level, Logger, MemorySink, TextFormat, info, debug};
/// use dl_time::{Offset, OffsetDateTime};
///
/// let mem = MemorySink::new();
/// let log = Logger::builder()
///     .filter(Filter::parse("info,booking::pricing=debug")?)
///     .offset(Offset::VIETNAM)
///     .clock(|| OffsetDateTime::from_unix_millis(1_794_712_830_123).unwrap())
///     .field("service", "booking-api")
///     .sink(mem.clone())
///     .build();
///
/// info!(logger: log, target: "booking", "booking confirmed"; booking_id = 42, nights = 3);
/// debug!(logger: log, target: "booking", "not shown");
/// debug!(logger: log, target: "booking::pricing", "rate applied"; rate = 1_250_000);
///
/// assert_eq!(mem.lines(), [
///     "2026-11-15T10:20:30.123+07:00 INFO  booking confirmed booking_id=42 nights=3 service=booking-api",
///     "2026-11-15T10:20:30.123+07:00 DEBUG rate applied rate=1250000 service=booking-api",
/// ]);
/// # Ok::<(), dl_log::Error>(())
/// ```
#[derive(Clone)]
pub struct Logger {
    inner: Arc<Inner>,
}

struct Inner {
    default: AtomicU8,
    /// The most verbose level any target can log at: the fast path of `enabled`.
    max: AtomicU8,
    directives: Vec<(String, LevelFilter)>,
    sinks: Vec<(LevelFilter, Box<dyn Sink>)>,
    statics: Option<Arc<[Field]>>,
    offset: Offset,
    clock: Option<Clock>,
    on_error: Option<ErrorHook>,
    errors: AtomicU64,
    shut_down: AtomicBool,
}

/// Configures a [`Logger`]. Returned by [`Logger::builder`].
pub struct LoggerBuilder {
    filter: Filter,
    sinks: Vec<(LevelFilter, Box<dyn Sink>)>,
    statics: Vec<Field>,
    offset: Offset,
    clock: Option<Clock>,
    on_error: Option<ErrorHook>,
}

impl LoggerBuilder {
    /// Sets the default minimum level (default `Info`), keeping per-target directives.
    pub fn level(mut self, level: impl Into<LevelFilter>) -> LoggerBuilder {
        self.filter = self.filter.with_default(level);
        self
    }

    /// Replaces the filter (default level and per-target directives).
    pub fn filter(mut self, filter: Filter) -> LoggerBuilder {
        self.filter = filter;
        self
    }

    /// Adds a sink that receives every record passing the filter.
    pub fn sink(self, sink: impl Sink + 'static) -> LoggerBuilder {
        self.sink_with_level(LevelFilter::Trace, sink)
    }

    /// Adds a sink that only receives records at `level` or above, like Serilog's
    /// `restrictedToMinimumLevel`. The logger's filter applies first.
    pub fn sink_with_level(
        mut self,
        level: impl Into<LevelFilter>,
        sink: impl Sink + 'static,
    ) -> LoggerBuilder {
        self.sinks.push((level.into(), Box::new(sink)));
        self
    }

    /// Adds a field to every record, such as `service`, `version` or `env` (Serilog
    /// `Enrich.WithProperty`). Call-site and scoped fields with the same name win.
    pub fn field(mut self, key: impl Into<Key>, value: impl Into<Value>) -> LoggerBuilder {
        self.statics.push((key.into(), value.into()));
        self
    }

    /// The offset timestamps are recorded at (default UTC), for example
    /// `Offset::VIETNAM` for `+07:00`. It also decides when daily log files roll.
    pub fn offset(mut self, offset: Offset) -> LoggerBuilder {
        self.offset = offset;
        self
    }

    /// Takes timestamps from `clock` instead of the system clock (converted to the
    /// logger's offset). Use it for deterministic tests, and on
    /// `wasm32-unknown-unknown`, which has no system clock: there, pass a function that
    /// reads JavaScript's `Date.now()` through your bindings, otherwise every record is
    /// stamped `1970-01-01T00:00:00Z`.
    pub fn clock(
        mut self,
        clock: impl Fn() -> OffsetDateTime + Send + Sync + 'static,
    ) -> LoggerBuilder {
        self.clock = Some(Box::new(clock));
        self
    }

    /// Calls `hook` when a sink fails to write or flush, instead of the default, which
    /// prints the 1st, 2nd, 4th, 8th, … error to standard error (nothing on
    /// `wasm32-unknown-unknown`). The hook must not panic; records it logs through the
    /// same logger are discarded to avoid loops.
    pub fn on_error(mut self, hook: impl Fn(&Error) + Send + Sync + 'static) -> LoggerBuilder {
        self.on_error = Some(Box::new(hook));
        self
    }

    /// Creates the logger.
    pub fn build(self) -> Logger {
        let max = self.filter.max_level();
        let (default, directives) = self.filter.into_parts();
        Logger {
            inner: Arc::new(Inner {
                default: AtomicU8::new(default as u8),
                max: AtomicU8::new(max as u8),
                directives,
                sinks: self.sinks,
                statics: (!self.statics.is_empty()).then(|| Arc::from(self.statics)),
                offset: self.offset,
                clock: self.clock,
                on_error: self.on_error,
                errors: AtomicU64::new(0),
                shut_down: AtomicBool::new(false),
            }),
        }
    }
}

impl fmt::Debug for LoggerBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoggerBuilder")
            .field("filter", &self.filter.to_string())
            .field("sinks", &self.sinks.len())
            .finish()
    }
}

thread_local! {
    static IN_DISPATCH: Cell<bool> = const { Cell::new(false) };
}

/// Marks the current thread as busy writing records while alive.
struct DispatchGuard {
    reset: bool,
}

impl DispatchGuard {
    fn enter() -> Option<DispatchGuard> {
        match IN_DISPATCH.try_with(|busy| busy.replace(true)) {
            Ok(true) => None,
            Ok(false) => Some(DispatchGuard { reset: true }),
            // Thread-local storage is being torn down: no recursion tracking possible.
            Err(_) => Some(DispatchGuard { reset: false }),
        }
    }
}

impl Drop for DispatchGuard {
    fn drop(&mut self) {
        if self.reset {
            let _ = IN_DISPATCH.try_with(|busy| busy.set(false));
        }
    }
}

/// Discards records logged from the current thread from now on (used by sink worker
/// threads).
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub(crate) fn mark_sink_thread() {
    let _ = IN_DISPATCH.try_with(|busy| busy.set(true));
}

impl Logger {
    /// Starts configuring a logger: level `Info`, no sinks, UTC timestamps.
    pub fn builder() -> LoggerBuilder {
        LoggerBuilder {
            filter: Filter::default(),
            sinks: Vec::new(),
            statics: Vec::new(),
            offset: Offset::UTC,
            clock: None,
            on_error: None,
        }
    }

    /// A logger that discards everything; the global logger before [`init`].
    pub fn disabled() -> Logger {
        Logger::builder().level(LevelFilter::Off).build()
    }

    /// Returns `true` when a record at `level` from `target` would be written. This is
    /// what the macros check before evaluating their arguments: one atomic load when
    /// the level is below every filter.
    #[inline]
    pub fn enabled(&self, level: Level, target: &str) -> bool {
        let inner = &*self.inner;
        if (level as u8) < inner.max.load(Ordering::Relaxed) {
            return false;
        }
        if inner.directives.is_empty() {
            // Without directives `max` is the default level.
            return true;
        }
        directive_for(&inner.directives, target)
            .unwrap_or_else(|| LevelFilter::from_u8(inner.default.load(Ordering::Relaxed)))
            .allows(level)
    }

    /// The default level (for targets without a directive).
    pub fn level(&self) -> LevelFilter {
        LevelFilter::from_u8(self.inner.default.load(Ordering::Relaxed))
    }

    /// Changes the default level at runtime, for example from an admin endpoint.
    /// Per-target directives stay as configured. Has no effect after
    /// [`shutdown`](Self::shutdown).
    pub fn set_level(&self, level: impl Into<LevelFilter>) {
        let inner = &*self.inner;
        if inner.shut_down.load(Ordering::Acquire) {
            return;
        }
        let level = level.into();
        inner.default.store(level as u8, Ordering::Relaxed);
        let max = inner
            .directives
            .iter()
            .map(|(_, l)| *l)
            .fold(level, Ord::min);
        inner.max.store(max as u8, Ordering::Relaxed);
    }

    /// The current filter: default level and per-target directives.
    pub fn filter(&self) -> Filter {
        self.inner
            .directives
            .iter()
            .fold(Filter::new(self.level()), |f, (name, level)| {
                f.with_target(name.as_str(), *level)
            })
    }

    /// The current time from the logger's clock, at the logger's offset.
    pub fn now(&self) -> OffsetDateTime {
        let t = match &self.inner.clock {
            Some(clock) => clock(),
            None => system_now(),
        };
        t.checked_to_offset(self.inner.offset).unwrap_or(t)
    }

    /// Writes a record built by hand (for example one bridged from another logging
    /// system), if it passes the filter. The logger attaches its fields and the calling
    /// thread's scoped context, replacing `record.context`; all other fields of the
    /// record are kept as they are.
    pub fn log(&self, mut record: Record) {
        if !self.enabled(record.level, &record.target) {
            return;
        }
        record.context = Context::current_with_base(self.inner.statics.clone());
        self.dispatch(&record);
    }

    /// Number of sink errors so far.
    pub fn error_count(&self) -> u64 {
        self.inner.errors.load(Ordering::Relaxed)
    }

    /// Flushes every sink. Errors go to the error hook.
    pub fn flush(&self) {
        for (_, sink) in &self.inner.sinks {
            if let Err(source) = sink.flush() {
                self.report(Error::Sink {
                    sink: sink.name().into(),
                    source,
                });
            }
        }
    }

    /// Stops logging and shuts every sink down: buffered and queued records are
    /// written, background threads exit. Later records are discarded. Call it (or the
    /// global [`shutdown`]) at the end of `main`; a logger that is dropped does the same
    /// through its sinks' `Drop`, but the global logger is never dropped.
    pub fn shutdown(&self) {
        let inner = &*self.inner;
        if inner.shut_down.swap(true, Ordering::AcqRel) {
            return;
        }
        inner.max.store(LevelFilter::Off as u8, Ordering::Relaxed);
        for (_, sink) in &inner.sinks {
            if let Err(source) = sink.shutdown() {
                self.report(Error::Sink {
                    sink: sink.name().into(),
                    source,
                });
            }
        }
    }

    pub(crate) fn dispatch(&self, record: &Record) {
        let Some(_guard) = DispatchGuard::enter() else {
            return;
        };
        for (min, sink) in &self.inner.sinks {
            if min.allows(record.level)
                && let Err(source) = sink.write(record)
            {
                self.report(Error::Sink {
                    sink: sink.name().into(),
                    source,
                });
            }
        }
    }

    fn report(&self, error: Error) {
        let n = self
            .inner
            .errors
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        match &self.inner.on_error {
            Some(hook) => hook(&error),
            None => default_hook(&error, n),
        }
    }
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn system_now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn system_now() -> OffsetDateTime {
    OffsetDateTime::UNIX_EPOCH
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn default_hook(error: &Error, n: u64) {
    use std::io::Write;
    if n.is_power_of_two() {
        let _ = writeln!(
            std::io::stderr(),
            "dl-log: {error} ({n} sink errors so far)"
        );
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn default_hook(_error: &Error, _n: u64) {}

impl fmt::Debug for Logger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sinks: Vec<&str> = self.inner.sinks.iter().map(|(_, s)| s.name()).collect();
        f.debug_struct("Logger")
            .field("filter", &self.filter().to_string())
            .field("sinks", &sinks)
            .field("offset", &self.inner.offset)
            .field("errors", &self.error_count())
            .finish()
    }
}

static GLOBAL: OnceLock<Logger> = OnceLock::new();
static DISABLED: OnceLock<Logger> = OnceLock::new();

/// The global logger used by the macros without `logger:`; a disabled logger until
/// [`init`] is called.
#[inline]
pub fn logger() -> &'static Logger {
    match GLOBAL.get() {
        Some(logger) => logger,
        None => DISABLED.get_or_init(Logger::disabled),
    }
}

/// Installs the global logger. Call it once, early in `main`.
///
/// If a global logger is already installed it stays in place, and
/// [`Error::AlreadyInitialized`] is passed to the error hook of `logger`, which is then
/// dropped. Use [`try_init`] to handle that case yourself. This never panics.
pub fn init(logger: Logger) {
    if let Err(rejected) = GLOBAL.set(logger) {
        rejected.report(Error::AlreadyInitialized);
    }
}

/// Installs the global logger, or returns [`Error::AlreadyInitialized`].
///
/// # Errors
///
/// [`Error::AlreadyInitialized`] when a global logger is already installed.
pub fn try_init(logger: Logger) -> Result<(), Error> {
    GLOBAL.set(logger).map_err(|_| Error::AlreadyInitialized)
}

/// Changes the global logger's default level at runtime. See [`Logger::set_level`].
pub fn set_level(level: impl Into<LevelFilter>) {
    logger().set_level(level);
}

/// Flushes the global logger's sinks. See [`Logger::flush`].
pub fn flush() {
    logger().flush();
}

/// Shuts the global logger down, writing queued records. See [`Logger::shutdown`].
pub fn shutdown() {
    logger().shutdown();
}

#[doc(hidden)]
pub mod __private {
    use std::borrow::Cow;
    use std::fmt;

    use dl_json::{ToJson, Value};

    use crate::record::{Context, Field, Key};
    use crate::{Level, Logger, Record};

    pub use std::vec;
    pub use std::vec::Vec;

    /// The out-of-line part of the logging macros, after the level check.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub fn log(
        logger: &Logger,
        level: Level,
        target: &'static str,
        args: fmt::Arguments<'_>,
        template: Option<&'static str>,
        file: &'static str,
        line: u32,
        fields: Vec<Field>,
    ) {
        let message = match args.as_str() {
            Some(s) => String::from(s),
            None => fmt::format(args),
        };
        let record = Record {
            time: logger.now(),
            level,
            target: Cow::Borrowed(target),
            message,
            template,
            fields,
            context: Context::current_with_base(logger.inner.statics.clone()),
            file: Some(file),
            line: Some(line),
        };
        logger.dispatch(&record);
    }

    #[inline]
    pub fn key(name: &'static str) -> Key {
        Cow::Borrowed(name)
    }

    pub fn display<T: fmt::Display + ?Sized>(value: &T) -> Value {
        Value::String(value.to_string())
    }

    pub fn debug<T: fmt::Debug + ?Sized>(value: &T) -> Value {
        Value::String(format!("{value:?}"))
    }

    #[inline]
    pub fn value<T: ToJson + ?Sized>(value: &T) -> Value {
        value.to_json()
    }
}
