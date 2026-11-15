#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;
#[cfg(all(test, not(feature = "std")))]
extern crate std;

mod algorithm;
mod claims;
mod error;
mod header;
mod key;
mod token;
mod validation;

#[cfg(test)]
mod tests;

pub use algorithm::Algorithm;
pub use claims::{Audience, Claims};
pub use error::{Error, Part};
pub use header::Header;
pub use key::{Key, KeySet, MIN_SECRET_LEN};
pub use token::{
    TokenData, Unverified, decode, decode_header, decode_unverified, decode_with, encode,
};
pub use validation::{DEFAULT_LEEWAY_SECS, DEFAULT_MAX_TOKEN_LEN, Validation};

#[cfg(all(
    feature = "std",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
pub use token::decode_now;

/// The current time in Unix seconds, from [`std::time::SystemTime`]. Returns 0 if the
/// system clock is set before 1970.
///
/// Only with the `std` feature (on by default) and not on `wasm32-unknown-unknown`,
/// where `SystemTime::now` panics; pass the time explicitly there.
#[cfg(all(
    feature = "std",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
