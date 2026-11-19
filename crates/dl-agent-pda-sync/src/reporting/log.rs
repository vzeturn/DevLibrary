//! The agent's structured logger: the part of Serilog (and of
//! `Microsoft.Extensions.Logging` over it) the C# agent uses. Events carry a message
//! template with `{Name}` holes, named property values, an optional exception, and go to
//! sinks ([`RollingFileSink`](super::file_sink::RollingFileSink) with the
//! [`JsonLogFormatter`](super::format) line format, and later the site event bridge).
//!
//! ```
//! use dl_agent_pda_sync::reporting::log::{Level, Logger, LoggerConfig, MemorySink};
//! use std::sync::Arc;
//!
//! let sink = Arc::new(MemorySink::default());
//! let root = Logger::new(LoggerConfig {
//!     sinks: vec![(sink.clone(), Level::Verbose)],
//!     ..LoggerConfig::default()
//! });
//! let log = root.for_category("MM.PdaOrder.SyncAgent.Transport.SyncHttpClient");
//! log.warn("Request {Method} {Path} failed (attempt {Attempt}/{Max})", &["POST".into(), "/sync/v1/token".into(), 1.into(), 6.into()]);
//! assert_eq!(sink.events()[0].render_message(), "Request POST /sync/v1/token failed (attempt 1/6)");
//! ```

use super::template::MessageTemplate;
use crate::config::time::{SharedClock, SystemClock, Timestamp, local_offset_minutes};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// Serilog's `LogEventLevel` (MEL `Trace` maps to `Verbose`, `Critical` to `Fatal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// `Verbose` / MEL `Trace`.
    Verbose,
    /// `Debug`.
    Debug,
    /// `Information`.
    Information,
    /// `Warning`.
    Warning,
    /// `Error`.
    Error,
    /// `Fatal` / MEL `Critical`.
    Fatal,
}

/// A property value (Serilog `ScalarValue`, `SequenceValue`, `StructureValue`,
/// `DictionaryValue` and the agent's `RawJsonValue`).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `null`.
    Null,
    /// `bool`.
    Bool(bool),
    /// Any signed integer type up to `long`.
    Int(i64),
    /// `ulong` (and `uint`, `ushort`).
    UInt(u64),
    /// `double`.
    Float(f64),
    /// `decimal`, as its invariant text (`1.50`).
    Decimal(String),
    /// `string` (also a `Guid`, written as its `D` text).
    Str(String),
    /// An enum, as its name: rendered without quotes (Serilog renders an enum scalar with
    /// `ToString()`), written to JSON as a string, never lifted as `code`.
    Enum(std::borrow::Cow<'static, str>),
    /// `DateTimeOffset`.
    DateTimeOffset(Timestamp),
    /// `DateTime` of kind UTC, as nanoseconds since the Unix epoch.
    DateTimeUtc(i128),
    /// `TimeSpan`, in ticks (100 ns).
    TimeSpan(i64),
    /// Already encoded JSON (`RawJsonValue`): written as is.
    Raw(String),
    /// A sequence.
    Seq(Vec<Value>),
    /// A structure: named members in order (`{@Obj}`).
    Structure(Vec<(String, Value)>),
    /// A dictionary: key/value pairs in order.
    Dict(Vec<(Value, Value)>),
}

impl Value {
    /// Whether this is a Serilog `ScalarValue`.
    pub fn is_scalar(&self) -> bool {
        !matches!(
            self,
            Value::Raw(_) | Value::Seq(_) | Value::Structure(_) | Value::Dict(_)
        )
    }

    /// A `TimeSpan` from a [`Duration`] (sub-tick nanoseconds dropped).
    pub fn time_span(d: Duration) -> Value {
        Value::TimeSpan(i64::try_from(d.as_nanos() / 100).unwrap_or(i64::MAX))
    }

    /// A C# enum value passed as a log property, by its member name.
    pub fn enum_name(name: impl Into<std::borrow::Cow<'static, str>>) -> Value {
        Value::Enum(name.into())
    }

    /// The text of a string value.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
}

macro_rules! value_from_int {
    ($($t:ty),*) => {$(
        impl From<$t> for Value {
            fn from(v: $t) -> Value {
                Value::Int(i64::from(v))
            }
        }
    )*};
}
value_from_int!(i8, i16, i32, i64, u8, u16);

