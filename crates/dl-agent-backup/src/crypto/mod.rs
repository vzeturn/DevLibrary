//! The hub's credential format: `base64url(salt).base64url(iv).base64url(ciphertext‖tag)`,
//! AES-256-GCM under HKDF-SHA256(ikm = the agent key string, salt, empty info). Byte-for-byte
//! the format of the hub's `encryptCredential` (`hub/src/lib/credentialCrypto.ts`) and the Go
//! agent's `hubclient.DecryptCredential`.
//!
//! The workspace has no AES, so this module carries a small AES-256 + GCM implementation,
//! checked against FIPS 197, the GCM paper's test cases and fixtures produced by the hub's
//! own code (`tests/fixtures/credentials.json`).

pub mod aes;
pub mod gcm;

use dl_encoding::base64::URL_SAFE_NO_PAD;
use dl_hash::{Hmac, Sha256};

use crate::Error;

const SALT_LEN: usize = 16;
const IV_LEN: usize = 12;

/// HKDF-SHA256 (RFC 5869): extract with `salt`, expand with `info` to `out.len()` bytes.
pub fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8], out: &mut [u8]) {
    let prk = Hmac::<Sha256>::mac(salt, ikm);
    let mut prev: Vec<u8> = Vec::new();
    for (i, chunk) in out.chunks_mut(32).enumerate() {
        let t = Hmac::<Sha256>::new(prk.as_ref())
            .chain(&prev)
            .chain(info)
            .chain([i as u8 + 1])
            .finalize();
        chunk.copy_from_slice(&t.as_ref()[..chunk.len()]);
        prev = t.as_ref().to_vec();
    }
}

fn derive(key: &str, salt: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    hkdf_sha256(key.as_bytes(), salt, &[], &mut out);
    out
}

/// Opens a credential the hub sealed under `key`. An empty `stored` is an empty secret:
/// the hub sends `""` for a server on Windows authentication or a destination with no
/// tokens (its `credentialError` flag, checked by the caller, marks the failure case).
pub fn decrypt_credential(stored: &str, key: &str) -> Result<String, Error> {
    if stored.is_empty() {
        return Ok(String::new());
    }
    let parts: Vec<&str> = stored.split('.').collect();
    if parts.len() != 3 {
        return Err(Error::Credential(format!(
            "malformed encrypted credential (want 3 dot-separated parts, got {})",
            parts.len()
        )));
    }
    let part = |i: usize, what: &str| {
        URL_SAFE_NO_PAD
            .decode(parts[i])
            .map_err(|e| Error::Credential(format!("decode {what}: {e}")))
    };
    let (salt, iv, sealed) = (part(0, "salt")?, part(1, "iv")?, part(2, "ciphertext")?);
    let plain = gcm::open(&derive(key, &salt), &iv, &[], &sealed)
        .map_err(|e| Error::Credential(format!("decrypt: {e}")))?;
    String::from_utf8(plain).map_err(|_| Error::Credential("decrypt: not UTF-8".into()))
}

/// Seals `plain` the way the hub does, with a fresh random salt and IV.
pub fn encrypt_credential(plain: &str, key: &str) -> Result<String, Error> {
    let salt: [u8; SALT_LEN] =
        dl_rand::secure_bytes().map_err(|e| Error::Credential(format!("random salt: {e}")))?;
    let iv: [u8; IV_LEN] =
        dl_rand::secure_bytes().map_err(|e| Error::Credential(format!("random iv: {e}")))?;
    encrypt_credential_with(plain, key, &salt, &iv)
}

/// [`encrypt_credential`] with a caller-chosen salt and IV (tests, fake hubs).
pub fn encrypt_credential_with(
    plain: &str,
    key: &str,
    salt: &[u8],
    iv: &[u8],
) -> Result<String, Error> {
    let sealed = gcm::seal(&derive(key, salt), iv, &[], plain.as_bytes())
        .map_err(|e| Error::Credential(format!("encrypt: {e}")))?;
    Ok(format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(salt),
        URL_SAFE_NO_PAD.encode(iv),
        URL_SAFE_NO_PAD.encode(sealed)
    ))
}

#[cfg(test)]
mod tests;
