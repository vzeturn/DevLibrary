#[cfg(feature = "std")]
use crate::Algorithm;
use crate::Error;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use dl_encoding::base32;

/// Minimum length of a newly generated secret: 128 bits (RFC 4226 section 4, R6).
#[cfg(feature = "std")]
const MIN_GENERATED_LEN: usize = 16;

/// A shared HOTP/TOTP secret (the HMAC key).
///
/// - `Debug` prints `Secret(<redacted>)`, never the key.
/// - `==` compares in constant time (for equal lengths) with [`dl_hash::ct_eq`].
/// - The bytes are overwritten with zeros when the value is dropped (see below).
///
/// # Zeroing on drop
///
/// `Drop` overwrites the whole heap allocation (length and spare capacity) with zeros and
/// passes it to [`core::hint::black_box`] so the compiler treats the zeros as observed.
/// This needs no `unsafe`, but it is best effort:
///
/// - `black_box` is documented as a hint, not a guarantee, so a future compiler could in
///   principle still drop the stores. (Volatile writes would need `unsafe`, which this
///   workspace denies.)
/// - Copies made outside this type are not covered: the buffer you passed to
///   [`from_bytes`](Self::from_bytes), the Base32 text (and the cleaned copy that
///   `dl_encoding::base32::decode_secret` makes internally), strings returned by
///   [`to_base32`](Self::to_base32) and the otpauth URI builders, and the HMAC key state
///   that `dl_hash::Hmac` keeps on the stack while a code is computed.
/// - Memory can still reach swap or core dumps while the secret is alive.
///
/// Treat it as defence in depth; keep secrets encrypted at rest and short-lived in memory.
pub struct Secret {
    /// Never reallocated after construction, so no stale copies are left behind by growth.
    bytes: Vec<u8>,
}

impl Secret {
    /// Wraps raw key bytes. Rejects an empty key with [`Error::SecretTooShort`].
    ///
    /// Any non-empty length is accepted, so legacy 80-bit secrets keep working. RFC 4226
    /// requires at least 128 bits and recommends 160 for new secrets; use
    /// `Secret::generate` (`std` feature) to create them.
    ///
    /// The bytes are copied: zero your own buffer afterwards if it matters.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        Self::from_vec(bytes.to_vec())
    }

    /// Decodes a Base32 secret as typed by a person or found in an otpauth URI.
    ///
    /// Lenient, through [`dl_encoding::base32::decode_secret`]: case-insensitive, ignores
    /// spaces and `-`, accepts missing or present `=` padding. Still rejects characters
    /// outside `A-Z2-7`, impossible lengths and non-zero trailing bits.
    ///
    /// ```
    /// use dl_otp::Secret;
    ///
    /// let a = Secret::from_base32("JBSW Y3DP EHPK 3PXP")?;
    /// let b = Secret::from_base32("jbswy3dpehpk3pxp")?;
    /// assert_eq!(a, b);
    /// assert_eq!(a.as_bytes(), b"Hello!\xde\xad\xbe\xef");
    /// assert!(Secret::from_base32("not base32!").is_err());
    /// # Ok::<(), dl_otp::Error>(())
    /// ```
    pub fn from_base32(text: &str) -> Result<Self, Error> {
        Self::from_vec(base32::decode_secret(text)?)
    }

    fn from_vec(bytes: Vec<u8>) -> Result<Self, Error> {
        if bytes.is_empty() {
            return Err(Error::SecretTooShort { len: 0, min: 1 });
        }
        Ok(Self { bytes })
    }

    /// Generates a random secret with the length recommended for `algorithm`
    /// ([`Algorithm::secret_len`]: 20, 32 or 64 bytes), from the OS CSPRNG.
    ///
    /// Requires the `std` feature. On `wasm32-unknown-unknown` the host must provide
    /// the `dl_rand.fill` import (see `dl-rand`).
    #[cfg(feature = "std")]
    pub fn generate(algorithm: Algorithm) -> Result<Self, Error> {
        Self::generate_len(algorithm.secret_len())
    }

    /// Generates a random secret of `len` bytes from the OS CSPRNG. `len` must be at least
    /// 16 (128 bits, the RFC 4226 minimum).
    ///
    /// Requires the `std` feature.
    #[cfg(feature = "std")]
    pub fn generate_len(len: usize) -> Result<Self, Error> {
        if len < MIN_GENERATED_LEN {
            return Err(Error::SecretTooShort {
                len,
                min: MIN_GENERATED_LEN,
            });
        }
        let mut bytes = alloc::vec![0u8; len];
        dl_rand::fill(&mut bytes)?;
        Ok(Self { bytes })
    }

    /// The raw key bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Encodes the key as upper-case RFC 4648 Base32 without padding, the form used in
    /// otpauth URIs and shown to users for manual entry.
    pub fn to_base32(&self) -> String {
        base32::STANDARD_NO_PAD.encode(&self.bytes)
    }
}

