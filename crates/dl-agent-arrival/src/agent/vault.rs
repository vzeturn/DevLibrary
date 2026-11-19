//! `LegalVault`: the hotel's own copy of legal documents, each file sealed with the tenant
//! master key and bound to its path, so a copied disk reveals nothing without the key.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::agent::secrets::TenantKeyStore;
use crate::contracts::vault_ops;
use crate::error::Error;

/// Why a vault operation failed (the cloud only ever sees "Vault operation rejected.").
#[derive(Debug)]
pub enum VaultError {
    /// The path leaves the vault directory.
    Escapes,
    /// The data does not hash to the checksum sent with it.
    ChecksumMismatch,
    /// Reading, writing or the tenant key failed.
    Agent(Error),
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VaultError::Escapes => f.write_str("Path escapes the vault."),
            VaultError::ChecksumMismatch => f.write_str("Checksum mismatch."),
            VaultError::Agent(e) => write!(f, "{e}"),
        }
    }
}

impl From<Error> for VaultError {
    fn from(e: Error) -> VaultError {
        VaultError::Agent(e)
    }
}

/// The vault under one directory.
#[derive(Debug, Clone)]
pub struct LegalVault {
    root: PathBuf,
    keys: Arc<TenantKeyStore>,
}

impl LegalVault {
    /// A vault rooted at `root`, sealing with `keys`.
    pub fn new(root: PathBuf, keys: Arc<TenantKeyStore>) -> LegalVault {
        LegalVault { root, keys }
    }

    /// `Resolve`: the file for a normalized vault path, never outside the root.
    ///
    /// # Errors
    /// [`VaultError::Escapes`].
    pub fn resolve(&self, normalized: &str) -> Result<PathBuf, VaultError> {
        let rel = Path::new(normalized);
        let inside =
            !normalized.is_empty() && rel.components().all(|c| matches!(c, Component::Normal(_)));
        if !inside {
            return Err(VaultError::Escapes);
        }
        Ok(self.root.join(rel))
    }

    /// `PutAsync`: checks the checksum, seals, writes via a temporary file; returns the
    /// checksum.
    ///
    /// # Errors
    /// [`VaultError`].
    pub fn put(
        &self,
        path: &str,
        data: &[u8],
        expected_sha256: &str,
    ) -> Result<String, VaultError> {
        let actual = vault_ops::sha256_hex(data);
        if !actual.eq_ignore_ascii_case(expected_sha256) {
            return Err(VaultError::ChecksumMismatch);
        }
        let file = self.resolve(path)?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io("vault", e))?;
        }
        let sealed = self.keys.vault()?.wrap(data, path)?;
        let tmp = tmp_path(&file);
        std::fs::write(&tmp, sealed).map_err(|e| Error::io("vault", e))?;
        std::fs::rename(&tmp, &file).map_err(|e| Error::io("vault", e))?;
        Ok(actual)
    }

    /// `GetAsync`: the opened file, or `None` when there is none.
    ///
    /// # Errors
    /// [`VaultError`].
    pub fn get(&self, path: &str) -> Result<Option<Vec<u8>>, VaultError> {
        let file = self.resolve(path)?;
        let sealed = match std::fs::read(&file) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::io("vault", e).into()),
        };
        Ok(Some(self.keys.vault()?.unwrap(&sealed, path)?))
    }

    /// `Delete`: removes the file when it exists.
    ///
    /// # Errors
    /// [`VaultError`].
    pub fn delete(&self, path: &str) -> Result<(), VaultError> {
        let file = self.resolve(path)?;
        match std::fs::remove_file(&file) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io("vault", e).into()),
        }
    }
}

/// `file + ".tmp"`.
pub(crate) fn tmp_path(file: &Path) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::values::to_base64;

    #[test]
    fn put_get_delete_round_trip_and_refuse_escapes() {
        let root = std::env::temp_dir().join(format!("dl-arrival-vault-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let keys = Arc::new(TenantKeyStore::new(
            &to_base64(&[9u8; 32]),
            root.join("k.bin"),
        ));
        let vault = LegalVault::new(root.join("vault"), keys);
        let data = b"contract".to_vec();
        let sha = vault_ops::sha256_hex(&data);
        assert!(matches!(
            vault.put("a/b.pdf", &data, "00"),
            Err(VaultError::ChecksumMismatch)
        ));
        assert_eq!(
            vault.put("a/b.pdf", &data, &sha.to_uppercase()).unwrap(),
            sha
        );
        let on_disk = std::fs::read(root.join("vault/a/b.pdf")).unwrap();
        assert_ne!(on_disk, data, "sealed at rest");
        assert_eq!(vault.get("a/b.pdf").unwrap(), Some(data));
        assert!(vault.get("a/other.pdf").unwrap().is_none());
        vault.delete("a/b.pdf").unwrap();
        vault.delete("a/b.pdf").unwrap();
        assert!(vault.get("a/b.pdf").unwrap().is_none());
        assert!(matches!(vault.resolve("../x"), Err(VaultError::Escapes)));
        assert!(matches!(vault.resolve("/etc/x"), Err(VaultError::Escapes)));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
