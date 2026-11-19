//! The agent identity file `config.dpapi`, shared with the .NET agent and installer.
//!
//! The file holds the `System.Text.Json` bytes of
//! `{"HubBaseUrl","AgentToken","HotelId","SmileConnectionString","OriginCode"}` sealed
//! with DPAPI (LocalMachine scope, entropy `MM.Membership.Agent.v1`). Saving over an
//! existing file first copies it to `config.dpapi.bak-{yyyyMMddTHHmmssZ}`, as
//! `AgentSecureConfig.Save` does.
//!
//! The sealing is behind [`Sealer`] so tests (and non-Windows builds) can run the same
//! code without DPAPI.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dl_time::OffsetDateTime;
use dl_uuid::Uuid;

use crate::stj::J;
use crate::wire::parse;

/// The DPAPI entropy of the .NET agent.
pub const ENTROPY: &[u8] = b"MM.Membership.Agent.v1";
/// The file name inside the program-data directory.
pub const FILE_NAME: &str = "config.dpapi";
/// The .NET agent's program-data directory name.
pub const DIR_NAME: &str = "MM.Membership.Agent";

/// `AgentConfig`: who this agent is and where its data lives.
#[derive(Clone, PartialEq, Eq)]
pub struct AgentConfig {
    /// The hub address.
    pub hub_base_url: String,
    /// The bearer token (secret).
    pub agent_token: String,
    /// The hotel id.
    pub hotel_id: Uuid,
    /// The SMILE connection string (secret: it carries the SQL password).
    pub smile_connection_string: String,
    /// The SMILE origin code for bookings the hub writes.
    pub origin_code: String,
}

impl fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentConfig")
            .field("hub_base_url", &self.hub_base_url)
            .field("agent_token", &"<redacted>")
            .field("hotel_id", &self.hotel_id)
            .field("smile_connection_string", &"<redacted>")
            .field("origin_code", &self.origin_code)
            .finish()
    }
}

impl AgentConfig {
    /// The plaintext the .NET agent seals (`JsonSerializer.SerializeToUtf8Bytes(cfg)`).
    pub fn to_json(&self) -> String {
        J::Obj(vec![
            ("HubBaseUrl", J::str(&self.hub_base_url)),
            ("AgentToken", J::str(&self.agent_token)),
            ("HotelId", J::uuid(self.hotel_id)),
            (
                "SmileConnectionString",
                J::str(&self.smile_connection_string),
            ),
            ("OriginCode", J::str(&self.origin_code)),
        ])
        .to_json()
    }

    /// Parses the plaintext.
    pub fn from_json(plain: &[u8]) -> Result<AgentConfig, ConfigError> {
        let v = parse::json(plain).map_err(|_| ConfigError::Corrupt("not JSON"))?;
        let s = |k: &str| parse::str_prop(&v, k);
        Ok(AgentConfig {
            hub_base_url: s("HubBaseUrl").ok_or(ConfigError::Corrupt("HubBaseUrl missing"))?,
            agent_token: s("AgentToken").ok_or(ConfigError::Corrupt("AgentToken missing"))?,
            hotel_id: parse::uuid_prop(&v, "HotelId")
                .ok_or(ConfigError::Corrupt("HotelId missing"))?,
            smile_connection_string: s("SmileConnectionString")
                .ok_or(ConfigError::Corrupt("SmileConnectionString missing"))?,
            origin_code: s("OriginCode").unwrap_or_default(),
        })
    }
}

/// Why the identity file could not be read or written. Messages never carry secrets.
#[derive(Debug)]
pub enum ConfigError {
    /// File I/O failed.
    Io(&'static str, std::io::Error),
    /// DPAPI (or the test sealer) refused.
    Seal(String),
    /// The plaintext is not a valid `AgentConfig`.
    Corrupt(&'static str),
    /// A backup with the same timestamp already exists.
    BackupExists(PathBuf),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(what, e) => write!(f, "{what}: {e}"),
            ConfigError::Seal(e) => write!(f, "DPAPI: {e}"),
            ConfigError::Corrupt(why) => write!(f, "config.dpapi is corrupt: {why}"),
            ConfigError::BackupExists(p) => write!(f, "backup {} already exists", p.display()),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Seals and unseals the identity file.
pub trait Sealer: Send + Sync {
    /// Seals `plain`.
    fn seal(&self, plain: &[u8]) -> Result<Vec<u8>, String>;
    /// Unseals `blob`.
    fn unseal(&self, blob: &[u8]) -> Result<Vec<u8>, String>;
}

/// DPAPI, LocalMachine scope, entropy [`ENTROPY`]. Fails off Windows.
#[derive(Debug, Default, Clone, Copy)]
pub struct Dpapi;

impl Sealer for Dpapi {
    fn seal(&self, plain: &[u8]) -> Result<Vec<u8>, String> {
        dl_winsvc::dpapi::protect_with_entropy(
            plain,
            dl_winsvc::dpapi::Scope::LocalMachine,
            ENTROPY,
        )
        .map_err(|e| e.to_string())
    }

    fn unseal(&self, blob: &[u8]) -> Result<Vec<u8>, String> {
        dl_winsvc::dpapi::unprotect_with_entropy(
            blob,
            dl_winsvc::dpapi::Scope::LocalMachine,
            ENTROPY,
        )
        .map_err(|e| e.to_string())
    }
}

/// `%ProgramData%\MM.Membership.Agent` (Windows), or `fallback` elsewhere.
pub fn default_dir(fallback: &Path) -> PathBuf {
    if cfg!(windows) {
        let base = std::env::var_os("ProgramData")
            .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
        base.join(DIR_NAME)
    } else {
        fallback.to_path_buf()
    }
}

/// Reads and writes `config.dpapi` in one directory.
#[derive(Clone)]
pub struct ConfigStore {
    dir: PathBuf,
    sealer: Arc<dyn Sealer>,
}

impl fmt::Debug for ConfigStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigStore")
            .field("dir", &self.dir)
            .finish()
    }
}

impl ConfigStore {
    /// A store in `dir` sealed by `sealer`.
    pub fn new(dir: impl Into<PathBuf>, sealer: Arc<dyn Sealer>) -> ConfigStore {
        ConfigStore {
            dir: dir.into(),
            sealer,
        }
    }

