//! Dynamic libraries: load a DLL that may or may not be installed, look up its exports as
//! typed function pointers, and get a clear error instead of a crash when the DLL, a
//! symbol or the CPU architecture is wrong.
//!
//! ```no_run
//! use dl_winos::codes;
//! use dl_winos::dll::{Library, Search, ansi};
//!
//! type GetTickCount = unsafe extern "system" fn() -> u32;
//! type LstrlenA = unsafe extern "system" fn(s: *const u8) -> i32;
//!
//! let kernel32 = Library::open("kernel32.dll", Search::System32)?;
//! // SAFETY: GetTickCount is `DWORD WINAPI GetTickCount(void)`.
//! let tick: GetTickCount = unsafe { kernel32.symbol("GetTickCount")? };
//! // SAFETY: lstrlenA is `int WINAPI lstrlenA(LPCSTR)`.
//! let strlen: LstrlenA = unsafe { kernel32.symbol("lstrlenA")? };
//! let room = ansi::encode("Phong 101")?; // NUL-terminated, system ANSI code page
//! // SAFETY: both pointers are valid and `room` outlives the call.
//! let (ms, len) = unsafe { (tick(), strlen(room.as_ptr())) };
//! println!("up {ms} ms, {len} bytes");
//!
//! match Library::open("LockSDK.dll", Search::ApplicationDir) {
//!     Ok(_sdk) => println!("encoder SDK available"),
//!     Err(e) if e.code() == Some(codes::MOD_NOT_FOUND) => println!("encoder SDK not installed"),
//!     Err(e) => println!("{e}"), // e.g. a 32-bit DLL in this 64-bit process
//! }
//! # Ok::<(), dl_winos::Error>(())
//! ```

use core::ffi::c_void;
use core::fmt;
use core::mem::size_of;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::wide::to_wide;
use crate::{Error, codes, dll_sys};

const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
const LOAD_LIBRARY_SEARCH_APPLICATION_DIR: u32 = 0x0000_0200;
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

/// Where [`Library::open`] looks for a DLL given by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Search {
    /// The standard search order (`LoadLibraryExW` flags 0): the application directory,
    /// System32, the Windows directory, the current directory and `PATH`, after
    /// already-loaded modules and KnownDLLs. Also accepts absolute and relative paths.
    #[default]
    Default,
    /// The application's directory, then System32
    /// (`LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32`). For SDK DLLs
    /// shipped next to the executable; the current directory and `PATH` are not searched.
    ApplicationDir,
    /// For an absolute path: the DLL's own directory is searched for its dependencies, then
    /// System32 (`LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32`). For a
    /// vendor SDK installed in its own folder with helper DLLs next to it. A relative path
    /// is rejected with [`Error::InvalidInput`].
    DllDir,
    /// System32 only (`LOAD_LIBRARY_SEARCH_SYSTEM32`), for Windows' own DLLs.
    System32,
}

impl Search {
    /// The `LoadLibraryExW` flags.
    fn flags(self) -> u32 {
        match self {
            Search::Default => 0,
            Search::ApplicationDir => {
                LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32
            }
            Search::DllDir => LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            Search::System32 => LOAD_LIBRARY_SEARCH_SYSTEM32,
        }
    }
}

/// The CPU architecture of a PE image (the COFF `Machine` field) or of this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Machine {
    /// `IMAGE_FILE_MACHINE_I386` (0x014C): 32-bit x86.
    X86,
    /// `IMAGE_FILE_MACHINE_AMD64` (0x8664): 64-bit x64.
    X64,
    /// `IMAGE_FILE_MACHINE_ARM64` (0xAA64): 64-bit ARM.
    Arm64,
    /// Any other `IMAGE_FILE_MACHINE_*` value, for example 0x01C4 (32-bit ARM) or 0
    /// (unknown).
    Other(u16),
}

impl Machine {
    /// The architecture this process runs as, from the compile target: `X86` for
    /// `i686-pc-windows-msvc`, `X64` for `x86_64-*`, `Arm64` for `aarch64-*`,
    /// `Other(0x01C4)` for 32-bit ARM and `Other(0)` for anything else (WebAssembly).
    pub const fn current() -> Machine {
        if cfg!(target_arch = "x86") {
            Machine::X86
        } else if cfg!(target_arch = "x86_64") {
            Machine::X64
        } else if cfg!(target_arch = "aarch64") {
            Machine::Arm64
        } else if cfg!(target_arch = "arm") {
            Machine::Other(0x01C4)
        } else {
            Machine::Other(0)
        }
    }

