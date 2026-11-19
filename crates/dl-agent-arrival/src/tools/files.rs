//! Files `arrival-enroll` writes: the settings (merged over what the install already has),
//! the enrol record `data/dl-enroll.json` and the restricted folders, each through a
//! temporary file and a rename.

use std::path::{Path, PathBuf};

use dl_json::{Map, Value};

use crate::agent::settings::PRODUCTION_FILE;
use crate::stj;

/// The record of the last enrol, next to the identity (no secrets).
pub const ENROLL_RECORD: &str = "data/dl-enroll.json";

/// The `Agent` keys `arrival-enroll` sets; the other keys of an existing
/// `appsettings.Production.json` stay as they were.
#[derive(Debug, Default)]
pub struct AgentKeys {
    /// `CloudBaseUrl`.
    pub cloud_base_url: String,
    /// `PmsConnectionString` (a secret: never printed).
    pub pms_connection_string: String,
    /// `IdentityFile`.
    pub identity_file: PathBuf,
    /// `IdempotencyFile`.
    pub idempotency_file: PathBuf,
    /// `TenantKeyFile`, when the tenant key was imported.
    pub tenant_key_file: Option<PathBuf>,
    /// `VaultDirectory`, when the old vault stays where it was.
    pub vault_directory: Option<PathBuf>,
}

/// The new `appsettings.Production.json` text: the `Agent` keys of `existing` (when given),
/// then `keys` over them.
///
/// # Errors
/// The existing file is not a JSON object (named, never echoed).
pub fn production_settings(existing: Option<&str>, keys: &AgentKeys) -> Result<String, String> {
    let mut root = match existing {
        Some(text) => {
            let text = text.strip_prefix('\u{feff}').unwrap_or(text);
            match stj::parse(text) {
                Ok(Value::Object(m)) => m,
                _ => return Err(format!("{PRODUCTION_FILE} is not a JSON object")),
            }
        }
        None => Map::new(),
    };
    let mut agent = match root.get("Agent") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    agent.insert("CloudBaseUrl", keys.cloud_base_url.as_str());
    agent.insert("PmsConnectionString", keys.pms_connection_string.as_str());
    agent.insert("IdentityFile", path_text(&keys.identity_file));
    agent.insert("IdempotencyFile", path_text(&keys.idempotency_file));
    if let Some(p) = &keys.tenant_key_file {
        agent.insert("TenantKeyFile", path_text(p));
    }
    if let Some(p) = &keys.vault_directory {
        agent.insert("VaultDirectory", path_text(p));
    }
    root.insert("Agent", Value::Object(agent));
    let mut text = dl_json::to_string_pretty(&Value::Object(root));
    text.push('\n');
    Ok(text)
}

/// Reads `path` when it exists.
///
/// # Errors
/// Any read failure but "not found".
pub fn read_optional(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read(path) {
        Ok(b) => String::from_utf8(b)
            .map(Some)
            .map_err(|_| format!("{} is not UTF-8", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// The record of the last enrol, read back (`None` when there is none or it does not parse).
pub fn read_record(install: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(install.join(ENROLL_RECORD)).ok()?;
    stj::parse(&text).ok()
}

/// A path as the settings spell it.
pub fn path_text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Writes through a temporary file and a rename; `secret` restricts it first.
///
/// # Errors
/// The step that failed, with the path (never the content).
pub fn write_file(path: &Path, bytes: &[u8], secret: bool) -> Result<(), String> {
    let e = |what: &str, x: &dyn std::fmt::Display| format!("{what} {}: {x}", path.display());
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|x| e("create the folder of", &x))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    if secret {
        crate::agent::secrets::write_secret(&tmp, bytes, "settings").map_err(|x| e("write", &x))?;
        restrict(&tmp).map_err(|x| e("restrict", &x))?;
    } else {
        std::fs::write(&tmp, bytes).map_err(|x| e("write", &x))?;
    }
    std::fs::rename(&tmp, path).map_err(|x| e("replace", &x))
}

/// Creates `dir` closed to everyone but the service (and administrators).
///
/// # Errors
/// The folder cannot be created or restricted.
pub fn restricted_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("restrict {}: {e}", dir.display()))?;
    }
    restrict(dir)
}

/// The Windows ACL of Install.ps1 (SYSTEM and Administrators only); a no-op elsewhere,
/// where files are written 0600.
///
/// # Errors
/// The ACL cannot be set.
pub fn restrict(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        dl_winsvc::acl::restrict(path).map_err(|e| format!("restrict {}: {e}", path.display()))?;
    }
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_other_agent_keys_and_sets_ours() {
        let existing = "\u{feff}{\"Agent\":{\"ServiceName\":\"MM.Arrival.Agent\",\"TenantKey\":\"k\",\"CloudBaseUrl\":\"old\"},\"Logging\":{\"x\":1}}";
        let keys = AgentKeys {
            cloud_base_url: "https://a".into(),
            pms_connection_string: "Server=.".into(),
            identity_file: PathBuf::from("/i/data/agent-identity.bin"),
            idempotency_file: PathBuf::from("/i/data/agent-idempotency.jsonl"),
            tenant_key_file: Some(PathBuf::from("/i/data/agent-tenant-key.bin")),
            vault_directory: None,
        };
        let text = production_settings(Some(existing), &keys).unwrap();
        let v = stj::parse(&text).unwrap();
        let agent = v.get("Agent").unwrap();
        assert_eq!(
            agent.get("ServiceName").unwrap().as_str(),
            Some("MM.Arrival.Agent")
        );
        assert_eq!(agent.get("TenantKey").unwrap().as_str(), Some("k"));
        assert_eq!(
            agent.get("CloudBaseUrl").unwrap().as_str(),
            Some("https://a")
        );
        assert_eq!(
            agent.get("TenantKeyFile").unwrap().as_str(),
            Some("/i/data/agent-tenant-key.bin")
        );
        assert!(agent.get("VaultDirectory").is_none());
        assert!(v.get("Logging").is_some());
        assert!(production_settings(Some("[1]"), &keys).is_err());
        assert!(production_settings(None, &keys).is_ok());
    }
}
