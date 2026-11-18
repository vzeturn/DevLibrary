//! Win32 declarations and the safe wrappers behind [`crate::dll`]: the DLL loader and the
//! ANSI code page conversions. With `sys_windows.rs`, `com_windows.rs` and
//! [`Library::symbol`](crate::dll::Library::symbol) the only places in the crate that
//! contain `unsafe`.
#![allow(unsafe_code)]

use core::ffi::c_void;
use core::ptr::{self, NonNull};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use crate::Error;

const SEM_FAILCRITICALERRORS: u32 = 0x0001;
const SEM_NOOPENFILEERRORBOX: u32 = 0x8000;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const CP_ACP: u32 = 0;
const CP_UTF8: u32 = 65_001;
const CP_GB18030: u32 = 54_936;
const WC_NO_BEST_FIT_CHARS: u32 = 0x0000_0400;
/// Long paths are at most 32 767 UTF-16 units.
const MAX_PATH_UNITS: usize = 32_768;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn GetModuleFileNameW(module: *mut c_void, buf: *mut u16, size: u32) -> u32;
    fn SetThreadErrorMode(mode: u32, old: *mut u32) -> i32;
    fn GetACP() -> u32;
    fn WideCharToMultiByte(
        code_page: u32,
        flags: u32,
        wide: *const u16,
        wide_len: i32,
        out: *mut u8,
        out_len: i32,
        default_char: *const u8,
        used_default: *mut i32,
    ) -> i32;
    fn MultiByteToWideChar(
        code_page: u32,
        flags: u32,
        bytes: *const u8,
        len: i32,
        out: *mut u16,
        out_len: i32,
    ) -> i32;
}

fn last_code() -> u32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32
}

fn last_error(op: &'static str) -> Error {
    Error::Win32 {
        op,
        code: last_code(),
    }
}

// ---------------------------------------------------------------------------------------
// Loader
// ---------------------------------------------------------------------------------------

/// A loaded module (`HMODULE`) this value holds one load reference to; `FreeLibrary` on
/// drop.
pub(crate) struct Module(NonNull<c_void>);

// SAFETY: module handles are process-wide; GetProcAddress, GetModuleFileNameW and
// FreeLibrary may be called with them from any thread.
unsafe impl Send for Module {}
// SAFETY: as above; the shared operations only read loader state, which the loader locks.
unsafe impl Sync for Module {}

impl Drop for Module {
    fn drop(&mut self) {
        // SAFETY: self.0 came from a successful LoadLibraryExW and this value owns that
        // reference, released exactly once (leak() forgets the value instead).
        unsafe { FreeLibrary(self.0.as_ptr()) };
    }
}

impl Module {
    pub(crate) fn addr(&self) -> usize {
        self.0.as_ptr() as usize
    }
}

/// Restores the thread error mode on drop.
struct ErrorMode(u32);

impl ErrorMode {
    /// Suppresses the "bad image" and "insert disk" message boxes on this thread, as
    /// a service or agent without a desktop must never block on a dialog.
    fn quiet() -> Option<ErrorMode> {
        let mut old = 0u32;
        // SAFETY: valid SEM_* flags; old is a valid place for the previous mode.
        let ok = unsafe {
            SetThreadErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX, &mut old)
        };
        (ok != 0).then_some(ErrorMode(old))
    }
}

impl Drop for ErrorMode {
    fn drop(&mut self) {
        // SAFETY: restores the mode saved by quiet(); NULL means the old value is unwanted.
        unsafe { SetThreadErrorMode(self.0, ptr::null_mut()) };
    }
}

/// `LoadLibraryExW(name, NULL, flags)`; on failure `Error::Win32` with the
/// `GetLastError` code.
pub(crate) fn load(name: &[u16], flags: u32) -> Result<Module, Error> {
    let mode = ErrorMode::quiet();
    // SAFETY: name is NUL-terminated (wide::to_wide) and outlives the call; the reserved
    // file handle must be NULL; flags are LOAD_LIBRARY_SEARCH_* values or 0.
    let h = unsafe { LoadLibraryExW(name.as_ptr(), ptr::null_mut(), flags) };
    // Read the code before restoring the error mode can touch it.
    let result = match NonNull::new(h) {
        Some(h) => Ok(Module(h)),
        None => Err(last_error("LoadLibraryExW")),
    };
    drop(mode);
    result
}

/// Gives up the load reference without `FreeLibrary`, so the module stays loaded.
pub(crate) fn leak(m: Module) {
    core::mem::forget(m);
}

/// `GetProcAddress`; `name` is NUL-terminated. On failure `Error::Win32` with the
/// `GetLastError` code.
pub(crate) fn proc_address(m: &Module, name: &[u8]) -> Result<NonNull<c_void>, Error> {
    // SAFETY: m holds a loaded module; name is NUL-terminated and outlives the call.
    let p = unsafe { GetProcAddress(m.0.as_ptr(), name.as_ptr()) };
    NonNull::new(p).ok_or_else(|| last_error("GetProcAddress"))
}

pub(crate) fn module_path(m: &Module) -> Result<PathBuf, Error> {
    let mut buf = vec![0u16; 260];
    loop {
        let size = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        // SAFETY: m holds a loaded module; buf has `size` writable units.
        let n = unsafe { GetModuleFileNameW(m.0.as_ptr(), buf.as_mut_ptr(), size) } as usize;
        if n == 0 {
            return Err(last_error("GetModuleFileNameW"));
        }
        // n == size means the path was truncated to fit.
        if n < buf.len() {
            return Ok(PathBuf::from(OsString::from_wide(&buf[..n])));
        }
        if buf.len() >= MAX_PATH_UNITS {
            return Err(Error::Win32 {
                op: "GetModuleFileNameW",
                code: ERROR_INSUFFICIENT_BUFFER,
            });
        }
        buf.resize(buf.len() * 2, 0);
    }
}

