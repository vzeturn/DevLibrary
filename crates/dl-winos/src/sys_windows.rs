//! Win32 declarations and the safe wrappers the registry and spooler modules use. With
//! `com_windows.rs` (COM) the only file in the crate that contains `unsafe`.
#![allow(unsafe_code)]

use core::ffi::c_void;
use core::mem::size_of;
use core::ptr;

use crate::Error;
use crate::wide::until_nul;

type Handle = *mut c_void;

const ERROR_SUCCESS: u32 = 0;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_WRITE_FAULT: u32 = 29;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_MORE_DATA: u32 = 234;
const ERROR_NO_MORE_ITEMS: u32 = 259;

/// How often a size query is retried when the data grows between two calls.
const MAX_TRIES: usize = 16;
/// Value names are at most 16 383 characters; key names at most 255.
const MAX_NAME_UNITS: usize = 32_768;
/// Bytes per `WritePrinter` call.
const WRITE_CHUNK: usize = 1 << 20;

const PRINTER_ENUM_LOCAL: u32 = 0x0000_0002;
const PRINTER_ENUM_CONNECTIONS: u32 = 0x0000_0004;
const RAW: [u16; 4] = [b'R' as u16, b'A' as u16, b'W' as u16, 0];

#[repr(C)]
#[derive(Clone, Copy)]
struct PrinterInfo2W {
    server_name: *const u16,
    printer_name: *const u16,
    share_name: *const u16,
    port_name: *const u16,
    driver_name: *const u16,
    comment: *const u16,
    location: *const u16,
    dev_mode: *const c_void,
    sep_file: *const u16,
    print_processor: *const u16,
    datatype: *const u16,
    parameters: *const u16,
    security_descriptor: *const c_void,
    attributes: u32,
    priority: u32,
    default_priority: u32,
    start_time: u32,
    until_time: u32,
    status: u32,
    jobs: u32,
    average_ppm: u32,
}

#[repr(C)]
struct DocInfo1W {
    doc_name: *const u16,
    output_file: *const u16,
    datatype: *const u16,
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        key: Handle,
        subkey: *const u16,
        options: u32,
        access: u32,
        result: *mut Handle,
    ) -> u32;
    fn RegCloseKey(key: Handle) -> u32;
    fn RegQueryValueExW(
        key: Handle,
        name: *const u16,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        data_len: *mut u32,
    ) -> u32;
    fn RegEnumKeyExW(
        key: Handle,
        index: u32,
        name: *mut u16,
        name_len: *mut u32,
        reserved: *mut u32,
        class: *mut u16,
        class_len: *mut u32,
        last_write: *mut c_void,
    ) -> u32;
    fn RegEnumValueW(
        key: Handle,
        index: u32,
        name: *mut u16,
        name_len: *mut u32,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        data_len: *mut u32,
    ) -> u32;
    fn RegQueryInfoKeyW(
        key: Handle,
        class: *mut u16,
        class_len: *mut u32,
        reserved: *mut u32,
        subkeys: *mut u32,
        max_subkey_len: *mut u32,
        max_class_len: *mut u32,
        values: *mut u32,
        max_value_name_len: *mut u32,
        max_value_len: *mut u32,
        security_descriptor_len: *mut u32,
        last_write: *mut c_void,
    ) -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn ExpandEnvironmentStringsW(src: *const u16, dst: *mut u16, size: u32) -> u32;
}

#[link(name = "winspool")]
unsafe extern "system" {
    fn EnumPrintersW(
        flags: u32,
        name: *const u16,
        level: u32,
        buf: *mut u8,
        size: u32,
        needed: *mut u32,
        returned: *mut u32,
    ) -> i32;
    fn GetDefaultPrinterW(buf: *mut u16, size: *mut u32) -> i32;
    fn OpenPrinterW(name: *const u16, printer: *mut Handle, defaults: *const c_void) -> i32;
    fn ClosePrinter(printer: Handle) -> i32;
    fn StartDocPrinterW(printer: Handle, level: u32, doc: *const DocInfo1W) -> u32;
    fn StartPagePrinter(printer: Handle) -> i32;
    fn WritePrinter(printer: Handle, buf: *const c_void, size: u32, written: *mut u32) -> i32;
    fn EndPagePrinter(printer: Handle) -> i32;
    fn EndDocPrinter(printer: Handle) -> i32;
    fn AbortPrinter(printer: Handle) -> i32;
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

fn status(code: u32, op: &'static str) -> Result<(), Error> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(Error::Win32 { op, code })
    }
}