    /// The machine for a COFF `Machine` value.
    pub const fn from_raw(raw: u16) -> Machine {
        match raw {
            0x014C => Machine::X86,
            0x8664 => Machine::X64,
            0xAA64 => Machine::Arm64,
            other => Machine::Other(other),
        }
    }

    /// The COFF `Machine` value.
    pub const fn to_raw(self) -> u16 {
        match self {
            Machine::X86 => 0x014C,
            Machine::X64 => 0x8664,
            Machine::Arm64 => 0xAA64,
            Machine::Other(raw) => raw,
        }
    }
}

impl fmt::Display for Machine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Machine::X86 => f.write_str("32-bit x86"),
            Machine::X64 => f.write_str("64-bit x64"),
            Machine::Arm64 => f.write_str("64-bit ARM64"),
            Machine::Other(0) => f.write_str("unknown-architecture"),
            Machine::Other(raw) => write!(f, "machine-0x{raw:04X}"),
        }
    }
}

/// A loaded DLL. Dropping it calls `FreeLibrary`, which may unload the DLL: function
/// pointers from [`Library::symbol`] must not be called after that. Use
/// [`Library::leak`] to keep the DLL loaded for the rest of the process.
///
/// `Library` is `Send` and `Sync`; whether the DLL's functions may be called from several
/// threads is up to the DLL.
pub struct Library {
    module: dll_sys::Module,
    name: Box<str>,
}

impl Library {
    /// Loads a DLL with `LoadLibraryExW`. `path_or_name` is a file name such as
    /// `LockSDK.dll` (`.dll` is appended when there is no extension) or a path; `search`
    /// picks the directories searched for it and its dependencies.
    ///
    /// Errors are [`Error::Library`] with the `GetLastError` code: [`codes::MOD_NOT_FOUND`]
    /// when the DLL or a dependency is missing, [`codes::BAD_EXE_FORMAT`] when it is built
    /// for another CPU architecture (for absolute paths the message names the DLL's
    /// architecture, read with [`Library::machine`]). Error dialogs are suppressed for the
    /// call (`SetThreadErrorMode`). A DLL already loaded into the process only gets its
    /// reference count raised; its `DllMain` runs only on the first load.
    pub fn open(path_or_name: &str, search: Search) -> Result<Library, Error> {
        if path_or_name.is_empty() {
            return Err(Error::InvalidInput("empty DLL name"));
        }
        if search == Search::DllDir && !is_windows_absolute(path_or_name) {
            return Err(Error::InvalidInput("Search::DllDir needs an absolute path"));
        }
        let wide = to_wide(path_or_name)?;
        match dll_sys::load(&wide, search.flags()) {
            Ok(module) => Ok(Library {
                module,
                name: path_or_name.into(),
            }),
            Err(Error::Win32 { op, code }) => Err(Error::Library {
                op,
                code,
                detail: load_detail(path_or_name, code),
            }),
            Err(e) => Err(e),
        }
    }

    /// The CPU architecture of the PE image (DLL or EXE) at `path`, read from its MZ and PE
    /// headers without loading it. Pure Rust, so it works on every platform; only the DOS
    /// header and the first 6 bytes of the PE header are read.
    ///
    /// Fails with [`Error::Io`] when the file cannot be read and [`Error::BadImage`] when
    /// it is not a PE image.
    pub fn machine(path: impl AsRef<Path>) -> Result<Machine, Error> {
        let path = path.as_ref();
        let mut file = std::fs::File::open(path).map_err(|e| io_error(path, &e))?;
        read_machine(&mut file).map_err(|e| match e {
            Error::Io { kind, detail } => Error::Io {
                kind,
                detail: format!("{}: {detail}", path.display()),
            },
            other => other,
        })
    }

