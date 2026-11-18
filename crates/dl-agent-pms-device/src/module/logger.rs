//! The agent's log lines in dl-agent's logger (target [`TARGET`]): the messages of the .NET
//! agent, without its console format, filters or service log file (dl-agent owns the log
//! file, its rotation and its level). The .NET category becomes the `category` field (its
//! last segment, e.g. `CommandProcessor`) and an exception the `exception` field.

use dl_json::Value;

use crate::agent::log::{Level, Logger, Record};

/// The `dl-log` target of every line the module writes.
pub const TARGET: &str = "pms-device";

/// A [`Logger`] writing to `dl_log::logger()`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DlLogger;

/// The dl-log level of a .NET level.
pub fn dl_level(level: Level) -> dl_log::Level {
    match level {
        Level::Trace => dl_log::Level::Trace,
        Level::Debug => dl_log::Level::Debug,
        Level::Information => dl_log::Level::Info,
        Level::Warning => dl_log::Level::Warn,
        Level::Error => dl_log::Level::Error,
        Level::Critical => dl_log::Level::Fatal,
    }
}

/// The last segment of a .NET category (`SmilePms.DeviceAgent.Agent.ResultReporter` →
/// `ResultReporter`).
pub fn short_category(category: &str) -> &str {
    category.rsplit('.').next().unwrap_or(category)
}

impl Logger for DlLogger {
    fn log(&self, record: &Record<'_>) {
        let level = dl_level(record.level);
        let lg = dl_log::logger();
        if !lg.enabled(level, TARGET) {
            return;
        }
        let mut rec = dl_log::Record::new(lg.now(), level, TARGET, record.message)
            .with_field("category", Value::from(short_category(record.category)));
        if let Some(e) = record.exception {
            rec = rec.with_field("exception", Value::from(e.to_string()));
        }
        lg.log(rec);
    }
}

/// Logs `message` at `level` under the module's own category.
pub fn module_log(level: Level, message: &str) {
    DlLogger.log(&Record {
        level,
        category: "Module",
        event_id: 0,
        message,
        exception: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DotnetError;

    #[test]
    fn levels_and_categories_map_to_dl_log() {
        assert_eq!(dl_level(Level::Information), dl_log::Level::Info);
        assert_eq!(dl_level(Level::Critical), dl_log::Level::Fatal);
        assert_eq!(dl_level(Level::Warning), dl_log::Level::Warn);
        assert_eq!(
            short_category("SmilePms.DeviceAgent.Agent.ResultReporter"),
            "ResultReporter"
        );
        assert_eq!(short_category("Module"), "Module");
        // Writing never panics, with or without an exception.
        let e = DotnetError::new("System.IO.IOException", "disk full");
        DlLogger.log(&Record {
            level: Level::Error,
            category: "x.y",
            event_id: 0,
            message: "m",
            exception: Some(&e),
        });
        module_log(Level::Information, "started");
    }
}
