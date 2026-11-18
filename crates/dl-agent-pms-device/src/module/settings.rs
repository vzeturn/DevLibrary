//! The `modules.pms-device` section of the dl-agent remote config.
//!
//! ```json
//! {"modules": {"pms-device": {
//!     "enabled": true,
//!     "appsettings_dir": "C:\\Program Files\\SmilePms\\DeviceAgent",
//!     "overrides": {"Adapter": "smile-lock", "Lock": {"LockKeyType": "MFCARD"}}
//! }}}
//! ```
//!
//! `enabled` is read by the supervisor (the module is opt-in). `appsettings_dir` is where
//! `appsettings.json` and `appsettings.{env}.json` are read (default: the install folder of
//! `SmilePmsDeviceAgent`). `overrides` is merged over the `Agent` section after every other
//! source, so the hub can set `Adapter` or `Lock:LockKeyType` without editing a file on the
//! workstation.

use std::path::{Path, PathBuf};

use dl_json::{Map, Value};

use crate::config::{Configuration, DotnetError, SECTION, parse_json_settings};

/// The module's settings.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Settings {
    /// Where the appsettings files are read (`None`: [`default_appsettings_dir`]).
    pub appsettings_dir: Option<PathBuf>,
    /// Keys merged over the `Agent` section (`None`: nothing).
    pub overrides: Option<Map>,
}

impl Settings {
    /// Reads the section; absent, empty or wrongly typed keys take their defaults.
    pub fn from_config(v: &Value) -> Settings {
        let appsettings_dir = v
            .get("appsettings_dir")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        let overrides = match v.get("overrides") {
            Some(Value::Object(m)) if !m.is_empty() => Some(m.clone()),
            _ => None,
        };
        Settings {
            appsettings_dir,
            overrides,
        }
    }

    /// The folder holding the appsettings files: the configured one, else
    /// [`default_appsettings_dir`] (with `module_dir` off Windows).
    pub fn appsettings_dir(&self, module_dir: &Path) -> PathBuf {
        self.appsettings_dir
            .clone()
            .unwrap_or_else(|| default_appsettings_dir(module_dir))
    }

    /// The overrides as configuration keys under `Agent:` (the .NET JSON rules: numbers keep
    /// their text, booleans become `True` / `False`).
    ///
    /// # Errors
    /// The JSON parser's exception when a key is repeated under another casing.
    pub fn override_configuration(&self) -> Result<Configuration, DotnetError> {
        let Some(map) = &self.overrides else {
            return Ok(Configuration::new());
        };
        let mut root = Map::new();
        root.insert(SECTION, Value::Object(map.clone()));
        let text = dl_json::to_string(&Value::Object(root));
        parse_json_settings(text.as_bytes())
    }
}

/// The install folder of `SmilePmsDeviceAgent` (`C:\Program Files\SmilePms\DeviceAgent`):
/// the 64-bit Program Files folder even in the x86 build, which the runbook installs there
/// too. Off Windows there is no install, so the module's own data directory is used.
pub fn default_appsettings_dir(module_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        let program_files = ["ProgramW6432", "ProgramFiles"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .find(|v| !v.trim().is_empty())
            .unwrap_or_else(|| r"C:\Program Files".to_owned());
        PathBuf::from(program_files)
            .join("SmilePms")
            .join("DeviceAgent")
    } else {
        module_dir.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_json::json;

    #[test]
    fn reads_the_dir_and_overrides_and_ignores_blank_or_wrong_types() {
        let s = Settings::from_config(&json!({
            "enabled": true,
            "appsettings_dir": " /opt/device ",
            "overrides": {"Adapter": "smile-lock", "Lock": {"LockKeyType": "MFCARD"}}
        }));
        assert_eq!(s.appsettings_dir, Some(PathBuf::from("/opt/device")));
        let c = s.override_configuration().unwrap();
        assert_eq!(c.get("agent:adapter"), Some("smile-lock"));
        assert_eq!(c.get("Agent:Lock:LockKeyType"), Some("MFCARD"));

        let s = Settings::from_config(&json!({"appsettings_dir": "", "overrides": []}));
        assert_eq!(s, Settings::default());
        assert_eq!(Settings::from_config(&Value::Null), Settings::default());
        assert_eq!(s.override_configuration().unwrap(), Configuration::new());
    }

    #[test]
    fn override_values_follow_the_dotnet_json_rules() {
        let s = Settings::from_config(&json!({
            "overrides": {"PollInterval": "00:00:05", "RunOnce": true, "Lock": {"MfLockComPort": 3}}
        }));
        let c = s.override_configuration().unwrap();
        assert_eq!(c.get("Agent:PollInterval"), Some("00:00:05"));
        assert_eq!(c.get("Agent:RunOnce"), Some("True"));
        assert_eq!(c.get("Agent:Lock:MfLockComPort"), Some("3"));
        let dup = Settings::from_config(&json!({"overrides": {"Adapter": "a", "adapter": "b"}}));
        assert!(dup.override_configuration().is_err());
    }

    #[test]
    fn default_dir_is_the_install_folder_on_windows_and_the_module_dir_elsewhere() {
        let module = Path::new("/var/lib/dl-agent/modules/pms-device");
        let d = Settings::default().appsettings_dir(module);
        if cfg!(windows) {
            assert!(d.ends_with(Path::new("SmilePms").join("DeviceAgent")));
        } else {
            assert_eq!(d, module);
        }
    }
}