    /// The export `name` (`GetProcAddress`) as a value of type `F`, normally an
    /// `unsafe extern "system" fn(...) -> ...` type (`__stdcall` on 32-bit Windows, the
    /// only convention on 64-bit) or `unsafe extern "C" fn` for `__cdecl` exports.
    ///
    /// Fails with [`Error::InvalidInput`] when `F` is not pointer-sized and with
    /// [`Error::Library`] / [`codes::PROC_NOT_FOUND`] when there is no such export.
    ///
    /// # Safety
    ///
    /// `F` must be a function pointer type (or, for a data export, a raw pointer type)
    /// whose signature and calling convention match the export exactly; a mismatch is
    /// undefined behaviour when the pointer is used. The returned value does not borrow
    /// this `Library`: it must not be called after the `Library` is dropped (unless it was
    /// [leaked](Library::leak) or another handle keeps the DLL loaded).
    #[allow(unsafe_code)]
    pub unsafe fn symbol<F: Copy>(&self, name: &str) -> Result<F, Error> {
        if size_of::<F>() != size_of::<usize>() {
            return Err(Error::InvalidInput(
                "symbol type must be a pointer-sized function pointer type",
            ));
        }
        if name.is_empty() {
            return Err(Error::InvalidInput("empty symbol name"));
        }
        if name.contains('\0') {
            return Err(Error::InvalidInput("symbol name contains a NUL character"));
        }
        let mut c_name = Vec::with_capacity(name.len() + 1);
        c_name.extend_from_slice(name.as_bytes());
        c_name.push(0);
        let addr: *mut c_void = match dll_sys::proc_address(&self.module, &c_name) {
            Ok(p) => p.as_ptr(),
            Err(Error::Win32 { op, code }) => {
                return Err(Error::Library {
                    op,
                    code,
                    detail: format!("`{name}` in `{}`", self.name),
                });
            }
            Err(e) => return Err(e),
        };
        // SAFETY: F has the size of a pointer (checked above), so transmute_copy reads
        // exactly the bytes of `addr`. That those bytes form a valid F with the right
        // signature is the caller's contract.
        Ok(unsafe { core::mem::transmute_copy::<*mut c_void, F>(&addr) })
    }

    /// The full path of the loaded DLL (`GetModuleFileNameW`), for example to log which
    /// copy of an SDK was picked up.
    pub fn path(&self) -> Result<PathBuf, Error> {
        dll_sys::module_path(&self.module)
    }

    /// Keeps the DLL loaded until the process exits: `FreeLibrary` is never called, so
    /// symbols stay valid forever. For SDKs that must not be unloaded, or whose function
    /// pointers are stored in statics.
    pub fn leak(self) {
        dll_sys::leak(self.module);
    }

    #[cfg_attr(not(all(test, windows)), allow(dead_code))]
    pub(crate) fn handle_addr(&self) -> usize {
        self.module.addr()
    }
}

impl fmt::Debug for Library {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Library")
            .field("name", &self.name)
            .field("handle", &format_args!("{:#x}", self.module.addr()))
            .finish()
    }
}

/// `X:\...`, `X:/...` or a UNC / device path (`\\server\share`, `\\?\...`).
fn is_windows_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    let sep = |c: u8| c == b'\\' || c == b'/';
    match b {
        [d, b':', s, ..] => d.is_ascii_alphabetic() && sep(*s),
        [a, c, ..] => sep(*a) && sep(*c),
        _ => false,
    }
}

/// The DLL named in a load error, with its architecture when it is a readable file.
fn load_detail(name: &str, code: u32) -> String {
    if code == codes::BAD_EXE_FORMAT
        && is_windows_absolute(name)
        && let Ok(m) = Library::machine(name)
    {
        return format!("`{name}` (a {m} DLL)");
    }
    format!("`{name}`")
}

fn io_error(path: &Path, e: &io::Error) -> Error {
    Error::Io {
        kind: e.kind(),
        detail: format!("{}: {e}", path.display()),
    }
}

/// Fills `buf` as far as the reader allows; returns the number of bytes read.
fn read_up_to<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

fn plain_io(e: io::Error) -> Error {
    Error::Io {
        kind: e.kind(),
        detail: e.to_string(),
    }
}

/// The COFF machine of the PE image `r`: the MZ signature, `e_lfanew` at 0x3C, the
/// `PE\0\0` signature there and the 16-bit machine after it.
fn read_machine<R: Read + Seek>(r: &mut R) -> Result<Machine, Error> {
    let mut dos = [0u8; 64];
    let n = read_up_to(r, &mut dos).map_err(plain_io)?;
    if n < 2 || dos[..2] != *b"MZ" {
        return Err(Error::BadImage {
            offset: 0,
            reason: "no MZ signature",
        });
    }
    if n < dos.len() {
        return Err(Error::BadImage {
            offset: n as u64,
            reason: "file ends inside the DOS header",
        });
    }
    let lfanew = u64::from(u32::from_le_bytes([dos[60], dos[61], dos[62], dos[63]]));
    r.seek(SeekFrom::Start(lfanew)).map_err(plain_io)?;
    let mut nt = [0u8; 6];
    let n = read_up_to(r, &mut nt).map_err(plain_io)?;
    if n < 4 {
        return Err(Error::BadImage {
            offset: lfanew + n as u64,
            reason: "file ends before the PE signature",
        });
    }
    if nt[..4] != *b"PE\0\0" {
        return Err(Error::BadImage {
            offset: lfanew,
            reason: "no PE signature",
        });
    }
    if n < nt.len() {
        return Err(Error::BadImage {
            offset: lfanew + n as u64,
            reason: "file ends inside the COFF header",
        });
    }
    Ok(Machine::from_raw(u16::from_le_bytes([nt[4], nt[5]])))
}

