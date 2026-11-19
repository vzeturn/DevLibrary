//! PIN lookup v2 (`Staff/StaffPin.cs`, ADR 0101): `HMAC-SHA256(K_t, pin)` with the
//! tenant's PIN key; only the lookup leaves the store.

use dl_hash::{Digest, Hmac, Sha256};

/// Length of `K_t`.
pub const KEY_BYTES: usize = 32;
/// Shortest PIN the cloud keypad accepts.
pub const MIN_LENGTH: usize = 1;
/// Longest PIN the cloud keypad accepts.
pub const MAX_LENGTH: usize = 12;

/// A PIN key of the wrong length (C#: `ArgumentException`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyLengthError {
    /// The length given.
    pub len: usize,
}

impl std::fmt::Display for KeyLengthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "A PIN key is {KEY_BYTES} bytes.")
    }
}

impl std::error::Error for KeyLengthError {}

/// `HMAC-SHA256(key, UTF-8(pin))`: the stored `app.users.pin_lookup` for lookup v2.
pub fn lookup(key: &[u8], pin: &str) -> Result<[u8; 32], KeyLengthError> {
    if key.len() != KEY_BYTES {
        return Err(KeyLengthError { len: key.len() });
    }
    Ok(Hmac::<Sha256>::mac(key, pin.as_bytes()))
}

/// Public fingerprint of `K_t`: the first 16 hex chars of its SHA-256.
pub fn key_id(key: &[u8]) -> String {
    let mut hex = dl_encoding::hex::encode(Sha256::digest(key));
    hex.truncate(16);
    hex
}

/// True for 1 to 12 ASCII digits.
pub fn is_usable(pin: Option<&str>) -> bool {
    pin.is_some_and(|p| {
        let n = p.encode_utf16().count();
        (MIN_LENGTH..=MAX_LENGTH).contains(&n) && p.bytes().all(|b| b.is_ascii_digit())
    })
}
