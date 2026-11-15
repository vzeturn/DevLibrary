//! HMAC keys and key sets for rotation.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use dl_encoding::base64::URL_SAFE_NO_PAD;

use crate::{Algorithm, Error};

/// The shortest secret [`Key::from_secret`] accepts: 32 bytes, the minimum for HS256.
pub const MIN_SECRET_LEN: usize = 32;

/// An HMAC secret for signing and verifying tokens.
///
/// RFC 7518 section 3.2 requires the key to be at least as long as the hash output.
/// [`Key::from_secret`] rejects anything shorter than 32 bytes, and
/// [`encode`](crate::encode) / [`decode`](crate::decode) reject keys shorter than 48
/// bytes for HS384 and 64 bytes for HS512, all with [`Error::KeyTooShort`].
///
/// Generate secrets with a cryptographically secure random generator (for example
/// `openssl rand -base64 64` or `dl-rand`); a human-chosen password is not a key.
///
/// `Debug` never prints the secret, and the bytes are overwritten with zeros when the
/// key is dropped (best effort: copies made by the allocator or the hash functions are
/// not tracked).
///
/// ```
/// use dl_jwt::{Error, Key};
///
/// let key = Key::from_secret(b"an example secret of 32+ bytes!!")?;
/// assert_eq!(key.len(), 32);
/// assert_eq!(format!("{key:?}"), "Key { len: 32, .. }");
///
/// assert_eq!(
///     Key::from_secret(b"too short").unwrap_err(),
///     Error::KeyTooShort { len: 9, min: 32 }
/// );
/// # Ok::<(), Error>(())
/// ```
#[derive(Clone)]
pub struct Key {
    secret: Vec<u8>,
}

impl Key {
    /// Creates a key from raw secret bytes. Fails with [`Error::KeyTooShort`] when the
    /// secret is shorter than [`MIN_SECRET_LEN`] (32 bytes).
    ///
    /// To use a UTF-8 secret from configuration, pass its bytes:
    /// `Key::from_secret(secret.as_bytes())`. That is what ASP.NET Core
    /// (`Encoding.UTF8.GetBytes`), npm `jsonwebtoken` and PyJWT do with string secrets.
    pub fn from_secret(secret: impl AsRef<[u8]>) -> Result<Key, Error> {
        let secret = secret.as_ref();
        if secret.len() < MIN_SECRET_LEN {
            return Err(Error::KeyTooShort {
                len: secret.len(),
                min: MIN_SECRET_LEN,
            });
        }
        Ok(Key {
            secret: secret.to_vec(),
        })
    }

    /// Creates a key from unpadded base64url text, the encoding of the `k` member of a
    /// JSON Web Key (RFC 7518 section 6.4.1).
    ///
    /// ```
    /// // The key from RFC 7515 Appendix A.1.
    /// let key = dl_jwt::Key::from_base64url(
    ///     "AyM1SysPpbyDfgZld3umj1qzKObwVMkoqQ-EstJQLr_T-1qS0gZH75aKtMN3Yj0iPS4hcgUuTwjAzZr1Z9CAow",
    /// )?;
    /// assert_eq!(key.len(), 64);
    /// # Ok::<(), dl_jwt::Error>(())
    /// ```
    pub fn from_base64url(text: &str) -> Result<Key, Error> {
        let mut secret = URL_SAFE_NO_PAD
            .decode(text)
            .map_err(|_| Error::InvalidKeyEncoding)?;
        let key = Key::from_secret(&secret);
        wipe(&mut secret);
        key
    }

    /// Length of the secret in bytes.
    pub fn len(&self) -> usize {
        self.secret.len()
    }

    /// Always `false`: a key holds at least [`MIN_SECRET_LEN`] bytes.
    pub fn is_empty(&self) -> bool {
        self.secret.is_empty()
    }

    /// Checks that the key is long enough for `alg`.
    pub(crate) fn check_len(&self, alg: Algorithm) -> Result<(), Error> {
        let min = alg.min_key_len();
        if self.secret.len() < min {
            return Err(Error::KeyTooShort {
                len: self.secret.len(),
                min,
            });
        }
        Ok(())
    }

    pub(crate) fn secret(&self) -> &[u8] {
        &self.secret
    }
}

fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    // Keeps the compiler from treating the writes as dead stores.
    core::hint::black_box(&*bytes);
}

impl Drop for Key {
    fn drop(&mut self) {
        wipe(&mut self.secret);
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Key")
            .field("len", &self.secret.len())
            .finish_non_exhaustive()
    }
}

/// Keys indexed by key ID (`kid`), for verifying tokens while keys rotate.
///
/// Sign new tokens with the current key and put its ID in the header
/// ([`Header::key_id`](crate::Header::key_id)); keep the previous keys in the set until
/// every token they signed has expired. [`KeySet::decode`] picks the key named by the
/// token's `kid` and fails with [`Error::UnknownKey`] when the token has no `kid` or an
/// unknown one.
///
/// ```
/// use dl_jwt::{Algorithm, Claims, Header, Key, KeySet, Validation};
///
/// let mut keys = KeySet::new();
/// keys.insert("2024-05", Key::from_secret([1u8; 32])?);
/// keys.insert("2024-06", Key::from_secret([2u8; 32])?);
///
/// let now = 1_717_200_000;
/// let header = Header::new(Algorithm::HS256).key_id("2024-06");
/// let claims = Claims::new().subject("42").expires_at(now + 900);
/// let token = dl_jwt::encode(&header, &claims, keys.get("2024-06").unwrap())?;
///
/// let data = keys.decode::<Claims>(&token, &Validation::new(Algorithm::HS256), now)?;
/// assert_eq!(data.claims.sub.as_deref(), Some("42"));
/// # Ok::<(), dl_jwt::Error>(())
/// ```
#[derive(Clone, Debug, Default)]
pub struct KeySet {
    keys: Vec<(String, Key)>,
}

