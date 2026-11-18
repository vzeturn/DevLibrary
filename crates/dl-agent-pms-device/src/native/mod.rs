//! Vendor door-lock DLLs called through function pointers from
//! `dl_winos::dll::Library::symbol`. The only module of the crate allowed `unsafe`; every
//! unsafe block carries a `// SAFETY:` comment.
//!
//! Only LockSDK.dll (TP_*, 32-bit, `__stdcall`, ANSI) has a known header, and it is a
//! 32-bit DLL, so its binding exists only in the `i686-pc-windows-msvc` build (the C#
//! win-x86 `LOCKSDK_NATIVE` build). Elsewhere [`tp_lock`] is `None` and MFCARD refuses with
//! DEVICE_SDK_NOT_INSTALLED, like the C# agent without the native.

#![allow(unsafe_code)]

use std::sync::Arc;

use crate::adapters::lock::TpLockNative;

#[cfg(all(windows, target_arch = "x86"))]
mod locksdk;

#[cfg(all(windows, target_arch = "x86"))]
pub use locksdk::LockSdk;

/// The LockSDK binding of this build: `Some` only on 32-bit Windows. The DLL itself is
/// loaded on the first call, so a missing LockSDK.dll is reported per command (C#
/// `DllNotFoundException`), not at start-up.
pub fn tp_lock() -> Option<Arc<dyn TpLockNative>> {
    #[cfg(all(windows, target_arch = "x86"))]
    {
        Some(Arc::new(LockSdk::new()))
    }
    #[cfg(not(all(windows, target_arch = "x86")))]
    {
        None
    }
}
