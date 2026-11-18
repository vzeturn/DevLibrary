//! Run-time loading of the operating system's PC/SC library and thin wrappers over its C
//! API. The only module with `unsafe`.
//!
//! Windows: `winscard.dll` from System32 (`LoadLibraryExW` + `GetProcAddress`), `__stdcall`
//! on x86 (`extern "system"`), wide-character (`W`) functions, 32-bit DWORD/LONG and
//! pointer-sized handles. Linux and other Unix systems: `dlopen` of `libpcsclite.so.1`,
//! where DWORD/LONG are C `unsigned long`/`long` (64-bit on LP64) and handles are `long`.
//! macOS: `/System/Library/Frameworks/PCSC.framework/PCSC`, 32-bit DWORD/LONG/handles and
//! packed structs. Reader names are UTF-16 on Windows and UTF-8 elsewhere.
#![allow(unsafe_code)]

use core::ffi::{CStr, c_void};
use core::ptr::{null, null_mut};
use std::sync::OnceLock;

use crate::error::codes;
use crate::{Error, ReaderStatus, ReaderWait, State};

type Ptr = *mut c_void;

#[cfg(windows)]
mod abi {
    /// `DWORD` (`unsigned long`, 32-bit on Windows).
    pub(crate) type Dword = u32;
    /// `LONG` (`long`, 32-bit on Windows).
    pub(crate) type Long = i32;
    /// `SCARDCONTEXT` / `SCARDHANDLE` (`ULONG_PTR`).
    pub(crate) type Handle = usize;
    /// Character of reader names (`WCHAR`, the `W` functions are used).
    pub(crate) type NameChar = u16;
    /// Size of `rgbAtr` in `SCARD_READERSTATEW`.
    pub(crate) const ATR_SIZE: usize = 36;
}

#[cfg(target_vendor = "apple")]
mod abi {
    /// `DWORD` (`uint32_t` in PCSC.framework).
    pub(crate) type Dword = u32;
    /// `LONG` (`int32_t` in PCSC.framework).
    pub(crate) type Long = i32;
    /// `SCARDCONTEXT` / `SCARDHANDLE` (`int32_t`).
    pub(crate) type Handle = i32;
    /// Character of reader names (`char`, UTF-8).
    pub(crate) type NameChar = u8;
    /// `MAX_ATR_SIZE`.
    pub(crate) const ATR_SIZE: usize = 33;
}

#[cfg(all(unix, not(target_vendor = "apple")))]
mod abi {
    /// `DWORD` (`unsigned long` in pcsc-lite).
    pub(crate) type Dword = core::ffi::c_ulong;
    /// `LONG` (`long` in pcsc-lite).
    pub(crate) type Long = core::ffi::c_long;
    /// `SCARDCONTEXT` / `SCARDHANDLE` (`LONG`).
    pub(crate) type Handle = core::ffi::c_long;
    /// Character of reader names (`char`, UTF-8).
    pub(crate) type NameChar = u8;
    /// `MAX_ATR_SIZE`.
    pub(crate) const ATR_SIZE: usize = 33;
}

pub(crate) use abi::Handle;
use abi::{ATR_SIZE, Dword, Long, NameChar};

/// `SCARD_READERSTATE` (`SCARD_READERSTATEW` on Windows).
#[cfg_attr(target_vendor = "apple", repr(C, packed))]
#[cfg_attr(not(target_vendor = "apple"), repr(C))]
struct ReaderState {
    reader: *const NameChar,
    user_data: Ptr,
    current_state: Dword,
    event_state: Dword,
    atr_len: Dword,
    atr: [u8; ATR_SIZE],
}

/// `SCARD_IO_REQUEST`: the protocol control information, built here instead of importing
/// the `g_rgSCardT0Pci`-style data symbols.
#[cfg_attr(target_vendor = "apple", repr(C, packed))]
#[cfg_attr(not(target_vendor = "apple"), repr(C))]
struct IoRequest {
    protocol: Dword,
    pci_len: Dword,
}

/// `INFINITE` timeout of `SCardGetStatusChange`.
pub(crate) const INFINITE: u32 = 0xFFFF_FFFF;

