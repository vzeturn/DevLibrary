//! Restricting a directory or file to SYSTEM and Administrators: a protected DACL (nothing
//! inherited from the parent) granting both full control, owner Administrators. This is
//! what a service needs for a state directory holding keys and tokens under
//! `C:\ProgramData`, whose default ACL lets every user read.
//!
//! [`check`] is platform neutral, so the rule can be tested anywhere; [`read`] and
//! [`restrict`] work on Windows only.

use std::path::Path;

use crate::Error;
use crate::sys;
use crate::wide::to_wide;

/// The LocalSystem SID.
pub const SID_LOCAL_SYSTEM: &str = "S-1-5-18";
/// The BUILTIN\Administrators SID.
pub const SID_ADMINISTRATORS: &str = "S-1-5-32-544";
/// `FILE_ALL_ACCESS`.
pub const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
const GENERIC_ALL: u32 = 0x1000_0000;
/// `ACCESS_ALLOWED_ACE_TYPE`.
pub const ACE_ALLOWED: u8 = 0;
/// `ACCESS_DENIED_ACE_TYPE`.
pub const ACE_DENIED: u8 = 1;

/// One DACL entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ace {
    /// ACE type (`ACE_ALLOWED`, `ACE_DENIED`, ...).
    pub kind: u8,
    /// The trustee as a string SID (empty for ACE types without a plain SID).
    pub sid: String,
    /// Access mask (allowed and denied ACEs only).
    pub mask: u32,
    /// Inherited from a parent (`INHERITED_ACE`).
    pub inherited: bool,
}

/// The owner and DACL of a path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Descriptor {
    /// Owner SID.
    pub owner: String,
    /// `SE_DACL_PROTECTED`: nothing is inherited from the parent.
    pub protected: bool,
    /// No DACL at all, which grants everyone full control.
    pub null_dacl: bool,
    /// The entries, in order.
    pub aces: Vec<Ace>,
}

fn allowed_sid(sid: &str) -> bool {
    sid == SID_LOCAL_SYSTEM || sid == SID_ADMINISTRATORS
}

/// Whether `d` is restricted: a protected DACL, owned by SYSTEM or Administrators, whose
/// allow entries name only SYSTEM and Administrators and give both full control. Deny
/// entries only narrow access and are accepted; other entry types are refused. The
/// error says why, naming SIDs only.
///
/// ```
/// use dl_winsvc::acl::{Ace, Descriptor, check};
/// let full = |sid: &str| Ace { kind: 0, sid: sid.into(), mask: 0x1F01FF, inherited: false };
/// let mut d = Descriptor {
///     owner: "S-1-5-32-544".into(),
///     protected: true,
///     null_dacl: false,
///     aces: vec![full("S-1-5-18"), full("S-1-5-32-544")],
/// };
/// assert!(check(&d).is_ok());
/// d.aces.push(Ace { kind: 0, sid: "S-1-5-32-545".into(), mask: 0x1200A9, inherited: true });
/// assert!(check(&d).unwrap_err().contains("S-1-5-32-545"));
/// ```
pub fn check(d: &Descriptor) -> Result<(), String> {
    if d.null_dacl {
        return Err("no DACL (everyone has full control)".into());
    }
    if !d.protected {
        return Err("DACL inherits from the parent directory".into());
    }
    if !allowed_sid(&d.owner) {
        return Err(format!(
            "owner {} is neither SYSTEM nor Administrators",
            d.owner
        ));
    }
    let (mut system, mut admins) = (false, false);
    for a in &d.aces {
        match a.kind {
            ACE_DENIED => continue,
            ACE_ALLOWED => {}
            k => return Err(format!("unexpected ACE type {k}")),
        }
        if !allowed_sid(&a.sid) {
            return Err(format!("access granted to {}", a.sid));
        }
        if a.mask & GENERIC_ALL != 0 || a.mask & FILE_ALL_ACCESS == FILE_ALL_ACCESS {
            if a.sid == SID_LOCAL_SYSTEM {
                system = true;
            } else {
                admins = true;
            }
        }
    }
    if !system {
        return Err(format!("{SID_LOCAL_SYSTEM} lacks full control"));
    }
    if !admins {
        return Err(format!("{SID_ADMINISTRATORS} lacks full control"));
    }
    Ok(())
}

