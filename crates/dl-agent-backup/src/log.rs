//! Logging into the agent's `dl-log` logger under target `backup`. Attributes are
//! structured fields; callers never pass passwords, tokens, connection strings or SQL with
//! values.

/// The `dl-log` target of every line this module writes.
pub const TARGET: &str = "backup";

/// Logs `msg` at `level` with `attrs`.
pub fn log(level: dl_log::Level, msg: &str, attrs: &[(&str, &str)]) {
    let lg = dl_log::logger();
    if lg.enabled(level, TARGET) {
        let mut rec = dl_log::Record::new(lg.now(), level, TARGET, msg);
        for (k, v) in attrs {
            rec = rec.with_field((*k).to_owned(), dl_log::Value::from(*v));
        }
        lg.log(rec);
    }
}

/// An info line.
pub fn info(msg: &str, attrs: &[(&str, &str)]) {
    log(dl_log::Level::Info, msg, attrs);
}

/// A warning line.
pub fn warn(msg: &str, attrs: &[(&str, &str)]) {
    log(dl_log::Level::Warn, msg, attrs);
}

/// An error line.
pub fn error(msg: &str, attrs: &[(&str, &str)]) {
    log(dl_log::Level::Error, msg, attrs);
}
