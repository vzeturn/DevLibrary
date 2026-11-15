//! Operating-system entropy sources. Each `fill` gets a non-empty buffer.
#![allow(unsafe_code)]

use crate::Error;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn fill(buf: &mut [u8]) -> Result<(), Error> {
    unsafe extern "C" {
        fn getrandom(buf: *mut u8, len: usize, flags: u32) -> isize;
    }
    let mut done = 0;
    while done < buf.len() {
        let rest = &mut buf[done..];
        // SAFETY: `rest` is a valid, writable slice; the kernel writes at most `rest.len()`
        // bytes into it. flags = 0 blocks only until the pool is initialised at boot.
        let n = unsafe { getrandom(rest.as_mut_ptr(), rest.len(), 0) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(Error::Os(err.raw_os_error().unwrap_or(-1)));
        }
        done += n as usize;
    }
    Ok(())
}

#[cfg(any(
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub(crate) fn fill(buf: &mut [u8]) -> Result<(), Error> {
    unsafe extern "C" {
        fn getentropy(buf: *mut u8, len: usize) -> i32;
    }
    // getentropy accepts at most 256 bytes per call.
    for chunk in buf.chunks_mut(256) {
        // SAFETY: `chunk` is a valid, writable slice of at most 256 bytes.
        if unsafe { getentropy(chunk.as_mut_ptr(), chunk.len()) } != 0 {
            return Err(Error::Os(
                std::io::Error::last_os_error().raw_os_error().unwrap_or(-1),
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn fill(buf: &mut [u8]) -> Result<(), Error> {
    #[link(name = "bcryptprimitives", kind = "raw-dylib")]
    unsafe extern "system" {
        fn ProcessPrng(data: *mut u8, len: usize) -> i32;
    }
    // SAFETY: `buf` is a valid, writable slice. ProcessPrng writes exactly `len` bytes and
    // is documented to always succeed on Windows 10 and later.
    let ok = unsafe { ProcessPrng(buf.as_mut_ptr(), buf.len()) };
    if ok == 0 { Err(Error::Os(0)) } else { Ok(()) }
}

#[cfg(all(target_arch = "wasm32", target_os = "wasi"))]
pub(crate) fn fill(buf: &mut [u8]) -> Result<(), Error> {
    #[link(wasm_import_module = "wasi_snapshot_preview1")]
    unsafe extern "C" {
        fn random_get(buf: *mut u8, len: usize) -> u16;
    }
    // SAFETY: `buf` is a valid, writable region of linear memory of `buf.len()` bytes.
    match unsafe { random_get(buf.as_mut_ptr(), buf.len()) } {
        0 => Ok(()),
        e => Err(Error::Os(i32::from(e))),
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub(crate) fn fill(buf: &mut [u8]) -> Result<(), Error> {
    #[link(wasm_import_module = "dl_rand")]
    unsafe extern "C" {
        fn fill(ptr: *mut u8, len: usize);
    }
    // crypto.getRandomValues rejects requests over 65 536 bytes.
    for chunk in buf.chunks_mut(65_536) {
        // SAFETY: `chunk` is a valid, writable region of linear memory; the host import
        // writes exactly `len` bytes starting at `ptr`.
        unsafe { fill(chunk.as_mut_ptr(), chunk.len()) };
    }
    Ok(())
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly",
    windows,
    target_arch = "wasm32"
)))]
pub(crate) fn fill(_buf: &mut [u8]) -> Result<(), Error> {
    Err(Error::Unsupported)
}