    /// The directory (also home of the cursor file).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The identity file.
    pub fn path(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }

    /// Whether the identity file exists.
    pub fn exists(&self) -> bool {
        self.path().is_file()
    }

    /// The configuration, or `None` when the file does not exist.
    pub fn load(&self) -> Result<Option<AgentConfig>, ConfigError> {
        let blob = match std::fs::read(self.path()) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(ConfigError::Io("read config.dpapi", e)),
        };
        let mut plain = self.sealer.unseal(&blob).map_err(ConfigError::Seal)?;
        let cfg = AgentConfig::from_json(&plain);
        plain.iter_mut().for_each(|b| *b = 0);
        cfg.map(Some)
    }

    /// Writes `cfg`, backing up an existing file first. Returns the backup path, if any.
    pub fn save(&self, cfg: &AgentConfig) -> Result<Option<PathBuf>, ConfigError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| ConfigError::Io("create dir", e))?;
        let path = self.path();
        let mut backup = None;
        if path.is_file() {
            let n = OffsetDateTime::now_utc().datetime();
            let stamp = format!(
                "{:04}{:02}{:02}T{:02}{:02}{:02}",
                n.year(),
                n.month().number(),
                n.day(),
                n.hour(),
                n.minute(),
                n.second()
            );
            let b = self.dir.join(format!("{FILE_NAME}.bak-{stamp}Z"));
            if b.exists() {
                return Err(ConfigError::BackupExists(b));
            }
            std::fs::copy(&path, &b).map_err(|e| ConfigError::Io("back up config.dpapi", e))?;
            backup = Some(b);
        }
        let mut plain = cfg.to_json().into_bytes();
        let sealed = self.sealer.seal(&plain);
        plain.iter_mut().for_each(|b| *b = 0);
        let blob = sealed.map_err(ConfigError::Seal)?;
        let tmp = self.dir.join(format!("{FILE_NAME}.tmp"));
        std::fs::write(&tmp, &blob).map_err(|e| ConfigError::Io("write config.dpapi", e))?;
        std::fs::rename(&tmp, &path).map_err(|e| ConfigError::Io("replace config.dpapi", e))?;
        Ok(backup)
    }
}

/// A reversible test sealer (XOR with the entropy). Not secret; for tests and
/// non-Windows development only.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlainSealer;

impl Sealer for PlainSealer {
    fn seal(&self, plain: &[u8]) -> Result<Vec<u8>, String> {
        Ok(plain
            .iter()
            .zip(ENTROPY.iter().cycle())
            .map(|(a, b)| a ^ b)
            .collect())
    }

    fn unseal(&self, blob: &[u8]) -> Result<Vec<u8>, String> {
        self.seal(blob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AgentConfig {
        AgentConfig {
            hub_base_url: "https://hub.example.test/".into(),
            agent_token: "s3cr3t-token-value".into(),
            hotel_id: "0f8fad5b-d9cb-469f-a165-70867728950e".parse().unwrap(),
            smile_connection_string: "Server=x;Database=y".into(),
            origin_code: "MM".into(),
        }
    }

    #[test]
    fn save_then_load_round_trips_and_backs_up() {
        let dir = std::env::temp_dir().join(format!("mb-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = ConfigStore::new(&dir, Arc::new(PlainSealer));
        assert!(store.load().unwrap().is_none());
        assert!(store.save(&cfg()).unwrap().is_none());
        assert_eq!(store.load().unwrap(), Some(cfg()));
        let backup = store.save(&cfg()).unwrap().expect("backup");
        assert!(
            backup
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("config.dpapi.bak-")
        );
        assert!(!format!("{:?}", cfg()).contains("s3cr3t"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_blob_the_sealer_rejects_is_an_error_not_an_empty_config() {
        struct Refuse;
        impl Sealer for Refuse {
            fn seal(&self, p: &[u8]) -> Result<Vec<u8>, String> {
                Ok(p.to_vec())
            }
            fn unseal(&self, _: &[u8]) -> Result<Vec<u8>, String> {
                Err("bad key".into())
            }
        }
        let dir = std::env::temp_dir().join(format!("mb-cfg-r-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = ConfigStore::new(&dir, Arc::new(Refuse));
        store.save(&cfg()).unwrap();
        assert!(matches!(store.load(), Err(ConfigError::Seal(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
