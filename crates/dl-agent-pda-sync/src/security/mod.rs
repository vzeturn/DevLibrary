//! Secret store and agent signing key (port of `Security/ISecretStore.cs` and
//! `Security/AgentKey.cs`).
//!
//! Secrets are files `{DataDir}\secrets\{name}.bin`. On Windows the content is a DPAPI
//! blob (machine scope, entropy `MM.PdaOrder.SyncAgent.v1`), so the service account and
//! the installing administrator can both read it; elsewhere it is the plain value in a
//! file with mode 0600.

use crate::config::{AgentError, AgentPaths, codes};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Well-known secret names (`SecretNames`).
pub mod names {
    /// The Ed25519 seed of the agent key.
    pub const SIGNING_SEED: &str = "ed25519-seed";
    /// The long-lived agent token.
    pub const AGENT_TOKEN: &str = "agent-token";
    /// The generated `pda_sync` SQL password.
    pub const SQL_PASSWORD: &str = "pda-sync-sql-password";
    /// The tenant's PIN key `K_t` for the staff sync (ADR 0101).
    pub const PIN_KEY: &str = "staff-pin-key";
}

/// The DPAPI entropy the C# store uses.
pub const DPAPI_ENTROPY: &[u8] = b"MM.PdaOrder.SyncAgent.v1";

/// Why a secret could not be loaded or saved.
#[derive(Debug)]
pub enum SecretError {
    /// The name is empty or has characters other than ASCII letters, digits, `-`, `_`
    /// (`ArgumentException: Invalid secret name.`).
    InvalidName(String),
    /// The file could not be read, written or removed.
    Io(io::Error),
    /// The stored secret cannot be decrypted on this machine (`KEY_STORE_FAILED`).
    KeyStore(AgentError),
    /// DPAPI refused to protect the value (`CryptographicException`).
    Protect(dl_winsvc::Error),
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecretError::InvalidName(_) => f.write_str("Invalid secret name. (Parameter 'name')"),
            SecretError::Io(e) => write!(f, "{e}"),
            SecretError::KeyStore(e) => write!(f, "{e}"),
            SecretError::Protect(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SecretError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SecretError::Io(e) => Some(e),
            SecretError::KeyStore(e) => Some(e),
            SecretError::Protect(e) => Some(e),
            SecretError::InvalidName(_) => None,
        }
    }
}

impl From<io::Error> for SecretError {
    fn from(e: io::Error) -> SecretError {
        SecretError::Io(e)
    }
}

impl SecretError {
    /// The coded failure, when this is one (`AgentException`).
    pub fn agent_error(&self) -> Option<&AgentError> {
        match self {
            SecretError::KeyStore(e) => Some(e),
            _ => None,
        }
    }
}

/// Named secret blobs protected at rest (`ISecretStore`).
pub trait SecretStore: Send + Sync {
    /// The value, or `None` when it was never saved.
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, SecretError>;
    /// Saves (replaces) the value.
    fn save(&self, name: &str, value: &[u8]) -> Result<(), SecretError>;
    /// Removes the value; nothing happens when it does not exist.
    fn delete(&self, name: &str) -> Result<(), SecretError>;
}

/// A shared secret store.
pub type SharedSecretStore = Arc<dyn SecretStore>;

/// How a [`FileSecretStore`] protects the bytes it writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    /// Plain bytes (the cross-platform fallback, also used by the C# tests).
    None,
    /// Windows DPAPI, machine scope, with [`DPAPI_ENTROPY`].
    Dpapi,
}

/// One file per secret (`FileSecretStore` / `DpapiSecretStore`).
#[derive(Debug, Clone)]
pub struct FileSecretStore {
    dir: PathBuf,
    protection: Protection,
}

impl FileSecretStore {
    /// Plain files in `dir`, mode 0600 on Unix.
    pub fn new(dir: impl Into<PathBuf>) -> FileSecretStore {
        FileSecretStore {
            dir: dir.into(),
            protection: Protection::None,
        }
    }

    /// DPAPI-protected files in `dir` (`DpapiSecretStore`). Outside Windows every
    /// operation that needs DPAPI fails.
    pub fn dpapi(dir: impl Into<PathBuf>) -> FileSecretStore {
        FileSecretStore {
            dir: dir.into(),
            protection: Protection::Dpapi,
        }
    }

    /// The folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The protection applied.
    pub fn protection(&self) -> Protection {
        self.protection
    }