impl From<u32> for Value {
    fn from(v: u32) -> Value {
        Value::UInt(u64::from(v))
    }
}

impl From<u64> for Value {
    fn from(v: u64) -> Value {
        Value::UInt(v)
    }
}

impl From<usize> for Value {
    /// C# would pass an `int`; a `usize` above `i32::MAX` becomes a `ulong`.
    fn from(v: usize) -> Value {
        match i64::try_from(v) {
            Ok(i) => Value::Int(i),
            Err(_) => Value::UInt(v as u64),
        }
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Value {
        Value::Float(v)
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Value {
        Value::Bool(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Value {
        Value::Str(v.to_owned())
    }
}

impl From<String> for Value {
    fn from(v: String) -> Value {
        Value::Str(v)
    }
}

impl From<&String> for Value {
    fn from(v: &String) -> Value {
        Value::Str(v.clone())
    }
}

impl From<Timestamp> for Value {
    fn from(v: Timestamp) -> Value {
        Value::DateTimeOffset(v)
    }
}

impl From<Duration> for Value {
    fn from(v: Duration) -> Value {
        Value::time_span(v)
    }
}

impl From<dl_uuid::Uuid> for Value {
    fn from(v: dl_uuid::Uuid) -> Value {
        Value::Str(v.to_string())
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Value {
        v.map_or(Value::Null, Into::into)
    }
}

/// A named property of an event.
#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    /// The name as written in the template or enricher (`StreamName`, `run_id`).
    pub name: String,
    /// The value.
    pub value: Value,
}

impl Property {
    /// A property.
    pub fn new(name: impl Into<String>, value: impl Into<Value>) -> Property {
        Property {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// An exception attached to an event: the .NET type name, the message, the inner
/// exception and, for a SQL Server failure, its error facts.
#[derive(Debug, Clone, PartialEq)]
pub struct Exception {
    /// The full .NET type name, e.g. `System.InvalidOperationException`.
    pub type_name: String,
    /// The message.
    pub message: String,
    /// The inner exception.
    pub inner: Option<Box<Exception>>,
    /// A stack trace, when one is known (Rust errors have none).
    pub stack_trace: Option<String>,
    /// The SQL Server errors, for a `SqlException`.
    pub sql: Option<super::sql_error::SqlFailure>,
    /// The `HResult` that `ToString()` shows after the type name, for the
    /// `ExternalException` family (`WebSocketException (0x80004005): ...`); `None` for
    /// the others.
    pub hresult: Option<u32>,
}

impl Exception {
    /// An exception of `type_name` with `message`.
    pub fn new(type_name: impl Into<String>, message: impl Into<String>) -> Exception {
        Exception {
            type_name: type_name.into(),
            message: message.into(),
            inner: None,
            stack_trace: None,
            sql: None,
            hresult: None,
        }
    }

    /// With an inner exception.
    pub fn with_inner(mut self, inner: Exception) -> Exception {
        self.inner = Some(Box::new(inner));
        self
    }

    /// An agent failure (`AgentException`) with its source chain as inner exceptions.
    pub fn from_agent_error(e: &crate::config::AgentError) -> Exception {
        let mut ex = Exception::new(crate::config::AGENT_EXCEPTION_TYPE, e.message());
        if let Some(src) = std::error::Error::source(e) {
            ex.inner = Some(Box::new(Exception::from_error(src)));
        }
        ex
    }

    /// Any error, with its `source()` chain as inner exceptions. Rust errors carry no type
    /// name: `System.Exception` is used.
    pub fn from_error(e: &(dyn std::error::Error + 'static)) -> Exception {
        if let Some(a) = e.downcast_ref::<crate::config::AgentError>() {
            return Exception::from_agent_error(a);
        }
        if let Some(t) = e.downcast_ref::<crate::transport::Error>() {
            return t.to_exception();
        }
        let type_name = if e.is::<crate::transport::http::RequestFailure>() {
            "System.Net.Http.HttpRequestException"
        } else if e.is::<std::io::Error>() {
            "System.IO.IOException"
        } else {
            "System.Exception"
        };
        let mut ex = Exception::new(type_name, e.to_string());
        if let Some(src) = e.source() {
            ex.inner = Some(Box::new(Exception::from_error(src)));
        }
        ex
    }

    /// `Exception.ToString()`: `Type: message`, then ` ---> inner` and the
    /// end-of-inner-stack marker on the platform newline, then the stack trace. With an
    /// [`hresult`](Self::hresult), `ExternalException.ToString()`:
    /// `Type (0x80004005): message` and no end-of-inner marker.
    pub fn to_dotnet_string(&self) -> String {
        let nl = crate::util::stj::NEWLINE;
        let mut s = self.type_name.clone();
        if let Some(h) = self.hresult {
            // `ExternalException.ToString()`: the HResult after the type name.
            s.push_str(&format!(" (0x{h:08X})"));
        }
        if !self.message.is_empty() {
            s.push_str(": ");
            s.push_str(&self.message);
        }
        if let Some(inner) = &self.inner {
            s.push_str(nl);
            s.push_str(" ---> ");
            s.push_str(&inner.to_dotnet_string());
            // `ExternalException.ToString()` writes no end-of-inner marker.
            if self.hresult.is_none() {
                s.push_str(nl);
                s.push_str("   --- End of inner exception stack trace ---");
            }
        }
        if let Some(st) = &self.stack_trace {
            s.push_str(nl);
            s.push_str(st);
        }
        s
    }

    /// This exception and its inner ones, outermost first.
    pub fn chain(&self) -> impl Iterator<Item = &Exception> {
        std::iter::successors(Some(self), |e| e.inner.as_deref())
    }
}

/// One log event.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// When it was written, at the machine's local offset (`DateTimeOffset.Now`).
    pub timestamp: Timestamp,
    /// The level.
    pub level: Level,
    /// The message template.
    pub template: MessageTemplate,
    /// Properties in order: template holes first, then context and enrichers.
    pub properties: Vec<Property>,
    /// The exception, if any.
    pub exception: Option<Exception>,
}

impl Event {
    /// An event without properties.
    pub fn new(timestamp: Timestamp, level: Level, template: &str) -> Event {
        Event {
            timestamp,
            level,
            template: MessageTemplate::parse(template),
            properties: Vec::new(),
            exception: None,
        }
    }

    /// The value of property `name` (exact name).
    pub fn property(&self, name: &str) -> Option<&Value> {
        self.properties
            .iter()
            .find(|p| p.name == name)
            .map(|p| &p.value)
    }

    /// Sets a property, replacing one with the same name in place (`AddOrUpdateProperty`).
    pub fn add_or_update(&mut self, p: Property) {
        match self.properties.iter_mut().find(|x| x.name == p.name) {
            Some(slot) => slot.value = p.value,
            None => self.properties.push(p),
        }
    }

    /// Adds a property unless one with the name exists (`AddPropertyIfAbsent`).
    pub fn add_if_absent(&mut self, p: Property) {
        if !self.properties.iter().any(|x| x.name == p.name) {
            self.properties.push(p);
        }
    }

    /// The message with each property rendered plainly (`JsonLogFormatter.RenderMessage`).
    pub fn render_message(&self) -> String {
        super::format::render_message(self)
    }
}

/// Receives events (`ILogEventSink`). Implementations must not panic.
pub trait Sink: Send + Sync {
    /// Handles one event.
    fn emit(&self, event: &Event);
}

/// Keeps every event (tests, and verbs that report what they logged).
#[derive(Debug, Default)]
pub struct MemorySink(Mutex<Vec<Event>>);

impl MemorySink {
    /// A copy of the events so far.
    pub fn events(&self) -> Vec<Event> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The rendered messages of the events at `level`.
    pub fn messages_at(&self, level: Level) -> Vec<String> {
        self.events()
            .iter()
            .filter(|e| e.level == level)
            .map(Event::render_message)
            .collect()
    }
}

impl Sink for MemorySink {
    fn emit(&self, event: &Event) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event.clone());
    }
}

/// How a root logger is built (`LoggerConfiguration`).
#[derive(Clone)]
pub struct LoggerConfig {
    /// `MinimumLevel`.
    pub minimum: Level,
    /// `MinimumLevel.Override(prefix, level)`: by `SourceContext`, longest prefix first.
    pub overrides: Vec<(String, Level)>,
    /// `Enrich.WithProperty`, in order.
    pub enrich: Vec<Property>,
    /// Sinks with their `restrictedToMinimumLevel`.
    pub sinks: Vec<(Arc<dyn Sink>, Level)>,
    /// The clock events are stamped with.
    pub clock: SharedClock,
}

impl Default for LoggerConfig {
    /// Everything from `Information` up, no enrichers, no sinks, the system clock.
    fn default() -> LoggerConfig {
        LoggerConfig {
            minimum: Level::Information,
            overrides: Vec::new(),
            enrich: Vec::new(),
            sinks: Vec::new(),
            clock: SystemClock::shared(),
        }
    }
}

impl std::fmt::Debug for LoggerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoggerConfig")
            .field("minimum", &self.minimum)
            .field("overrides", &self.overrides)
            .field("enrich", &self.enrich)
            .field("sinks", &self.sinks.len())
            .finish()
    }
}

/// The `SourceContext` property name (the MEL category).
pub const SOURCE_CONTEXT: &str = "SourceContext";

struct Root {
    config: LoggerConfig,
}

/// A logger: a root configuration plus context properties (`ForContext`). Cheap to clone.
#[derive(Clone)]
pub struct Logger {
    root: Option<Arc<Root>>,
    /// Context properties, innermost (most recent `for_context`) first.
    context: Vec<Property>,
    minimum: Level,
}

impl std::fmt::Debug for Logger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Logger")
            .field("context", &self.context)
            .field("minimum", &self.minimum)
            .finish()
    }
}

impl Default for Logger {
    fn default() -> Logger {
        Logger::none()
    }
}

impl Logger {
    /// A root logger.
    pub fn new(mut config: LoggerConfig) -> Logger {
        config
            .overrides
            .sort_by_key(|o| std::cmp::Reverse(o.0.len()));
        let minimum = config.minimum;
        Logger {
            root: Some(Arc::new(Root { config })),
            context: Vec::new(),
            minimum,
        }
    }

