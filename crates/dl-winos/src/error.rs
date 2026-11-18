use core::fmt;

use crate::dll::Machine;

/// Win32 error codes callers commonly check, as returned by [`Error::code`].
pub mod codes {
    /// `ERROR_FILE_NOT_FOUND`: the registry key does not exist (or there is no default
    /// printer).
    pub const FILE_NOT_FOUND: u32 = 2;
    /// `ERROR_ACCESS_DENIED`: the caller lacks the rights to open the key or printer.
    pub const ACCESS_DENIED: u32 = 5;
    /// `ERROR_MOD_NOT_FOUND`: the DLL, or a DLL it depends on, was not found.
    pub const MOD_NOT_FOUND: u32 = 126;
    /// `ERROR_PROC_NOT_FOUND`: the DLL does not export the symbol (or, when loading, a DLL
    /// it depends on lacks a function it imports).
    pub const PROC_NOT_FOUND: u32 = 127;
    /// `ERROR_BAD_EXE_FORMAT`: the DLL (or a DLL it depends on) is built for another CPU
    /// architecture, typically a 32-bit DLL in a 64-bit process or the other way round.
    pub const BAD_EXE_FORMAT: u32 = 193;
    /// `ERROR_INVALID_PRINTER_NAME`: no printer with that name.
    pub const INVALID_PRINTER_NAME: u32 = 1801;
}

/// Errors from registry, print spooler, COM and DLL calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The platform has no Windows registry, print spooler, console, COM or DLL loader.
    Unsupported,
    /// Loading a DLL or looking up one of its symbols failed (see [`crate::dll`]). The
    /// message explains the common codes [`codes::MOD_NOT_FOUND`],
    /// [`codes::PROC_NOT_FOUND`] and [`codes::BAD_EXE_FORMAT`].
    Library {
        /// The Win32 function that failed: `LoadLibraryExW` or `GetProcAddress`.
        op: &'static str,
        /// The `GetLastError` code.
        code: u32,
        /// The DLL as given (and for symbol lookups the symbol name), plus the DLL's CPU
        /// architecture when it could be read from the file.
        detail: String,
    },
    /// A file is not a valid PE image (EXE or DLL).
    BadImage {
        /// Byte offset in the file where the problem was found.
        offset: u64,
        /// What is wrong.
        reason: &'static str,
    },
    /// Reading a file failed.
    Io {
        /// The kind of I/O error.
        kind: std::io::ErrorKind,
        /// The file and the system's message.
        detail: String,
    },
    /// A string has a character that the ANSI code page cannot represent (see
    /// [`dll::ansi::encode`](crate::dll::ansi::encode)).
    Unmappable {
        /// The ANSI code page (`GetACP`), for example 1252 or 1258.
        code_page: u32,
        /// Byte offset of the character in the string.
        index: usize,
        /// The character.
        ch: char,
    },
    /// A Win32 call failed.
    Win32 {
        /// The Win32 function that failed.
        op: &'static str,
        /// The Win32 error code (`GetLastError` or the returned `LSTATUS`).
        code: u32,
    },
    /// A COM call returned a failure `HRESULT` (see [`com::hresult`](crate::com::hresult)).
    Com {
        /// The COM function that failed, for example `CoCreateInstance` or
        /// `IDispatch::Invoke`.
        op: &'static str,
        /// The `HRESULT` as an unsigned number (`0x8002_0006` and so on). When the object
        /// raised an exception (`DISP_E_EXCEPTION`) this is the exception's `scode` if it
        /// set one.
        hresult: u32,
        /// The ProgID or member involved and, for exceptions, the source and description
        /// the object reported. May be empty.
        detail: String,
    },
    /// A COM value has a different type than the conversion asked for.
    UnexpectedVariant {
        /// The Rust type that was asked for.
        expected: &'static str,
        /// The `VARTYPE` of the value (see [`com::Variant::vt`](crate::com::Variant::vt)).
        vt: u16,
    },
    /// An argument cannot be passed to Windows (for example a string with a NUL character).
    InvalidInput(&'static str),
    /// A registry value exists but has a type the convenience accessor does not return.
    UnexpectedType {
        /// The `REG_*` type of the value (see [`registry::kind`](crate::registry::kind)).
        kind: u32,
    },
}