/// Strings in the system ANSI code page (`GetACP`, for example 1252 or 1258), the `char*`
/// that most vendor SDKs take and return.
pub mod ansi {
    use crate::{Error, dll_sys};

    /// The system ANSI code page (`GetACP`).
    pub fn code_page() -> Result<u32, Error> {
        dll_sys::acp()
    }

    /// `s` in the ANSI code page, NUL-terminated so `as_ptr()` can be passed as a
    /// `const char*` (the string's length is `len() - 1`).
    ///
    /// Uses `WideCharToMultiByte(CP_ACP, WC_NO_BEST_FIT_CHARS)`: a character without an
    /// exact mapping fails with [`Error::Unmappable`] (its byte offset and the character)
    /// instead of silently becoming `?` or a look-alike, like .NET's
    /// `ThrowOnUnmappableChar`. A NUL inside `s` is [`Error::InvalidInput`].
    pub fn encode(s: &str) -> Result<Vec<u8>, Error> {
        if s.contains('\0') {
            return Err(Error::InvalidInput("string contains a NUL character"));
        }
        let cp = code_page()?;
        let mut out = if s.is_empty() {
            Vec::new()
        } else {
            let wide: Vec<u16> = s.encode_utf16().collect();
            match dll_sys::to_acp(&wide, cp)? {
                Some(bytes) => bytes,
                None => char_by_char(s, cp)?,
            }
        };
        out.push(0);
        Ok(out)
    }

    /// Finds the first character that does not map. ANSI code pages are stateless, so if
    /// every character maps on its own the concatenation is the encoding.
    fn char_by_char(s: &str, cp: u32) -> Result<Vec<u8>, Error> {
        let mut out = Vec::with_capacity(s.len());
        let mut units = [0u16; 2];
        for (index, ch) in s.char_indices() {
            match dll_sys::to_acp(ch.encode_utf16(&mut units), cp)? {
                Some(bytes) => out.extend_from_slice(&bytes),
                None => {
                    return Err(Error::Unmappable {
                        code_page: cp,
                        index,
                        ch,
                    });
                }
            }
        }
        Ok(out)
    }

