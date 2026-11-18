//! `Adapters/Lock/TpNative.cs`: the binding to the hotel's vendor build of LockSDK.dll
//! (32-bit, `__stdcall`, ANSI; signatures from LockSDK.h), compiled only for 32-bit Windows.
//!
//! ```c
//! int __stdcall TP_Configuration(int lock_type);
//! int __stdcall TP_MakeGuestCardEx(char *card_snr, char *room_no, char *checkin_time, char *checkout_time, int iflags);
//! int __stdcall TP_ReadGuestCardEx(char *card_snr, char *room_no, char *checkin_time, char *checkout_time, int *iFlags);
//! int __stdcall TP_CancelCard(char *card_snr);
//! int __stdcall TP_GetCardSnr(char *card_snr);
//! ```
//!
//! Like the C# `DllImport`, the DLL is loaded on the first call (and retried on each call
//! until it loads), each export is looked up when it is first needed, and failures are the
//! .NET exceptions (`DllNotFoundException`, `BadImageFormatException`,
//! `EntryPointNotFoundException`, `ArgumentException` for an unmappable character), which
//! the processor reports as DEVICE_ADAPTER_FAILED.

use std::sync::{Mutex, PoisonError};

use dl_winos::dll::{Library, Search, ansi};
use dl_winos::{Error, codes as win};

use crate::adapters::adapter_failed;
use crate::adapters::lock::{TpGuestCard, TpLockNative};
use crate::contracts::AdapterError;

/// `TpNative.Dll`.
const DLL: &str = "LockSDK.dll";
/// `TpNative.SnrLength`: StringBuilder capacity of the serial buffers.
const SNR_LENGTH: usize = 32;
/// `TpNative.TextLength`: StringBuilder capacity of the room and time buffers.
const TEXT_LENGTH: usize = 64;
/// Bytes per character reserved in a buffer: at least .NET's `SystemMaxDBCSCharSize` for
/// any ANSI code page (UTF-8 needs 4), so a buffer is never smaller than the C# one.
const MAX_CHAR_BYTES: usize = 4;

type Configuration = unsafe extern "system" fn(lock_type: i32) -> i32;
type MakeGuestCardEx = unsafe extern "system" fn(
    card_snr: *mut u8,
    room_no: *mut u8,
    checkin_time: *mut u8,
    checkout_time: *mut u8,
    iflags: i32,
) -> i32;
type ReadGuestCardEx = unsafe extern "system" fn(
    card_snr: *mut u8,
    room_no: *mut u8,
    checkin_time: *mut u8,
    checkout_time: *mut u8,
    iflags: *mut i32,
) -> i32;
type SnrCall = unsafe extern "system" fn(card_snr: *mut u8) -> i32;

/// The LockSDK.dll TP_* binding ([`TpLockNative`]). Calls are serialised: the DLL is a
/// single encoder driver.
pub struct LockSdk {
    lib: Mutex<Option<Library>>,
}

impl Default for LockSdk {
    fn default() -> Self {
        LockSdk::new()
    }
}

impl LockSdk {
    /// A binding that loads LockSDK.dll from the agent's directory (then System32, which
    /// is SysWOW64 for this 32-bit process) on the first call.
    pub fn new() -> LockSdk {
        LockSdk {
            lib: Mutex::new(None),
        }
    }

    /// Runs `f` with the loaded DLL, loading it first when needed.
    fn call<R>(
        &self,
        f: impl FnOnce(&Library) -> Result<R, AdapterError>,
    ) -> Result<R, AdapterError> {
        let mut slot = self.lib.lock().unwrap_or_else(PoisonError::into_inner);
        let lib = match slot.take() {
            Some(lib) => lib,
            None => Library::open(DLL, Search::ApplicationDir).map_err(load_error)?,
        };
        let result = f(&lib);
        *slot = Some(lib);
        result
    }
}

/// The export `name` as `F`. Like .NET's `DllImport` (`ExactSpelling = false`, ANSI,
/// x86 `StdCall`) it tries `name`, `_name@N`, `nameA` and `_nameA@N`, where `N` is the
/// size of the arguments in bytes.
///
/// # Safety
///
/// `F` must be the exact LockSDK.h signature of `name`, as `unsafe extern "system" fn`.
unsafe fn export<F: Copy>(lib: &Library, name: &str, arg_bytes: u32) -> Result<F, AdapterError> {
    let candidates = [
        name.to_owned(),
        format!("_{name}@{arg_bytes}"),
        format!("{name}A"),
        format!("_{name}A@{arg_bytes}"),
    ];
    for candidate in &candidates {
        // SAFETY: the caller guarantees F matches the export's signature and calling
        // convention; every candidate is a spelling of the same export.
        match unsafe { lib.symbol::<F>(candidate) } {
            Ok(f) => return Ok(f),
            Err(e) if e.code() == Some(win::PROC_NOT_FOUND) => {}
            Err(_) => break,
        }
    }
    Err(adapter_failed(
        "EntryPointNotFoundException",
        &format!("Unable to find an entry point named '{name}' in DLL '{DLL}'."),
    ))
}