impl Error {
    /// The Win32 error code, when the error came from a Win32 call.
    pub fn code(&self) -> Option<u32> {
        match self {
            Error::Win32 { code, .. } | Error::Library { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// The `HRESULT`, when the error came from a COM call.
    pub fn hresult(&self) -> Option<u32> {
        match self {
            Error::Com { hresult, .. } => Some(*hresult),
            _ => None,
        }
    }

    /// The Win32 or COM function that failed, when the error came from Windows.
    pub fn operation(&self) -> Option<&'static str> {
        match self {
            Error::Win32 { op, .. } | Error::Com { op, .. } | Error::Library { op, .. } => Some(op),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported => f.write_str(
                "the Windows registry, print spooler, console, COM and DLL loader are not \
                 available on this platform",
            ),
            Error::Library { op, code, detail } => {
                write!(f, "{op} failed for {detail}: ")?;
                match *code {
                    codes::BAD_EXE_FORMAT => write!(
                        f,
                        "the DLL (or a DLL it depends on) is built for a different CPU \
                         architecture than this {} process; 32-bit and 64-bit code cannot be \
                         mixed in one process (os error 193)",
                        Machine::current()
                    ),
                    codes::MOD_NOT_FOUND => {
                        f.write_str("the DLL or a DLL it depends on was not found (os error 126)")
                    }
                    codes::PROC_NOT_FOUND if *op == "GetProcAddress" => {
                        f.write_str("the DLL has no export with this name (os error 127)")
                    }
                    codes::PROC_NOT_FOUND => f.write_str(
                        "a DLL it depends on lacks a function it imports (os error 127)",
                    ),
                    c => write!(f, "{}", std::io::Error::from_raw_os_error(c as i32)),
                }
            }
            Error::BadImage { offset, reason } => {
                write!(f, "not a valid PE image: {reason} (at byte {offset})")
            }
            Error::Io { detail, .. } => write!(f, "I/O error: {detail}"),
            Error::Unmappable {
                code_page,
                index,
                ch,
            } => write!(
                f,
                "character {ch:?} (U+{:04X}) at byte {index} has no representation in ANSI \
                 code page {code_page}",
                u32::from(*ch)
            ),
            Error::Win32 { op, code } => {
                let os = std::io::Error::from_raw_os_error(*code as i32);
                write!(f, "{op} failed: {os}")
            }
            Error::Com {
                op,
                hresult,
                detail,
            } => {
                write!(f, "{op} failed with HRESULT 0x{hresult:08X}")?;
                if !detail.is_empty() {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            Error::UnexpectedVariant { expected, vt } => {
                write!(f, "COM value of VARTYPE {vt} is not {expected}")
            }
            Error::InvalidInput(what) => write!(f, "invalid input: {what}"),
            Error::UnexpectedType { kind } => {
                write!(f, "registry value has unexpected type {kind}")
            }
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_accessors() {
        let e = Error::Win32 {
            op: "OpenPrinterW",
            code: codes::INVALID_PRINTER_NAME,
        };
        assert_eq!(e.code(), Some(1801));
        assert_eq!(e.hresult(), None);
        assert_eq!(e.operation(), Some("OpenPrinterW"));
        assert!(e.to_string().starts_with("OpenPrinterW failed: "), "{e}");
        assert_eq!(Error::Unsupported.code(), None);
        assert_eq!(Error::Unsupported.operation(), None);
        assert_eq!(Error::InvalidInput("NUL").to_string(), "invalid input: NUL");
        assert_eq!(
            Error::UnexpectedType { kind: 4 }.to_string(),
            "registry value has unexpected type 4"
        );
    }

    #[test]
    fn com_display_and_accessors() {
        let e = Error::Com {
            op: "GetIDsOfNames",
            hresult: 0x8002_0006,
            detail: "unknown member `Foo`".into(),
        };
        assert_eq!(e.hresult(), Some(0x8002_0006));
        assert_eq!(e.code(), None);
        assert_eq!(e.operation(), Some("GetIDsOfNames"));
        assert_eq!(
            e.to_string(),
            "GetIDsOfNames failed with HRESULT 0x80020006: unknown member `Foo`"
        );
        let e = Error::Com {
            op: "CoInitializeEx",
            hresult: 0x8000_FFFF,
            detail: String::new(),
        };
        assert_eq!(
            e.to_string(),
            "CoInitializeEx failed with HRESULT 0x8000FFFF"
        );
        assert_eq!(
            Error::UnexpectedVariant {
                expected: "i32",
                vt: 8
            }
            .to_string(),
            "COM value of VARTYPE 8 is not i32"
        );
    }

    #[test]
    fn library_display_and_accessors() {
        let lib = |op, code| Error::Library {
            op,
            code,
            detail: "`LockSDK.dll`".into(),
        };
        let e = lib("LoadLibraryExW", codes::BAD_EXE_FORMAT);
        assert_eq!(e.code(), Some(193));
        assert_eq!(e.operation(), Some("LoadLibraryExW"));
        let text = e.to_string();
        assert!(
            text.starts_with("LoadLibraryExW failed for `LockSDK.dll`: the DLL"),
            "{text}"
        );
        assert!(text.contains("different CPU architecture"), "{text}");
        assert!(text.contains(&Machine::current().to_string()), "{text}");
        assert!(text.contains("32-bit and 64-bit"), "{text}");
        assert!(
            lib("LoadLibraryExW", codes::MOD_NOT_FOUND)
                .to_string()
                .contains("was not found (os error 126)")
        );
        assert!(
            lib("GetProcAddress", codes::PROC_NOT_FOUND)
                .to_string()
                .contains("no export with this name")
        );
        assert!(
            lib("LoadLibraryExW", codes::PROC_NOT_FOUND)
                .to_string()
                .contains("lacks a function it imports")
        );
        assert!(lib("LoadLibraryExW", 5).to_string().contains("os error 5"));

        let e = Error::BadImage {
            offset: 64,
            reason: "no PE signature",
        };
        assert_eq!(e.code(), None);
        assert_eq!(
            e.to_string(),
            "not a valid PE image: no PE signature (at byte 64)"
        );
        assert_eq!(
            Error::Unmappable {
                code_page: 1252,
                index: 3,
                ch: 'ễ'
            }
            .to_string(),
            "character 'ễ' (U+1EC5) at byte 3 has no representation in ANSI code page 1252"
        );
        let e = Error::Io {
            kind: std::io::ErrorKind::NotFound,
            detail: "x.dll: not found".into(),
        };
        assert_eq!(e.to_string(), "I/O error: x.dll: not found");
    }
}