    /// The ANSI string in `bytes` up to the first NUL (or all of it), for example an
    /// output buffer an SDK filled in (`MultiByteToWideChar(CP_ACP)`). Byte sequences
    /// invalid in the code page become a replacement character.
    pub fn decode(bytes: &[u8]) -> Result<String, Error> {
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        let wide = dll_sys::from_acp(&bytes[..end])?;
        Ok(String::from_utf16_lossy(&wide))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A minimal PE header: DOS header with `e_lfanew` = `lfanew`, then `PE\0\0` and the
    /// machine.
    fn image(lfanew: u32, machine: u16) -> Vec<u8> {
        assert!(lfanew >= 64);
        let mut b = vec![0u8; lfanew as usize];
        b[..2].copy_from_slice(b"MZ");
        b[60..64].copy_from_slice(&lfanew.to_le_bytes());
        b.extend_from_slice(b"PE\0\0");
        b.extend_from_slice(&machine.to_le_bytes());
        b.extend_from_slice(&[0; 18]);
        b
    }

    fn parse(b: &[u8]) -> Result<Machine, Error> {
        read_machine(&mut Cursor::new(b))
    }

    #[test]
    fn machine_values() {
        for (raw, m) in [
            (0x014C, Machine::X86),
            (0x8664, Machine::X64),
            (0xAA64, Machine::Arm64),
            (0x01C4, Machine::Other(0x01C4)),
            (0, Machine::Other(0)),
        ] {
            assert_eq!(Machine::from_raw(raw), m);
            assert_eq!(m.to_raw(), raw);
        }
        assert_eq!(Machine::X86.to_string(), "32-bit x86");
        assert_eq!(Machine::X64.to_string(), "64-bit x64");
        assert_eq!(Machine::Arm64.to_string(), "64-bit ARM64");
        assert_eq!(Machine::Other(0x1C4).to_string(), "machine-0x01C4");
        assert_eq!(Machine::Other(0).to_string(), "unknown-architecture");
        let want = if cfg!(target_arch = "x86_64") {
            Machine::X64
        } else if cfg!(target_arch = "x86") {
            Machine::X86
        } else if cfg!(target_arch = "aarch64") {
            Machine::Arm64
        } else {
            Machine::current()
        };
        assert_eq!(Machine::current(), want);
    }

    #[test]
    fn parses_headers() {
        assert_eq!(parse(&image(64, 0x8664)), Ok(Machine::X64));
        assert_eq!(parse(&image(0xF8, 0x014C)), Ok(Machine::X86));
        assert_eq!(parse(&image(0x80, 0xAA64)), Ok(Machine::Arm64));
        assert_eq!(parse(&image(64, 0x5032)), Ok(Machine::Other(0x5032)));
        // e_lfanew inside the DOS header is legal (tiny PE files use it).
        let mut tiny = vec![0u8; 64];
        tiny[..2].copy_from_slice(b"MZ");
        tiny[4..8].copy_from_slice(b"PE\0\0");
        tiny[8..10].copy_from_slice(&0x8664u16.to_le_bytes());
        tiny[60..64].copy_from_slice(&4u32.to_le_bytes());
        assert_eq!(parse(&tiny), Ok(Machine::X64));
    }

    #[test]
    fn rejects_bad_headers_with_positions() {
        let bad = |b: &[u8]| match parse(b) {
            Err(Error::BadImage { offset, reason }) => (offset, reason),
            other => panic!("{other:?}"),
        };
        assert_eq!(bad(b""), (0, "no MZ signature"));
        assert_eq!(bad(b"M"), (0, "no MZ signature"));
        assert_eq!(bad(b"ZM\0\0"), (0, "no MZ signature"));
        assert_eq!(bad(b"MZ"), (2, "file ends inside the DOS header"));
        let mut b = image(64, 0x8664);
        b[64] = b'X';
        assert_eq!(bad(&b), (64, "no PE signature"));
        let mut b = image(64, 0x8664);
        b[60..64].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            bad(&b),
            (u64::from(u32::MAX), "file ends before the PE signature")
        );
        let full = image(0x80, 0x8664);
        for len in 0..0x80 + 6 {
            let r = parse(&full[..len]);
            assert!(matches!(r, Err(Error::BadImage { .. })), "len {len}: {r:?}");
        }
        assert_eq!(
            bad(&full[..0x82]),
            (0x82, "file ends before the PE signature")
        );
        assert_eq!(
            bad(&full[..0x85]),
            (0x85, "file ends inside the COFF header")
        );
        assert_eq!(parse(&full[..0x86]), Ok(Machine::X64));
    }

    #[test]
    fn garbage_never_panics() {
        // xorshift: deterministic pseudo-random bytes, some with a valid MZ prefix.
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for round in 0..2_000 {
            let len = (next() % 300) as usize;
            let mut b: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            if round % 2 == 0 && b.len() >= 2 {
                b[..2].copy_from_slice(b"MZ");
            }
            if round % 4 == 0 && b.len() >= 64 {
                let off = (next() % (b.len() as u64 + 8)) as u32;
                b[60..64].copy_from_slice(&off.to_le_bytes());
            }
            let _ = parse(&b);
        }
    }