    fn path_for(&self, name: &str) -> Result<PathBuf, SecretError> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(SecretError::InvalidName(name.to_owned()));
        }
        Ok(self.dir.join(format!("{name}.bin")))
    }

    fn protect(&self, value: &[u8]) -> Result<Vec<u8>, SecretError> {
        match self.protection {
            Protection::None => Ok(value.to_vec()),
            Protection::Dpapi => dl_winsvc::dpapi::protect_with_entropy(
                value,
                dl_winsvc::dpapi::Scope::LocalMachine,
                DPAPI_ENTROPY,
            )
            .map_err(SecretError::Protect),
        }
    }

    fn unprotect(&self, stored: Vec<u8>) -> Result<Vec<u8>, SecretError> {
        match self.protection {
            Protection::None => Ok(stored),
            Protection::Dpapi => dl_winsvc::dpapi::unprotect_with_entropy(
                &stored,
                dl_winsvc::dpapi::Scope::LocalMachine,
                DPAPI_ENTROPY,
            )
            .map_err(|e| {
                SecretError::KeyStore(AgentError::with_source(
                    codes::KEY_STORE_FAILED,
                    "Stored secret cannot be decrypted on this machine; re-enroll the agent.",
                    e,
                ))
            }),
        }
    }
}

impl SecretStore for FileSecretStore {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, SecretError> {
        let path = self.path_for(name)?;
        if !path.is_file() {
            return Ok(None);
        }
        let stored = std::fs::read(&path)?;
        self.unprotect(stored).map(Some)
    }

    fn save(&self, name: &str, value: &[u8]) -> Result<(), SecretError> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path_for(name)?;
        let protected = self.protect(value)?;
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, &protected)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        let path = self.path_for(name)?;
        if path.is_file() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }
}

/// `SecretStoreFactory.Create`: DPAPI files on Windows, plain 0600 files elsewhere, in
/// `{DataDir}\secrets`.
pub fn create(paths: &AgentPaths) -> SharedSecretStore {
    if cfg!(windows) {
        Arc::new(FileSecretStore::dpapi(paths.secrets_dir()))
    } else {
        Arc::new(FileSecretStore::new(paths.secrets_dir()))
    }
}

/// Why an agent key could not be made.
#[derive(Debug)]
pub enum KeyError {
    /// The stored seed is shorter than 32 bytes.
    SeedTooShort(usize),
    /// The OS random generator failed.
    Random(dl_rand::Error),
    /// The secret store failed.
    Store(SecretError),
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::SeedTooShort(n) => {
                write!(f, "The Ed25519 seed has {n} bytes; 32 are needed.")
            }
            KeyError::Random(e) => write!(f, "{e}"),
            KeyError::Store(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for KeyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            KeyError::Random(e) => Some(e),
            KeyError::Store(e) => Some(e),
            KeyError::SeedTooShort(_) => None,
        }
    }
}

/// The agent's long-lived Ed25519 identity key (`AgentKey`).
#[derive(Clone)]
pub struct AgentKey {
    key: dl_ed25519::SigningKey,
}

impl fmt::Debug for AgentKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentKey")
            .field("public_key", &self.public_key_base64())
            .finish_non_exhaustive()
    }
}

impl AgentKey {
    /// A key from the first 32 bytes of `seed` (longer input is accepted like
    /// BouncyCastle's `Ed25519PrivateKeyParameters(seed, 0)`).
    pub fn from_seed(seed: &[u8]) -> Result<AgentKey, KeyError> {
        let bytes: [u8; 32] = seed
            .get(..32)
            .and_then(|s| s.try_into().ok())
            .ok_or(KeyError::SeedTooShort(seed.len()))?;
        Ok(AgentKey {
            key: dl_ed25519::SigningKey::from_seed(&bytes),
        })
    }

    /// A new random key.
    pub fn generate() -> Result<AgentKey, KeyError> {
        let seed = dl_rand::secure_bytes::<32>().map_err(KeyError::Random)?;
        AgentKey::from_seed(&seed)
    }

    /// Loads the key from the store, generating and saving one when absent; `true` when
    /// it was created.
    pub fn load_or_create(store: &dyn SecretStore) -> Result<(AgentKey, bool), KeyError> {
        if let Some(seed) = store.load(names::SIGNING_SEED).map_err(KeyError::Store)? {
            return Ok((AgentKey::from_seed(&seed)?, false));
        }
        let key = AgentKey::generate()?;
        store
            .save(names::SIGNING_SEED, key.seed())
            .map_err(KeyError::Store)?;
        Ok((key, true))
    }

