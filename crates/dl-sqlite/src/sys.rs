//! Run-time loading of the operating system's SQLite library and thin wrappers over its C
//! API. The only module with `unsafe`.
//!
//! Windows: `winsqlite3.dll` from System32 (`LoadLibraryExW` + `GetProcAddress`), whose
//! functions are `__stdcall` on x86 (`extern "system"`). Unix: `dlopen` of
//! `libsqlite3.so.0` / `libsqlite3.so` (macOS: `/usr/lib/libsqlite3.dylib`). Elsewhere
//! nothing is loaded and every call returns `Error::Unsupported`.
#![allow(unsafe_code)]

use core::ffi::{CStr, c_char, c_int, c_void};
use core::ptr::{null, null_mut};
use std::sync::OnceLock;

use crate::{Error, Type, ValueRef, codes};

type Ptr = *mut c_void;

/// Environment variable with the full path of an SQLite library to load instead.
#[cfg(any(windows, unix))]
const ENV: &str = "DL_SQLITE_LIBRARY";

macro_rules! api {
    ($($name:ident : fn($($arg:ty),*) -> $ret:ty;)*) => {
        #[cfg(windows)]
        struct Api { $($name: unsafe extern "system" fn($($arg),*) -> $ret,)* }
        #[cfg(not(windows))]
        struct Api { $($name: unsafe extern "C" fn($($arg),*) -> $ret,)* }

        impl Api {
            fn load(handle: Ptr, library: &str) -> Result<Api, String> {
                Ok(Api { $($name: {
                    const NAME: &CStr =
                        match CStr::from_bytes_with_nul(concat!(stringify!($name), "\0").as_bytes()) {
                            Ok(c) => c,
                            Err(_) => panic!("symbol name with NUL"),
                        };
                    #[cfg(windows)]
                    type F = unsafe extern "system" fn($($arg),*) -> $ret;
                    #[cfg(not(windows))]
                    type F = unsafe extern "C" fn($($arg),*) -> $ret;
                    let p = os::symbol(handle, NAME);
                    if p.is_null() {
                        return Err(format!("symbol {} not found in {library}", stringify!($name)));
                    }
                    // SAFETY: `p` is the address of the SQLite C API function of this name,
                    // whose C signature is the declared one; the calling convention is the
                    // platform's SQLITE_APICALL (__stdcall for winsqlite3.dll on x86, the C
                    // convention everywhere else).
                    unsafe { core::mem::transmute::<Ptr, F>(p) }
                },)* })
            }
        }
    };
}

api! {
    sqlite3_libversion: fn() -> *const c_char;
    sqlite3_libversion_number: fn() -> c_int;
    sqlite3_threadsafe: fn() -> c_int;
    sqlite3_open_v2: fn(*const c_char, *mut Ptr, c_int, *const c_char) -> c_int;
    sqlite3_close_v2: fn(Ptr) -> c_int;
    sqlite3_extended_result_codes: fn(Ptr, c_int) -> c_int;
    sqlite3_errmsg: fn(Ptr) -> *const c_char;
    sqlite3_errstr: fn(c_int) -> *const c_char;
    sqlite3_extended_errcode: fn(Ptr) -> c_int;
    sqlite3_busy_timeout: fn(Ptr, c_int) -> c_int;
    sqlite3_changes: fn(Ptr) -> c_int;
    sqlite3_last_insert_rowid: fn(Ptr) -> i64;
    sqlite3_get_autocommit: fn(Ptr) -> c_int;
    sqlite3_prepare_v2: fn(Ptr, *const c_char, c_int, *mut Ptr, *mut *const c_char) -> c_int;
    sqlite3_finalize: fn(Ptr) -> c_int;
    sqlite3_reset: fn(Ptr) -> c_int;
    sqlite3_clear_bindings: fn(Ptr) -> c_int;
    sqlite3_step: fn(Ptr) -> c_int;
    sqlite3_bind_parameter_count: fn(Ptr) -> c_int;
    sqlite3_bind_null: fn(Ptr, c_int) -> c_int;
    sqlite3_bind_int64: fn(Ptr, c_int, i64) -> c_int;
    sqlite3_bind_double: fn(Ptr, c_int, f64) -> c_int;
    sqlite3_bind_text: fn(Ptr, c_int, *const c_char, c_int, Ptr) -> c_int;
    sqlite3_bind_blob: fn(Ptr, c_int, *const c_void, c_int, Ptr) -> c_int;
    sqlite3_column_count: fn(Ptr) -> c_int;
    sqlite3_column_name: fn(Ptr, c_int) -> *const c_char;
    sqlite3_column_type: fn(Ptr, c_int) -> c_int;
    sqlite3_column_int64: fn(Ptr, c_int) -> i64;
    sqlite3_column_double: fn(Ptr, c_int) -> f64;
    sqlite3_column_text: fn(Ptr, c_int) -> *const u8;
    sqlite3_column_blob: fn(Ptr, c_int) -> *const c_void;
    sqlite3_column_bytes: fn(Ptr, c_int) -> c_int;
}

