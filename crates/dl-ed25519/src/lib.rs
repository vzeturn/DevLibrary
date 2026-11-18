#![doc = include_str!("../README.md")]
#![no_std]

#[cfg(test)]
extern crate std;

mod field;
mod point;
mod scalar;

use core::fmt;

use dl_hash::{Digest, Sha512};
use point::Point;

/// Length of a seed (the private key as Go's `Seed()` and RFC 8032 store it).
pub const SEED_LEN: usize = 32;
/// Length of a public key.
pub const PUBLIC_KEY_LEN: usize = 32;
/// Length of a signature.
pub const SIGNATURE_LEN: usize = 64;

/// Why a key or signature was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The public key is not 32 bytes or not a point on the curve.
    InvalidPublicKey,
    /// The signature is not 64 bytes, its S is not below the group order, or it does not
    /// match the message and key.
    InvalidSignature,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Error::InvalidPublicKey => "invalid Ed25519 public key",
            Error::InvalidSignature => "invalid Ed25519 signature",
        })
    }
}

impl core::error::Error for Error {}

/// A private key: the 32-byte seed and what is derived from it.
///
/// `Debug` does not print the secret. The memory is not wiped on drop.
#[derive(Clone)]
pub struct SigningKey {
    seed: [u8; 32],
    scalar: [u8; 32],
    prefix: [u8; 32],
    public: VerifyingKey,
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SigningKey")
            .field("public", &self.public)
            .finish_non_exhaustive()
    }
}

impl SigningKey {
    /// The key for a 32-byte seed (RFC 8032 section 5.1.5; Go's `NewKeyFromSeed`). Use
    /// 32 bytes from a cryptographically secure source, e.g. `dl_rand::secure_bytes`.
    ///
    /// ```
    /// let key = dl_ed25519::SigningKey::from_seed(&[7u8; 32]);
    /// let sig = key.sign(b"hello");
    /// assert!(key.verifying_key().verify(b"hello", &sig).is_ok());
    /// ```
    pub fn from_seed(seed: &[u8; SEED_LEN]) -> SigningKey {
        let h = Sha512::digest(seed);
        let mut scalar = [0u8; 32];
        scalar.copy_from_slice(&h[..32]);
        scalar[0] &= 248;
        scalar[31] &= 127;
        scalar[31] |= 64;
        let mut prefix = [0u8; 32];
        prefix.copy_from_slice(&h[32..]);
        let public = VerifyingKey(Point::base().mul(&scalar).encode());
        SigningKey {
            seed: *seed,
            scalar,
            prefix,
            public,
        }
    }

    /// The seed this key was made from.
    pub fn seed(&self) -> &[u8; SEED_LEN] {
        &self.seed
    }

    /// The public key.
    pub fn verifying_key(&self) -> VerifyingKey {
        self.public
    }

    /// Signs `message` (RFC 8032 section 5.1.6). Deterministic: the same key and message
    /// always give the same signature.
    pub fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LEN] {
        let r = scalar::reduce_wide(
            &Sha512::default()
                .chain(self.prefix)
                .chain(message)
                .finalize(),
        );
        let r_enc = Point::base().mul(&scalar::to_bytes(&r)).encode();
        let k = scalar::reduce_wide(
            &Sha512::default()
                .chain(r_enc)
                .chain(self.public.0)
                .chain(message)
                .finalize(),
        );
        let s = scalar::mul_add(&k, &scalar::from_bytes(&self.scalar), &r);
        let mut sig = [0u8; SIGNATURE_LEN];
        sig[..32].copy_from_slice(&r_enc);
        sig[32..].copy_from_slice(&scalar::to_bytes(&s));
        sig
    }
}

/// A public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VerifyingKey([u8; PUBLIC_KEY_LEN]);

impl VerifyingKey {
    /// Parses a public key.
    ///
    /// # Errors
    /// [`Error::InvalidPublicKey`] if it is not a point on the curve.
    pub fn from_bytes(bytes: &[u8; PUBLIC_KEY_LEN]) -> Result<VerifyingKey, Error> {
        Point::decode(bytes)
            .map(|_| VerifyingKey(*bytes))
            .ok_or(Error::InvalidPublicKey)
    }

    /// The 32-byte encoding.
    pub fn as_bytes(&self) -> &[u8; PUBLIC_KEY_LEN] {
        &self.0
    }