    /// A logger that drops everything (`NullLogger`).
    pub fn none() -> Logger {
        Logger {
            root: None,
            context: Vec::new(),
            minimum: Level::Fatal,
        }
    }

    /// A child logger adding `name` to every event (`ForContext(name, value)`); a
    /// `SourceContext` applies the minimum-level overrides.
    pub fn for_context(&self, name: &str, value: impl Into<Value>) -> Logger {
        let value = value.into();
        let mut minimum = self.minimum;
        if name == SOURCE_CONTEXT
            && let (Some(root), Value::Str(ctx)) = (&self.root, &value)
        {
            minimum = root.config.minimum;
            for (prefix, level) in &root.config.overrides {
                if ctx.starts_with(prefix.as_str())
                    && (ctx.len() == prefix.len() || ctx[prefix.len()..].starts_with('.'))
                {
                    minimum = *level;
                    break;
                }
            }
        }
        let mut context = Vec::with_capacity(self.context.len() + 1);
        context.push(Property {
            name: name.to_owned(),
            value,
        });
        context.extend(self.context.iter().cloned());
        Logger {
            root: self.root.clone(),
            context,
            minimum,
        }
    }

    /// The logger of a C# `ILogger<T>` / MEL category: `SourceContext` set to the full
    /// type name, e.g. `MM.PdaOrder.SyncAgent.Config.AgentHealth`.
    pub fn for_category(&self, category: &str) -> Logger {
        self.for_context(SOURCE_CONTEXT, category)
    }