impl Clone for Secret {
    fn clone(&self) -> Self {
        Self {
            bytes: self.bytes.as_slice().to_vec(),
        }
    }
}

impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        dl_hash::ct_eq(&self.bytes, &other.bytes)
    }
}

impl Eq for Secret {}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.bytes);
    }
}

/// Overwrites the whole allocation of `v` with zeros, without reallocating.
pub(crate) fn wipe(v: &mut Vec<u8>) {
    let cap = v.capacity();
    v.clear();
    // `cap >= len`, so this writes zeros over the full buffer in place.
    v.resize(cap, 0);
    core::hint::black_box(v.as_mut_slice());
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::format;

    #[test]
    fn base32_roundtrip_and_leniency() {
        let s = Secret::from_bytes(b"12345678901234567890").unwrap();
        assert_eq!(s.to_base32(), "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        let t = Secret::from_base32("gezd gnbv-gy3t qojq gezd gnbv gy3t qojq").unwrap();
        assert_eq!(s, t);
        // Padding present or absent.
        assert_eq!(Secret::from_base32("MZXW6===").unwrap().as_bytes(), b"foo");
        assert_eq!(Secret::from_base32("MZXW6").unwrap().as_bytes(), b"foo");
        // No padding in the output, even when the length is not a multiple of 5.
        assert_eq!(Secret::from_bytes(b"foo").unwrap().to_base32(), "MZXW6");
    }

    #[test]
    fn rejects_bad_secrets() {
        assert_eq!(
            Secret::from_bytes(b""),
            Err(Error::SecretTooShort { len: 0, min: 1 })
        );
        assert_eq!(
            Secret::from_base32(""),
            Err(Error::SecretTooShort { len: 0, min: 1 })
        );
        assert_eq!(
            Secret::from_base32(" - = "),
            Err(Error::SecretTooShort { len: 0, min: 1 })
        );
        assert!(matches!(
            Secret::from_base32("JBSW Y3D!"),
            Err(Error::InvalidSecret(dl_encoding::Error::InvalidByte {
                index: 7,
                byte: b'!'
            }))
        ));
        assert!(matches!(
            Secret::from_base32("A"),
            Err(Error::InvalidSecret(_))
        ));
        assert!(matches!(
            Secret::from_base32("MZ"),
            Err(Error::InvalidSecret(_))
        ));
        assert!(matches!(
            Secret::from_base32("JBSWY3DPEHPK3PX0"),
            Err(Error::InvalidSecret(_))
        ));
        // Non-ASCII input is rejected, not a panic.
        assert!(matches!(
            Secret::from_base32("JBSWY3DPÉ"),
            Err(Error::InvalidSecret(_))
        ));
    }

    #[test]
    fn debug_is_redacted() {
        let s = Secret::from_bytes(b"super secret key").unwrap();
        let dbg = format!("{s:?}");
        assert_eq!(dbg, "Secret(<redacted>)");
        assert!(!dbg.contains("super"));
        assert!(!dbg.contains(&s.to_base32()));
        let alt = format!("{s:#?}");
        assert_eq!(alt, "Secret(<redacted>)");
    }

    #[test]
    fn equality() {
        let a = Secret::from_bytes(b"abc").unwrap();
        assert_eq!(a, a.clone());
        assert_ne!(a, Secret::from_bytes(b"abd").unwrap());
        assert_ne!(a, Secret::from_bytes(b"abcd").unwrap());
    }

    #[test]
    fn wipe_zeroes_len_and_capacity() {
        let mut v = std::vec::Vec::with_capacity(32);
        v.extend_from_slice(b"secret");
        let cap = v.capacity();
        wipe(&mut v);
        assert_eq!(v.capacity(), cap);
        assert_eq!(v.len(), cap);
        assert!(v.iter().all(|&b| b == 0));
    }

    #[cfg(feature = "std")]
    #[test]
    fn generate() {
        for (alg, len) in [
            (Algorithm::Sha1, 20),
            (Algorithm::Sha256, 32),
            (Algorithm::Sha512, 64),
        ] {
            let s = Secret::generate(alg).unwrap();
            assert_eq!(s.as_bytes().len(), len);
            assert_ne!(s, Secret::generate(alg).unwrap());
        }
        assert_eq!(Secret::generate_len(16).unwrap().as_bytes().len(), 16);
        assert_eq!(
            Secret::generate_len(15),
            Err(Error::SecretTooShort { len: 15, min: 16 })
        );
        assert_eq!(
            Secret::generate_len(0),
            Err(Error::SecretTooShort { len: 0, min: 16 })
        );
        // Base32 of a 20-byte secret is 32 characters, no padding.
        let b32 = Secret::generate(Algorithm::Sha1).unwrap().to_base32();
        assert_eq!(b32.len(), 32);
        assert_eq!(Secret::from_base32(&b32).unwrap().to_base32(), b32);
    }
}
