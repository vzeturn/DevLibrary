//! Secrets at rest, as the .NET agent keeps them: DPAPI LocalMachine with a per-file entropy
//! on Windows (useless when copied off the server), a plain owner-only (0600) file elsewhere.
//! Holds the enrollment identity (`AgentIdentityStore`) and the tenant master key
//! (`TenantKeyStore`).

use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;

use crate::contracts::values::parse_base64;
use dl_json::Value;

use crate::contracts::EnvelopeCipher;
use crate::contracts::key_vault::TenantKeyVault;
use crate::error::Error;
use crate::stj;

/// DPAPI entropy of `agent-identity.bin`.
pub const IDENTITY_ENTROPY: &[u8] = b"MM.Arrival.Agent/identity/v1";
/// DPAPI entropy of `agent-tenant-key.bin`.
pub const TENANT_KEY_ENTROPY: &[u8] = b"MM.Arrival.Agent/tenant-key/v1";

/// Seals `data` for this machine (Windows) or returns it as is.
///
/// # Errors
/// [`Error::Crypto`] when DPAPI fails.
pub fn protect(data: &[u8], entropy: &[u8]) -> Result<Vec<u8>, Error> {
    if cfg!(windows) {
        dl_winsvc::dpapi::protect_with_entropy(data, dl_winsvc::dpapi::Scope::LocalMachine, entropy)
            .map_err(|e| Error::Crypto(format!("DPAPI protect failed: {e}")))
    } else {
        Ok(data.to_vec())
    }
}

/// Opens what [`protect`] sealed.
///
/// # Errors
/// [`Error::Crypto`] when DPAPI fails (another machine's file, other entropy).
pub fn unprotect(blob: &[u8], entropy: &[u8]) -> Result<Vec<u8>, Error> {
    if cfg!(windows) {
        dl_winsvc::dpapi::unprotect_with_entropy(
            blob,
            dl_winsvc::dpapi::Scope::LocalMachine,
            entropy,
        )
        .map_err(|e| Error::Crypto(format!("DPAPI unprotect failed: {e}")))
    } else {
        Ok(blob.to_vec())
    }
}

/// Writes a secret file: created owner-only (0600) off Windows, replaced in place.
///
/// # Errors
/// [`Error::Io`] naming `role`.
pub fn write_secret(path: &Path, bytes: &[u8], role: &str) -> Result<(), Error> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        opts.mode(0o600);
        if path.exists() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| Error::io(role, e))?;
        }
    }
    let mut f = opts.open(path).map_err(|e| Error::io(role, e))?;
    f.write_all(bytes).map_err(|e| Error::io(role, e))?;
    f.sync_all().map_err(|e| Error::io(role, e))
}

/// `AgentIdentity`: who this agent is, as the cloud issued it at enrollment.
#[derive(Clone, PartialEq, Eq)]
pub struct Identity {
    /// Tenant id, lower-case and dashed (`Guid.ToString()`).
    pub tenant_id: String,
    /// Agent id.
    pub agent_id: String,
    /// Key id.
    pub kid: String,
    /// The envelope secret, base64.
    pub secret: String,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("tenant_id", &self.tenant_id)
            .field("agent_id", &self.agent_id)
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl Identity {
    /// `AgentIdentity.FromToken`: base64url JSON, padding optional.
    ///
    /// # Errors
    /// [`Error::Config`] `Invalid enrollment token.` for anything else.
    pub fn from_token(token: &str) -> Result<Identity, Error> {
        let bad = || Error::Config("Invalid enrollment token.".into());
        let mut b64: String = token
            .trim()
            .chars()
            .map(|c| match c {
                '-' => '+',
                '_' => '/',
                c => c,
            })
            .collect();
        while !b64.len().is_multiple_of(4) {
            b64.push('=');
        }
        let json = parse_base64(&b64).ok_or_else(bad)?;
        Identity::from_json_bytes(&json).map_err(|_| bad())
    }

    /// Parses the stored JSON (property names case-insensitive, `tenantId` a GUID).
    ///
    /// # Errors
    /// [`Error::Json`] for anything that is not an identity.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Identity, Error> {
        let bad = |what: &str| Error::Json(format!("identity: {what}"));
        let text = std::str::from_utf8(bytes).map_err(|_| bad("not UTF-8"))?;
        let v = stj::parse(text).map_err(|_| bad("not JSON"))?;
        let Value::Object(m) = v else {
            return Err(bad("not an object"));
        };
        let field = |name: &str| -> Result<String, Error> {
            match stj::get_ci(&m, name) {
                Some(Value::String(s)) => Ok(s.clone()),
                _ => Err(bad(name)),
            }
        };
        let tenant = field("tenantId")?;
        let uuid: dl_uuid::Uuid = tenant.trim().parse().map_err(|_| bad("tenantId"))?;
        Ok(Identity {
            tenant_id: uuid.to_string().to_ascii_lowercase(),
            agent_id: field("agentId")?,
            kid: field("kid")?,
            secret: field("secret")?,
        })
    }

