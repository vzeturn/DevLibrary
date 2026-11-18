//! Read-only access to the Windows registry.
//!
//! ```no_run
//! use dl_winos::registry::{Hive, Key, View};
//!
//! let k = Key::open(
//!     Hive::LocalMachine,
//!     r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
//!     View::Default,
//! )?;
//! println!("{:?}", k.string("ProductName")?);
//! # Ok::<(), dl_winos::Error>(())
//! ```

use core::fmt;

use crate::wide::{to_wide, units_le, until_nul};
use crate::{Error, sys};

/// `REG_*` value type codes, as returned by [`Value::kind`].
pub mod kind {
    /// `REG_NONE`: no defined type.
    pub const NONE: u32 = 0;
    /// `REG_SZ`: a string.
    pub const SZ: u32 = 1;
    /// `REG_EXPAND_SZ`: a string with unexpanded `%VAR%` references.
    pub const EXPAND_SZ: u32 = 2;
    /// `REG_BINARY`: raw bytes.
    pub const BINARY: u32 = 3;
    /// `REG_DWORD`: a little-endian 32-bit number.
    pub const DWORD: u32 = 4;
    /// `REG_DWORD_BIG_ENDIAN`: a big-endian 32-bit number.
    pub const DWORD_BIG_ENDIAN: u32 = 5;
    /// `REG_LINK`: a symbolic link (only seen by `REG_OPTION_OPEN_LINK` callers).
    pub const LINK: u32 = 6;
    /// `REG_MULTI_SZ`: a list of strings.
    pub const MULTI_SZ: u32 = 7;
    /// `REG_RESOURCE_LIST`: a device-driver resource list.
    pub const RESOURCE_LIST: u32 = 8;
    /// `REG_FULL_RESOURCE_DESCRIPTOR`: a hardware resource description.
    pub const FULL_RESOURCE_DESCRIPTOR: u32 = 9;
    /// `REG_RESOURCE_REQUIREMENTS_LIST`: a resource requirements list.
    pub const RESOURCE_REQUIREMENTS_LIST: u32 = 10;
    /// `REG_QWORD`: a little-endian 64-bit number.
    pub const QWORD: u32 = 11;
}

/// The root key a path is opened under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hive {
    /// `HKEY_LOCAL_MACHINE`.
    LocalMachine,
    /// `HKEY_CURRENT_USER`.
    CurrentUser,
    /// `HKEY_CLASSES_ROOT`.
    ClassesRoot,
    /// `HKEY_USERS`.
    Users,
}

impl Hive {
    /// The predefined `HKEY` value (the low 32 bits, sign-extended by the Windows code).
    pub(crate) fn raw(self) -> u32 {
        match self {
            Hive::ClassesRoot => 0x8000_0000,
            Hive::CurrentUser => 0x8000_0001,
            Hive::LocalMachine => 0x8000_0002,
            Hive::Users => 0x8000_0003,
        }
    }
}

/// Which registry view a 32- or 64-bit process sees (WOW64 redirection of `SOFTWARE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum View {
    /// The view that matches the process: 64-bit for a 64-bit process, the redirected
    /// 32-bit view (`WOW6432Node`) for a 32-bit process on 64-bit Windows.
    #[default]
    Default,
    /// The 64-bit view (`KEY_WOW64_64KEY`), also from a 32-bit process.
    Wow64_64,
    /// The 32-bit view (`KEY_WOW64_32KEY`), also from a 64-bit process.
    Wow64_32,
}

const KEY_READ: u32 = 0x0002_0019;

impl View {
    fn access(self) -> u32 {
        KEY_READ
            | match self {
                View::Default => 0,
                View::Wow64_64 => 0x0100,
                View::Wow64_32 => 0x0200,
            }
    }
}

/// A registry value, decoded by type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// `REG_SZ`.
    String(String),
    /// `REG_EXPAND_SZ`, as stored (with `%VAR%` references). Use [`expand`] to resolve them.
    ExpandString(String),
    /// `REG_MULTI_SZ`.
    MultiString(Vec<String>),
    /// `REG_DWORD` with exactly 4 bytes of data.
    Dword(u32),
    /// `REG_QWORD` with exactly 8 bytes of data.
    Qword(u64),
    /// `REG_BINARY`.
    Binary(Vec<u8>),
    /// Any other type, and `REG_DWORD` / `REG_QWORD` values whose data has the wrong length.
    Other {
        /// The `REG_*` type code (see [`kind`](mod@kind)).
        kind: u32,
        /// The raw data.
        data: Vec<u8>,
    },
}

