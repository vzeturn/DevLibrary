//! The agent's structured JSON logger, line for line what Go's `slog.NewJSONHandler`
//! writes: `{"time":…,"level":"INFO","msg":"agent.start","component":"pms-agent",…}`.
//! Lines carry ids, codes and counts only; as a second line of defence any attribute
//! whose key names a secret is replaced with `[redacted]`.
//!
//! One difference: `time` is UTC (`…Z`). The Go agent wrote local time; this build has
//! no time zone database.
//!
//! Inside dl-agent the lines go to the agent's `dl-log` logger instead ([`Logger::agent`],
//! target [`TARGET`]), with the same redaction; dl-agent owns the file and its rotation.

use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use crate::json;

/// Key fragments that make an attribute sensitive.
const SENSITIVE: [&str; 15] = [
    "token",
    "secret",
    "password",
    "signature",
    "private",
    "authorization",
    "enrollment_code",
    "code_value",
    "args",
    "result_body",
    "results",
    "payload",
    "jwt",
    "seed",
    "sealed",
];

/// The `dl-log` target of every line the module writes.
pub const TARGET: &str = "pms-site";

/// What a sensitive attribute is replaced with.
pub const REDACTED: &str = "[redacted]";

fn is_sensitive(key: &str) -> bool {
    let k = key.to_lowercase();
    SENSITIVE.iter().any(|s| k.contains(s))
}

/// Log levels, ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// DEBUG.
    Debug,
    /// INFO.
    Info,
    /// WARN.
    Warn,
    /// ERROR.
    Error,
}

impl Level {
    fn dl(self) -> dl_log::Level {
        match self {
            Level::Debug => dl_log::Level::Debug,
            Level::Info => dl_log::Level::Info,
            Level::Warn => dl_log::Level::Warn,
            Level::Error => dl_log::Level::Error,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

/// `debug|info|warn|warning|error` (any case, trimmed); anything else is info.
pub fn parse_level(s: &str) -> Level {
    match s.trim().to_lowercase().as_str() {
        "debug" => Level::Debug,
        "warn" | "warning" => Level::Warn,
        "error" => Level::Error,
        _ => Level::Info,
    }
}

/// An attribute value.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    /// A string.
    Str(String),
    /// An integer.
    Int(i64),
    /// A bool.
    Bool(bool),
    /// A list of strings.
    Strs(Vec<String>),
}

impl From<&str> for Val {
    fn from(s: &str) -> Val {
        Val::Str(s.to_owned())
    }
}
impl From<String> for Val {
    fn from(s: String) -> Val {
        Val::Str(s)
    }
}
impl From<&String> for Val {
    fn from(s: &String) -> Val {
        Val::Str(s.clone())
    }
}
impl From<i64> for Val {
    fn from(n: i64) -> Val {
        Val::Int(n)
    }
}
impl From<i32> for Val {
    fn from(n: i32) -> Val {
        Val::Int(i64::from(n))
    }
}
impl From<u32> for Val {
    fn from(n: u32) -> Val {
        Val::Int(i64::from(n))
    }
}
impl From<usize> for Val {
    fn from(n: usize) -> Val {
        Val::Int(i64::try_from(n).unwrap_or(i64::MAX))
    }
}
impl From<u64> for Val {
    fn from(n: u64) -> Val {
        Val::Int(i64::try_from(n).unwrap_or(i64::MAX))
    }
}
impl From<bool> for Val {
    fn from(b: bool) -> Val {
        Val::Bool(b)
    }
}
impl From<Vec<String>> for Val {
    fn from(v: Vec<String>) -> Val {
        Val::Strs(v)
    }
}

enum Sink {
    /// JSON lines into a writer (tests, the `pms-migrate` tool's stderr).
    Writer(Mutex<Box<dyn Write + Send>>),
    /// The agent's `dl-log` logger, target [`TARGET`].
    Agent,
}

struct Inner {
    sink: Sink,
    level: Level,
}

/// A JSON logger; clones share the output.
#[derive(Clone)]
pub struct Logger(Option<Arc<Inner>>);

impl std::fmt::Debug for Logger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Logger")
    }
}

/// Builds one attribute list: `attrs!["agent_id" => id, "n" => 3]`.
#[macro_export]
macro_rules! attrs {
    ($($k:expr => $v:expr),* $(,)?) => {
        &[$(($k, $crate::log::Val::from($v))),*]
    };
}

impl Logger {
    /// Writes to `w` at `level` and above, tagged `component=pms-agent`.
    pub fn new(w: impl Write + Send + 'static, level: Level) -> Logger {
        Logger(Some(Arc::new(Inner {
            sink: Sink::Writer(Mutex::new(Box::new(w))),
            level,
        })))
    }

    /// Writes through the agent's `dl-log` logger at `level` and above, target
    /// [`TARGET`]; sensitive attributes are redacted the same way.
    pub fn agent(level: Level) -> Logger {
        Logger(Some(Arc::new(Inner {
            sink: Sink::Agent,
            level,
        })))
    }

    /// Drops every line.
    pub fn discard() -> Logger {
        Logger(None)
    }

