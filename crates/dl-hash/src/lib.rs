#![doc = include_str!("../README.md")]
#![no_std]

#[cfg(test)]
extern crate std;

mod block;
mod crc;
mod hmac;
mod md5;
mod sha1;
mod sha256;
mod sha512;

pub use crc::{Crc32, crc32};
pub use hmac::{Hmac, pbkdf2};
pub use md5::Md5;
pub use sha1::Sha1;
pub use sha256::{Sha224, Sha256};
pub use sha512::{Sha384, Sha512};

/// A streaming cryptographic hash function.
pub trait Digest: Clone + Default {
    /// Digest size in bytes.
    const OUTPUT_LEN: usize;
    /// Internal block size in bytes (used by HMAC).
    const BLOCK_LEN: usize;
    /// The digest value, a byte array of `OUTPUT_LEN` bytes.
    type Output: AsRef<[u8]> + AsMut<[u8]> + Copy + core::fmt::Debug + PartialEq;

    /// Feeds more data into the hash.
    fn update(&mut self, data: &[u8]);

    /// Consumes the hasher and returns the digest.
    fn finalize(self) -> Self::Output;

    /// Feeds data and returns `self`, for chaining.
    fn chain(mut self, data: impl AsRef<[u8]>) -> Self {
        self.update(data.as_ref());
        self
    }

    /// Hashes `data` in one call.
    fn digest(data: impl AsRef<[u8]>) -> Self::Output {
        Self::default().chain(data).finalize()
    }
}

/// Compares two byte strings in time that depends only on their lengths.
///
/// Use this to compare MACs, password hashes and tokens. `==` on slices returns early
/// at the first difference and leaks timing information.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    core::hint::black_box(diff) == 0
}

#[cfg(test)]
pub(crate) fn hex(bytes: &[u8]) -> std::string::String {
    dl_encoding::hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_works() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
        assert!(ct_eq(b"", b""));
    }
}