impl Value {
    /// Decodes raw registry data of type `kind`.
    ///
    /// Strings are UTF-16LE: they end at the first NUL or at the end of the data (a missing
    /// terminator is accepted), a trailing odd byte is ignored and unpaired surrogates
    /// become U+FFFD. A `REG_MULTI_SZ` list ends at the first empty string (the double
    /// NUL) or at the end of the data.
    pub fn from_raw(kind: u32, data: &[u8]) -> Value {
        match kind {
            kind::SZ => Value::String(until_nul(&units_le(data))),
            kind::EXPAND_SZ => Value::ExpandString(until_nul(&units_le(data))),
            kind::MULTI_SZ => {
                let units = units_le(data);
                let mut list = Vec::new();
                for part in units.split(|&u| u == 0) {
                    if part.is_empty() {
                        break;
                    }
                    list.push(String::from_utf16_lossy(part));
                }
                Value::MultiString(list)
            }
            kind::BINARY => Value::Binary(data.to_vec()),
            kind::DWORD => match <[u8; 4]>::try_from(data) {
                Ok(b) => Value::Dword(u32::from_le_bytes(b)),
                Err(_) => Value::Other {
                    kind,
                    data: data.to_vec(),
                },
            },
            kind::QWORD => match <[u8; 8]>::try_from(data) {
                Ok(b) => Value::Qword(u64::from_le_bytes(b)),
                Err(_) => Value::Other {
                    kind,
                    data: data.to_vec(),
                },
            },
            _ => Value::Other {
                kind,
                data: data.to_vec(),
            },
        }
    }

    /// The `REG_*` type code of this value.
    pub fn kind(&self) -> u32 {
        match self {
            Value::String(_) => kind::SZ,
            Value::ExpandString(_) => kind::EXPAND_SZ,
            Value::MultiString(_) => kind::MULTI_SZ,
            Value::Dword(_) => kind::DWORD,
            Value::Qword(_) => kind::QWORD,
            Value::Binary(_) => kind::BINARY,
            Value::Other { kind, .. } => *kind,
        }
    }

    /// The text of a `String` or `ExpandString` (unexpanded) value.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) | Value::ExpandString(s) => Some(s),
            _ => None,
        }
    }
}

/// An open registry key, closed on drop. Opened with `KEY_READ` access only.
pub struct Key {
    raw: sys::RegKey,
    view: View,
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Key")
            .field("view", &self.view)
            .finish_non_exhaustive()
    }
}

impl Key {
    /// Opens `subkey` (backslash-separated, no leading backslash; empty opens the hive
    /// itself) under `hive` for reading.
    ///
    /// A missing key fails with [`Error::Win32`] whose [`code`](Error::code) is
    /// [`codes::FILE_NOT_FOUND`](crate::codes::FILE_NOT_FOUND) (2).
    pub fn open(hive: Hive, subkey: &str, view: View) -> Result<Key, Error> {
        let path = to_wide(subkey)?;
        let raw = sys::reg_open_hive(hive.raw(), &path, view.access())?;
        Ok(Key { raw, view })
    }

    /// Opens `subkey` relative to this key, in the same view.
    pub fn open_subkey(&self, subkey: &str) -> Result<Key, Error> {
        let path = to_wide(subkey)?;
        let raw = sys::reg_open_sub(&self.raw, &path, self.view.access())?;
        Ok(Key {
            raw,
            view: self.view,
        })
    }

    /// The view this key was opened in.
    pub fn view(&self) -> View {
        self.view
    }

    /// The value `name` (empty for the key's default value), or `Ok(None)` when the key has
    /// no such value.
    pub fn value(&self, name: &str) -> Result<Option<Value>, Error> {
        let name = to_wide(name)?;
        Ok(
            sys::reg_query_value(&self.raw, &name)?
                .map(|(kind, data)| Value::from_raw(kind, &data)),
        )
    }