fn len_u32(len: usize) -> u32 {
    // Buffers here are capped well below 4 GiB; saturate rather than wrap just in case.
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// A buffer for Win32 calls that return variable-size structures: allocated as `u64` so it
/// is 8-byte aligned, as those structures require.
struct AlignedBuf(Vec<u64>);

impl AlignedBuf {
    fn new(bytes: usize) -> AlignedBuf {
        AlignedBuf(vec![0; bytes.div_ceil(8).max(1)])
    }

    fn len(&self) -> usize {
        self.0.len() * 8
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.0.as_mut_ptr().cast()
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: the pointer and length describe exactly the Vec's initialised allocation;
        // u8 has alignment 1 and every bit pattern is a valid u8.
        unsafe { core::slice::from_raw_parts(self.0.as_ptr().cast::<u8>(), self.len()) }
    }
}

/// Reads a `T` at byte offset `off` of `buf`, or `None` if it does not fit.
///
/// Only used with the `#[repr(C)]` structs above, whose fields are integers and raw
/// pointers: every bit pattern is a valid value for them.
fn read_pod<T: Copy>(buf: &[u8], off: usize) -> Option<T> {
    let end = off.checked_add(size_of::<T>())?;
    if end > buf.len() {
        return None;
    }
    // SAFETY: `off..end` is inside `buf` (checked above); read_unaligned has no alignment
    // requirement; T is a plain-data struct for which any bytes are a valid value.
    Some(unsafe { ptr::read_unaligned(buf.as_ptr().add(off).cast::<T>()) })
}

/// The NUL-terminated UTF-16 string that `p` points to, provided `p` points inside `buf`
/// (Win32 returns string pointers into the caller's buffer). Anything else, including NULL,
/// gives an empty string. Reads through `buf`, never through `p`, and stops at the end of
/// `buf` when the terminator is missing.
fn wide_in_buf(buf: &[u8], p: *const u16) -> String {
    let base = buf.as_ptr() as usize;
    let addr = p as usize;
    if p.is_null() || addr < base || addr - base >= buf.len() {
        return String::new();
    }
    let units: Vec<u16> = buf[addr - base..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_ne_bytes(*b))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

// ---------------------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------------------

/// An owned registry key handle, closed on drop.
pub(crate) struct RegKey(Handle);

// SAFETY: registry key handles are not tied to the thread that opened them; the Reg*
// functions may be called with them from any thread.
unsafe impl Send for RegKey {}
// SAFETY: as above; this crate only reads through the handle, and the registry serialises
// concurrent calls on one key itself.
unsafe impl Sync for RegKey {}

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: self.0 is a valid key handle owned by this value and closed exactly once.
        unsafe { RegCloseKey(self.0) };
    }
}

fn reg_open(parent: Handle, subkey: &[u16], access: u32) -> Result<RegKey, Error> {
    let mut out: Handle = ptr::null_mut();
    // SAFETY: parent is a predefined key or an open handle; subkey is NUL-terminated (built
    // by wide::to_wide) and outlives the call; out is a valid place for the result.
    let rc = unsafe { RegOpenKeyExW(parent, subkey.as_ptr(), 0, access, &mut out) };
    status(rc, "RegOpenKeyExW")?;
    Ok(RegKey(out))
}

pub(crate) fn reg_open_hive(hive: u32, subkey: &[u16], access: u32) -> Result<RegKey, Error> {
    // Predefined keys are the 32-bit value sign-extended to pointer width, as in winreg.h.
    let root: Handle = ptr::without_provenance_mut(hive as i32 as isize as usize);
    reg_open(root, subkey, access)
}

pub(crate) fn reg_open_sub(parent: &RegKey, subkey: &[u16], access: u32) -> Result<RegKey, Error> {
    reg_open(parent.0, subkey, access)
}