#[cfg(any(windows, target_vendor = "apple"))]
fn dw(v: u32) -> Dword {
    v
}
#[cfg(all(unix, not(target_vendor = "apple")))]
fn dw(v: u32) -> Dword {
    Dword::from(v)
}

/// A DWORD flag/state value from the library. PC/SC defines them all within 32 bits, so
/// the upper half of pcsc-lite's 64-bit DWORD is always zero.
#[cfg(any(windows, target_vendor = "apple"))]
fn from_dw(v: Dword) -> u32 {
    v
}
#[cfg(all(unix, not(target_vendor = "apple")))]
// `unsigned long` is 32-bit on 32-bit Linux, where the cast is a no-op.
#[allow(clippy::unnecessary_cast)]
fn from_dw(v: Dword) -> u32 {
    v as u32
}

/// The result code as the unsigned 32-bit value of the `SCARD_*` constants. On pcsc-lite
/// LP64 the codes are positive 64-bit values below 2^32.
fn code(rc: Long) -> u32 {
    rc as u32
}

fn len_dw(n: usize) -> Result<Dword, Error> {
    Dword::try_from(n).map_err(|_| Error::InvalidArgument(format!("length {n} is too large")))
}

fn dw_len(v: Dword) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

fn check(op: &'static str, rc: Long) -> Result<(), Error> {
    if rc == 0 {
        Ok(())
    } else {
        Err(Error::Pcsc { op, code: code(rc) })
    }
}

#[cfg(windows)]
macro_rules! wide {
    ($n:literal) => {
        concat!($n, "W\0")
    };
}
#[cfg(not(windows))]
macro_rules! wide {
    ($n:literal) => {
        concat!($n, "\0")
    };
}

macro_rules! api {
    ($($field:ident = $sym:expr => fn($($arg:ty),*) -> $ret:ty;)*) => {
        #[cfg(windows)]
        struct Api { $($field: unsafe extern "system" fn($($arg),*) -> $ret,)* }
        #[cfg(not(windows))]
        struct Api { $($field: unsafe extern "C" fn($($arg),*) -> $ret,)* }

        impl Api {
            fn load(handle: Ptr, library: &str) -> Result<Api, String> {
                Ok(Api { $($field: {
                    #[cfg(windows)]
                    type F = unsafe extern "system" fn($($arg),*) -> $ret;
                    #[cfg(not(windows))]
                    type F = unsafe extern "C" fn($($arg),*) -> $ret;
                    let sym: &str = $sym;
                    let Ok(name) = CStr::from_bytes_with_nul(sym.as_bytes()) else {
                        return Err(format!("bad symbol name {sym:?}"));
                    };
                    let p = os::symbol(handle, name);
                    if p.is_null() {
                        return Err(format!(
                            "symbol {} not found in {library}",
                            name.to_string_lossy()
                        ));
                    }
                    // SAFETY: `p` is the address of the PC/SC function of this name, whose
                    // C signature with this platform's DWORD/LONG/handle types is the
                    // declared one; the calling convention is WINAPI (__stdcall on x86
                    // Windows) for winscard.dll and the C convention elsewhere.
                    unsafe { core::mem::transmute::<Ptr, F>(p) }
                },)* })
            }
        }
    };
}

api! {
    establish = "SCardEstablishContext\0" =>
        fn(Dword, *const c_void, *const c_void, *mut Handle) -> Long;
    release = "SCardReleaseContext\0" => fn(Handle) -> Long;
    cancel = "SCardCancel\0" => fn(Handle) -> Long;
    list_readers = wide!("SCardListReaders") =>
        fn(Handle, *const NameChar, *mut NameChar, *mut Dword) -> Long;
    get_status_change = wide!("SCardGetStatusChange") =>
        fn(Handle, Dword, *mut ReaderState, Dword) -> Long;
    connect = wide!("SCardConnect") =>
        fn(Handle, *const NameChar, Dword, Dword, *mut Handle, *mut Dword) -> Long;
    disconnect = "SCardDisconnect\0" => fn(Handle, Dword) -> Long;
    begin_transaction = "SCardBeginTransaction\0" => fn(Handle) -> Long;
    end_transaction = "SCardEndTransaction\0" => fn(Handle, Dword) -> Long;
    transmit = "SCardTransmit\0" => fn(
        Handle, *const IoRequest, *const u8, Dword, *mut IoRequest, *mut u8, *mut Dword
    ) -> Long;
    status = wide!("SCardStatus") => fn(
        Handle, *mut NameChar, *mut Dword, *mut Dword, *mut Dword, *mut u8, *mut Dword
    ) -> Long;
}

