//! The `modules.hotellink` section of the dl-agent remote config.
//!
//! ```json
//! {"modules": {"hotellink": {"enabled": true, "config_path": "C:\\ProgramData\\SMILE-HL-Agent\\config.json"}}}
//! ```
//!
//! `enabled` is read by the supervisor (the module is opt-in). The HL `config.json` stays
//! where the Go agent and the hub's install script keep it, so a site that runs
//! `SmileHLAgent` switches over without being provisioned again.

use std::path::PathBuf;

use dl_json::Value;

use crate::config::default_config_path;

/// The module's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The HL `config.json`.
    pub config_path: PathBuf,
}

impl Settings {
    /// Reads the section: `config_path`, else `SMILEHL_AGENT_CONFIG`, else
    /// `%ProgramData%\SMILE-HL-Agent\config.json`.
    pub fn from_config(v: &Value) -> Settings {
        let config_path = v
            .get("config_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map_or_else(default_config_path, PathBuf::from);
        Settings { config_path }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_path_from_the_section() {
        let v = dl_json::parse(r#"{"enabled":true,"config_path":" D:\\hl\\config.json "}"#)
            .expect("json");
        assert_eq!(
            Settings::from_config(&v).config_path,
            PathBuf::from("D:\\hl\\config.json")
        );
    }

    #[test]
    fn default_path_without_the_key() {
        let v = dl_json::parse(r#"{"enabled":true,"config_path":""}"#).expect("json");
        assert_eq!(Settings::from_config(&v).config_path, default_config_path());
    }
}