/// The .NET exception for a DLL that does not load.
fn load_error(e: Error) -> AdapterError {
    match e.code() {
        Some(win::BAD_EXE_FORMAT) => adapter_failed(
            "BadImageFormatException",
            "An attempt was made to load a program with an incorrect format. (0x8007000B)",
        ),
        Some(code) => adapter_failed(
            "DllNotFoundException",
            &format!(
                "Unable to load DLL '{DLL}' or one of its dependencies: {} (0x{:08X})",
                os_message(code),
                0x8007_0000 | (code & 0xFFFF)
            ),
        ),
        None => adapter_failed(
            "DllNotFoundException",
            &format!("Unable to load DLL '{DLL}' or one of its dependencies: {e}"),
        ),
    }
}

/// The system message of a Win32 error code (`FormatMessage`), as .NET puts it in the
/// exception.
fn os_message(code: u32) -> String {
    let text = std::io::Error::from_raw_os_error(code as i32).to_string();
    let suffix = format!(" (os error {code})");
    text.strip_suffix(&suffix)
        .unwrap_or(&text)
        .trim()
        .to_owned()
}

/// The `ArgumentException` of an ANSI marshalling failure.
fn marshal_error(e: Error) -> AdapterError {
    match e {
        Error::Unmappable { .. } => adapter_failed(
            "ArgumentException",
            "Cannot marshal: Encountered unmappable character.",
        ),
        other => adapter_failed("ArgumentException", &other.to_string()),
    }
}

/// An empty output buffer for a StringBuilder of `capacity` characters.
fn out_buffer(capacity: usize) -> Vec<u8> {
    vec![0; capacity * MAX_CHAR_BYTES + 2]
}

/// `new StringBuilder(s, capacity)` marshalled as an ANSI in/out buffer. The native sees
/// the string up to an embedded NUL, like the C# marshaller's copy; a character the ANSI
/// code page cannot carry fails like `ThrowOnUnmappableChar`.
fn in_buffer(s: &str, capacity: usize) -> Result<Vec<u8>, AdapterError> {
    let s = s.split('\0').next().unwrap_or("");
    let mut bytes = ansi::encode(s).map_err(marshal_error)?;
    let chars = s.encode_utf16().count().max(capacity);
    let size = (chars * MAX_CHAR_BYTES + 2).max(bytes.len());
    bytes.resize(size, 0);
    Ok(bytes)
}

/// `StringBuilder.ToString()` of a buffer the native filled in.
fn out_string(buf: &[u8]) -> Result<String, AdapterError> {
    ansi::decode(buf).map_err(marshal_error)
}

impl TpLockNative for LockSdk {
    fn configuration(&self, lock_type: i32) -> Result<i32, AdapterError> {
        self.call(|lib| {
            // SAFETY: `int __stdcall TP_Configuration(int)`.
            let f: Configuration = unsafe { export(lib, "TP_Configuration", 4) }?;
            // SAFETY: an int by value; `lib` keeps the DLL loaded during the call.
            Ok(unsafe { f(lock_type) })
        })
    }

    fn make_guest_card(
        &self,
        room_no: &str,
        checkin_time: &str,
        checkout_time: &str,
        flags: i32,
    ) -> Result<(i32, String), AdapterError> {
        self.call(|lib| {
            // SAFETY: `int __stdcall TP_MakeGuestCardEx(char*, char*, char*, char*, int)`.
            let f: MakeGuestCardEx = unsafe { export(lib, "TP_MakeGuestCardEx", 20) }?;
            let mut snr = out_buffer(SNR_LENGTH);
            let mut room = in_buffer(room_no, TEXT_LENGTH)?;
            let mut cin = in_buffer(checkin_time, TEXT_LENGTH)?;
            let mut cout = in_buffer(checkout_time, TEXT_LENGTH)?;
            // SAFETY: four writable, NUL-terminated buffers at least as large as the C#
            // StringBuilder ones (LockSDK.h: card_snr >= 20 bytes), alive for the call;
            // `lib` keeps the DLL loaded.
            let rc = unsafe {
                f(
                    snr.as_mut_ptr(),
                    room.as_mut_ptr(),
                    cin.as_mut_ptr(),
                    cout.as_mut_ptr(),
                    flags,
                )
            };
            Ok((rc, out_string(&snr)?))
        })
    }