    /// Logs at `level`.
    pub fn log(&self, level: Level, msg: &str, attrs: &[(&str, Val)]) {
        let Some(inner) = &self.0 else { return };
        if level < inner.level {
            return;
        }
        match &inner.sink {
            Sink::Writer(out) => {
                let line = format_line(&now_millis(), level, msg, attrs);
                let mut out = out.lock().unwrap_or_else(PoisonError::into_inner);
                // A log line that cannot be written has nowhere else to go.
                let _ = out.write_all(line.as_bytes());
                let _ = out.flush();
            }
            Sink::Agent => log_agent(level, msg, attrs),
        }
    }

    /// DEBUG.
    pub fn debug(&self, msg: &str, attrs: &[(&str, Val)]) {
        self.log(Level::Debug, msg, attrs);
    }
    /// INFO.
    pub fn info(&self, msg: &str, attrs: &[(&str, Val)]) {
        self.log(Level::Info, msg, attrs);
    }
    /// WARN.
    pub fn warn(&self, msg: &str, attrs: &[(&str, Val)]) {
        self.log(Level::Warn, msg, attrs);
    }
    /// ERROR.
    pub fn error(&self, msg: &str, attrs: &[(&str, Val)]) {
        self.log(Level::Error, msg, attrs);
    }
}

/// `2006-01-02T15:04:05.000Z`, slog's time format, in UTC.
fn now_millis() -> String {
    let t = dl_time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        t.month().number(),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.nanosecond() / 1_000_000
    )
}

fn format_line(time: &str, level: Level, msg: &str, attrs: &[(&str, Val)]) -> String {
    let mut s = String::with_capacity(128);
    s.push_str("{\"time\":");
    json::write_str(&mut s, time);
    s.push_str(",\"level\":");
    json::write_str(&mut s, level.as_str());
    s.push_str(",\"msg\":");
    json::write_str(&mut s, msg);
    s.push_str(",\"component\":\"pms-agent\"");
    for (k, v) in attrs {
        s.push(',');
        json::write_str(&mut s, k);
        s.push(':');
        if is_sensitive(k) {
            json::write_str(&mut s, REDACTED);
            continue;
        }
        match v {
            Val::Str(x) => json::write_str(&mut s, x),
            Val::Int(n) => s.push_str(&n.to_string()),
            Val::Bool(b) => s.push_str(if *b { "true" } else { "false" }),
            Val::Strs(list) => {
                s.push('[');
                for (i, x) in list.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    json::write_str(&mut s, x);
                }
                s.push(']');
            }
        }
    }
    s.push_str("}\n");
    s
}

fn log_agent(level: Level, msg: &str, attrs: &[(&str, Val)]) {
    let lg = dl_log::logger();
    if !lg.enabled(level.dl(), TARGET) {
        return;
    }
    let mut rec = dl_log::Record::new(lg.now(), level.dl(), TARGET, msg);
    for (k, v) in attrs {
        let value = if is_sensitive(k) {
            dl_json::Value::from(REDACTED)
        } else {
            match v {
                Val::Str(x) => dl_json::Value::from(x.as_str()),
                Val::Int(n) => dl_json::Value::from(*n),
                Val::Bool(b) => dl_json::Value::from(*b),
                Val::Strs(list) => dl_json::Value::Array(
                    list.iter()
                        .map(|x| dl_json::Value::from(x.as_str()))
                        .collect(),
                ),
            }
        };
        rec = rec.with_field((*k).to_owned(), value);
    }
    lg.log(rec);
}

/// `os.MkdirAll(dir, 0o700)`.
pub fn mkdir_private(dir: &Path) -> io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
}

/// A `Write` that appends into a shared buffer (tests).
#[derive(Clone, Default)]
pub struct Capture(pub Arc<Mutex<Vec<u8>>>);

impl Capture {
    /// The text written so far.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }
}

impl Write for Capture {
    fn write(&mut self, p: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(p);
        Ok(p.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_match_slog_and_redact() {
        let line = format_line(
            "2026-11-15T01:02:03.004Z",
            Level::Info,
            "agent.start",
            attrs!["agent_id" => "a1", "n" => 3i64, "Token" => "t", "op_args" => "x", "dbs" => vec!["FO".to_owned()]],
        );
        assert_eq!(
            line,
            "{\"time\":\"2026-11-15T01:02:03.004Z\",\"level\":\"INFO\",\"msg\":\"agent.start\",\"component\":\"pms-agent\",\"agent_id\":\"a1\",\"n\":3,\"Token\":\"[redacted]\",\"op_args\":\"[redacted]\",\"dbs\":[\"FO\"]}\n"
        );
        assert_eq!(parse_level(" WARNING "), Level::Warn);
        assert_eq!(parse_level("verbose"), Level::Info);
        let cap = Capture::default();
        let log = Logger::new(cap.clone(), Level::Warn);
        log.info("hidden", &[]);
        log.warn("shown", &[]);
        assert!(!cap.text().contains("hidden"));
        assert!(cap.text().contains("\"level\":\"WARN\",\"msg\":\"shown\""));
    }
}
