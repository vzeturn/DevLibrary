//! The host context of the module: the shipped `appsettings.json`, then the hub's
//! `modules.pda-sync` section bound as the C# `Agent` section, over the module's data
//! folder (`<data>/modules/pda-sync`).
//!
//! The hub section uses the C# option names, for example:
//!
//! ```text
//! {"modules": {"pda-sync": {"enabled": true, "SiteTimeZone": "SE Asia Standard Time",
//!                           "PrintRelay": {"Enabled": true, "PrinterSubnets": ["192.168.1.0/24"]}}}}
//! ```
//!
//! `DataDir` and `AutoUpdate` are not taken from the hub: the data folder is the module's,
//! and self-update is dl-agent's.

use std::path::Path;

use dl_json::{Map, Value};

use crate::config::{AgentOptions, AgentPaths, Configuration, Provider};
use crate::hosting::HostContext;

/// The agent's shipped defaults (the C# `appsettings.json`).
pub const SHIPPED_SETTINGS: &str = include_str!("../../assets/appsettings.json");

/// Keys of the hub section that belong to dl-agent, not to the `Agent` options.
const CORE_KEYS: [&str; 3] = ["enabled", "DataDir", "AutoUpdate"];

/// Builds the host context of the module over `data_dir` (created with its `logs`).
///
/// # Errors
/// A message naming the setting that does not bind, or the folder that cannot be created.
pub fn host_context(config: &Value, data_dir: &Path) -> Result<HostContext, String> {
    let mut c = Configuration::new();
    c.add(Provider::json_text("appsettings.json", SHIPPED_SETTINGS)?);
    let mut section = match config {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    for k in CORE_KEYS {
        section.remove(k);
    }
    let mut doc = Map::new();
    doc.insert("Agent", Value::Object(section));
    c.add(Provider::json_text(
        "hub modules.pda-sync",
        &dl_json::to_string(&Value::Object(doc)),
    )?);
    let mut local = Provider::new("module data folder");
    local.set("Agent:DataDir", Some(data_dir.display().to_string()));
    local.set("Agent:AutoUpdate", Some("false".to_owned()));
    c.add(local);
    let options = AgentOptions::load(&c).map_err(|e| format!("modules.pda-sync settings: {e}"))?;
    let paths = AgentPaths::new(data_dir);
    paths
        .ensure_created()
        .map_err(|e| format!("create {}: {e}", data_dir.display()))?;
    Ok(HostContext {
        configuration: c,
        options,
        paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_json::json;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("pda-sync-settings-{tag}-{}", dl_rand::u64()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn shipped_defaults_apply_and_the_data_folder_is_the_module_folder() {
        let dir = temp_dir("defaults");
        let host = host_context(&Value::Null, &dir).unwrap();
        assert_eq!(host.options.heartbeat_seconds, 15);
        assert_eq!(host.options.upload_batches_per_cycle, 8);
        assert_eq!(
            host.options.data_dir.as_deref(),
            Some(dir.display().to_string().as_str())
        );
        assert!(!host.options.auto_update);
        assert!(dir.join("logs").is_dir());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_hub_section_binds_as_the_agent_section_but_not_core_keys() {
        let dir = temp_dir("hub");
        let cfg = json!({
            "enabled": true,
            "DataDir": "C:\\elsewhere",
            "AutoUpdate": true,
            "SiteTimeZone": "SE Asia Standard Time",
            "HeartbeatSeconds": 30,
            "PrintRelay": {"Enabled": false}
        });
        let host = host_context(&cfg, &dir).unwrap();
        assert_eq!(host.options.heartbeat_seconds, 30);
        assert_eq!(
            host.options.site_time_zone.as_deref(),
            Some("SE Asia Standard Time")
        );
        assert!(!host.options.print_relay.enabled);
        assert!(!host.options.auto_update);
        assert_eq!(
            host.options.data_dir.as_deref(),
            Some(dir.display().to_string().as_str())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_setting_of_the_wrong_type_is_an_error_naming_it() {
        let dir = temp_dir("bad");
        let err = host_context(&json!({"HeartbeatSeconds": "often"}), &dir).unwrap_err();
        assert!(err.contains("HeartbeatSeconds"), "{err}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