    fn read_guest_card(&self) -> Result<(i32, TpGuestCard), AdapterError> {
        self.call(|lib| {
            // SAFETY: `int __stdcall TP_ReadGuestCardEx(char*, char*, char*, char*, int*)`.
            let f: ReadGuestCardEx = unsafe { export(lib, "TP_ReadGuestCardEx", 20) }?;
            let mut snr = out_buffer(SNR_LENGTH);
            let mut room = out_buffer(TEXT_LENGTH);
            let mut cin = out_buffer(TEXT_LENGTH);
            let mut cout = out_buffer(TEXT_LENGTH);
            let mut flags = 0i32;
            // SAFETY: four zeroed writable buffers sized like the C# ones and a valid int
            // place, all alive for the call; `lib` keeps the DLL loaded.
            let rc = unsafe {
                f(
                    snr.as_mut_ptr(),
                    room.as_mut_ptr(),
                    cin.as_mut_ptr(),
                    cout.as_mut_ptr(),
                    &mut flags,
                )
            };
            let card = TpGuestCard {
                card_snr: out_string(&snr)?,
                room_no: out_string(&room)?,
                checkin_time: out_string(&cin)?,
                checkout_time: out_string(&cout)?,
            };
            Ok((rc, card))
        })
    }

    fn cancel_card(&self) -> Result<(i32, String), AdapterError> {
        self.snr_call("TP_CancelCard")
    }

    fn get_card_snr(&self) -> Result<(i32, String), AdapterError> {
        self.snr_call("TP_GetCardSnr")
    }
}

impl LockSdk {
    /// `TP_CancelCard` / `TP_GetCardSnr`: `int __stdcall f(char *card_snr)`.
    fn snr_call(&self, name: &str) -> Result<(i32, String), AdapterError> {
        self.call(|lib| {
            // SAFETY: both exports are `int __stdcall f(char*)`.
            let f: SnrCall = unsafe { export(lib, name, 4) }?;
            let mut snr = out_buffer(SNR_LENGTH);
            // SAFETY: a zeroed writable buffer sized like the C# one, alive for the call;
            // `lib` keeps the DLL loaded.
            let rc = unsafe { f(snr.as_mut_ptr()) };
            Ok((rc, out_string(&snr)?))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffers_are_at_least_the_csharp_size_and_nul_terminated() {
        assert_eq!(out_buffer(SNR_LENGTH).len(), 130);
        let b = in_buffer("0101", TEXT_LENGTH).unwrap();
        assert!(b.len() > TEXT_LENGTH && b.starts_with(b"0101\0"));
        let long = "9".repeat(100);
        let b = in_buffer(&long, TEXT_LENGTH).unwrap();
        assert!(b.len() > 100 && b[100] == 0);
        assert!(
            in_buffer("ab\0cd", TEXT_LENGTH)
                .unwrap()
                .starts_with(b"ab\0\0")
        );
        assert_eq!(out_string(b"A1B2\0junk").unwrap(), "A1B2");
    }

    #[test]
    fn a_missing_export_is_entry_point_not_found() {
        let lib = Library::open("kernel32.dll", Search::System32).unwrap();
        // SAFETY: never called; the lookup fails.
        let e = unsafe { export::<SnrCall>(&lib, "TP_NoSuchExport", 4) }.unwrap_err();
        assert_eq!(
            e.message,
            "EntryPointNotFoundException: Unable to find an entry point named 'TP_NoSuchExport' in DLL 'LockSDK.dll'."
        );
        // The A-suffixed spelling is found like .NET: kernel32 has only GetStartupInfoA/W.
        // SAFETY: never called; only the lookup is tested.
        assert!(unsafe { export::<SnrCall>(&lib, "GetStartupInfo", 4) }.is_ok());
    }

    #[test]
    fn unmappable_text_is_the_dotnet_argument_exception() {
        let e = marshal_error(Error::Unmappable {
            code_page: 1252,
            index: 2,
            ch: 'ễ',
        });
        assert_eq!(
            e.message,
            "ArgumentException: Cannot marshal: Encountered unmappable character."
        );
    }

    #[test]
    fn load_errors_read_like_dotnet() {
        let e = load_error(Error::Library {
            op: "LoadLibraryExW",
            code: win::BAD_EXE_FORMAT,
            detail: "`LockSDK.dll`".into(),
        });
        assert_eq!(
            e.message,
            "BadImageFormatException: An attempt was made to load a program with an incorrect format. (0x8007000B)"
        );
        let e = load_error(Error::Library {
            op: "LoadLibraryExW",
            code: win::MOD_NOT_FOUND,
            detail: "`LockSDK.dll`".into(),
        });
        assert!(
            e.message
                .starts_with("DllNotFoundException: Unable to load DLL 'LockSDK.dll' or one of its dependencies: ")
                && e.message.ends_with(" (0x8007007E)"),
            "{}",
            e.message
        );
    }
}