fn wide_path(path: &Path) -> Result<Vec<u16>, Error> {
    let s = path
        .to_str()
        .ok_or(Error::InvalidInput("path is not valid Unicode"))?;
    to_wide(s)
}

/// Reads the owner and DACL of `path`. Windows only ([`Error::Unsupported`] elsewhere).
pub fn read(path: &Path) -> Result<Descriptor, Error> {
    sys::read_security(&wide_path(path)?)
}

/// Sets the restricted owner and DACL on `path`. For a directory the entries are inherited
/// by everything inside (existing children get them too). Needs an elevated Administrator
/// or LocalSystem. Windows only ([`Error::Unsupported`] elsewhere).
pub fn restrict(path: &Path) -> Result<(), Error> {
    let is_dir = path.is_dir();
    sys::restrict_path(&wide_path(path)?, is_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ace(kind: u8, sid: &str, mask: u32) -> Ace {
        Ace {
            kind,
            sid: sid.into(),
            mask,
            inherited: false,
        }
    }

    fn good() -> Descriptor {
        Descriptor {
            owner: SID_ADMINISTRATORS.into(),
            protected: true,
            null_dacl: false,
            aces: vec![
                ace(ACE_ALLOWED, SID_LOCAL_SYSTEM, FILE_ALL_ACCESS),
                ace(ACE_ALLOWED, SID_ADMINISTRATORS, FILE_ALL_ACCESS),
            ],
        }
    }

    #[test]
    fn the_go_agent_rules() {
        assert!(check(&good()).is_ok());
        let mut d = good();
        d.owner = SID_LOCAL_SYSTEM.into();
        assert!(check(&d).is_ok(), "SYSTEM may own it");
        d.aces.push(ace(ACE_DENIED, "S-1-1-0", FILE_ALL_ACCESS));
        assert!(check(&d).is_ok(), "deny entries only narrow access");
        let mut d = good();
        d.aces[1].mask = GENERIC_ALL;
        assert!(check(&d).is_ok());
        for (bad, why) in [
            (
                Descriptor {
                    null_dacl: true,
                    ..good()
                },
                "no DACL",
            ),
            (
                Descriptor {
                    protected: false,
                    ..good()
                },
                "inherits",
            ),
            (
                Descriptor {
                    owner: "S-1-5-21-1-2-3-1001".into(),
                    ..good()
                },
                "owner",
            ),
        ] {
            assert!(check(&bad).unwrap_err().contains(why), "{why}");
        }
        let mut d = good();
        d.aces.push(ace(ACE_ALLOWED, "S-1-5-32-545", 0x1200A9));
        assert!(check(&d).unwrap_err().contains("S-1-5-32-545"));
        let mut d = good();
        d.aces[0].mask = 0x1200A9;
        assert!(check(&d).unwrap_err().contains("S-1-5-18 lacks"));
        let mut d = good();
        d.aces.push(ace(9, "", 0));
        assert!(check(&d).unwrap_err().contains("ACE type 9"));
    }

    /// Needs an elevated prompt; skipped (with a note) otherwise.
    #[cfg(windows)]
    #[test]
    fn restricts_and_reads_back_a_directory() {
        let dir = std::env::temp_dir().join(format!("dl-winsvc-acl-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("f.txt"), "x").unwrap();
        let before = read(&dir).unwrap();
        assert!(
            check(&before).is_err(),
            "a temp dir inherits its parent's ACL"
        );
        match restrict(&dir) {
            Err(Error::Win32 { code: 5 | 1307, .. }) => {
                eprintln!("note: not elevated; skipping the restrict half of the ACL test");
            }
            Err(e) => panic!("{e}"),
            Ok(()) => {
                let after = read(&dir).unwrap();
                assert!(check(&after).is_ok(), "{after:?}");
                // The file inside inherits the two entries (not protected itself).
                let f = read(&dir.join("sub").join("f.txt")).unwrap();
                assert!(
                    f.aces.iter().all(|a| a.inherited && allowed_sid(&a.sid)),
                    "{f:?}"
                );
                let file = dir.join("cfg.json");
                std::fs::write(&file, "{}").unwrap();
                restrict(&file).unwrap();
                assert!(check(&read(&file).unwrap()).is_ok());
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_elsewhere() {
        assert_eq!(read(Path::new("/tmp")), Err(Error::Unsupported));
    }
}