    /// Whether events at `level` would be written.
    pub fn is_enabled(&self, level: Level) -> bool {
        self.root.is_some() && level >= self.minimum
    }

    /// Writes an event: `args` bind to the template's holes in order (positionally for
    /// `{0}`-style templates), then context properties and the root enrichers are added
    /// unless already present.
    pub fn write(
        &self,
        level: Level,
        exception: Option<Exception>,
        template: &str,
        args: &[Value],
    ) {
        let Some(root) = &self.root else { return };
        if level < self.minimum {
            return;
        }
        let template = MessageTemplate::parse(template);
        let mut properties: Vec<Property> = Vec::new();
        for (name, value) in template.bind(args) {
            match properties.iter_mut().find(|p| p.name == name) {
                Some(slot) => slot.value = value,
                None => properties.push(Property { name, value }),
            }
        }
        let now = root.config.clock.now();
        let mut event = Event {
            timestamp: now.to_offset(local_offset_minutes()),
            level,
            template,
            properties,
            exception,
        };
        for p in self.context.iter().chain(root.config.enrich.iter()) {
            event.add_if_absent(p.clone());
        }
        Logger::dispatch(root, &event);
    }

    /// Sends an already built event to the sinks (after enrichment), regardless of the
    /// minimum level.
    pub fn emit(&self, mut event: Event) {
        let Some(root) = &self.root else { return };
        for p in self.context.iter().chain(root.config.enrich.iter()) {
            event.add_if_absent(p.clone());
        }
        Logger::dispatch(root, &event);
    }

