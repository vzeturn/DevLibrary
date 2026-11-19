//! `TenantKeyVault` and `AgentKeyOps`: the hotel's master key wraps data keys and legal
//! files so the cloud never holds them in the clear. Wrapped form:
//! `fingerprint(8) | nonce(12) | ciphertext | tag(16)`, AAD = the UTF-8 context.

use crate::crypto::{self, AesGcm, NONCE_LEN, TAG_LEN};
use crate::error::Error;

/// `crypto.wrap-key`.
pub const OP_WRAP: &str = "crypto.wrap-key";
/// `crypto.unwrap-key`.
pub const OP_UNWRAP: &str = "crypto.unwrap-key";
/// `crypto.export-key`.
pub const OP_EXPORT: &str = "crypto.export-key";
/// Key parameter (base64).
pub const KEY_PARAM: &str = "key";
/// Context parameter (bound as AAD).
pub const CONTEXT_PARAM: &str = "context";
/// Result column.
pub const RESULT_COLUMN: &str = "key";

const FINGERPRINT_LEN: usize = 8;

/// `AgentKeyOps.IsKeyOp`.
pub fn is_key_op(operation: &str) -> bool {
    matches!(operation, OP_WRAP | OP_UNWRAP | OP_EXPORT)
}

/// The tenant master key.
pub struct TenantKeyVault {
    tmk: [u8; 32],
    fingerprint: [u8; FINGERPRINT_LEN],
    aead: AesGcm,
}

impl std::fmt::Debug for TenantKeyVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TenantKeyVault")
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

impl TenantKeyVault {
    /// A vault over a 32-byte key.
    ///
    /// # Errors
    /// [`Error::Crypto`] for any other length.
    pub fn new(tmk: &[u8]) -> Result<TenantKeyVault, Error> {
        let tmk: [u8; 32] = tmk
            .try_into()
            .map_err(|_| Error::Crypto("Tenant master key must be 32 bytes.".into()))?;
        let mut fingerprint = [0u8; FINGERPRINT_LEN];
        fingerprint.copy_from_slice(&crypto::sha256(&tmk)[..FINGERPRINT_LEN]);
        Ok(TenantKeyVault {
            aead: AesGcm::new(&tmk),
            tmk,
            fingerprint,
        })
    }

    /// `NewKey`.
    ///
    /// # Errors
    /// [`Error::Crypto`] when the system random source fails.
    pub fn new_key() -> Result<[u8; 32], Error> {
        crypto::random::<32>()
    }

    /// Lower-case hex of the first 8 bytes of SHA-256(key).
    pub fn fingerprint(&self) -> String {
        crypto::hex(&self.fingerprint)
    }

    /// The raw key.
    pub fn export(&self) -> [u8; 32] {
        self.tmk
    }

    /// Wraps `key` under a fresh nonce.
    ///
    /// # Errors
    /// [`Error::Crypto`] when the system random source fails.
    pub fn wrap(&self, key: &[u8], context: &str) -> Result<Vec<u8>, Error> {
        Ok(self.wrap_with(key, context, crypto::random::<NONCE_LEN>()?))
    }

    /// Wraps with the given nonce (fixtures).
    pub fn wrap_with(&self, key: &[u8], context: &str, nonce: [u8; NONCE_LEN]) -> Vec<u8> {
        let mut out = Vec::with_capacity(FINGERPRINT_LEN + NONCE_LEN + key.len() + TAG_LEN);
        out.extend_from_slice(&self.fingerprint);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&self.aead.seal(&nonce, key, context.as_bytes()));
        out
    }

    /// Unwraps a value this key wrapped under the same context.
    ///
    /// # Errors
    /// [`Error::Crypto`] for a short value, another key's value or a failed tag.
    pub fn unwrap(&self, wrapped: &[u8], context: &str) -> Result<Vec<u8>, Error> {
        if wrapped.len() < FINGERPRINT_LEN + NONCE_LEN + TAG_LEN {
            return Err(Error::Crypto("Malformed wrapped key.".into()));
        }
        if wrapped[..FINGERPRINT_LEN] != self.fingerprint {
            return Err(Error::Crypto(
                "This agent does not hold the key that wrapped the value.".into(),
            ));
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&wrapped[FINGERPRINT_LEN..FINGERPRINT_LEN + NONCE_LEN]);
        self.aead
            .open(
                &nonce,
                &wrapped[FINGERPRINT_LEN + NONCE_LEN..],
                context.as_bytes(),
            )
            .ok_or_else(|| Error::Crypto("The wrapped value failed authentication.".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_round_trips_and_binds_context() {
        let v = TenantKeyVault::new(&[3u8; 32]).unwrap();
        let w = v.wrap_with(b"secret", "ctx", [9; 12]);
        assert_eq!(v.unwrap(&w, "ctx").unwrap(), b"secret");
        assert!(v.unwrap(&w, "other").is_err());
        let other = TenantKeyVault::new(&[4u8; 32]).unwrap();
        assert!(other.unwrap(&w, "ctx").is_err());
        assert!(TenantKeyVault::new(&[0u8; 31]).is_err());
    }
}