fn load() -> Result<Api, String> {
    let (handle, name) = os::open()?;
    Api::load(handle, &name)
}

/// The function table, loaded on first use. The error explains why it is unavailable.
fn lib() -> Result<&'static Api, Error> {
    static LIB: OnceLock<Result<Api, String>> = OnceLock::new();
    match LIB.get_or_init(load) {
        Ok(api) => Ok(api),
        Err(e) => Err(Error::Unsupported(e.clone())),
    }
}

/// Loads the library (once); errors with `Unsupported` when it is missing.
pub(crate) fn load_library() -> Result<(), Error> {
    lib().map(|_| ())
}

/// A reader name as a NUL-terminated string in the platform's encoding.
fn to_name(name: &str) -> Result<Vec<NameChar>, Error> {
    if name.contains('\0') {
        return Err(Error::InvalidArgument(format!(
            "reader name {name:?} contains NUL"
        )));
    }
    #[cfg(windows)]
    let v: Vec<NameChar> = name.encode_utf16().chain(Some(0)).collect();
    #[cfg(not(windows))]
    let v: Vec<NameChar> = name.bytes().chain(Some(0)).collect();
    Ok(v)
}

fn split_names(buf: &[NameChar]) -> Vec<String> {
    #[cfg(windows)]
    let v = crate::convert::split_multi_u16(buf);
    #[cfg(not(windows))]
    let v = crate::convert::split_multi_u8(buf);
    v
}

pub(crate) fn establish(scope: u32) -> Result<Handle, Error> {
    let api = lib()?;
    let mut ctx: Handle = 0;
    // SAFETY: both reserved pointers must be NULL; `ctx` is a valid out pointer.
    let rc = unsafe { (api.establish)(dw(scope), null(), null(), &mut ctx) };
    check("SCardEstablishContext", rc)?;
    Ok(ctx)
}

pub(crate) fn release(ctx: Handle) {
    if let Ok(api) = lib() {
        // SAFETY: `ctx` was returned by SCardEstablishContext and is released only once
        // (by the owning `Context`); the result is ignored as nothing can be done about it.
        unsafe { (api.release)(ctx) };
    }
}

pub(crate) fn cancel(ctx: Handle) -> Result<(), Error> {
    let api = lib()?;
    // SAFETY: `ctx` is a live context (the caller holds the lock that keeps it from being
    // released); SCardCancel may be called from any thread.
    let rc = unsafe { (api.cancel)(ctx) };
    check("SCardCancel", rc)
}

/// Upper bound for a reader list or name buffer, to reject absurd lengths.
const MAX_NAMES_LEN: usize = 1 << 20;

pub(crate) fn list_readers(ctx: Handle) -> Result<Vec<String>, Error> {
    const OP: &str = "SCardListReaders";
    let api = lib()?;
    // A reader may appear between the length query and the read: retry a few times.
    for _ in 0..4 {
        let mut len: Dword = 0;
        // SAFETY: NULL groups selects all readers; a NULL buffer asks for the needed length
        // (in characters, terminators included) in `len`.
        let rc = unsafe { (api.list_readers)(ctx, null(), null_mut(), &mut len) };
        if code(rc) == codes::SCARD_E_NO_READERS_AVAILABLE {
            return Ok(Vec::new());
        }
        check(OP, rc)?;
        let n = dw_len(len);
        if n == 0 {
            return Ok(Vec::new());
        }
        if n > MAX_NAMES_LEN {
            return Err(Error::Pcsc {
                op: OP,
                code: codes::SCARD_E_NO_MEMORY,
            });
        }
        let mut buf: Vec<NameChar> = vec![0; n];
        let mut got = len;
        // SAFETY: `buf` is writable for `got` characters.
        let rc = unsafe { (api.list_readers)(ctx, null(), buf.as_mut_ptr(), &mut got) };
        match code(rc) {
            codes::SCARD_S_SUCCESS => {
                let used = dw_len(got).min(buf.len());
                return Ok(split_names(&buf[..used]));
            }
            codes::SCARD_E_NO_READERS_AVAILABLE => return Ok(Vec::new()),
            codes::SCARD_E_INSUFFICIENT_BUFFER => {}
            c => return Err(Error::Pcsc { op: OP, code: c }),
        }
    }
    Err(Error::Pcsc {
        op: OP,
        code: codes::SCARD_E_INSUFFICIENT_BUFFER,
    })
}

