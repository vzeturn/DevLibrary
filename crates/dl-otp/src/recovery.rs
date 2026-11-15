//! Single-use recovery codes for when the authenticator is lost.

use alloc::string::String;
use dl_hash::{Digest, Sha256};

/// 32 characters without the look-alikes `0`, `o`, `1` and `l`. 32 is a power of two, so
/// masking a random byte to 5 bits picks each character with equal probability.
pub const RECOVERY_ALPHABET: &[u8; 32] = b"23456789abcdefghijkmnpqrstuvwxyz";

/// Shortest and longest accepted code, in characters (5 bits each).
#[cfg(feature = "std")]
const MIN_CHARS: usize = 8;
#[cfg(feature = "std")]
const MAX_CHARS: usize = 128;

/// Generates `count` distinct recovery codes of `groups` groups of `group_len`
/// characters joined by `-`, for example `recovery_codes(10, 2, 4)` gives ten codes like
/// `"k7m2-9xq4"`.
///
/// Characters come from [`RECOVERY_ALPHABET`] (lower case, no `0 o 1 l`), 5 bits of OS
/// entropy each: `2 x 4` characters is 40 bits. A code must have 8 to 128 characters in
/// total, otherwise [`Error::InvalidRecoveryFormat`](crate::Error::InvalidRecoveryFormat).
///
/// Show the codes to the user once, store only their hashes, and delete each hash when it
/// is used. With the plain [`hash_recovery_code`] use at least 16 characters (80 bits,
/// `recovery_codes(10, 4, 4)`); shorter codes need a salted KDF such as
/// `dl_hash::pbkdf2` (see the README).
///
/// Requires the `std` feature.
#[cfg(feature = "std")]
pub fn recovery_codes(
    count: usize,
    groups: usize,
    group_len: usize,
) -> Result<alloc::vec::Vec<String>, crate::Error> {
    let chars = groups
        .checked_mul(group_len)
        .filter(|n| (MIN_CHARS..=MAX_CHARS).contains(n));
    let chars = match chars {
        Some(n) if groups > 0 && group_len > 0 => n,
        _ => return Err(crate::Error::InvalidRecoveryFormat),
    };
    let mut codes = alloc::vec::Vec::with_capacity(count);
    let mut seen = alloc::collections::BTreeSet::new();
    let mut random = [0u8; MAX_CHARS];
    while codes.len() < count {
        dl_rand::fill(&mut random[..chars])?;
        let mut code = String::with_capacity(chars + groups - 1);
        for (i, &r) in random[..chars].iter().enumerate() {
            if i > 0 && i % group_len == 0 {
                code.push('-');
            }
            code.push(char::from(RECOVERY_ALPHABET[usize::from(r & 31)]));
        }
        // Duplicates are astronomically rare at 40+ bits but cost nothing to exclude.
        if seen.insert(normalize_recovery_code(&code)) {
            codes.push(code);
        }
    }
    random.fill(0);
    core::hint::black_box(&mut random);
    Ok(codes)
}