    #[test]
    fn machine_of_files() {
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let write = |name: &str, bytes: &[u8]| {
            let p = dir.join(format!("dl-winos-pe-{pid}-{name}"));
            std::fs::write(&p, bytes).unwrap();
            p
        };
        let ok = write("ok.dll", &image(0xE8, 0x014C));
        assert_eq!(Library::machine(&ok), Ok(Machine::X86));
        // The DOS header is complete but e_lfanew points past the end.
        let short = write("short.dll", &image(0xE8, 0x014C)[..100]);
        assert!(matches!(
            Library::machine(&short),
            Err(Error::BadImage { offset: 0xE8, .. })
        ));
        let dos_only = write("dos.dll", &image(0xE8, 0x014C)[..40]);
        assert!(matches!(
            Library::machine(&dos_only),
            Err(Error::BadImage { offset: 40, .. })
        ));
        let text = write("text.dll", b"not a dll at all");
        assert!(matches!(
            Library::machine(&text),
            Err(Error::BadImage { offset: 0, .. })
        ));
        let empty = write("empty.dll", b"");
        assert!(matches!(
            Library::machine(&empty),
            Err(Error::BadImage { offset: 0, .. })
        ));
        for p in [ok, short, dos_only, text, empty] {
            let _ = std::fs::remove_file(p);
        }
        let missing = dir.join(format!("dl-winos-pe-{pid}-missing.dll"));
        match Library::machine(&missing) {
            Err(Error::Io { kind, detail }) => {
                assert_eq!(kind, io::ErrorKind::NotFound);
                assert!(detail.contains("missing.dll"), "{detail}");
            }
            other => panic!("{other:?}"),
        }
        // A directory is not a PE file either (an I/O error on Windows, where opening a
        // directory fails; BadImage or Io elsewhere).
        assert!(Library::machine(&dir).is_err());
    }

    #[test]
    fn search_flags() {
        assert_eq!(Search::default(), Search::Default);
        assert_eq!(Search::Default.flags(), 0);
        assert_eq!(Search::ApplicationDir.flags(), 0x0A00);
        assert_eq!(Search::DllDir.flags(), 0x0900);
        assert_eq!(Search::System32.flags(), 0x0800);
    }

    #[test]
    fn absolute_paths() {
        for p in [
            r"C:\x.dll",
            "c:/x.dll",
            r"\\server\share\x.dll",
            r"\\?\C:\x.dll",
        ] {
            assert!(is_windows_absolute(p), "{p}");
        }
        for p in ["x.dll", r"sub\x.dll", r"C:x.dll", r"\x.dll", "C:", ""] {
            assert!(!is_windows_absolute(p), "{p}");
        }
    }

    #[test]
    fn invalid_arguments() {
        assert_eq!(
            Library::open("", Search::Default).unwrap_err(),
            Error::InvalidInput("empty DLL name")
        );
        assert_eq!(
            Library::open("LockSDK.dll", Search::DllDir).unwrap_err(),
            Error::InvalidInput("Search::DllDir needs an absolute path")
        );
        assert!(matches!(
            Library::open("kernel32.dll\0", Search::Default),
            Err(Error::InvalidInput(_))
        ));
        assert_eq!(load_detail("x.dll", codes::BAD_EXE_FORMAT), "`x.dll`");
        assert_eq!(
            load_detail(r"C:\no such dir\x.dll", codes::BAD_EXE_FORMAT),
            r"`C:\no such dir\x.dll`"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_elsewhere() {
        assert_eq!(
            Library::open("kernel32.dll", Search::System32).unwrap_err(),
            Error::Unsupported
        );
        assert_eq!(ansi::code_page(), Err(Error::Unsupported));
        assert_eq!(ansi::encode("Phong 101"), Err(Error::Unsupported));
        assert_eq!(ansi::decode(b"Phong 101"), Err(Error::Unsupported));
    }
}

#[cfg(all(test, windows))]
#[allow(unsafe_code)]
mod windows_tests {
    use super::*;

    type GetTickCount = unsafe extern "system" fn() -> u32;
    type LstrlenA = unsafe extern "system" fn(s: *const u8) -> i32;

    fn windir() -> PathBuf {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()))
    }

    #[test]
    fn calls_kernel32_exports() {
        for search in [Search::Default, Search::System32, Search::ApplicationDir] {
            let k = Library::open("kernel32", search).unwrap();
            // SAFETY: GetTickCount is `DWORD WINAPI GetTickCount(void)`.
            let tick: GetTickCount = unsafe { k.symbol("GetTickCount") }.unwrap();
            // SAFETY: lstrlenA is `int WINAPI lstrlenA(LPCSTR)`.
            let strlen: LstrlenA = unsafe { k.symbol("lstrlenA") }.unwrap();
            // SAFETY: the export takes no arguments; k keeps kernel32 loaded.
            let t1 = unsafe { tick() };
            // SAFETY: as above.
            let t2 = unsafe { tick() };
            assert!(t2.wrapping_sub(t1) < 60_000);
            let room = ansi::encode("Phong 101").unwrap();
            // SAFETY: room is NUL-terminated and outlives the call.
            let n = unsafe { strlen(room.as_ptr()) };
            assert_eq!(n, 9);
            // SAFETY: an empty C string.
            let n = unsafe { strlen(c"".as_ptr().cast()) };
            assert_eq!(n, 0);
            let path = k.path().unwrap();
            let file = path.file_name().unwrap().to_string_lossy().to_lowercase();
            assert_eq!(file, "kernel32.dll", "{path:?}");
            assert!(path.is_absolute(), "{path:?}");
            assert!(format!("{k:?}").starts_with("Library { name: \"kernel32\""));
        }
    }