pub(crate) fn reg_query_value(k: &RegKey, name: &[u16]) -> Result<Option<(u32, Vec<u8>)>, Error> {
    let mut buf = vec![0u8; 256];
    for _ in 0..MAX_TRIES {
        let mut kind = 0u32;
        let mut len = len_u32(buf.len());
        // SAFETY: k is open; name is NUL-terminated; buf has `len` writable bytes; kind and
        // len are valid places for the results.
        let rc = unsafe {
            RegQueryValueExW(
                k.0,
                name.as_ptr(),
                ptr::null_mut(),
                &mut kind,
                buf.as_mut_ptr(),
                &mut len,
            )
        };
        match rc {
            ERROR_SUCCESS => {
                buf.truncate(len as usize);
                return Ok(Some((kind, buf)));
            }
            ERROR_FILE_NOT_FOUND => return Ok(None),
            // `len` now holds the size needed; the value may still grow before the retry.
            ERROR_MORE_DATA => buf.resize((len as usize).max(buf.len() * 2), 0),
            code => {
                return Err(Error::Win32 {
                    op: "RegQueryValueExW",
                    code,
                });
            }
        }
    }
    Err(Error::Win32 {
        op: "RegQueryValueExW",
        code: ERROR_MORE_DATA,
    })
}

/// `(max subkey name, max value name)` in UTF-16 units without the NUL, and the largest
/// value data in bytes.
fn reg_info(k: &RegKey) -> Result<(usize, usize, usize), Error> {
    let (mut max_sub, mut max_name, mut max_data) = (0u32, 0u32, 0u32);
    // SAFETY: k is open; every non-NULL pointer is a valid u32 place; NULL marks the
    // outputs this call does not need.
    let rc = unsafe {
        RegQueryInfoKeyW(
            k.0,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut max_sub,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut max_name,
            &mut max_data,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    status(rc, "RegQueryInfoKeyW")?;
    Ok((max_sub as usize, max_name as usize, max_data as usize))
}

fn grow_name(name: &mut Vec<u16>) {
    let n = (name.len() * 2).clamp(256, MAX_NAME_UNITS);
    name.resize(n.max(name.len()), 0);
}

pub(crate) fn reg_subkeys(k: &RegKey) -> Result<Vec<String>, Error> {
    let (max_sub, _, _) = reg_info(k)?;
    let mut name = vec![0u16; max_sub.saturating_add(1).clamp(256, MAX_NAME_UNITS)];
    let mut out = Vec::new();
    let mut index = 0u32;
    let mut tries = 0;
    loop {
        let mut len = len_u32(name.len());
        // SAFETY: k is open; name has `len` writable units; NULL class and time outputs
        // are allowed.
        let rc = unsafe {
            RegEnumKeyExW(
                k.0,
                index,
                name.as_mut_ptr(),
                &mut len,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        match rc {
            ERROR_SUCCESS => {
                let n = (len as usize).min(name.len());
                out.push(String::from_utf16_lossy(&name[..n]));
                index += 1;
                tries = 0;
            }
            ERROR_NO_MORE_ITEMS => return Ok(out),
            ERROR_MORE_DATA if tries < MAX_TRIES && name.len() < MAX_NAME_UNITS => {
                grow_name(&mut name);
                tries += 1;
            }
            code => {
                return Err(Error::Win32 {
                    op: "RegEnumKeyExW",
                    code,
                });
            }
        }
    }
}

pub(crate) fn reg_values(k: &RegKey) -> Result<Vec<(String, u32, Vec<u8>)>, Error> {
    let (_, max_name, max_data) = reg_info(k)?;
    let mut name = vec![0u16; max_name.saturating_add(1).clamp(256, MAX_NAME_UNITS)];
    let mut data = vec![0u8; max_data.max(256)];
    let mut out = Vec::new();
    let mut index = 0u32;
    let mut tries = 0;
    loop {
        let mut name_len = len_u32(name.len());
        let mut data_len = len_u32(data.len());
        let mut kind = 0u32;
        // SAFETY: k is open; name has `name_len` writable units and data `data_len` bytes;
        // kind and the lengths are valid places for the results.
        let rc = unsafe {
            RegEnumValueW(
                k.0,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                ptr::null_mut(),
                &mut kind,
                data.as_mut_ptr(),
                &mut data_len,
            )
        };
        match rc {
            ERROR_SUCCESS => {
                let n = (name_len as usize).min(name.len());
                let d = (data_len as usize).min(data.len());
                out.push((
                    String::from_utf16_lossy(&name[..n]),
                    kind,
                    data[..d].to_vec(),
                ));
                index += 1;
                tries = 0;
            }
            ERROR_NO_MORE_ITEMS => return Ok(out),
            // Either buffer may be too small; `data_len` holds the data size needed when the
            // data was the problem, otherwise the name was.
            ERROR_MORE_DATA if tries < MAX_TRIES => {
                if data_len as usize > data.len() {
                    data.resize(data_len as usize, 0);
                } else if name.len() < MAX_NAME_UNITS {
                    grow_name(&mut name);
                } else {
                    data.resize(data.len() * 2, 0);
                }
                tries += 1;
            }
            code => {
                return Err(Error::Win32 {
                    op: "RegEnumValueW",
                    code,
                });
            }
        }
    }
}

pub(crate) fn expand_env(src: &[u16]) -> Result<String, Error> {
    let mut buf = vec![0u16; src.len().max(64) * 2];
    for _ in 0..MAX_TRIES {
        let cap = len_u32(buf.len());
        // SAFETY: src is NUL-terminated; buf has `cap` writable units.
        let need = unsafe { ExpandEnvironmentStringsW(src.as_ptr(), buf.as_mut_ptr(), cap) };
        if need == 0 {
            return Err(last_error("ExpandEnvironmentStringsW"));
        }
        let need = need as usize;
        if need <= buf.len() {
            // `need` counts the terminating NUL; until_nul also stops at it.
            return Ok(until_nul(&buf[..need]));
        }
        buf.resize(need, 0);
    }
    Err(Error::Win32 {
        op: "ExpandEnvironmentStringsW",
        code: ERROR_INSUFFICIENT_BUFFER,
    })
}

// ---------------------------------------------------------------------------------------
// Print spooler
// ---------------------------------------------------------------------------------------

/// `(name, port, driver, attributes)` for every local printer and connection.
pub(crate) fn enum_printers() -> Result<Vec<(String, String, String, u32)>, Error> {
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let mut buf = AlignedBuf::new(4096);
    for _ in 0..MAX_TRIES {
        let mut needed = 0u32;
        let mut returned = 0u32;
        let size = len_u32(buf.len());
        // SAFETY: buf has `size` writable bytes, 8-byte aligned for PRINTER_INFO_2W; NULL
        // name means the local machine; needed and returned are valid u32 places.
        let ok = unsafe {
            EnumPrintersW(
                flags,
                ptr::null(),
                2,
                buf.as_mut_ptr(),
                size,
                &mut needed,
                &mut returned,
            )
        };
        if ok == 0 {
            let code = last_code();
            if code == ERROR_INSUFFICIENT_BUFFER && needed as usize > buf.len() {
                buf = AlignedBuf::new(needed as usize);
                continue;
            }
            return Err(Error::Win32 {
                op: "EnumPrintersW",
                code,
            });
        }
        let bytes = buf.bytes();
        let mut out = Vec::with_capacity(returned as usize);
        for i in 0..returned as usize {
            let Some(off) = i.checked_mul(size_of::<PrinterInfo2W>()) else {
                break;
            };
            let Some(info) = read_pod::<PrinterInfo2W>(bytes, off) else {
                break;
            };
            out.push((
                wide_in_buf(bytes, info.printer_name),
                wide_in_buf(bytes, info.port_name),
                wide_in_buf(bytes, info.driver_name),
                info.attributes,
            ));
        }
        return Ok(out);
    }
    Err(Error::Win32 {
        op: "EnumPrintersW",
        code: ERROR_INSUFFICIENT_BUFFER,
    })
}

pub(crate) fn default_printer() -> Result<Option<String>, Error> {
    let mut buf = vec![0u16; 256];
    for _ in 0..MAX_TRIES {
        let mut size = len_u32(buf.len());
        // SAFETY: buf has `size` writable units; size is a valid place for the length.
        let ok = unsafe { GetDefaultPrinterW(buf.as_mut_ptr(), &mut size) };
        if ok != 0 {
            let n = (size as usize).min(buf.len());
            return Ok(Some(until_nul(&buf[..n])));
        }
        match last_code() {
            ERROR_FILE_NOT_FOUND => return Ok(None),
            ERROR_INSUFFICIENT_BUFFER if size as usize > buf.len() => {
                buf.resize(size as usize, 0);
            }
            code => {
                return Err(Error::Win32 {
                    op: "GetDefaultPrinterW",
                    code,
                });
            }
        }
    }
    Err(Error::Win32 {
        op: "GetDefaultPrinterW",
        code: ERROR_INSUFFICIENT_BUFFER,
    })
}

/// An open printer handle, closed on drop.
struct Printer(Handle);

impl Drop for Printer {
    fn drop(&mut self) {
        // SAFETY: self.0 is a valid printer handle owned by this value and closed once.
        unsafe { ClosePrinter(self.0) };
    }
}

impl Printer {
    fn check(&self, ok: i32, op: &'static str) -> Result<(), Error> {
        if ok == 0 { Err(last_error(op)) } else { Ok(()) }
    }

    /// StartPagePrinter, the WritePrinter loop and EndPagePrinter.
    fn write_page(&self, data: &[u8]) -> Result<(), Error> {
        // SAFETY: self.0 is an open printer with a document started.
        let ok = unsafe { StartPagePrinter(self.0) };
        self.check(ok, "StartPagePrinter")?;
        let mut rest = data;
        while !rest.is_empty() {
            let chunk = rest.len().min(WRITE_CHUNK);
            let mut written = 0u32;
            // SAFETY: rest has at least `chunk` readable bytes; written is a valid place.
            let ok =
                unsafe { WritePrinter(self.0, rest.as_ptr().cast(), len_u32(chunk), &mut written) };
            self.check(ok, "WritePrinter")?;
            let n = (written as usize).min(chunk);
            if n == 0 {
                // Success without progress would loop forever.
                return Err(Error::Win32 {
                    op: "WritePrinter",
                    code: ERROR_WRITE_FAULT,
                });
            }
            rest = &rest[n..];
        }
        // SAFETY: self.0 is an open printer with a page started.
        let ok = unsafe { EndPagePrinter(self.0) };
        self.check(ok, "EndPagePrinter")
    }
}

pub(crate) fn write_raw(printer: &[u16], document: &[u16], data: &[u8]) -> Result<u32, Error> {
    let mut h: Handle = ptr::null_mut();
    // SAFETY: printer is NUL-terminated; h is a valid place for the handle; NULL defaults
    // request the printer's default data type and access.
    let ok = unsafe { OpenPrinterW(printer.as_ptr(), &mut h, ptr::null()) };
    if ok == 0 {
        return Err(last_error("OpenPrinterW"));
    }
    let p = Printer(h);
    let doc = DocInfo1W {
        doc_name: document.as_ptr(),
        output_file: ptr::null(),
        datatype: RAW.as_ptr(),
    };
    // SAFETY: p.0 is open; doc and the strings it points to (NUL-terminated) outlive the call.
    let job = unsafe { StartDocPrinterW(p.0, 1, &doc) };
    if job == 0 {
        return Err(last_error("StartDocPrinterW"));
    }
    if let Err(e) = p.write_page(data) {
        // SAFETY: p.0 is open with a document started; cancelling it is best effort and the
        // first error is what the caller sees.
        unsafe { AbortPrinter(p.0) };
        return Err(e);
    }
    // SAFETY: p.0 is open with a document started and its page ended.
    let ok = unsafe { EndDocPrinter(p.0) };
    p.check(ok, "EndDocPrinter")?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_in_buf_is_bounded() {
        let mut buf = AlignedBuf::new(16);
        let units: [u16; 8] = [0x41, 0x42, 0, 0x43, 0x44, 0x45, 0x46, 0x47];
        for (i, u) in units.iter().enumerate() {
            buf.0[i / 4] |= (*u as u64) << ((i % 4) * 16);
        }
        let bytes = buf.bytes();
        let base = bytes.as_ptr().cast::<u16>();
        assert_eq!(wide_in_buf(bytes, base), "AB");
        // No terminator before the end of the buffer: stops at the end.
        assert_eq!(wide_in_buf(bytes, base.wrapping_add(3)), "CDEFG");
        assert_eq!(wide_in_buf(bytes, ptr::null()), "");
        assert_eq!(wide_in_buf(bytes, base.wrapping_add(8)), "");
        assert_eq!(wide_in_buf(bytes, base.wrapping_sub(1)), "");
        // An odd offset reads whole units only up to the end.
        let odd = bytes.as_ptr().wrapping_add(15).cast::<u16>();
        assert_eq!(wide_in_buf(bytes, odd), "");
    }

    #[test]
    fn read_pod_is_bounded() {
        let buf = [1u8, 0, 0, 0, 2, 0, 0];
        assert_eq!(read_pod::<u32>(&buf, 0), Some(1));
        assert_eq!(read_pod::<u32>(&buf, 3), Some(0x0000_0200));
        assert_eq!(read_pod::<u32>(&buf, 4), None);
        assert_eq!(read_pod::<u32>(&buf, usize::MAX), None);
    }

    #[test]
    fn printer_info_layout() {
        let p = size_of::<usize>();
        assert_eq!(size_of::<PrinterInfo2W>(), 13 * p + 8 * 4);
    }
}