    /// Checks a signature (RFC 8032 section 5.1.7, the cofactorless equation that Go's
    /// `crypto/ed25519.Verify` uses): S must be below the group order and
    /// `[S]B - [k]A` must encode to R.
    ///
    /// # Errors
    /// [`Error::InvalidSignature`] for a bad signature, [`Error::InvalidPublicKey`] if the
    /// key is not a point (only possible for keys built with `from_bytes` on other data).
    pub fn verify(&self, message: &[u8], signature: &[u8; SIGNATURE_LEN]) -> Result<(), Error> {
        let mut s = [0u8; 32];
        s.copy_from_slice(&signature[32..]);
        if !scalar::is_canonical(&s) {
            return Err(Error::InvalidSignature);
        }
        let a = Point::decode(&self.0).ok_or(Error::InvalidPublicKey)?;
        let k = scalar::reduce_wide(
            &Sha512::default()
                .chain(&signature[..32])
                .chain(self.0)
                .chain(message)
                .finalize(),
        );
        let r = Point::base()
            .mul(&s)
            .add(&a.neg().mul(&scalar::to_bytes(&k)));
        if dl_hash::ct_eq(&r.encode(), &signature[..32]) {
            Ok(())
        } else {
            Err(Error::InvalidSignature)
        }
    }
}

/// Go's `ed25519.Verify`: `false` for a key or signature of the wrong length, a key that
/// is not a point, or a signature that does not verify.
///
/// ```
/// let key = dl_ed25519::SigningKey::from_seed(&[1u8; 32]);
/// let sig = key.sign(b"m");
/// assert!(dl_ed25519::verify(key.verifying_key().as_bytes(), b"m", &sig));
/// assert!(!dl_ed25519::verify(key.verifying_key().as_bytes(), b"n", &sig));
/// assert!(!dl_ed25519::verify(&[0u8; 31], b"m", &sig));
/// ```
pub fn verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    let (Ok(pk), Ok(sig)) = (
        <&[u8; PUBLIC_KEY_LEN]>::try_from(public_key),
        <&[u8; SIGNATURE_LEN]>::try_from(signature),
    ) else {
        return false;
    };
    VerifyingKey(*pk).verify(message, sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::String;
    use std::vec::Vec;

    fn hex(s: &str) -> Vec<u8> {
        dl_encoding::hex::decode(s).unwrap()
    }

    fn arr<const N: usize>(v: &[u8]) -> [u8; N] {
        v.try_into().unwrap()
    }

    /// RFC 8032 section 7.1, TEST 1-3 and TEST SHA(abc).
    #[test]
    fn rfc8032_vectors() {
        let cases = [
            (
                "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
                "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
                "",
                "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
            ),
            (
                "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
                "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
                "72",
                "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
            ),
            (
                "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
                "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
                "af82",
                "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
            ),
        ];
        for (seed, public, msg, sig) in cases {
            let key = SigningKey::from_seed(&arr(&hex(seed)));
            assert_eq!(
                key.verifying_key().as_bytes().as_slice(),
                hex(public).as_slice()
            );
            let got = key.sign(&hex(msg));
            assert_eq!(got.as_slice(), hex(sig).as_slice(), "seed {seed}");
            assert!(key.verifying_key().verify(&hex(msg), &got).is_ok());
        }
    }

    /// Vectors generated with Go's crypto/ed25519 (testdata/gen_go_vectors.go): signing
    /// with random seeds and messages, and Go's Verify verdict on altered inputs.
    #[test]
    fn go_vectors() {
        let data = include_str!("../testdata/go_vectors.txt");
        let (mut signs, mut verifies, mut rejected) = (0, 0, 0);
        for line in data
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            let f: Vec<&str> = line.split(' ').collect();
            match f[0] {
                "sign" => {
                    let key = SigningKey::from_seed(&arr(&hex(f[1])));
                    assert_eq!(hex(f[2]), key.verifying_key().as_bytes(), "{line}");
                    assert_eq!(hex(f[4]), key.sign(&hex(f[3])), "{line}");
                    signs += 1;
                }
                "verify" => {
                    let want = f[4] == "1";
                    assert_eq!(verify(&hex(f[1]), &hex(f[2]), &hex(f[3])), want, "{line}");
                    verifies += 1;
                    if !want {
                        rejected += 1;
                    }
                }
                other => panic!("unknown line kind {other}"),
            }
        }
        assert!(
            signs >= 100 && verifies >= 500 && rejected >= 300,
            "{signs} {verifies} {rejected}"
        );
        let _ = String::new();
    }

    #[test]
    fn rejections() {
        let key = SigningKey::from_seed(&[3u8; 32]);
        let mut sig = key.sign(b"x");
        // S + L is not canonical.
        let s = scalar::from_bytes(&arr(&sig[32..]));
        let l = scalar::from_bytes(&arr(&hex(
            "edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010",
        )));
        let mut sum = [0u64; 4];
        let mut carry = 0u128;
        for i in 0..4 {
            let t = u128::from(s[i]) + u128::from(l[i]) + carry;
            sum[i] = t as u64;
            carry = t >> 64;
        }
        sig[32..].copy_from_slice(&scalar::to_bytes(&sum));
        assert_eq!(
            key.verifying_key().verify(b"x", &sig),
            Err(Error::InvalidSignature)
        );
        assert!(!verify(key.verifying_key().as_bytes(), b"x", &sig[..63]));
        let dbg = std::format!("{key:?}");
        assert!(!dbg.contains("seed") && dbg.contains("public"));
    }
}
