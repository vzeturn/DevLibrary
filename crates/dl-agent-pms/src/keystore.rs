//! The agent's Ed25519 key and enrolled identity in the state directory. The key is
//! generated on first run and sealed through a [`Protector`]: DPAPI machine scope with
//! the agent's entropy on Windows, a plain 0600 file elsewhere (dev and tests only).
//!
//! The files are the Go agent's: `agent.key` is `{"scheme":…,"sealed":"<base64>"}` over
//! the 32-byte seed, `identity.json` holds `agent_id`, `site_id`, `tenant_id`. An agent
//! upgraded from Go keeps its key and identity.

use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use dl_ed25519::SigningKey;
use dl_json::Value;

use crate::json::{self, Obj};

/// Seals key material at rest.
pub trait Protector: Send + Sync {
    /// Seals.
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, String>;
    /// Opens.
    fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>, String>;
    /// The scheme name in the key file.
    fn name(&self) -> &'static str;
}

/// DPAPI entropy binding the blob to this use (identical to the Go agent's).
pub const DPAPI_ENTROPY: &[u8] = b"smile-pms-agent/ed25519/v1";

/// `CryptProtectData` in machine scope with [`DPAPI_ENTROPY`]: any process on this
/// machine can unseal, the blob is useless elsewhere; the state directory ACL limits
/// who can read it.
pub struct DpapiProtector;

impl Protector for DpapiProtector {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, String> {
        dl_winsvc::dpapi::protect_with_entropy(
            plain,
            dl_winsvc::dpapi::Scope::LocalMachine,
            DPAPI_ENTROPY,
        )
        .map_err(|e| format!("CryptProtectData: {e}"))
    }
    fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>, String> {
        dl_winsvc::dpapi::unprotect_with_entropy(
            sealed,
            dl_winsvc::dpapi::Scope::LocalMachine,
            DPAPI_ENTROPY,
        )
        .map_err(|e| format!("CryptUnprotectData: {e}"))
    }
    fn name(&self) -> &'static str {
        "dpapi-machine"
    }
}

/// Stores the key as is; the file mode (0600) is the only protection. Dev and test
/// machines only.
pub struct FileProtector;

impl Protector for FileProtector {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, String> {
        Ok(plain.to_vec())
    }
    fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>, String> {
        Ok(sealed.to_vec())
    }
    fn name(&self) -> &'static str {
        "file-0600"
    }
}

/// The platform protector.
pub fn platform_protector() -> Box<dyn Protector> {
    if cfg!(windows) {
        Box::new(DpapiProtector)
    } else {
        Box::new(FileProtector)
    }
}

/// What enrollment returned; persisted so restarts skip enrollment.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Identity {
    /// Agent id.
    pub agent_id: String,
    /// Site id.
    pub site_id: String,
    /// Tenant id.
    pub tenant_id: String,
}

/// Keystore failures.
#[derive(Debug)]
pub enum KeystoreError {
    /// The agent has not enrolled yet.
    NoIdentity,
    /// Anything else (the message never holds key material).
    Other(String),
}

impl fmt::Display for KeystoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeystoreError::NoIdentity => f.write_str("agent is not enrolled"),
            KeystoreError::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for KeystoreError {}

fn other(m: impl Into<String>) -> KeystoreError {
    KeystoreError::Other(m.into())
}

/// The sealed Ed25519 key inside the state directory.
pub const KEY_FILE: &str = "agent.key";
/// The enrolled identity inside the state directory.
pub const IDENTITY_FILE: &str = "identity.json";

/// The state directory.
pub struct Store {
    dir: PathBuf,
    p: Box<dyn Protector>,
}

impl Store {
    /// Uses `dir` (created 0700 when missing) with the platform protector.
    pub fn open(dir: &Path) -> Result<Store, KeystoreError> {
        Store::open_with(dir, platform_protector())
    }

    /// Uses an explicit protector.
    pub fn open_with(dir: &Path, p: Box<dyn Protector>) -> Result<Store, KeystoreError> {
        crate::log::mkdir_private(dir)
            .map_err(|e| other(format!("keystore: create state dir: {e}")))?;
        Ok(Store {
            dir: dir.to_owned(),
            p,
        })
    }