    #[test]
    fn dll_dir_with_an_absolute_path() {
        let p = windir().join("System32").join("version.dll");
        let lib = Library::open(p.to_str().unwrap(), Search::DllDir).unwrap();
        type GetFileVersionInfoSizeW = unsafe extern "system" fn(*const u16, *mut u32) -> u32;
        // SAFETY: `DWORD WINAPI GetFileVersionInfoSizeW(LPCWSTR, LPDWORD)`.
        let size: GetFileVersionInfoSizeW =
            unsafe { lib.symbol("GetFileVersionInfoSizeW") }.unwrap();
        let k32 = crate::wide::to_wide(&windir().join(r"System32\kernel32.dll").to_string_lossy())
            .unwrap();
        let mut handle = 0u32;
        // SAFETY: k32 is NUL-terminated; handle is a valid place.
        let n = unsafe { size(k32.as_ptr(), &mut handle) };
        assert!(n > 0);
    }

    #[test]
    fn missing_dll_and_symbol() {
        let e = Library::open("dl-winos-no-such-sdk.dll", Search::Default).unwrap_err();
        assert_eq!(e.code(), Some(codes::MOD_NOT_FOUND), "{e}");
        assert_eq!(e.operation(), Some("LoadLibraryExW"));
        let text = e.to_string();
        assert!(text.contains("`dl-winos-no-such-sdk.dll`"), "{text}");
        assert!(text.contains("not found"), "{text}");
        let p = windir().join(r"System32\dl-winos-no-such-sdk.dll");
        let e = Library::open(p.to_str().unwrap(), Search::DllDir).unwrap_err();
        assert_eq!(e.code(), Some(codes::MOD_NOT_FOUND), "{e}");

        let k = Library::open("kernel32.dll", Search::System32).unwrap();
        // SAFETY: never called; the lookup fails.
        let e = unsafe { k.symbol::<GetTickCount>("DlWinosNoSuchExport") }.unwrap_err();
        assert_eq!(e.code(), Some(codes::PROC_NOT_FOUND), "{e}");
        assert_eq!(e.operation(), Some("GetProcAddress"));
        let text = e.to_string();
        assert!(
            text.contains("`DlWinosNoSuchExport` in `kernel32.dll`"),
            "{text}"
        );
        assert!(text.contains("no export with this name"), "{text}");

        // SAFETY: the size check fails before any lookup.
        let e = unsafe { k.symbol::<[usize; 2]>("GetTickCount") }.unwrap_err();
        assert!(matches!(e, Error::InvalidInput(_)), "{e}");
        // SAFETY: as above.
        let e = unsafe { k.symbol::<u8>("GetTickCount") }.unwrap_err();
        assert!(matches!(e, Error::InvalidInput(_)), "{e}");
        // SAFETY: never called.
        let e = unsafe { k.symbol::<GetTickCount>("") }.unwrap_err();
        assert_eq!(e, Error::InvalidInput("empty symbol name"));
        // SAFETY: never called.
        let e = unsafe { k.symbol::<GetTickCount>("Get\0TickCount") }.unwrap_err();
        assert!(matches!(e, Error::InvalidInput(_)), "{e}");
        // A data-sized pointer works for any export.
        // SAFETY: only the address is used, never dereferenced.
        let p = unsafe { k.symbol::<*const u8>("GetTickCount") }.unwrap();
        assert!(!p.is_null());
    }