/// SQLITE_TRANSIENT: SQLite copies the bound bytes before the bind call returns.
fn transient() -> Ptr {
    core::ptr::without_provenance_mut(usize::MAX)
}

/// A valid pointer for zero-length binds (SQLite binds NULL for a null data pointer).
static EMPTY: [u8; 1] = [0];

const SQLITE_INTEGER: c_int = 1;
const SQLITE_FLOAT: c_int = 2;
const SQLITE_TEXT: c_int = 3;
const SQLITE_BLOB: c_int = 4;
/// Oldest version with every function used here (`sqlite3_errstr` is 3.7.15).
const MIN_VERSION: c_int = 3_007_015;

/// The loaded library: function table and version.
pub(crate) struct Lib {
    api: Api,
    pub(crate) version: String,
    pub(crate) version_number: i32,
}

/// Copies a NUL-terminated C string (lossy UTF-8); empty for NULL.
///
/// # Safety
/// `p` is NULL or points to a NUL-terminated string that stays valid during the call.
unsafe fn copy_cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: non-null and NUL-terminated per the caller's contract.
    let s = unsafe { CStr::from_ptr(p) };
    s.to_string_lossy().into_owned()
}

fn load() -> Result<Lib, String> {
    let (handle, name) = os::open()?;
    let api = Api::load(handle, &name)?;
    // SAFETY: no arguments; returns a compile-time constant of the library.
    let threadsafe = unsafe { (api.sqlite3_threadsafe)() };
    if threadsafe == 0 {
        return Err(format!("{name} was built with SQLITE_THREADSAFE=0"));
    }
    // SAFETY: returns a pointer to a static NUL-terminated string.
    let v = unsafe { (api.sqlite3_libversion)() };
    // SAFETY: `v` is NULL or a static NUL-terminated string.
    let version = unsafe { copy_cstr(v) };
    // SAFETY: no arguments; returns a constant.
    let version_number = unsafe { (api.sqlite3_libversion_number)() };
    if version_number < MIN_VERSION {
        return Err(format!(
            "{name} is SQLite {version}; 3.7.15 or later is needed"
        ));
    }
    Ok(Lib {
        api,
        version,
        version_number,
    })
}

/// The library, loaded on first use. The error explains why it is unavailable.
pub(crate) fn lib() -> Result<&'static Lib, Error> {
    static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
    match LIB.get_or_init(load) {
        Ok(l) => Ok(l),
        Err(e) => Err(Error::Unsupported(e.clone())),
    }
}

fn errstr(lib: &Lib, rc: c_int) -> String {
    // SAFETY: sqlite3_errstr returns a static NUL-terminated string for any code.
    let p = unsafe { (lib.api.sqlite3_errstr)(rc) };
    // SAFETY: `p` is NULL or a static NUL-terminated string.
    unsafe { copy_cstr(p) }
}

/// An error for a result code that came without a connection message.
fn code_error(lib: &Lib, rc: c_int) -> Error {
    Error::Sqlite {
        code: rc & 0xff,
        extended_code: rc,
        message: errstr(lib, rc),
    }
}

/// An open `sqlite3*` connection, closed with `sqlite3_close_v2` on drop.
pub(crate) struct RawDb {
    lib: &'static Lib,
    db: Ptr,
}

