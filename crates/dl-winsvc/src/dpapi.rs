//! Windows Data Protection API (`CryptProtectData` / `CryptUnprotectData`).
//!
//! The blob is bound to this machine ([`Scope::LocalMachine`]: any account on the machine
//! can open it, which is what a service needs when an administrator sealed the data) or to
//! the current user ([`Scope::CurrentUser`]). No optional entropy and no description are
//! used, so blobs interoperate with other programs that call DPAPI the same way.
//!
//! Interoperating with the Go on-prem agent: it seals `BE32(len(plaintext)) || plaintext`
//! with `CRYPTPROTECT_LOCAL_MACHINE` and checks the prefix after opening. These functions
//! do no framing; add and strip the 4-byte big-endian length yourself.

use crate::Error;
use crate::raw::{CRYPTPROTECT_LOCAL_MACHINE, CRYPTPROTECT_UI_FORBIDDEN};
use crate::sys;

/// Who can open a protected blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Any account on this machine (`CRYPTPROTECT_LOCAL_MACHINE`).
    LocalMachine,
    /// Only the user that protected it.
    CurrentUser,
}

fn flags(scope: Scope) -> u32 {
    match scope {
        Scope::LocalMachine => CRYPTPROTECT_UI_FORBIDDEN | CRYPTPROTECT_LOCAL_MACHINE,
        Scope::CurrentUser => CRYPTPROTECT_UI_FORBIDDEN,
    }
}

/// Encrypts `data` (may be empty) into an opaque, integrity-protected blob.
///
/// Fails with [`Error::Unsupported`] outside Windows.
pub fn protect(data: &[u8], scope: Scope) -> Result<Vec<u8>, Error> {
    sys::crypt_protect(data, flags(scope))
}

/// Decrypts a blob made by [`protect`] or any DPAPI caller with the same scope. A blob that
/// was altered, or sealed for another user or machine, fails with [`Error::Win32`] (usually
/// code 13, `ERROR_INVALID_DATA`, or `0x8009000B`).
pub fn unprotect(blob: &[u8], scope: Scope) -> Result<Vec<u8>, Error> {
    if blob.is_empty() {
        return Err(Error::InvalidInput("empty DPAPI blob"));
    }
    sys::crypt_unprotect(blob, flags(scope))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn round_trips_in_both_scopes() {
        for scope in [Scope::LocalMachine, Scope::CurrentUser] {
            for data in [&b""[..], b"x", "Mật khẩu: p@ss".as_bytes()] {
                let blob = protect(data, scope).unwrap();
                assert_ne!(blob, data);
                assert_eq!(unprotect(&blob, scope).unwrap(), data);
            }
        }
    }

    #[test]
    fn one_mebibyte() {
        let data: Vec<u8> = (0..1u32 << 20).map(|i| (i * 31 % 251) as u8).collect();
        let blob = protect(&data, Scope::LocalMachine).unwrap();
        assert_eq!(unprotect(&blob, Scope::LocalMachine).unwrap(), data);
    }

    #[test]
    fn tampering_is_detected() {
        let mut blob = protect(b"secret", Scope::LocalMachine).unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        assert!(matches!(
            unprotect(&blob, Scope::LocalMachine),
            Err(Error::Win32 { .. })
        ));
        assert!(matches!(
            unprotect(&[], Scope::LocalMachine),
            Err(Error::InvalidInput(_))
        ));
        assert!(unprotect(b"not a blob", Scope::LocalMachine).is_err());
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn unsupported_elsewhere() {
        assert_eq!(protect(b"x", Scope::LocalMachine), Err(Error::Unsupported));
        assert_eq!(unprotect(b"x", Scope::CurrentUser), Err(Error::Unsupported));
    }
}
