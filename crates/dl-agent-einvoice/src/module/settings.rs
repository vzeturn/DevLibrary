//! The `modules.einvoice` section of the dl-agent remote config.
//!
//! ```json
//! {"modules": {"einvoice": {
//!     "enabled": true,
//!     "config_dir": "C:\\ProgramData\\MM-EInvoice",
//!     "poll_interval_seconds": 30
//! }}}
//! ```
//!
//! `enabled` is read by the supervisor (the module is opt-in). The sealed `config.enc`
//! (platform URL, API key, SQL connection strings) stays where the Go agent wrote it, so a
//! site that already runs the Go agent switches over without enrolling again.

use std::path::PathBuf;
use std::time::Duration;

use dl_json::Value;

/// The module's settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Where `config.enc` lives.
    pub config_dir: PathBuf,
    /// Overrides the sealed config's poll interval when set.
    pub poll_interval: Option<Duration>,
}

impl Settings {
    /// Reads the section; absent or wrongly typed keys take their defaults.
    pub fn from_config(v: &Value) -> Settings {
        let config_dir = v
            .get("config_dir")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map_or_else(default_config_dir, PathBuf::from);
        let poll_interval = v
            .get("poll_interval_seconds")
            .and_then(Value::as_u64)
            .filter(|s| *s > 0)
            .map(Duration::from_secs);
        Settings {
            config_dir,
            poll_interval,
        }
    }
}

/// The Go agent's `defaultConfigDir`: machine-wide, so enroll (admin) and the service
/// (LocalSystem) share it.
pub fn default_config_dir() -> PathBuf {
    if let Some(pd) = std::env::var_os("ProgramData").filter(|v| !v.is_empty()) {
        return PathBuf::from(pd).join("MM-EInvoice");
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("mm-einvoice-agent")
}

#[cfg(test)]
mod tests {
    use dl_json::json;

    use super::*;

    #[test]
    fn defaults_follow_the_go_agent() {
        let s = Settings::from_config(&Value::Null);
        assert_eq!(s.config_dir, default_config_dir());
        assert_eq!(s.poll_interval, None);
        let d = default_config_dir();
        assert!(
            d.ends_with("MM-EInvoice") || d.ends_with("mm-einvoice-agent"),
            "{}",
            d.display()
        );
    }

    #[test]
    fn overrides() {
        let s = Settings::from_config(&json!({
            "enabled": true,
            "config_dir": " /srv/einvoice ",
            "poll_interval_seconds": 45
        }));
        assert_eq!(s.config_dir, PathBuf::from("/srv/einvoice"));
        assert_eq!(s.poll_interval, Some(Duration::from_secs(45)));
    }

    #[test]
    fn bad_values_fall_back() {
        let s = Settings::from_config(&json!({
            "config_dir": "",
            "poll_interval_seconds": 0
        }));
        assert_eq!(s.config_dir, default_config_dir());
        assert_eq!(s.poll_interval, None);
        let s = Settings::from_config(&json!({"poll_interval_seconds": "30"}));
        assert_eq!(s.poll_interval, None);
    }
}