    /// Loads the key, generating and storing one on first run; the bool is true when the
    /// key was just created.
    pub fn key(&self) -> Result<(SigningKey, bool), KeystoreError> {
        let path = self.dir.join(KEY_FILE);
        match std::fs::read(&path) {
            Ok(raw) => return self.decode_key(&raw).map(|k| (k, false)),
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                return Err(other(format!("keystore: read key: {e}")));
            }
            Err(_) => {}
        }
        let mut seed = [0u8; 32];
        dl_rand::fill(&mut seed).map_err(|e| other(format!("keystore: generate key: {e}")))?;
        let key = SigningKey::from_seed(&seed);
        let sealed = self
            .p
            .protect(&seed)
            .map_err(|e| other(format!("keystore: protect key: {e}")))?;
        seed.fill(0);
        let env = Obj::new()
            .str("scheme", self.p.name())
            .str("sealed", &dl_encoding::base64::STANDARD.encode(&sealed))
            .finish();
        write_exclusive(&path, env.as_bytes())?;
        Ok((key, true))
    }

    fn decode_key(&self, raw: &[u8]) -> Result<SigningKey, KeystoreError> {
        let corrupt = || other("keystore: key file is corrupt");
        let obj = json::parse_object(raw).ok_or_else(corrupt)?;
        let scheme = match json::field_ci(&obj, "scheme") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(_) => return Err(corrupt()),
        };
        let sealed = match json::field_ci(&obj, "sealed") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::String(s)) => {
                let clean: String = s.chars().filter(|c| *c != '\r' && *c != '\n').collect();
                dl_encoding::base64::STANDARD
                    .decode(&clean)
                    .map_err(|_| corrupt())?
            }
            Some(_) => return Err(corrupt()),
        };
        if scheme != self.p.name() {
            return Err(other(format!(
                "keystore: key sealed with {}, this build uses {}",
                json::quote(&scheme),
                json::quote(self.p.name())
            )));
        }
        let seed = self
            .p
            .unprotect(&sealed)
            .map_err(|e| other(format!("keystore: unprotect key: {e}")))?;
        let seed: [u8; 32] = seed.as_slice().try_into().map_err(|_| corrupt())?;
        Ok(SigningKey::from_seed(&seed))
    }

    /// The enrolled identity, or [`KeystoreError::NoIdentity`].
    pub fn identity(&self) -> Result<Identity, KeystoreError> {
        let raw = match std::fs::read(self.dir.join(IDENTITY_FILE)) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(KeystoreError::NoIdentity);
            }
            Err(e) => return Err(other(format!("keystore: read identity: {e}"))),
        };
        let corrupt = || other("keystore: identity file is corrupt");
        let obj = json::parse_object(&raw).ok_or_else(corrupt)?;
        let s = |name: &str| match json::field_ci(&obj, name) {
            None | Some(Value::Null) => Ok(String::new()),
            Some(Value::String(s)) => Ok(s.clone()),
            Some(_) => Err(corrupt()),
        };
        let id = Identity {
            agent_id: s("agent_id")?,
            site_id: s("site_id")?,
            tenant_id: s("tenant_id")?,
        };
        if id.agent_id.is_empty() || id.site_id.is_empty() {
            return Err(corrupt());
        }
        Ok(id)
    }

    /// Persists the identity (atomic replace, 0600), formatted like Go's
    /// `json.MarshalIndent(id, "", "  ")`.
    pub fn save_identity(&self, id: &Identity) -> Result<(), KeystoreError> {
        let text = format!(
            "{{\n  \"agent_id\": {},\n  \"site_id\": {},\n  \"tenant_id\": {}\n}}",
            json::quote(&id.agent_id),
            json::quote(&id.site_id),
            json::quote(&id.tenant_id)
        );
        let path = self.dir.join(IDENTITY_FILE);
        let tmp = self.dir.join(format!("{IDENTITY_FILE}.tmp"));
        let e = |x: io::Error| other(format!("keystore: write identity: {x}"));
        write_private(&tmp, text.as_bytes()).map_err(e)?;
        std::fs::rename(&tmp, &path).map_err(e)
    }
}

fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)?.write_all(data)
}

/// Creates `path` 0600 and fails if it exists, so two processes starting at once cannot
/// end up with different keys on disk.
fn write_exclusive(path: &Path, data: &[u8]) -> Result<(), KeystoreError> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o
        .open(path)
        .map_err(|e| other(format!("keystore: create key file: {e}")))?;
    if let Err(e) = f.write_all(data) {
        drop(f);
        let _ = std::fs::remove_file(path);
        return Err(other(format!("keystore: write key file: {e}")));
    }
    f.sync_all()
        .map_err(|e| other(format!("keystore: sync key file: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pms-ks-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn key_is_created_once_and_identity_round_trips() {
        let d = dir("a");
        let s = Store::open_with(&d, Box::new(FileProtector)).unwrap();
        let (k1, created) = s.key().unwrap();
        assert!(created);
        let (k2, created) = s.key().unwrap();
        assert!(!created);
        assert_eq!(k1.seed(), k2.seed());
        assert!(matches!(s.identity(), Err(KeystoreError::NoIdentity)));
        let id = Identity {
            agent_id: "a".into(),
            site_id: "s".into(),
            tenant_id: "t".into(),
        };
        s.save_identity(&id).unwrap();
        assert_eq!(s.identity().unwrap(), id);
        std::fs::write(d.join(IDENTITY_FILE), r#"{"agent_id":"a"}"#).unwrap();
        assert!(s.identity().is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn reads_a_go_key_file_and_refuses_other_schemes() {
        let d = dir("b");
        std::fs::create_dir_all(&d).unwrap();
        // Written by the Go agent with FileProtector: base64 of a 32-byte seed.
        let seed = [7u8; 32];
        let go = format!(
            r#"{{"scheme":"file-0600","sealed":"{}"}}"#,
            dl_encoding::base64::STANDARD.encode(seed)
        );
        std::fs::write(d.join(KEY_FILE), go).unwrap();
        let s = Store::open_with(&d, Box::new(FileProtector)).unwrap();
        assert_eq!(s.key().unwrap().0.seed(), &seed);
        std::fs::write(
            d.join(KEY_FILE),
            r#"{"scheme":"dpapi-machine","sealed":""}"#,
        )
        .unwrap();
        assert!(s.key().unwrap_err().to_string().contains("dpapi-machine"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_round_trip() {
        let p = DpapiProtector;
        let sealed = p.protect(b"seed").unwrap();
        assert_eq!(p.unprotect(&sealed).unwrap(), b"seed");
    }
}
