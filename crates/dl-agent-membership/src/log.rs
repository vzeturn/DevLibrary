//! Logging: every line goes to the agent's `dl-log` logger under target `membership`.
//!
//! Attributes are structured fields. Callers never pass tokens, connection strings,
//! SQL text, SQL Server messages or guest data (names, phones).

/// The `dl-log` target of every line this module writes.
pub const TARGET: &str = "membership";

/// A structured attribute value.
#[derive(Debug, Clone, Copy)]
pub enum V<'a> {
    /// Text.
    S(&'a str),
    /// Integer.
    I(i64),
    /// Flag.
    B(bool),
}

impl V<'_> {
    fn value(self) -> dl_log::Value {
        match self {
            V::S(s) => dl_log::Value::from(s),
            V::I(i) => dl_log::Value::from(i),
            V::B(b) => dl_log::Value::from(b),
        }
    }
}

fn log(level: dl_log::Level, msg: &str, attrs: &[(&str, V<'_>)]) {
    let lg = dl_log::logger();
    if !lg.enabled(level, TARGET) {
        return;
    }
    let mut rec = dl_log::Record::new(lg.now(), level, TARGET, msg);
    for (k, v) in attrs {
        rec = rec.with_field((*k).to_owned(), v.value());
    }
    lg.log(rec);
}

/// Logs at debug level.
pub fn debug(msg: &str, attrs: &[(&str, V<'_>)]) {
    log(dl_log::Level::Debug, msg, attrs);
}

/// Logs at info level.
pub fn info(msg: &str, attrs: &[(&str, V<'_>)]) {
    log(dl_log::Level::Info, msg, attrs);
}

/// Logs at warn level.
pub fn warn(msg: &str, attrs: &[(&str, V<'_>)]) {
    log(dl_log::Level::Warn, msg, attrs);
}

/// Logs at error level.
pub fn error(msg: &str, attrs: &[(&str, V<'_>)]) {
    log(dl_log::Level::Error, msg, attrs);
}