pub(crate) fn get_status_change(
    ctx: Handle,
    timeout_ms: u32,
    readers: &[ReaderWait],
) -> Result<Vec<ReaderStatus>, Error> {
    let api = lib()?;
    let names = readers
        .iter()
        .map(|r| to_name(&r.name))
        .collect::<Result<Vec<_>, _>>()?;
    let mut states: Vec<ReaderState> = names
        .iter()
        .zip(readers)
        .map(|(name, r)| ReaderState {
            reader: name.as_ptr(),
            user_data: null_mut(),
            current_state: dw(r.current.bits() & !State::CHANGED.bits()),
            event_state: 0,
            atr_len: 0,
            atr: [0; ATR_SIZE],
        })
        .collect();
    let count = len_dw(states.len())?;
    // SAFETY: `states` holds `count` initialized entries; their name pointers point into
    // `names`, which lives until the end of this function; the library writes only the
    // event state, ATR length and ATR of each entry.
    let rc = unsafe { (api.get_status_change)(ctx, dw(timeout_ms), states.as_mut_ptr(), count) };
    check("SCardGetStatusChange", rc)?;
    Ok(states
        .iter()
        .zip(readers)
        .map(|(s, r)| {
            // Copies: the struct is packed on macOS, so no references into it.
            let atr = s.atr;
            let len = dw_len(s.atr_len).min(ATR_SIZE);
            ReaderStatus {
                name: r.name.clone(),
                state: State::from_bits(from_dw(s.event_state)),
                atr: atr[..len].to_vec(),
            }
        })
        .collect())
}

pub(crate) fn connect(
    ctx: Handle,
    reader: &str,
    share: u32,
    protocols: u32,
) -> Result<(Handle, u32), Error> {
    let api = lib()?;
    let name = to_name(reader)?;
    let mut card: Handle = 0;
    let mut active: Dword = 0;
    // SAFETY: `name` is NUL-terminated; `card` and `active` are valid out pointers.
    let rc = unsafe {
        (api.connect)(
            ctx,
            name.as_ptr(),
            dw(share),
            dw(protocols),
            &mut card,
            &mut active,
        )
    };
    check("SCardConnect", rc)?;
    Ok((card, from_dw(active)))
}

pub(crate) fn disconnect(card: Handle, disposition: u32) -> Result<(), Error> {
    let api = lib()?;
    // SAFETY: `card` is a live card handle, disconnected only once by its owner.
    let rc = unsafe { (api.disconnect)(card, dw(disposition)) };
    check("SCardDisconnect", rc)
}

pub(crate) fn begin_transaction(card: Handle) -> Result<(), Error> {
    let api = lib()?;
    // SAFETY: `card` is a live card handle.
    let rc = unsafe { (api.begin_transaction)(card) };
    check("SCardBeginTransaction", rc)
}

pub(crate) fn end_transaction(card: Handle, disposition: u32) -> Result<(), Error> {
    let api = lib()?;
    // SAFETY: `card` is a live card handle.
    let rc = unsafe { (api.end_transaction)(card, dw(disposition)) };
    check("SCardEndTransaction", rc)
}

