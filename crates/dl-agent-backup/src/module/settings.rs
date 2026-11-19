//! The `modules.backup` section of the dl-agent remote config.
//!
//! ```json
//! {"modules": {"backup": {
//!     "enabled": true,
//!     "backup_dir": "D:\\SQLBackups",
//!     "concurrency": 1,
//!     "stop_grace_seconds": 10,
//!     "poll_interval_seconds": 60
//! }}}
//! ```
//!
//! `enabled` is read by the supervisor (the module is opt-in). The hub URL and agent token
//! come from the sealed enrollment `backup-enroll` writes, never from this section.

use std::time::Duration;

use dl_json::Value;

use crate::core::Settings;

/// The most backups the module runs at once, whatever the config asks.
pub const MAX_CONCURRENCY: usize = 8;

/// The longest stop grace honoured: the service manager does not wait much longer.
pub const MAX_STOP_GRACE: Duration = Duration::from_secs(60);

/// Reads the section; absent, wrongly typed or out-of-range keys take their defaults.
pub fn from_config(v: &Value) -> Settings {
    let d = Settings::default();
    let num = |k: &str| v.get(k).and_then(Value::as_u64);
    Settings {
        backup_dir: v
            .get("backup_dir")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_owned(),
        concurrency: num("concurrency")
            .filter(|n| *n >= 1)
            .map_or(d.concurrency, |n| {
                usize::try_from(n)
                    .unwrap_or(MAX_CONCURRENCY)
                    .min(MAX_CONCURRENCY)
            }),
        stop_grace: num("stop_grace_seconds")
            .map_or(d.stop_grace, |s| Duration::from_secs(s).min(MAX_STOP_GRACE)),
        poll: num("poll_interval_seconds")
            .filter(|s| *s > 0)
            .map_or(d.poll, Duration::from_secs),
        worker_poll: d.worker_poll,
    }
}

#[cfg(test)]
mod tests {
    use dl_json::json;

    use super::*;

    #[test]
    fn defaults_follow_go() {
        let s = from_config(&Value::Null);
        assert_eq!(s, Settings::default());
        assert_eq!(s.concurrency, 1);
        assert_eq!(s.poll, Duration::from_secs(60));
        assert_eq!(s.worker_poll, Duration::from_secs(5));
    }

    #[test]
    fn keys_are_read_and_clamped() {
        let s = from_config(&json!({
            "backup_dir": "  D:\\SQLBackups ",
            "concurrency": 50,
            "stop_grace_seconds": 600,
            "poll_interval_seconds": 15,
        }));
        assert_eq!(s.backup_dir, "D:\\SQLBackups");
        assert_eq!(s.concurrency, MAX_CONCURRENCY);
        assert_eq!(s.stop_grace, MAX_STOP_GRACE);
        assert_eq!(s.poll, Duration::from_secs(15));
        let s =
            from_config(&json!({"concurrency": 0, "poll_interval_seconds": 0, "backup_dir": 3}));
        assert_eq!(
            (s.concurrency, s.poll, s.backup_dir.as_str()),
            (1, Duration::from_secs(60), "")
        );
    }
}
