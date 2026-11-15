//! JWS signature algorithms (RFC 7518 section 3).

use core::{fmt, str::FromStr};

use dl_hash::{Digest, Hmac, Sha256, Sha384, Sha512};

use crate::Error;

/// A JWS `alg` value supported by this crate: HMAC with SHA-2 (RFC 7518 section 3.2).
///
/// Asymmetric algorithms (`RS256`, `PS256`, `ES256`, `EdDSA`) are on the roadmap; until
/// then tokens that use them are rejected with [`Error::UnsupportedAlgorithm`]. The
/// unsecured `none` algorithm is never accepted.
///
/// ```
/// use dl_jwt::Algorithm;
///
/// assert_eq!(Algorithm::HS256.as_str(), "HS256");
/// assert_eq!("HS512".parse::<Algorithm>(), Ok(Algorithm::HS512));
/// assert!("none".parse::<Algorithm>().is_err());
/// assert_eq!(Algorithm::HS384.min_key_len(), 48);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
#[allow(clippy::upper_case_acronyms)]
pub enum Algorithm {
    /// HMAC using SHA-256. The most widely deployed JWT algorithm.
    HS256,
    /// HMAC using SHA-384.
    HS384,
    /// HMAC using SHA-512.
    HS512,
}

impl Algorithm {
    /// The registered `alg` name, e.g. `"HS256"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Algorithm::HS256 => "HS256",
            Algorithm::HS384 => "HS384",
            Algorithm::HS512 => "HS512",
        }
    }

    /// Size of the signature in bytes (the hash output size): 32, 48 or 64.
    pub const fn signature_len(self) -> usize {
        match self {
            Algorithm::HS256 => 32,
            Algorithm::HS384 => 48,
            Algorithm::HS512 => 64,
        }
    }

    /// Minimum HMAC key length in bytes. RFC 7518 section 3.2 requires a key at least as
    /// long as the hash output, so this equals [`signature_len`](Self::signature_len).
    pub const fn min_key_len(self) -> usize {
        self.signature_len()
    }

    /// Computes the HMAC of `msg` under `key`.
    pub(crate) fn sign(self, key: &[u8], msg: &[u8]) -> Mac {
        let mut mac = Mac {
            bytes: [0; 64],
            len: self.signature_len(),
        };
        match self {
            Algorithm::HS256 => mac.bytes[..32].copy_from_slice(&Hmac::<Sha256>::mac(key, msg)),
            Algorithm::HS384 => mac.bytes[..48].copy_from_slice(&Hmac::<Sha384>::mac(key, msg)),
            Algorithm::HS512 => mac.bytes[..64].copy_from_slice(&Hmac::<Sha512>::mac(key, msg)),
        }
        mac
    }
}

/// An HMAC tag of up to 64 bytes, kept on the stack.
pub(crate) struct Mac {
    bytes: [u8; 64],
    len: usize,
}

impl Mac {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

// The output sizes above must agree with the hash implementations.
const _: () = assert!(Sha256::OUTPUT_LEN == 32 && Sha384::OUTPUT_LEN == 48);
const _: () = assert!(Sha512::OUTPUT_LEN == 64);

impl fmt::Display for Algorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Algorithm {
    type Err = Error;

    /// Parses a registered `alg` name. Names are case-sensitive (RFC 7515 section 4.1.1),
    /// so `"hs256"` is rejected.
    fn from_str(s: &str) -> Result<Self, Error> {
        match s {
            "HS256" => Ok(Algorithm::HS256),
            "HS384" => Ok(Algorithm::HS384),
            "HS512" => Ok(Algorithm::HS512),
            other => Err(Error::UnsupportedAlgorithm(other.into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn names_round_trip() {
        for alg in [Algorithm::HS256, Algorithm::HS384, Algorithm::HS512] {
            assert_eq!(alg.as_str().parse::<Algorithm>(), Ok(alg));
            assert_eq!(alg.to_string(), alg.as_str());
        }
        for bad in [
            "none", "None", "hs256", "RS256", "ES256", "EdDSA", "PS256", "", " HS256",
        ] {
            assert_eq!(
                bad.parse::<Algorithm>(),
                Err(Error::UnsupportedAlgorithm(bad.into()))
            );
        }
    }

    #[test]
    fn rfc4231_through_sign() {
        // RFC 4231 test case 1 truncated key is too short for JWT use, but `sign` itself
        // does not check lengths; that happens in `encode`/`decode`.
        let key = [0x0b; 20];
        let tag = Algorithm::HS256.sign(&key, b"Hi There");
        assert_eq!(
            dl_encoding::hex::encode(tag.as_bytes()),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            Algorithm::HS384.sign(&key, b"Hi There").as_bytes().len(),
            48
        );
        assert_eq!(
            Algorithm::HS512.sign(&key, b"Hi There").as_bytes().len(),
            64
        );
    }
}