// SAFETY: the connection is opened with SQLITE_OPEN_NOMUTEX in a library whose threading
// mode is multi-thread or serialized (`sqlite3_threadsafe() != 0` is checked at load), so it
// may be used from any thread provided no two threads use it at the same time. `RawDb` is
// not `Sync` and statements borrow the owning `Connection` (making them `!Send`), so the
// connection and its statements are only ever used from one thread at a time.
unsafe impl Send for RawDb {}

impl Drop for RawDb {
    fn drop(&mut self) {
        // SAFETY: `db` is an open connection owned by this value; all statements borrow
        // the connection and are already finalized (close_v2 would defer otherwise).
        unsafe { (self.lib.api.sqlite3_close_v2)(self.db) };
    }
}

impl RawDb {
    /// `sqlite3_open_v2` with the given flags; enables extended result codes.
    pub(crate) fn open(path: &CStr, flags: c_int) -> Result<RawDb, Error> {
        let lib = lib()?;
        let mut db: Ptr = null_mut();
        // SAFETY: `path` is NUL-terminated, `db` is a valid out pointer, a NULL VFS name
        // selects the default VFS.
        let rc = unsafe { (lib.api.sqlite3_open_v2)(path.as_ptr(), &mut db, flags, null()) };
        if db.is_null() {
            return Err(code_error(lib, rc));
        }
        // From here `raw` owns the handle and closes it on every path.
        let raw = RawDb { lib, db };
        if rc != codes::OK {
            return Err(raw.error(rc));
        }
        // SAFETY: `db` is an open connection.
        unsafe { (lib.api.sqlite3_extended_result_codes)(db, 1) };
        Ok(raw)
    }

    /// The error for result code `rc` of the last call on this connection.
    pub(crate) fn error(&self, rc: c_int) -> Error {
        // SAFETY: `db` is an open connection; this only reads its error state.
        let ext = unsafe { (self.lib.api.sqlite3_extended_errcode)(self.db) };
        if ext & 0xff != rc & 0xff {
            // The connection's error state belongs to another call (e.g. our own checks).
            return code_error(self.lib, rc);
        }
        // SAFETY: `db` is open; the message is owned by the connection and copied before
        // any other call on it.
        let p = unsafe { (self.lib.api.sqlite3_errmsg)(self.db) };
        // SAFETY: `p` is NULL or a NUL-terminated string valid until the next call.
        let mut message = unsafe { copy_cstr(p) };
        if message.is_empty() {
            message = errstr(self.lib, rc);
        }
        Error::Sqlite {
            code: rc & 0xff,
            extended_code: if rc & 0xff != rc { rc } else { ext },
            message,
        }
    }

    pub(crate) fn busy_timeout(&self, ms: c_int) -> c_int {
        // SAFETY: `db` is an open connection.
        unsafe { (self.lib.api.sqlite3_busy_timeout)(self.db, ms) }
    }

    pub(crate) fn changes(&self) -> c_int {
        // SAFETY: `db` is an open connection.
        unsafe { (self.lib.api.sqlite3_changes)(self.db) }
    }

    pub(crate) fn last_insert_rowid(&self) -> i64 {
        // SAFETY: `db` is an open connection.
        unsafe { (self.lib.api.sqlite3_last_insert_rowid)(self.db) }
    }

    pub(crate) fn is_autocommit(&self) -> bool {
        // SAFETY: `db` is an open connection.
        let r = unsafe { (self.lib.api.sqlite3_get_autocommit)(self.db) };
        r != 0
    }

