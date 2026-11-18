//! Logging: every line goes to the agent's `dl-log` logger (target `einvoice`), and the
//! lines the platform wants in the heartbeat are kept in the ring buffer of Go's
//! `loghandler.go`.
//!
//! Attributes are structured fields. Callers never pass request or response bodies, SQL
//! text with values, DSNs or API keys.

use std::sync::{Mutex, OnceLock, PoisonError};

use dl_time::{Offset, OffsetDateTime};

use crate::platform::HeartbeatLog;

/// The `dl-log` target of every line this module writes.
pub const TARGET: &str = "einvoice";

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
    fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }

    fn dl(self) -> dl_log::Level {
        match self {
            Level::Debug => dl_log::Level::Debug,
            Level::Info => dl_log::Level::Info,
            Level::Warn => dl_log::Level::Warn,
            Level::Error => dl_log::Level::Error,
        }
    }
}

/// `heartbeatLogCap`.
const RING_CAP: usize = 50;

fn ring() -> &'static Mutex<Vec<HeartbeatLog>> {
    static R: OnceLock<Mutex<Vec<HeartbeatLog>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Vec::new()))
}

/// Local wall-clock time with the machine's UTC offset, like Go's `time.Now()`.
pub fn now_local() -> OffsetDateTime {
    let utc = OffsetDateTime::now_utc();
    let off = local_offset_seconds()
        .and_then(|s| Offset::from_seconds(s).ok())
        .unwrap_or(Offset::UTC);
    utc.to_offset(off)
}

/// The machine's offset from `TZ`-free sources: Vietnam installs run at +07:00, and the
/// only use is the log timestamp, so read it once from the environment override
/// `MMEI_TZ_OFFSET_MINUTES`, defaulting to +07:00.
fn local_offset_seconds() -> Option<i32> {
    static OFF: OnceLock<i32> = OnceLock::new();
    Some(*OFF.get_or_init(|| {
        std::env::var("MMEI_TZ_OFFSET_MINUTES")
            .ok()
            .and_then(|v| v.trim().parse::<i32>().ok())
            .unwrap_or(7 * 60)
            * 60
    }))
}

/// A structured attribute value.
pub enum V<'a> {
    /// Text.
    S(&'a str),
    /// Integer.
    I(i64),
    /// Flag.
    B(bool),
}

impl V<'_> {
    fn json(&self) -> dl_log::Value {
        match self {
            V::S(s) => dl_log::Value::from(*s),
            V::I(i) => dl_log::Value::from(*i),
            V::B(b) => dl_log::Value::from(*b),
        }
    }
}

/// Logs `msg` with attributes. Messages that qualify for the heartbeat (WARN and above,
/// "cycle complete", "FO schema probe") are also buffered for [`drain`].
pub fn log(level: Level, msg: &str, attrs: &[(&str, V<'_>)]) {
    let lg = dl_log::logger();
    if lg.enabled(level.dl(), TARGET) {
        let mut rec = dl_log::Record::new(lg.now(), level.dl(), TARGET, msg);
        for (k, v) in attrs {
            rec = rec.with_field((*k).to_owned(), v.json());
        }
        lg.log(rec);
    }
    if level >= Level::Warn || msg == "cycle complete" || msg == "FO schema probe" {
        let mut text = msg.to_owned();
        for (k, v) in attrs {
            text.push(' ');
            text.push_str(k);
            text.push('=');
            match v {
                V::S(s) => text.push_str(s),
                V::I(i) => text.push_str(&i.to_string()),
                V::B(b) => text.push_str(if *b { "true" } else { "false" }),
            }
        }
        let mut ring = ring().lock().unwrap_or_else(PoisonError::into_inner);
        ring.push(HeartbeatLog {
            ts: now_local(),
            level: level.as_str().to_owned(),
            msg: text,
        });
        if ring.len() > RING_CAP {
            let drop = ring.len() - RING_CAP;
            ring.drain(..drop);
        }
    }
}

/// The lines captured since the last call (for the heartbeat).
pub fn drain() -> Vec<HeartbeatLog> {
    std::mem::take(&mut *ring().lock().unwrap_or_else(PoisonError::into_inner))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_ring_keeps_warnings_and_cycle_lines_only() {
        info("logtest noise", &[]);
        warn(
            "logtest push",
            &[("status", V::I(503)), ("retry", V::B(true))],
        );
        info("cycle complete", &[("source", V::S("logtest"))]);
        let got: Vec<String> = drain()
            .into_iter()
            .map(|l| l.msg)
            .filter(|m| m.contains("logtest"))
            .collect();
        assert_eq!(
            got,
            [
                "logtest push status=503 retry=true",
                "cycle complete source=logtest"
            ]
        );
        for n in 0..(RING_CAP + 5) {
            error("e", &[("n", V::I(n as i64))]);
        }
        assert_eq!(drain().len(), RING_CAP);
    }
}