/// `SCardTransmit` with a receive buffer of `capacity` bytes; returns the response.
pub(crate) fn transmit(
    card: Handle,
    protocol: u32,
    apdu: &[u8],
    capacity: usize,
) -> Result<Vec<u8>, Error> {
    let api = lib()?;
    let send_len = len_dw(apdu.len())?;
    let pci = IoRequest {
        protocol: dw(protocol),
        pci_len: len_dw(size_of::<IoRequest>())?,
    };
    let mut buf = vec![0u8; capacity];
    let mut recv_len = len_dw(capacity)?;
    // SAFETY: `pci` is a valid SCARD_IO_REQUEST; `apdu` is readable for `send_len` bytes;
    // `buf` is writable for `recv_len` bytes; a NULL receive PCI is allowed.
    let rc = unsafe {
        (api.transmit)(
            card,
            &pci,
            apdu.as_ptr(),
            send_len,
            null_mut(),
            buf.as_mut_ptr(),
            &mut recv_len,
        )
    };
    check("SCardTransmit", rc)?;
    buf.truncate(dw_len(recv_len).min(capacity));
    Ok(buf)
}

/// `SCardStatus`: reader name, raw state, raw protocol and ATR.
pub(crate) fn status(card: Handle) -> Result<(String, u32, u32, Vec<u8>), Error> {
    let api = lib()?;
    let mut name: Vec<NameChar> = vec![0; 1024];
    let mut name_len = len_dw(name.len())?;
    let mut state: Dword = 0;
    let mut protocol: Dword = 0;
    // Larger than both 33 (pcsc-lite) and 36 (Windows).
    let mut atr = [0u8; 64];
    let mut atr_len = len_dw(atr.len())?;
    // SAFETY: `name` is writable for `name_len` characters and `atr` for `atr_len` bytes;
    // the other arguments are valid out pointers.
    let rc = unsafe {
        (api.status)(
            card,
            name.as_mut_ptr(),
            &mut name_len,
            &mut state,
            &mut protocol,
            atr.as_mut_ptr(),
            &mut atr_len,
        )
    };
    check("SCardStatus", rc)?;
    let n = dw_len(name_len).min(name.len());
    let reader = split_names(&name[..n])
        .into_iter()
        .next()
        .unwrap_or_default();
    let atr_len = dw_len(atr_len).min(atr.len());
    Ok((
        reader,
        from_dw(state),
        from_dw(protocol),
        atr[..atr_len].to_vec(),
    ))
}