    /// Compiles the first statement of `sql`. Returns it (None when the text holds only
    /// whitespace, `;` or comments) and the number of bytes consumed.
    pub(crate) fn prepare(&self, sql: &[u8]) -> Result<(Option<RawStmt>, usize), Error> {
        if sql.is_empty() {
            return Ok((None, 0));
        }
        let Ok(len) = c_int::try_from(sql.len()) else {
            return Err(code_error(self.lib, codes::TOOBIG));
        };
        let mut stmt: Ptr = null_mut();
        let mut tail: *const c_char = null();
        // SAFETY: `sql` is readable for `len` bytes (an explicit length, so no NUL is
        // needed); `stmt` and `tail` are valid out pointers.
        let rc = unsafe {
            (self.lib.api.sqlite3_prepare_v2)(
                self.db,
                sql.as_ptr().cast(),
                len,
                &mut stmt,
                &mut tail,
            )
        };
        if rc != codes::OK {
            // On error SQLite sets `stmt` to NULL.
            return Err(self.error(rc));
        }
        let consumed = if tail.is_null() {
            sql.len()
        } else {
            tail.addr().wrapping_sub(sql.as_ptr().addr()).min(sql.len())
        };
        if stmt.is_null() {
            return Ok((None, consumed));
        }
        let stmt = RawStmt {
            lib: self.lib,
            stmt,
            has_row: false,
        };
        Ok((Some(stmt), consumed))
    }
}

/// A prepared `sqlite3_stmt*`, finalized on drop. It must not outlive its connection:
/// `Statement` holds a borrow of the `Connection` for that.
pub(crate) struct RawStmt {
    lib: &'static Lib,
    stmt: Ptr,
    /// The last `step` returned SQLITE_ROW and no reset happened since: column reads are
    /// defined only in this state.
    has_row: bool,
}

impl Drop for RawStmt {
    fn drop(&mut self) {
        // SAFETY: `stmt` is a prepared statement owned by this value.
        unsafe { (self.lib.api.sqlite3_finalize)(self.stmt) };
    }
}

impl RawStmt {
    pub(crate) fn step(&mut self) -> c_int {
        // SAFETY: `stmt` is a live prepared statement.
        let rc = unsafe { (self.lib.api.sqlite3_step)(self.stmt) };
        self.has_row = rc == codes::ROW;
        rc
    }

    /// `sqlite3_reset`. Its result repeats the last `step` error, which was already
    /// reported, so it is not returned.
    pub(crate) fn reset(&mut self) {
        self.has_row = false;
        // SAFETY: `stmt` is a live prepared statement.
        unsafe { (self.lib.api.sqlite3_reset)(self.stmt) };
    }

    pub(crate) fn clear_bindings(&mut self) {
        // SAFETY: `stmt` is a live prepared statement.
        unsafe { (self.lib.api.sqlite3_clear_bindings)(self.stmt) };
    }

    pub(crate) fn parameter_count(&self) -> usize {
        // SAFETY: `stmt` is a live prepared statement.
        let n = unsafe { (self.lib.api.sqlite3_bind_parameter_count)(self.stmt) };
        usize::try_from(n).unwrap_or(0)
    }

    /// Binds one value at a 1-based index; returns the SQLite result code (SQLite checks
    /// the index and the statement state itself).
    pub(crate) fn bind(&mut self, index: c_int, value: ValueRef<'_>) -> c_int {
        let a = &self.lib.api;
        let s = self.stmt;
        match value {
            // SAFETY: `s` is a live prepared statement.
            ValueRef::Null => unsafe { (a.sqlite3_bind_null)(s, index) },
            // SAFETY: as above.
            ValueRef::Integer(v) => unsafe { (a.sqlite3_bind_int64)(s, index, v) },
            // SAFETY: as above.
            ValueRef::Real(v) => unsafe { (a.sqlite3_bind_double)(s, index, v) },
            ValueRef::Text(b) => {
                let Ok(n) = c_int::try_from(b.len()) else {
                    return codes::TOOBIG;
                };
                let p = if b.is_empty() {
                    EMPTY.as_ptr()
                } else {
                    b.as_ptr()
                };
                // SAFETY: `p` is readable for `n` bytes; SQLITE_TRANSIENT makes SQLite copy
                // them before returning, so the borrow may end afterwards.
                unsafe { (a.sqlite3_bind_text)(s, index, p.cast(), n, transient()) }
            }
            ValueRef::Blob(b) => {
                let Ok(n) = c_int::try_from(b.len()) else {
                    return codes::TOOBIG;
                };
                let p = if b.is_empty() {
                    EMPTY.as_ptr()
                } else {
                    b.as_ptr()
                };
                // SAFETY: as for text.
                unsafe { (a.sqlite3_bind_blob)(s, index, p.cast(), n, transient()) }
            }
        }
    }

