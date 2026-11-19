//! The primitives the .NET agent took from `System.Security.Cryptography`: AES-256-GCM,
//! HKDF-SHA256, SHA-256 hex and secure random bytes. Written here because the workspace
//! takes no third-party crates; [`gcm`] is checked against vectors .NET produced.

pub mod aes;
pub mod gcm;

use dl_hash::{Digest, Hmac, Sha256};

use crate::Error;

pub use gcm::{AesGcm, NONCE_LEN, TAG_LEN};

/// `HKDF.DeriveKey(SHA256, ikm, 32, salt, info)` (RFC 5869, one output block).
pub fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let prk = Hmac::<Sha256>::mac(salt, ikm);
    Hmac::<Sha256>::new(&prk)
        .chain(info)
        .chain([1u8])
        .finalize()
}

/// SHA-256 of `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data)
}

/// Lower-case hex of the SHA-256 of `data` (`Convert.ToHexStringLower(SHA256.HashData(..))`).
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&sha256(data))
}

/// Lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// `N` bytes from the operating system's secure generator.
///
/// # Errors
/// [`Error::Crypto`] when the generator is unavailable.
pub fn random<const N: usize>() -> Result<[u8; N], Error> {
    dl_rand::secure_bytes::<N>().map_err(|e| Error::Crypto(format!("random: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hkdf_matches_rfc_5869_case_1_first_block() {
        let ikm = [0x0bu8; 22];
        let salt: Vec<u8> = (0u8..=0x0c).collect();
        let info: Vec<u8> = (0xf0u8..=0xf9).collect();
        assert_eq!(
            hex(&hkdf_sha256(&ikm, &salt, &info)),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf"
        );
    }
}