// ---------------------------------------------------------------------------------------
// ANSI code page
// ---------------------------------------------------------------------------------------

pub(crate) fn acp() -> Result<u32, Error> {
    // SAFETY: no arguments; always succeeds.
    Ok(unsafe { GetACP() })
}

fn len_i32(len: usize) -> Result<i32, Error> {
    i32::try_from(len).map_err(|_| Error::InvalidInput("string too long for the ANSI code page"))
}

/// `wide` (not empty, no NUL) in the ANSI code page `code_page` (the value of
/// [`acp`]), or `None` when a character has no exact mapping.
pub(crate) fn to_acp(wide: &[u16], code_page: u32) -> Result<Option<Vec<u8>>, Error> {
    // UTF-8 and GB18030 map all of Unicode and reject both the flag and the
    // used-default output.
    let exact = !matches!(code_page, CP_UTF8 | CP_GB18030);
    let (need, used) = wide_to_mb(wide, exact, &mut [])?;
    if used {
        return Ok(None);
    }
    let mut out = vec![0u8; need];
    let (n, used) = wide_to_mb(wide, exact, &mut out)?;
    if used {
        return Ok(None);
    }
    out.truncate(n);
    Ok(Some(out))
}

/// One `WideCharToMultiByte(CP_ACP)` call into `out` (an empty `out` asks for the size).
/// Returns the byte count and whether the default character was used.
fn wide_to_mb(wide: &[u16], exact: bool, out: &mut [u8]) -> Result<(usize, bool), Error> {
    let wide_len = len_i32(wide.len())?;
    let out_len = len_i32(out.len())?;
    let out_ptr = if out.is_empty() {
        ptr::null_mut()
    } else {
        out.as_mut_ptr()
    };
    let flags = if exact { WC_NO_BEST_FIT_CHARS } else { 0 };
    let mut used = 0i32;
    let used_ptr: *mut i32 = if exact { &mut used } else { ptr::null_mut() };
    // SAFETY: wide has `wide_len` readable units; out_ptr is NULL with length 0 (size
    // query) or has `out_len` writable bytes; used_ptr is NULL or a valid i32 place.
    let n = unsafe {
        WideCharToMultiByte(
            CP_ACP,
            flags,
            wide.as_ptr(),
            wide_len,
            out_ptr,
            out_len,
            ptr::null(),
            used_ptr,
        )
    };
    if n <= 0 {
        return Err(last_error("WideCharToMultiByte"));
    }
    Ok((n as usize, used != 0))
}

/// `bytes` (no NUL) in the ANSI code page as UTF-16. Invalid sequences become the
/// system's replacement character.
pub(crate) fn from_acp(bytes: &[u8]) -> Result<Vec<u16>, Error> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let len = len_i32(bytes.len())?;
    // SAFETY: bytes has `len` readable bytes; NULL output with length 0 asks for the size.
    let need = unsafe { MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), len, ptr::null_mut(), 0) };
    if need <= 0 {
        return Err(last_error("MultiByteToWideChar"));
    }
    let mut out = vec![0u16; need as usize];
    // SAFETY: as above, and out has `need` writable units.
    let n = unsafe { MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), len, out.as_mut_ptr(), need) };
    if n <= 0 {
        return Err(last_error("MultiByteToWideChar"));
    }
    out.truncate(n as usize);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dll::{Library, Search};
    use crate::wide::to_wide;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }

    fn is_loaded(name: &str) -> bool {
        let w = to_wide(name).unwrap();
        // SAFETY: w is NUL-terminated; GetModuleHandleW adds no reference.
        let h = unsafe { GetModuleHandleW(w.as_ptr()) };
        !h.is_null()
    }

    #[test]
    fn drop_unloads_and_leak_keeps() {
        // No other test in this crate loads winsqlite3.dll, so its load count is ours.
        assert!(!is_loaded("winsqlite3.dll"));
        let lib = Library::open("winsqlite3.dll", Search::System32).unwrap();
        let again = Library::open("winsqlite3.dll", Search::System32).unwrap();
        assert_eq!(lib.handle_addr(), again.handle_addr());
        assert!(is_loaded("winsqlite3.dll"));
        drop(again);
        assert!(is_loaded("winsqlite3.dll"), "one reference left");
        drop(lib);
        assert!(!is_loaded("winsqlite3.dll"));

        // Likewise only this test loads msimg32.dll.
        Library::open("msimg32.dll", Search::System32)
            .unwrap()
            .leak();
        assert!(is_loaded("msimg32.dll"));
    }

    #[test]
    fn error_mode_is_restored() {
        let before = ErrorMode::quiet().map(|m| m.0);
        let _ = load(&to_wide("dl-winos-no-such.dll").unwrap(), 0);
        let after = ErrorMode::quiet().map(|m| m.0);
        assert_eq!(before, after);
    }

    #[test]
    fn conversions_of_empty_and_ascii() {
        assert_eq!(from_acp(&[]).unwrap(), Vec::<u16>::new());
        assert_eq!(from_acp(b"AB").unwrap(), vec![0x41, 0x42]);
        let cp = acp().unwrap();
        let w: Vec<u16> = "Phong 101".encode_utf16().collect();
        assert_eq!(to_acp(&w, cp).unwrap().as_deref(), Some(&b"Phong 101"[..]));
    }
}