    pub(crate) fn column_count(&self) -> usize {
        // SAFETY: `stmt` is a live prepared statement.
        let n = unsafe { (self.lib.api.sqlite3_column_count)(self.stmt) };
        usize::try_from(n).unwrap_or(0)
    }

    fn column_index(&self, index: usize) -> Result<c_int, Error> {
        let count = self.column_count();
        match c_int::try_from(index) {
            Ok(i) if index < count => Ok(i),
            _ => Err(Error::ColumnIndex { index, count }),
        }
    }

    /// The column name (copied: SQLite may free it on the next name call).
    pub(crate) fn column_name(&self, index: usize) -> Result<String, Error> {
        let i = self.column_index(index)?;
        // SAFETY: live statement, `i` in range.
        let p = unsafe { (self.lib.api.sqlite3_column_name)(self.stmt, i) };
        if p.is_null() {
            return Err(code_error(self.lib, codes::NOMEM));
        }
        // SAFETY: non-null NUL-terminated string owned by the statement, copied at once.
        Ok(unsafe { copy_cstr(p) })
    }

    fn raw_type(&self, index: usize) -> Result<(c_int, c_int), Error> {
        if !self.has_row {
            return Err(Error::NoCurrentRow);
        }
        let i = self.column_index(index)?;
        // SAFETY: the statement is on a row and `i` is in range.
        let t = unsafe { (self.lib.api.sqlite3_column_type)(self.stmt, i) };
        Ok((i, t))
    }

    pub(crate) fn column_type(&self, index: usize) -> Result<Type, Error> {
        Ok(match self.raw_type(index)?.1 {
            SQLITE_INTEGER => Type::Integer,
            SQLITE_FLOAT => Type::Real,
            SQLITE_TEXT => Type::Text,
            SQLITE_BLOB => Type::Blob,
            _ => Type::Null,
        })
    }

    /// The value of a column of the current row, read with the accessor matching its
    /// storage class (so SQLite never converts it and earlier borrows stay valid).
    pub(crate) fn value(&self, index: usize) -> Result<ValueRef<'_>, Error> {
        let (i, t) = self.raw_type(index)?;
        let (a, s) = (&self.lib.api, self.stmt);
        Ok(match t {
            SQLITE_INTEGER => {
                // SAFETY: on a row, `i` in range.
                ValueRef::Integer(unsafe { (a.sqlite3_column_int64)(s, i) })
            }
            SQLITE_FLOAT => {
                // SAFETY: on a row, `i` in range.
                ValueRef::Real(unsafe { (a.sqlite3_column_double)(s, i) })
            }
            SQLITE_TEXT => {
                // SAFETY: on a row, `i` in range; the column is TEXT, so this returns its
                // UTF-8 bytes (converting once if the database is UTF-16).
                let p = unsafe { (a.sqlite3_column_text)(s, i) };
                if p.is_null() {
                    // NULL for TEXT means the conversion ran out of memory.
                    return Err(code_error(self.lib, codes::NOMEM));
                }
                // SAFETY: as above; called after sqlite3_column_text as documented.
                let n = unsafe { (a.sqlite3_column_bytes)(s, i) };
                // SAFETY: `p` points to `n` bytes owned by the statement, valid until the
                // next step/reset/finalize, all of which need `&mut self` or drop, so they
                // cannot happen while the returned borrow of `self` lives.
                ValueRef::Text(unsafe { slice(p, n) })
            }
            SQLITE_BLOB => {
                // SAFETY: on a row, `i` in range; the column is a BLOB.
                let p = unsafe { (a.sqlite3_column_blob)(s, i) };
                // SAFETY: as above.
                let n = unsafe { (a.sqlite3_column_bytes)(s, i) };
                // SAFETY: as for text; a zero-length blob gives NULL, handled by `slice`.
                ValueRef::Blob(unsafe { slice(p.cast(), n) })
            }
            _ => ValueRef::Null,
        })
    }
}

