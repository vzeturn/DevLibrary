// The README is the crate documentation. Its examples use `std`-only items
// (`Secret::generate`, `recovery_codes`), so builds without `std` get a short summary
// instead; docs.rs and `cargo doc` use the default features and show the full guide.
#![cfg_attr(feature = "std", doc = include_str!("../README.md"))]
#![cfg_attr(
    not(feature = "std"),
    doc = "HOTP (RFC 4226) and TOTP (RFC 6238) one-time passwords, `otpauth://` URIs and \
           recovery-code hashing, built without the `std` feature. See README.md for the \
           full guide."
)]
#![no_std]

extern crate alloc;
#[cfg(any(feature = "std", test))]
extern crate std;

mod error;
mod hotp;
mod recovery;
mod secret;
mod totp;
mod uri;

pub use error::Error;
pub use hotp::{Algorithm, Code, Hotp};
#[cfg(feature = "std")]
pub use recovery::recovery_codes;
pub use recovery::{
    RECOVERY_ALPHABET, hash_recovery_code, normalize_recovery_code, verify_recovery_code,
};
pub use secret::Secret;
pub use totp::Totp;
pub use uri::{OtpAuthUri, OtpKind};
