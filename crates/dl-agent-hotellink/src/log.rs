//! Logging: every line goes to the agent's `dl-log` logger (target `hotellink`).
//!
//! Attributes are structured fields. Callers never pass the hub token, SQL text, the
//! connection string or guest names.

/// The `dl-log` target of every line this module writes.
pub const TARGET: &str = "hotellink";

/// slog levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Diagnostics.
    Debug,
    /// Normal operation.
    Info,
    /// Something went wrong and will be retried.
    Warn,
    /// A step failed.
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
}

/// A structured attribute value.
pub enum V<'a> {
    /// Text.
    S(&'a str),
    /// Integer.
    I(i64),
}

impl V<'_> {
    fn json(&self) -> dl_log::Value {
        match self {
            V::S(s) => dl_log::Value::from(*s),
            V::I(i) => dl_log::Value::from(*i),
        }
    }
}

/// Logs `msg` with attributes.
pub fn log(level: Level, msg: &str, attrs: &[(&str, V<'_>)]) {
    let lg = dl_log::logger();
    if lg.enabled(level.dl(), TARGET) {
        let mut rec = dl_log::Record::new(lg.now(), level.dl(), TARGET, msg);
        for (k, v) in attrs {
            rec = rec.with_field((*k).to_owned(), v.json());
        }
        lg.log(rec);
    }
}

/// `log(Level::Info, ..)`.
pub fn info(msg: &str, attrs: &[(&str, V<'_>)]) {
    log(Level::Info, msg, attrs);
}

/// `log(Level::Warn, ..)`.
pub fn warn(msg: &str, attrs: &[(&str, V<'_>)]) {
    log(Level::Warn, msg, attrs);
}

/// `log(Level::Error, ..)`.
pub fn error(msg: &str, attrs: &[(&str, V<'_>)]) {
    log(Level::Error, msg, attrs);
}

/// `log(Level::Debug, ..)`.
pub fn debug(msg: &str, attrs: &[(&str, V<'_>)]) {
    log(Level::Debug, msg, attrs);
}