    /// The JSON the .NET agent stores (camelCase, `SyncJson.Options`).
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\"tenantId\":");
        stj::escape_into(&mut out, &self.tenant_id);
        out.push_str(",\"agentId\":");
        stj::escape_into(&mut out, &self.agent_id);
        out.push_str(",\"kid\":");
        stj::escape_into(&mut out, &self.kid);
        out.push_str(",\"secret\":");
        stj::escape_into(&mut out, &self.secret);
        out.push('}');
        out
    }

    /// The tenant id in `N` format (32 hex digits), as the envelope cipher binds it.
    pub fn tenant_n(&self) -> String {
        self.tenant_id.replace('-', "")
    }

    /// `CreateCipher`.
    ///
    /// # Errors
    /// [`Error::Config`] when the secret is not base64; [`Error::Crypto`] from the cipher.
    pub fn cipher(&self) -> Result<EnvelopeCipher, Error> {
        let secret = parse_base64(&self.secret)
            .ok_or_else(|| Error::Config("identity secret is not base64".into()))?;
        EnvelopeCipher::new(&self.tenant_n(), &self.agent_id, &self.kid, &secret)
    }

    /// `AgentIdentityStore.Load`: `None` when the file does not exist.
    ///
    /// # Errors
    /// [`Error::Io`], [`Error::Crypto`] (DPAPI) or [`Error::Json`].
    pub fn load(path: &Path) -> Result<Option<Identity>, Error> {
        let blob = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::io("identity", e)),
        };
        let json = unprotect(&blob, IDENTITY_ENTROPY)?;
        Identity::from_json_bytes(&json).map(Some)
    }

    /// `AgentIdentityStore.Save`.
    ///
    /// # Errors
    /// [`Error::Crypto`] (DPAPI) or [`Error::Io`].
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let blob = protect(self.to_json().as_bytes(), IDENTITY_ENTROPY)?;
        write_secret(path, &blob, "identity")
    }
}

/// `TenantKeyStore`: `TenantKey` from the settings when set, else the key file. Loaded on first
/// use and kept (a failure too, as `Lazy` does). Unlike the .NET store it never makes a key on
/// its own: a new key would leave the legal vault and the cloud data sealed by the old one
/// unreadable. Only the enrol of a fresh install ([`TenantKeyStore::create`]) makes one.
#[derive(Debug)]
pub struct TenantKeyStore {
    configured: String,
    file: std::path::PathBuf,
    vault: OnceLock<Result<TenantKeyVault, String>>,
}

impl TenantKeyStore {
    /// A store over the configured key (may be empty) and file.
    pub fn new(configured: &str, file: std::path::PathBuf) -> TenantKeyStore {
        TenantKeyStore {
            configured: configured.to_string(),
            file,
            vault: OnceLock::new(),
        }
    }

    /// The key vault, loading the key on first use (never creating one).
    ///
    /// # Errors
    /// [`Error::Crypto`] with why the key could not be had (never the key), [`missing`] when
    /// there is none.
    pub fn vault(&self) -> Result<&TenantKeyVault, Error> {
        self.vault
            .get_or_init(|| {
                let key = self
                    .load_existing()
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| missing(&self.file))?;
                TenantKeyVault::new(&key).map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(|e| Error::Crypto(e.clone()))
    }

    /// The configured key or the key file, without creating one: `None` when neither
    /// exists yet (the self-test reads, it never writes a key).
    ///
    /// # Errors
    /// [`Error::Config`] for a configured key that is not base64; [`Error::Io`] or
    /// [`Error::Crypto`] for the file.
    pub fn load_existing(&self) -> Result<Option<Vec<u8>>, Error> {
        if !self.configured.trim().is_empty() {
            return parse_base64(self.configured.trim())
                .map(Some)
                .ok_or_else(|| Error::Config("Agent:TenantKey is not base64".into()));
        }
        match std::fs::read(&self.file) {
            Ok(blob) => unprotect(&blob, TENANT_KEY_ENTROPY).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::io("tenant key", e)),
        }
    }