/// # Safety
/// `p` is NULL or points to at least `n` bytes that live as long as `'a`.
unsafe fn slice<'a>(p: *const u8, n: c_int) -> &'a [u8] {
    match usize::try_from(n) {
        Ok(n) if n > 0 && !p.is_null() => {
            // SAFETY: per the caller's contract.
            unsafe { core::slice::from_raw_parts(p, n) }
        }
        _ => &[],
    }
}

#[cfg(windows)]
mod os {
    use super::{ENV, Ptr};
    use core::ffi::{CStr, c_char};
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryExW(name: *const u16, file: Ptr, flags: u32) -> Ptr;
        fn GetProcAddress(module: Ptr, name: *const c_char) -> Ptr;
    }

    const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

    fn load(name: &OsStr, flags: u32) -> Result<Ptr, String> {
        let wide: Vec<u16> = name.encode_wide().chain(Some(0)).collect();
        // SAFETY: `wide` is NUL-terminated UTF-16, the file handle must be NULL; the module
        // stays loaded for the life of the process.
        let h = unsafe { LoadLibraryExW(wide.as_ptr(), core::ptr::null_mut(), flags) };
        if h.is_null() {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(h)
        }
    }

    pub(super) fn open() -> Result<(Ptr, String), String> {
        if let Some(p) = std::env::var_os(ENV).filter(|p| !p.is_empty()) {
            let path = Path::new(&p);
            let shown = path.display().to_string();
            if !path.is_absolute() {
                return Err(format!("{ENV} must be an absolute path, got {shown}"));
            }
            // Dependencies of a custom build are searched next to it, then in System32.
            let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32;
            return match load(&p, flags) {
                Ok(h) => Ok((h, shown)),
                Err(e) => Err(format!("cannot load {shown} ({ENV}): {e}")),
            };
        }
        match load(OsStr::new("winsqlite3.dll"), LOAD_LIBRARY_SEARCH_SYSTEM32) {
            Ok(h) => Ok((h, "winsqlite3.dll".to_owned())),
            Err(e) => Err(format!("cannot load winsqlite3.dll from System32: {e}")),
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
    use super::{ENV, Ptr};
    use core::ffi::{CStr, c_char, c_int};
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flag: c_int) -> Ptr;
        fn dlsym(handle: Ptr, symbol: *const c_char) -> Ptr;
        fn dlerror() -> *mut c_char;
    }

    const RTLD_NOW: c_int = 2;

    #[cfg(target_vendor = "apple")]
    const CANDIDATES: &[&CStr] = &[c"/usr/lib/libsqlite3.dylib"];
    #[cfg(not(target_vendor = "apple"))]
    const CANDIDATES: &[&CStr] = &[c"libsqlite3.so.0", c"libsqlite3.so"];

    fn load(name: &CStr) -> Result<Ptr, String> {
        // SAFETY: `name` is NUL-terminated; the handle is kept for the life of the process.
        let h = unsafe { dlopen(name.as_ptr(), RTLD_NOW) };
        if !h.is_null() {
            return Ok(h);
        }
        // SAFETY: dlerror returns NULL or a NUL-terminated message valid until the next
        // dl* call on this thread.
        let e = unsafe { dlerror() };
        // SAFETY: as above; copied at once.
        let msg = unsafe { super::copy_cstr(e) };
        Err(if msg.is_empty() {
            format!("cannot load {}", name.to_string_lossy())
        } else {
            msg
        })
    }

    pub(super) fn open() -> Result<(Ptr, String), String> {
        if let Some(p) = std::env::var_os(ENV).filter(|p| !p.is_empty()) {
            let shown = p.to_string_lossy().into_owned();
            let Ok(c) = CString::new(p.as_bytes()) else {
                return Err(format!("{ENV} contains NUL"));
            };
            return match load(&c) {
                Ok(h) => Ok((h, shown)),
                Err(e) => Err(format!("{ENV}: {e}")),
            };
        }
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

#[cfg(not(any(windows, unix)))]
mod os {
    use super::Ptr;
    use core::ffi::CStr;

    pub(super) fn open() -> Result<(Ptr, String), String> {
        Err("no SQLite library can be loaded on this platform".to_owned())
    }

    pub(super) fn symbol(_handle: Ptr, _name: &CStr) -> Ptr {
        core::ptr::null_mut()
    }
}