    #[test]
    fn machine_of_system_dlls() {
        // In a 32-bit process on 64-bit Windows, System32 is redirected to SysWOW64, so the
        // System32 kernel32.dll is the process's own architecture in both builds (on an x64
        // host).
        let system32 = Library::machine(windir().join(r"System32\kernel32.dll")).unwrap();
        if cfg!(any(target_arch = "x86_64", target_arch = "x86")) {
            assert_eq!(system32, Machine::current());
        }
        let wow = windir().join(r"SysWOW64\kernel32.dll");
        if wow.exists() {
            assert_eq!(Library::machine(&wow), Ok(Machine::X86));
        }
        // Every truncation of a real header fails cleanly.
        let real = std::fs::read(windir().join(r"System32\kernel32.dll")).unwrap();
        let lfanew = u32::from_le_bytes(real[60..64].try_into().unwrap()) as usize;
        for len in 0..lfanew + 6 {
            let r = read_machine(&mut std::io::Cursor::new(&real[..len]));
            assert!(matches!(r, Err(Error::BadImage { .. })), "len {len}: {r:?}");
        }
        assert_eq!(
            read_machine(&mut std::io::Cursor::new(&real[..lfanew + 6])),
            Ok(system32)
        );
    }

    #[test]
    fn other_architecture_is_a_clear_error() {
        // winsqlite3.dll exists in both System32 and SysWOW64. A 64-bit process loads the
        // 32-bit copy from SysWOW64; a 32-bit process reaches the 64-bit copy through
        // Sysnative (System32 itself would be redirected to SysWOW64).
        let (dir, other) = if cfg!(target_pointer_width = "64") {
            ("SysWOW64", Machine::X86)
        } else {
            ("Sysnative", Machine::X64)
        };
        let p = windir().join(dir).join("winsqlite3.dll");
        if !p.exists() {
            eprintln!("note: skipping, {} does not exist", p.display());
            return;
        }
        let name = p.to_str().unwrap();
        for search in [Search::Default, Search::DllDir] {
            let e = Library::open(name, search).unwrap_err();
            assert_eq!(e.code(), Some(codes::BAD_EXE_FORMAT), "{e}");
            let text = e.to_string();
            eprintln!("{text}");
            assert!(text.contains(&format!("(a {other} DLL)")), "{text}");
            assert!(text.contains("different CPU architecture"), "{text}");
            assert!(
                text.contains(&format!("this {} process", Machine::current())),
                "{text}"
            );
        }
    }

    #[test]
    fn ansi_round_trips() {
        let cp = ansi::code_page().unwrap();
        assert!(cp > 0);
        assert_eq!(ansi::encode("Phong 101").unwrap(), b"Phong 101\0");
        assert_eq!(ansi::decode(b"Phong 101\0garbage").unwrap(), "Phong 101");
        assert_eq!(ansi::decode(b"Phong 101").unwrap(), "Phong 101");
        assert_eq!(ansi::encode("").unwrap(), b"\0");
        assert_eq!(ansi::decode(b"").unwrap(), "");
        assert_eq!(ansi::decode(b"\0abc").unwrap(), "");
        assert!(matches!(ansi::encode("a\0b"), Err(Error::InvalidInput(_))));

        // Whether these map depends on the code page (1258 has Vietnamese, 1252 only some
        // of it, UTF-8 everything); the behaviour must be consistent either way.
        for text in [
            "Phòng 101",
            "Phòng 101 – Nguyễn Văn Ánh",
            "Khách sạn Đà Nẵng",
            "€ 50.000",
            "日本語",
            "🍜",
        ] {
            let r = ansi::encode(text);
            eprintln!("cp {cp}: {text:?} -> {r:?}");
            match r {
                Ok(bytes) => {
                    assert_eq!(bytes.last(), Some(&0), "{text}");
                    assert_eq!(ansi::decode(&bytes).unwrap(), text, "cp {cp}");
                }
                Err(Error::Unmappable {
                    code_page,
                    index,
                    ch,
                }) => {
                    assert_eq!(code_page, cp);
                    assert_eq!(text[index..].chars().next(), Some(ch), "{text}");
                    // Everything before the character maps and round-trips.
                    let prefix = ansi::encode(&text[..index]).unwrap();
                    assert_eq!(ansi::decode(&prefix).unwrap(), &text[..index]);
                    // The character on its own does not.
                    let one = ch.to_string();
                    assert!(
                        matches!(ansi::encode(&one), Err(Error::Unmappable { index: 0, .. })),
                        "{one}"
                    );
                }
                Err(e) => panic!("{text}: {e}"),
            }
        }
        if cp == 65_001 || cp == 54_936 {
            assert!(ansi::encode("🍜").is_ok());
        } else {
            // A supplementary-plane character is in no single- or double-byte code page.
            assert!(matches!(
                ansi::encode("ab🍜"),
                Err(Error::Unmappable {
                    index: 2,
                    ch: '🍜',
                    ..
                })
            ));
        }
    }
}