    /// The key of a fresh install (the enrol plan `created`): a new key written to the file,
    /// only when no key exists and `vault_dir` holds no file (a file there was sealed by a key
    /// this install would not hold).
    ///
    /// # Errors
    /// [`Error::Config`] when a key exists already or the vault holds a file; [`Error::Io`]
    /// or [`Error::Crypto`] for the vault folder or the write.
    pub fn create(&self, vault_dir: &Path) -> Result<Vec<u8>, Error> {
        if self.load_existing()?.is_some() {
            return Err(Error::Config(format!(
                "a tenant key exists already ({}); a new one is never made over it",
                if self.configured.trim().is_empty() {
                    self.file.display().to_string()
                } else {
                    "Agent:TenantKey".to_string()
                }
            )));
        }
        if let Some(file) = first_file(vault_dir)? {
            return Err(Error::Config(format!(
                "the legal vault {} holds {}: a new tenant key could not open it; restore the key that sealed it",
                vault_dir.display(),
                file.display()
            )));
        }
        let key = TenantKeyVault::new_key()?;
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io("tenant key", e))?;
        }
        write_secret(
            &self.file,
            &protect(&key, TENANT_KEY_ENTROPY)?,
            "tenant key",
        )?;
        Ok(key.to_vec())
    }
}

/// Why a store has no key, for the operator and the problem report.
pub fn missing(file: &Path) -> String {
    format!(
        "no tenant key: {} does not exist and Agent:TenantKey is empty. The agent never makes a key on its own; restore the key file (or the escrowed key) and enrol again",
        file.display()
    )
}

/// The first file under `dir` (none when it does not exist).
fn first_file(dir: &Path) -> Result<Option<std::path::PathBuf>, Error> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = match std::fs::read_dir(&d) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(Error::io("legal vault", e)),
        };
        for entry in entries {
            let entry = entry.map_err(|e| Error::io("legal vault", e))?;
            if entry
                .file_type()
                .map_err(|e| Error::io("legal vault", e))?
                .is_dir()
            {
                stack.push(entry.path());
            } else {
                return Ok(Some(entry.path()));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::values::to_base64;

    fn temp(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "arr-key-{tag}-{}-{:016x}",
            std::process::id(),
            dl_rand::u64()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn the_store_never_makes_a_key_on_its_own() {
        let dir = temp("none");
        let file = dir.join("agent-tenant-key.bin");
        let store = TenantKeyStore::new("", file.clone());
        let e = store.vault().expect_err("no key").to_string();
        assert!(e.contains("no tenant key"), "{e}");
        assert!(!file.exists(), "nothing was written");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_key_is_created_only_with_none_and_an_empty_vault() {
        let dir = temp("create");
        let file = dir.join("data").join("agent-tenant-key.bin");
        let vault = dir.join("vault");
        std::fs::create_dir_all(vault.join("guests")).unwrap();
        std::fs::write(vault.join("guests").join("a"), b"sealed").unwrap();
        let e = TenantKeyStore::new("", file.clone())
            .create(&vault)
            .expect_err("the vault holds a file")
            .to_string();
        assert!(e.contains("legal vault"), "{e}");
        assert!(!file.exists());

        std::fs::remove_dir_all(&vault).unwrap();
        let key = TenantKeyStore::new("", file.clone())
            .create(&vault)
            .unwrap();
        let store = TenantKeyStore::new("", file.clone());
        assert_eq!(
            store.vault().unwrap().fingerprint(),
            TenantKeyVault::new(&key).unwrap().fingerprint()
        );
        let e = TenantKeyStore::new("", file.clone())
            .create(&vault)
            .expect_err("a key exists")
            .to_string();
        assert!(e.contains("exists already"), "{e}");
        let configured = to_base64(&[3u8; 32]);
        assert!(
            TenantKeyStore::new(&configured, dir.join("other.bin"))
                .create(&vault)
                .is_err(),
            "a configured key is a key"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn token_round_trips_through_the_stored_form() {
        let id = Identity {
            tenant_id: "0f8fad5b-d9cb-469f-a165-70867728950e".into(),
            agent_id: "agt".into(),
            kid: "k1".into(),
            secret: to_base64(&[7u8; 32]),
        };
        let token = to_base64(id.to_json().as_bytes())
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_");
        assert_eq!(Identity::from_token(&format!(" {token}\n")).unwrap(), id);
        assert!(Identity::from_token("not a token").is_err());
        assert!(!format!("{id:?}").contains(&id.secret));
        assert!(id.cipher().is_ok());
    }
}