/// Canonical form used for hashing: ASCII letters lower-cased, ASCII whitespace and `-`
/// removed, so `"K7M2 9XQ4"`, `"k7m2-9xq4"` and `"k7m29xq4"` are the same code.
pub fn normalize_recovery_code(code: &str) -> String {
    code.chars()
        .filter(|c| !(c.is_ascii_whitespace() || *c == '-'))
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// SHA-256 of the [normalised](normalize_recovery_code) code, for storage.
///
/// A plain hash is only adequate when the code is long enough to resist offline
/// guessing if the hashes leak: a GPU computes billions of SHA-256 per second, so a
/// 40-bit code (8 characters) falls in minutes. For short codes use a salted, slow KDF
/// instead (`dl_hash::pbkdf2`, shown in the README), or use at least 16 characters
/// (80 bits), e.g. `recovery_codes(10, 4, 4)`.
pub fn hash_recovery_code(code: &str) -> [u8; 32] {
    Sha256::digest(normalize_recovery_code(code).as_bytes())
}

/// Checks `code` against a hash from [`hash_recovery_code`] in constant time.
///
/// When it returns `true`, delete that hash so the code cannot be used again.
pub fn verify_recovery_code(code: &str, stored_hash: &[u8]) -> bool {
    dl_hash::ct_eq(&hash_recovery_code(code), stored_hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alphabet_is_unambiguous() {
        for bad in b"0o1l" {
            assert!(!RECOVERY_ALPHABET.contains(bad));
        }
        let mut sorted = *RECOVERY_ALPHABET;
        sorted.sort_unstable();
        assert!(sorted.windows(2).all(|w| w[0] < w[1]), "no duplicates");
        assert!(
            RECOVERY_ALPHABET
                .iter()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
    }

    #[test]
    fn hash_and_verify() {
        // Python: hashlib.sha256(b"k7m29xq4").hexdigest()
        let expected = "69cceef717c4305699401af22925817422a6ae31103437cb39bc2534cd4a8351";
        let h = hash_recovery_code("k7m2-9xq4");
        assert_eq!(dl_encoding::hex::encode(h), expected);
        for same in ["k7m29xq4", "K7M2-9XQ4", " k7m2 9xq4\n", "k7m2--9xq4"] {
            assert_eq!(hash_recovery_code(same), h, "{same:?}");
            assert!(verify_recovery_code(same, &h));
        }
        assert!(!verify_recovery_code("k7m2-9xq5", &h));
        assert!(!verify_recovery_code("", &h));
        assert!(!verify_recovery_code("k7m2-9xq4", &h[..31]));
        assert_eq!(normalize_recovery_code("AB-cd ÉF"), "abcdÉf");
    }

    #[cfg(feature = "std")]
    #[test]
    fn generation_format_and_uniqueness() {
        let codes = recovery_codes(10, 2, 4).unwrap();
        assert_eq!(codes.len(), 10);
        for c in &codes {
            assert_eq!(c.len(), 9, "{c}");
            assert_eq!(c.as_bytes()[4], b'-');
            assert!(
                c.bytes()
                    .filter(|&b| b != b'-')
                    .all(|b| RECOVERY_ALPHABET.contains(&b))
            );
        }
        let set: std::collections::BTreeSet<_> = codes.iter().collect();
        assert_eq!(set.len(), 10);

        let codes = recovery_codes(3, 4, 4).unwrap();
        for c in &codes {
            let groups: std::vec::Vec<_> = c.split('-').collect();
            assert_eq!(groups.len(), 4);
            assert!(groups.iter().all(|g| g.len() == 4));
        }
        assert_eq!(recovery_codes(1, 1, 8).unwrap()[0].len(), 8);
        assert_eq!(recovery_codes(1, 1, 128).unwrap()[0].len(), 128);
        assert!(recovery_codes(0, 2, 4).unwrap().is_empty());

        // Many codes: all distinct, and every character of the alphabet shows up.
        let many = recovery_codes(2000, 2, 4).unwrap();
        let set: std::collections::BTreeSet<_> = many.iter().collect();
        assert_eq!(set.len(), 2000);
        for &ch in RECOVERY_ALPHABET {
            assert!(many.iter().any(|c| c.as_bytes().contains(&ch)));
        }
    }

    #[cfg(feature = "std")]
    #[test]
    fn generation_rejects_bad_shapes() {
        use crate::Error::InvalidRecoveryFormat;
        for (g, n) in [
            (0, 8),
            (8, 0),
            (1, 7),
            (1, 129),
            (usize::MAX, 2),
            (3, usize::MAX),
        ] {
            assert_eq!(
                recovery_codes(1, g, n),
                Err(InvalidRecoveryFormat),
                "{g}x{n}"
            );
        }
    }
}