    fn dispatch(root: &Root, event: &Event) {
        for (sink, min) in &root.config.sinks {
            if event.level >= *min {
                sink.emit(event);
            }
        }
    }

    /// `LogDebug`.
    pub fn debug(&self, template: &str, args: &[Value]) {
        self.write(Level::Debug, None, template, args);
    }

    /// `LogInformation`.
    pub fn info(&self, template: &str, args: &[Value]) {
        self.write(Level::Information, None, template, args);
    }

    /// `LogWarning`.
    pub fn warn(&self, template: &str, args: &[Value]) {
        self.write(Level::Warning, None, template, args);
    }

    /// `LogWarning(exception, ...)`.
    pub fn warn_ex(&self, exception: Exception, template: &str, args: &[Value]) {
        self.write(Level::Warning, Some(exception), template, args);
    }

    /// `LogError`.
    pub fn error(&self, template: &str, args: &[Value]) {
        self.write(Level::Error, None, template, args);
    }

    /// `LogError(exception, ...)`.
    pub fn error_ex(&self, exception: Exception, template: &str, args: &[Value]) {
        self.write(Level::Error, Some(exception), template, args);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_and_enrichment_follow_serilog() {
        let sink = Arc::new(MemorySink::default());
        let root = Logger::new(LoggerConfig {
            overrides: vec![("MM.PdaOrder.SyncAgent.Reporting".into(), Level::Debug)],
            enrich: vec![
                Property::new("source", "agent"),
                Property::new("run_id", "r1"),
            ],
            sinks: vec![(sink.clone(), Level::Verbose)],
            ..LoggerConfig::default()
        });
        root.for_category("MM.PdaOrder.SyncAgent.Config.AgentHealth")
            .debug("dropped", &[]);
        root.for_category("MM.PdaOrder.SyncAgent.ReportingX")
            .debug("dropped too", &[]);
        let rep = root.for_category("MM.PdaOrder.SyncAgent.Reporting.SiteEventFlusher");
        rep.debug(
            "kept {source} {A} {A}",
            &["mine".into(), 1.into(), 2.into()],
        );
        let e = &sink.events()[0];
        assert_eq!(e.render_message(), "kept mine 2 2");
        let names: Vec<_> = e.properties.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["source", "A", "SourceContext", "run_id"]);
        assert_eq!(sink.events().len(), 1);
    }

    #[test]
    fn exceptions_render_like_dotnet() {
        let ex = Exception::new("System.InvalidOperationException", "Lỗi \"x\"")
            .with_inner(Exception::new("System.TimeoutException", "bên trong"));
        let nl = crate::util::stj::NEWLINE;
        assert_eq!(
            ex.to_dotnet_string(),
            format!(
                "System.InvalidOperationException: Lỗi \"x\"{nl} ---> System.TimeoutException: bên trong{nl}   --- End of inner exception stack trace ---"
            )
        );
        let agent = crate::config::AgentError::new("X", "boom");
        assert_eq!(
            Exception::from_error(&agent).to_dotnet_string(),
            "MM.PdaOrder.SyncAgent.Config.AgentException: boom"
        );
    }

    #[test]
    fn external_exceptions_show_their_hresult_and_no_end_of_inner_marker() {
        // As `ClientWebSocket.ConnectAsync` prints it on .NET 10 (stack frames aside).
        let nl = crate::util::stj::NEWLINE;
        let mut ex = Exception::new(
            "System.Net.WebSockets.WebSocketException",
            "Unable to connect to the remote server",
        )
        .with_inner(Exception::new(
            "System.Net.Http.HttpRequestException",
            "refused",
        ));
        ex.hresult = Some(0x8000_4005);
        assert_eq!(
            ex.to_dotnet_string(),
            format!(
                "System.Net.WebSockets.WebSocketException (0x80004005): Unable to connect to the remote server{nl} ---> System.Net.Http.HttpRequestException: refused"
            )
        );
    }
}