    /// The 32-byte seed.
    pub fn seed(&self) -> &[u8; 32] {
        self.key.seed()
    }

    /// The 32-byte public key.
    pub fn public_key(&self) -> [u8; 32] {
        *self.key.verifying_key().as_bytes()
    }

    /// The public key in standard base64.
    pub fn public_key_base64(&self) -> String {
        dl_encoding::base64::STANDARD.encode(self.public_key())
    }

    /// The Ed25519 signature of `message`.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.key.sign(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir()
            .join("pda-sync-agent-tests")
            .join(dl_uuid::Uuid::new_v4().simple().to_string())
    }

    #[test]
    fn file_store_round_trips_and_validates_names() {
        let dir = temp_dir();
        let s = FileSecretStore::new(dir.join("secrets"));
        assert!(s.load("agent-token").unwrap().is_none());
        s.save("agent-token", b"tok").unwrap();
        assert_eq!(
            std::fs::read(dir.join("secrets").join("agent-token.bin")).unwrap(),
            b"tok"
        );
        assert_eq!(s.load("agent-token").unwrap().unwrap(), b"tok");
        s.save("agent-token", b"tok2").unwrap();
        assert_eq!(s.load("agent-token").unwrap().unwrap(), b"tok2");
        s.delete("agent-token").unwrap();
        s.delete("agent-token").unwrap();
        assert!(s.load("agent-token").unwrap().is_none());
        for bad in ["", "a/b", "..", "a.b", "é"] {
            assert!(
                matches!(s.load(bad), Err(SecretError::InvalidName(_))),
                "{bad}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `File_secret_store_uses_owner_only_permissions_on_unix`.
    #[cfg(unix)]
    #[test]
    fn file_secret_store_uses_owner_only_permissions_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir();
        let s = FileSecretStore::new(dir.join("secrets"));
        s.save("probe", &[1, 2, 3]).unwrap();
        let mode = std::fs::metadata(dir.join("secrets").join("probe.bin"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_store_seals_with_the_csharp_entropy() {
        let dir = temp_dir();
        let s = FileSecretStore::dpapi(&dir);
        s.save("ed25519-seed", &[7; 32]).unwrap();
        let blob = std::fs::read(dir.join("ed25519-seed.bin")).unwrap();
        assert_ne!(blob, [7; 32]);
        assert_eq!(
            dl_winsvc::dpapi::unprotect_with_entropy(
                &blob,
                dl_winsvc::dpapi::Scope::LocalMachine,
                DPAPI_ENTROPY
            )
            .unwrap(),
            [7; 32]
        );
        assert_eq!(s.load("ed25519-seed").unwrap().unwrap(), [7; 32]);
        std::fs::write(dir.join("ed25519-seed.bin"), b"garbage").unwrap();
        let err = s.load("ed25519-seed").unwrap_err();
        assert_eq!(
            err.agent_error().map(AgentError::code),
            Some(codes::KEY_STORE_FAILED)
        );
        assert_eq!(
            err.to_string(),
            "Stored secret cannot be decrypted on this machine; re-enroll the agent."
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_key_matches_bouncy_castle() {
        // .NET 10 + BouncyCastle (`sa-core` check, "PUB").
        let seed: Vec<u8> = (1..=32).collect();
        let key = AgentKey::from_seed(&seed).unwrap();
        assert_eq!(
            key.public_key_base64(),
            "ebVWLo/mVPlAeLES6KmLp5AfhTrmlb7X4OORC60ElmQ="
        );
        let mut longer = seed.clone();
        longer.push(99);
        assert_eq!(
            AgentKey::from_seed(&longer).unwrap().public_key(),
            key.public_key()
        );
        assert!(matches!(
            AgentKey::from_seed(&seed[..31]),
            Err(KeyError::SeedTooShort(31))
        ));
    }

    #[test]
    fn load_or_create_reuses_the_stored_key() {
        let dir = temp_dir();
        let s = FileSecretStore::new(&dir);
        let (a, created) = AgentKey::load_or_create(&s).unwrap();
        assert!(created);
        let (b, created) = AgentKey::load_or_create(&s).unwrap();
        assert!(!created);
        assert_eq!(a.seed(), b.seed());
        assert_eq!(s.load(names::SIGNING_SEED).unwrap().unwrap().len(), 32);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
