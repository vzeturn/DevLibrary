//! The `modules.pms-site` section of the dl-agent remote config.
//!
//! ```json
//! {"modules": {"pms-site": {
//!     "enabled": true,
//!     "config_path": "C:\\ProgramData\\SmilePMS\\Agent\\config.json",
//!     "state_dir": "C:\\ProgramData\\SmilePMS\\Agent\\state"
//! }}}
//! ```
//!
//! `enabled` is read by the supervisor (the module is opt-in). Both paths are optional:
//! without them the module reads the config file and state directory `SmilePmsAgent` uses
//! (`config::default_path`, then the config's own `state_dir`), so a live site switches
//! over without enrolling again.

use std::path::PathBuf;

use dl_json::Value;

/// The module's settings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    /// The PMS agent config file (`None`: `$PMS_AGENT_CONFIG` or the port's default).
    pub config_path: Option<PathBuf>,
    /// Overrides the config's state directory.
    pub state_dir: Option<PathBuf>,
}

fn path(v: &Value, key: &str) -> Option<PathBuf> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

impl Settings {
    /// Reads the section; absent, empty or wrongly typed keys take their defaults.
    pub fn from_config(v: &Value) -> Settings {
        Settings {
            config_path: path(v, "config_path"),
            state_dir: path(v, "state_dir"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_json::json;

    #[test]
    fn reads_paths_and_ignores_blank_or_wrong_types() {
        let s = Settings::from_config(&json!({
            "enabled": true,
            "config_path": " /etc/pms/config.json ",
            "state_dir": "/var/lib/pms"
        }));
        assert_eq!(s.config_path, Some(PathBuf::from("/etc/pms/config.json")));
        assert_eq!(s.state_dir, Some(PathBuf::from("/var/lib/pms")));
        let s = Settings::from_config(&json!({"config_path": "", "state_dir": 7}));
        assert_eq!(s, Settings::default());
        assert_eq!(Settings::from_config(&Value::Null), Settings::default());
    }
}