#[cfg(windows)]
mod os {
    use super::Ptr;
    use core::ffi::{CStr, c_char};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryExW(name: *const u16, file: Ptr, flags: u32) -> Ptr;
        fn GetProcAddress(module: Ptr, name: *const c_char) -> Ptr;
    }

    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

    pub(super) fn open() -> Result<(Ptr, String), String> {
        let wide: Vec<u16> = "winscard.dll\0".encode_utf16().collect();
        // SAFETY: `wide` is NUL-terminated UTF-16, the file handle must be NULL; the module
        // stays loaded for the life of the process.
        let h = unsafe {
            LoadLibraryExW(
                wide.as_ptr(),
                core::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if h.is_null() {
            let e = std::io::Error::last_os_error();
            Err(format!("cannot load winscard.dll from System32: {e}"))
        } else {
            Ok((h, "winscard.dll".to_owned()))
        }
    }

    pub(super) fn symbol(handle: Ptr, name: &CStr) -> Ptr {
        // SAFETY: `handle` is a module loaded by `open` and never freed; `name` is
        // NUL-terminated.
        unsafe { GetProcAddress(handle, name.as_ptr()) }
    }
}

#[cfg(unix)]
mod os {
    use super::Ptr;
    use core::ffi::{CStr, c_char, c_int};

    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flag: c_int) -> Ptr;
        fn dlsym(handle: Ptr, symbol: *const c_char) -> Ptr;
        fn dlerror() -> *mut c_char;
    }

    const RTLD_NOW: c_int = 2;

    #[cfg(target_vendor = "apple")]
    const CANDIDATES: &[&CStr] = &[c"/System/Library/Frameworks/PCSC.framework/PCSC"];
    #[cfg(not(target_vendor = "apple"))]
    const CANDIDATES: &[&CStr] = &[c"libpcsclite.so.1", c"libpcsclite.so"];

    fn load(name: &CStr) -> Result<Ptr, String> {
        // SAFETY: `name` is NUL-terminated; the handle is kept for the life of the process.
        let h = unsafe { dlopen(name.as_ptr(), RTLD_NOW) };
        if !h.is_null() {
            return Ok(h);
        }
        // SAFETY: dlerror returns NULL or a NUL-terminated message valid until the next
        // dl* call on this thread.
        let e = unsafe { dlerror() };
        if e.is_null() {
            return Err(format!("cannot load {}", name.to_string_lossy()));
        }
        // SAFETY: non-null and NUL-terminated as above; copied at once.
        let msg = unsafe { CStr::from_ptr(e) };
        Err(msg.to_string_lossy().into_owned())
    }

    pub(super) fn open() -> Result<(Ptr, String), String> {
        let mut errors = Vec::new();
        for name in CANDIDATES {
            match load(name) {
                Ok(h) => return Ok((h, name.to_string_lossy().into_owned())),
                Err(e) => errors.push(e),
            }
        }
        Err(errors.join("; "))
    }

    pub(super) fn symbol(handle: Ptr, name: &CStr) -> Ptr {
        // SAFETY: `handle` is a live dlopen handle (never closed); `name` is NUL-terminated.
        unsafe { dlsym(handle, name.as_ptr()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::offset_of;

    /// Layouts from winscard.h (`SCARD_READERSTATEW`, `SCARD_IO_REQUEST`) and pcsc-lite /
    /// PCSC.framework `pcsclite.h`, computed by hand from the C declarations.
    #[test]
    fn struct_layouts() {
        let rs = size_of::<ReaderState>();
        let atr = offset_of!(ReaderState, atr);
        let current = offset_of!(ReaderState, current_state);
        let io = size_of::<IoRequest>();
        println!("SCARD_READERSTATE size {rs}, rgbAtr at {atr}; SCARD_IO_REQUEST size {io}");
        #[cfg(all(windows, target_pointer_width = "64"))]
        {
            // 2 pointers (16) + 3 DWORD (12) + 36 bytes = 64, 8-byte aligned.
            assert_eq!((rs, current, atr, io), (64, 16, 28, 8));
            assert_eq!(align_of::<ReaderState>(), 8);
        }
        #[cfg(all(windows, target_pointer_width = "32"))]
        {
            // 2 pointers (8) + 3 DWORD (12) + 36 bytes = 56.
            assert_eq!((rs, current, atr, io), (56, 8, 20, 8));
            assert_eq!(align_of::<ReaderState>(), 4);
        }
        #[cfg(all(unix, not(target_vendor = "apple"), target_pointer_width = "64"))]
        {
            // 2 pointers (16) + 3 unsigned long (24) + 33 bytes = 73, padded to 80.
            assert_eq!((rs, current, atr, io), (80, 16, 40, 16));
            assert_eq!(align_of::<ReaderState>(), 8);
        }
        #[cfg(all(unix, not(target_vendor = "apple"), target_pointer_width = "32"))]
        {
            // 2 pointers (8) + 3 unsigned long (12) + 33 bytes = 53, padded to 56.
            assert_eq!((rs, current, atr, io), (56, 8, 20, 8));
        }
        #[cfg(target_vendor = "apple")]
        {
            // #pragma pack(1): 2 pointers (16) + 3 uint32_t (12) + 33 bytes = 61.
            assert_eq!((rs, current, atr, io), (61, 16, 28, 8));
            assert_eq!(align_of::<ReaderState>(), 1);
        }
    }

    #[test]
    fn integer_conversions() {
        assert_eq!(code(0), 0);
        // `(LONG)0x8010001D`: negative for a 32-bit LONG, positive for pcsc-lite's 64-bit one.
        let rc = 0x8010_001D_u32 as Long;
        assert_eq!(code(rc), codes::SCARD_E_NO_SERVICE);
        assert_eq!(from_dw(dw(INFINITE)), INFINITE);
        assert_eq!(dw_len(dw(36)), 36);
    }

    #[test]
    fn names() {
        assert!(to_name("a\0b").is_err());
        let n = to_name("ACS").unwrap();
        assert_eq!(n.len(), 4);
        assert_eq!(n[3], 0);
        let mut multi = to_name("R1").unwrap();
        multi.extend(to_name("R2").unwrap());
        multi.push(0);
        assert_eq!(split_names(&multi), ["R1", "R2"]);
    }

    #[cfg(windows)]
    #[test]
    fn winscard_loads() {
        // winscard.dll ships with every Windows version, even with the service stopped.
        load_library().unwrap();
    }
}