    /// The text of a `REG_SZ` or `REG_EXPAND_SZ` value (unexpanded), or `Ok(None)` when the
    /// value does not exist. Other types fail with [`Error::UnexpectedType`].
    pub fn string(&self, name: &str) -> Result<Option<String>, Error> {
        match self.value(name)? {
            None => Ok(None),
            Some(Value::String(s) | Value::ExpandString(s)) => Ok(Some(s)),
            Some(v) => Err(Error::UnexpectedType { kind: v.kind() }),
        }
    }

    /// The names of the direct subkeys, in registry order.
    pub fn subkeys(&self) -> Result<Vec<String>, Error> {
        sys::reg_subkeys(&self.raw)
    }

    /// All values of the key as `(name, value)` pairs, in registry order. The default value,
    /// when set, has an empty name.
    pub fn values(&self) -> Result<Vec<(String, Value)>, Error> {
        Ok(sys::reg_values(&self.raw)?
            .into_iter()
            .map(|(name, kind, data)| (name, Value::from_raw(kind, &data)))
            .collect())
    }
}

/// Replaces `%VAR%` references in `s` with the current process environment, as
/// `ExpandEnvironmentStringsW` does. Unknown variables are left as written.
pub fn expand(s: &str) -> Result<String, Error> {
    sys::expand_env(&to_wide(s)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn strings_with_and_without_nul() {
        assert_eq!(
            Value::from_raw(kind::SZ, &utf16("abc\0")),
            Value::String("abc".into())
        );
        assert_eq!(
            Value::from_raw(kind::SZ, &utf16("abc")),
            Value::String("abc".into())
        );
        assert_eq!(
            Value::from_raw(kind::SZ, &utf16("ab\0junk")),
            Value::String("ab".into())
        );
        let mut odd = utf16("đx");
        odd.push(0x41);
        assert_eq!(Value::from_raw(kind::SZ, &odd), Value::String("đx".into()));
        assert_eq!(Value::from_raw(kind::SZ, &[]), Value::String(String::new()));
        assert_eq!(
            Value::from_raw(kind::SZ, &[7]),
            Value::String(String::new())
        );
        assert_eq!(
            Value::from_raw(kind::EXPAND_SZ, &utf16("%TEMP%\\x\0")),
            Value::ExpandString("%TEMP%\\x".into())
        );
        // Unpaired surrogate.
        assert_eq!(
            Value::from_raw(kind::SZ, &[0x00, 0xD8, 0x41, 0x00]),
            Value::String("\u{FFFD}A".into())
        );
    }

    #[test]
    fn multi_strings() {
        let ms = |v: &[&str]| Value::MultiString(v.iter().map(|s| s.to_string()).collect());
        assert_eq!(
            Value::from_raw(kind::MULTI_SZ, &utf16("a\0bc\0\0")),
            ms(&["a", "bc"])
        );
        assert_eq!(
            Value::from_raw(kind::MULTI_SZ, &utf16("a\0bc\0")),
            ms(&["a", "bc"])
        );
        assert_eq!(
            Value::from_raw(kind::MULTI_SZ, &utf16("a\0bc")),
            ms(&["a", "bc"])
        );
        assert_eq!(Value::from_raw(kind::MULTI_SZ, &utf16("\0")), ms(&[]));
        assert_eq!(Value::from_raw(kind::MULTI_SZ, &[]), ms(&[]));
        assert_eq!(
            Value::from_raw(kind::MULTI_SZ, &utf16("a\0\0b\0\0")),
            ms(&["a"])
        );
        let mut odd = utf16("x\0y");
        odd.push(1);
        assert_eq!(Value::from_raw(kind::MULTI_SZ, &odd), ms(&["x", "y"]));
    }

    #[test]
    fn numbers_and_binary() {
        assert_eq!(
            Value::from_raw(kind::DWORD, &[0x78, 0x56, 0x34, 0x12]),
            Value::Dword(0x1234_5678)
        );
        assert_eq!(
            Value::from_raw(kind::QWORD, &0x0123_4567_89AB_CDEF_u64.to_le_bytes()),
            Value::Qword(0x0123_4567_89AB_CDEF)
        );
        assert_eq!(
            Value::from_raw(kind::DWORD, &[1, 2, 3]),
            Value::Other {
                kind: kind::DWORD,
                data: vec![1, 2, 3]
            }
        );
        assert_eq!(
            Value::from_raw(kind::QWORD, &[1, 2, 3, 4]),
            Value::Other {
                kind: kind::QWORD,
                data: vec![1, 2, 3, 4]
            }
        );
        assert_eq!(
            Value::from_raw(kind::BINARY, &[0, 0xFF]),
            Value::Binary(vec![0, 0xFF])
        );
        assert_eq!(
            Value::from_raw(kind::DWORD_BIG_ENDIAN, &[0, 0, 0, 1]),
            Value::Other {
                kind: kind::DWORD_BIG_ENDIAN,
                data: vec![0, 0, 0, 1]
            }
        );
        assert_eq!(Value::from_raw(kind::NONE, &[]).kind(), kind::NONE);
        assert_eq!(Value::Dword(1).kind(), kind::DWORD);
        assert_eq!(Value::String("a".into()).as_str(), Some("a"));
        assert_eq!(Value::ExpandString("%a%".into()).as_str(), Some("%a%"));
        assert_eq!(Value::Qword(1).as_str(), None);
    }

    #[test]
    fn views_and_hives() {
        assert_eq!(View::default(), View::Default);
        assert_eq!(View::Default.access(), 0x20019);
        assert_eq!(View::Wow64_64.access(), 0x20119);
        assert_eq!(View::Wow64_32.access(), 0x20219);
        assert_eq!(Hive::LocalMachine.raw(), 0x8000_0002);
    }

    #[test]
    fn nul_in_names_is_rejected() {
        assert!(matches!(
            Key::open(Hive::LocalMachine, "SOFTWARE\0x", View::Default),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(expand("a\0b"), Err(Error::InvalidInput(_))));
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_elsewhere() {
        assert_eq!(
            Key::open(Hive::LocalMachine, "SOFTWARE", View::Default).unwrap_err(),
            Error::Unsupported
        );
        assert_eq!(expand("%HOME%").unwrap_err(), Error::Unsupported);
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use crate::codes;
    use std::process::Command;

    const CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

    #[test]
    fn current_version() {
        for view in [View::Default, View::Wow64_64, View::Wow64_32] {
            let k = Key::open(Hive::LocalMachine, CURRENT_VERSION, view).unwrap();
            assert_eq!(k.view(), view);
            let product = k.string("ProductName").unwrap().unwrap();
            assert!(product.starts_with("Windows"), "{product}");
            let build = k.string("CurrentBuildNumber").unwrap().unwrap();
            assert!(build.parse::<u32>().unwrap() >= 7600, "{build}");
            assert!(matches!(
                k.value("CurrentMajorVersionNumber").unwrap(),
                Some(Value::Dword(10))
            ));
        }
    }

    #[test]
    fn missing_key_and_value() {
        let e = Key::open(
            Hive::LocalMachine,
            r"SOFTWARE\dl-winos-no-such-key",
            View::Default,
        )
        .unwrap_err();
        assert_eq!(e.code(), Some(codes::FILE_NOT_FOUND), "{e}");
        assert_eq!(e.operation(), Some("RegOpenKeyExW"));
        let k = Key::open(Hive::LocalMachine, CURRENT_VERSION, View::Default).unwrap();
        assert_eq!(k.value("dl-winos-no-such-value").unwrap(), None);
        assert_eq!(k.string("dl-winos-no-such-value").unwrap(), None);
        assert_eq!(
            k.string("CurrentMajorVersionNumber").unwrap_err(),
            Error::UnexpectedType { kind: kind::DWORD }
        );
        let e = k.open_subkey("dl-winos-no-such-key").unwrap_err();
        assert_eq!(e.code(), Some(codes::FILE_NOT_FOUND));
    }

    #[test]
    fn printers_key_matches_spooler() {
        let k = Key::open(
            Hive::LocalMachine,
            r"SYSTEM\CurrentControlSet\Control\Print\Printers",
            View::Default,
        )
        .unwrap();
        let names = k.subkeys().unwrap();
        assert!(
            names.iter().any(|n| n == "Microsoft Print to PDF"),
            "{names:?}"
        );
        for n in &names {
            let p = k.open_subkey(n).unwrap();
            // Every queue key stores its port; the subkey list is the local printers.
            assert!(p.string("Port").unwrap().is_some(), "{n}");
        }
        let pdf = k.open_subkey("Microsoft Print to PDF").unwrap();
        assert_eq!(
            pdf.string("Name").unwrap().as_deref(),
            Some("Microsoft Print to PDF")
        );
        let vals = pdf.values().unwrap();
        assert!(
            vals.iter()
                .any(|(n, v)| n == "Port" && v.as_str() == Some("PORTPROMPT:"))
        );
        let local: Vec<String> = crate::spooler::printers()
            .unwrap()
            .into_iter()
            .filter(|p| p.attributes & crate::spooler::attributes::LOCAL != 0)
            .map(|p| p.name)
            .collect();
        for p in &local {
            // Per-user queues (for example "OneNote (Desktop) - Protected") are stored as
            // "<user SID>:<name>".
            let found = names.iter().any(|n| {
                n.eq_ignore_ascii_case(p)
                    || n.rsplit_once(':')
                        .is_some_and(|(_, tail)| tail.eq_ignore_ascii_case(p))
            });
            assert!(found, "{p} not in {names:?}");
        }
        // The Printers key has its own values too.
        assert!(k.string("DefaultSpoolDirectory").unwrap().is_some());
    }

    #[test]
    fn expand_environment() {
        let root = std::env::var("SystemRoot").unwrap();
        assert_eq!(expand("%SystemRoot%").unwrap(), root);
        assert_eq!(
            expand(r"%SystemRoot%\x\%SystemRoot%").unwrap(),
            format!(r"{root}\x\{root}")
        );
        assert_eq!(expand("").unwrap(), "");
        assert_eq!(expand("plain đ").unwrap(), "plain đ");
        assert_eq!(
            expand("%DL_WINOS_UNSET_VAR%").unwrap(),
            "%DL_WINOS_UNSET_VAR%"
        );
        let long = "y".repeat(5000);
        assert_eq!(
            expand(&format!("%SystemRoot%{long}")).unwrap(),
            format!("{root}{long}")
        );
    }

    fn reg(args: &[&str]) {
        let out = Command::new("reg.exe").args(args).output().unwrap();
        assert!(
            out.status.success(),
            "reg {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Serialises the tests that create keys, so one never deletes the shared parent while
    /// the other is still using it.
    static TEMP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Deletes the temporary key even when an assertion fails.
    struct TempKey(String);

    impl Drop for TempKey {
        fn drop(&mut self) {
            let _ = Command::new("reg.exe")
                .args(["delete", &format!(r"HKCU\{}", self.0), "/f"])
                .output();
            // Remove the shared parent when no other run left keys in it.
            if let Ok(parent) =
                Key::open(Hive::CurrentUser, r"Software\dl-winos-test", View::Default)
                && parent.subkeys().is_ok_and(|s| s.is_empty())
            {
                drop(parent);
                let _ = Command::new("reg.exe")
                    .args(["delete", r"HKCU\Software\dl-winos-test", "/f"])
                    .output();
            }
        }
    }

    #[test]
    fn every_value_type_round_trips() {
        let _guard = TEMP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path = format!(r"Software\dl-winos-test\{}", std::process::id());
        let tmp = TempKey(path.clone());
        let full = format!(r"HKCU\{path}");
        let add = |name: &str, ty: &str, data: &str| {
            reg(&["add", &full, "/v", name, "/t", ty, "/d", data, "/f"]);
        };
        add("sz", "REG_SZ", "hello đ 世界");
        add("empty", "REG_SZ", "");
        add("expand", "REG_EXPAND_SZ", r"%SystemRoot%\dl");
        add("multi", "REG_MULTI_SZ", r"one\0two\0đ");
        add("dword", "REG_DWORD", "305419896");
        add("dwmax", "REG_DWORD", "4294967295");
        add("qword", "REG_QWORD", "81985529216486895");
        add("binary", "REG_BINARY", "00ff10");
        add("none", "REG_NONE", "");
        add("bigendian", "REG_DWORD_BIG_ENDIAN", "1");
        reg(&["add", &full, "/ve", "/d", "default value", "/f"]);
        reg(&["add", &format!(r"{full}\child"), "/f"]);

        let k = Key::open(Hive::CurrentUser, &path, View::Default).unwrap();
        assert_eq!(
            k.value("sz").unwrap(),
            Some(Value::String("hello đ 世界".into()))
        );
        assert_eq!(k.string("empty").unwrap().as_deref(), Some(""));
        let raw = k.string("expand").unwrap().unwrap();
        assert_eq!(raw, r"%SystemRoot%\dl");
        assert!(matches!(
            k.value("expand").unwrap(),
            Some(Value::ExpandString(_))
        ));
        assert_eq!(
            expand(&raw).unwrap(),
            format!(r"{}\dl", std::env::var("SystemRoot").unwrap())
        );
        assert_eq!(
            k.value("multi").unwrap(),
            Some(Value::MultiString(vec![
                "one".into(),
                "two".into(),
                "đ".into()
            ]))
        );
        assert_eq!(k.value("dword").unwrap(), Some(Value::Dword(0x1234_5678)));
        assert_eq!(k.value("dwmax").unwrap(), Some(Value::Dword(u32::MAX)));
        assert_eq!(
            k.value("qword").unwrap(),
            Some(Value::Qword(0x0123_4567_89AB_CDEF))
        );
        assert_eq!(
            k.value("binary").unwrap(),
            Some(Value::Binary(vec![0, 0xFF, 0x10]))
        );
        assert_eq!(k.value("none").unwrap().map(|v| v.kind()), Some(kind::NONE));
        // reg.exe stores REG_DWORD_BIG_ENDIAN data in little-endian order; the point here is
        // that the type and the raw bytes come back untouched.
        assert_eq!(
            k.value("bigendian").unwrap(),
            Some(Value::Other {
                kind: kind::DWORD_BIG_ENDIAN,
                data: vec![1, 0, 0, 0]
            })
        );
        assert_eq!(k.string("").unwrap().as_deref(), Some("default value"));
        assert_eq!(k.value("missing").unwrap(), None);

        let mut names: Vec<String> = k.values().unwrap().into_iter().map(|(n, _)| n).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "",
                "bigendian",
                "binary",
                "dwmax",
                "dword",
                "empty",
                "expand",
                "multi",
                "none",
                "qword",
                "sz"
            ]
        );
        for (n, v) in k.values().unwrap() {
            assert_eq!(k.value(&n).unwrap(), Some(v), "{n}");
        }
        assert_eq!(k.subkeys().unwrap(), ["child"]);
        let child = k.open_subkey("child").unwrap();
        assert!(child.subkeys().unwrap().is_empty());
        // `reg add <key>` without /v also sets an empty default value.
        assert_eq!(
            child.values().unwrap(),
            [(String::new(), Value::String(String::new()))]
        );
        drop((child, k));
        drop(tmp);
        let e = Key::open(Hive::CurrentUser, &path, View::Default).unwrap_err();
        assert_eq!(e.code(), Some(codes::FILE_NOT_FOUND));
    }

    #[test]
    fn long_values_and_names() {
        let _guard = TEMP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path = format!(r"Software\dl-winos-test\{}-long", std::process::id());
        let tmp = TempKey(path.clone());
        let full = format!(r"HKCU\{path}");
        let long_name = "n".repeat(300);
        let long_data = "d".repeat(6000);
        reg(&[
            "add", &full, "/v", &long_name, "/t", "REG_SZ", "/d", &long_data, "/f",
        ]);
        let long_key = "k".repeat(200);
        reg(&["add", &format!(r"{full}\{long_key}"), "/f"]);
        let k = Key::open(Hive::CurrentUser, &path, View::Default).unwrap();
        assert_eq!(k.string(&long_name).unwrap(), Some(long_data.clone()));
        assert_eq!(
            k.values().unwrap(),
            [(long_name.clone(), Value::String(long_data))]
        );
        assert_eq!(k.subkeys().unwrap(), [long_key]);
        drop(k);
        drop(tmp);
    }
}