impl KeySet {
    /// Creates an empty set.
    pub const fn new() -> Self {
        KeySet { keys: Vec::new() }
    }

    /// Adds a key, or replaces the key with the same ID and returns it.
    pub fn insert(&mut self, kid: impl Into<String>, key: Key) -> Option<Key> {
        let kid = kid.into();
        match self.keys.iter_mut().find(|(k, _)| *k == kid) {
            Some((_, slot)) => Some(core::mem::replace(slot, key)),
            None => {
                self.keys.push((kid, key));
                None
            }
        }
    }

    /// Removes the key with this ID and returns it.
    pub fn remove(&mut self, kid: &str) -> Option<Key> {
        let pos = self.keys.iter().position(|(k, _)| k == kid)?;
        Some(self.keys.remove(pos).1)
    }

    /// Returns the key with this ID.
    pub fn get(&self, kid: &str) -> Option<&Key> {
        self.keys.iter().find(|(k, _)| k == kid).map(|(_, key)| key)
    }

    /// Number of keys.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// `true` if the set has no keys.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Iterates over the key IDs in insertion order.
    pub fn key_ids(&self) -> impl Iterator<Item = &str> {
        self.keys.iter().map(|(k, _)| k.as_str())
    }

    /// Verifies and validates `token` with the key named by its `kid` header.
    /// See [`decode`](crate::decode) for the checks and their order.
    pub fn decode<T: dl_json::FromJson>(
        &self,
        token: &str,
        validation: &crate::Validation,
        now: u64,
    ) -> Result<crate::TokenData<T>, Error> {
        crate::decode_with(
            token,
            |h| h.kid.as_deref().and_then(|kid| self.get(kid)),
            validation,
            now,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn lengths() {
        assert!(Key::from_secret([0u8; 32]).is_ok());
        assert_eq!(
            Key::from_secret([0u8; 31]).unwrap_err(),
            Error::KeyTooShort { len: 31, min: 32 }
        );
        assert_eq!(
            Key::from_secret(b"").unwrap_err(),
            Error::KeyTooShort { len: 0, min: 32 }
        );
        let k = Key::from_secret([0u8; 47]).unwrap();
        assert!(k.check_len(Algorithm::HS256).is_ok());
        assert_eq!(
            k.check_len(Algorithm::HS384),
            Err(Error::KeyTooShort { len: 47, min: 48 })
        );
        assert_eq!(
            k.check_len(Algorithm::HS512),
            Err(Error::KeyTooShort { len: 47, min: 64 })
        );
        assert!(!k.is_empty());
    }

    #[test]
    fn base64url() {
        // 32 bytes of 0xff.
        let k = Key::from_base64url("__________________________________________8").unwrap();
        assert_eq!(k.secret(), [0xff; 32]);
        // Padding, standard alphabet, whitespace and short keys are rejected.
        let padded = "__________________________________________8=";
        assert_eq!(
            Key::from_base64url(padded).unwrap_err(),
            Error::InvalidKeyEncoding
        );
        let std = "//////////////////////////////////////////8";
        assert_eq!(
            Key::from_base64url(std).unwrap_err(),
            Error::InvalidKeyEncoding
        );
        assert_eq!(
            Key::from_base64url(" AAAA").unwrap_err(),
            Error::InvalidKeyEncoding
        );
        assert_eq!(
            Key::from_base64url("AAAA").unwrap_err(),
            Error::KeyTooShort { len: 3, min: 32 }
        );
    }

    #[test]
    fn debug_is_redacted() {
        let k = Key::from_secret(b"super secret value that must not leak").unwrap();
        let s = format!("{k:?}");
        assert_eq!(s, "Key { len: 37, .. }");
        let mut set = KeySet::new();
        set.insert("a", k);
        assert!(!format!("{set:?}").contains("super"));
    }

    #[test]
    fn key_set() {
        let mut set = KeySet::new();
        assert!(set.is_empty());
        assert!(
            set.insert("a", Key::from_secret([1u8; 32]).unwrap())
                .is_none()
        );
        assert!(
            set.insert("b", Key::from_secret([2u8; 32]).unwrap())
                .is_none()
        );
        let old = set
            .insert("a", Key::from_secret([3u8; 32]).unwrap())
            .unwrap();
        assert_eq!(old.secret(), [1u8; 32]);
        assert_eq!(set.len(), 2);
        assert_eq!(set.get("a").unwrap().secret(), [3u8; 32]);
        assert!(set.get("c").is_none());
        assert_eq!(set.key_ids().collect::<alloc::vec::Vec<_>>(), ["a", "b"]);
        assert_eq!(set.remove("a").unwrap().secret(), [3u8; 32]);
        assert!(set.remove("a").is_none());
        assert_eq!(set.len(), 1);
    }
}
