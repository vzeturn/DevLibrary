//! `CardIdentity.cs`: what a card read answers (POS-HW-04), the masked card (last four kept)
//! and the lowercase hex SHA-256 of the trimmed value, the hash EMP-01 card login and the
//! member lookups compare against. The number itself never leaves the adapter: it is not in
//! the result, an error or a log line.

use dl_hash::{Digest, Sha256};
use dl_json::{Value, json};

use crate::contracts::{AdapterError, codes};

/// Masked form and hash of a card read.
pub struct CardIdentity;

impl CardIdentity {
    /// The longest card value accepted, in UTF-16 units.
    pub const MAX_LENGTH: usize = 64;

    /// `{"masked", "hash", "length", "source"}` for the trimmed `raw`.
    pub fn of(raw: &str, source: &str) -> Result<Value, AdapterError> {
        let value = raw.trim();
        let length = value.encode_utf16().count();
        if length == 0 {
            return Err(AdapterError::new(
                codes::NO_CARD,
                "the reader sent an empty card read",
            ));
        }
        if length > Self::MAX_LENGTH {
            return Err(AdapterError::new(
                codes::ERROR,
                format!(
                    "the reader sent {length} characters, more than a card holds ({})",
                    Self::MAX_LENGTH
                ),
            ));
        }
        Ok(json!({
            "masked": Self::mask(value),
            "hash": Self::hash(value),
            "length": length,
            "source": source,
        }))
    }

    /// All but the last four characters starred; four or fewer are starred entirely.
    pub fn mask(value: &str) -> String {
        let chars: Vec<char> = value.chars().collect();
        if chars.len() <= 4 {
            return "*".repeat(chars.len());
        }
        let keep = chars.len() - 4;
        let mut out = "*".repeat(keep);
        out.extend(&chars[keep..]);
        out
    }

    /// Lowercase hex SHA-256 of the UTF-8 bytes.
    pub fn hash(value: &str) -> String {
        dl_encoding::hex::encode(Sha256::digest(value.as_bytes()))
    }
}
