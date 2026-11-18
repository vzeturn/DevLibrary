//! Keeps the state directory (key, identity, logs) and the config file closed to everyone
//! but the service. On Windows: a protected DACL granting full control to SYSTEM and
//! Administrators only, owned by Administrators (`dl_winsvc::acl`). Elsewhere: mode 0700
//! for directories and 0600 for files. [`ensure_dir`] and [`ensure_file`] create, verify
//! and repair; the agent refuses to start when the permissions cannot be brought back.

use std::fmt;
use std::io;
use std::path::Path;

/// Why a path failed.
#[derive(Debug)]
pub enum AclError {
    /// The path grants more (or other) access than [`Guard::restrict`] sets.
    NotRestricted(String),
    /// Reading or changing the permissions failed.
    Failed(String),
}

impl fmt::Display for AclError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AclError::NotRestricted(m) => write!(
                f,
                "{m}: permissions are wider than SYSTEM and Administrators only"
            ),
            AclError::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for AclError {}

/// Applies and verifies the restricted permissions of one path.
pub trait Guard: Send + Sync {
    /// Applies the restricted permissions to `path` (a directory or a file).
    fn restrict(&self, path: &Path) -> Result<(), AclError>;
    /// [`AclError::NotRestricted`] when `path` grants more than `restrict` sets.
    fn check(&self, path: &Path) -> Result<(), AclError>;
}

/// What [`ensure_dir`] or [`ensure_file`] had to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Already restricted (or, for a file, missing).
    Unchanged,
    /// The directory did not exist and was created restricted.
    Created,
    /// Too open, now restricted.
    Repaired,
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Outcome::Unchanged => "unchanged",
            Outcome::Created => "created",
            Outcome::Repaired => "repaired",
        })
    }
}

fn failed(m: String) -> AclError {
    AclError::Failed(m)
}

/// Creates `dir` restricted when it is missing; an existing directory is checked and,
/// when too open, repaired and checked again.
pub fn ensure_dir(g: &dyn Guard, dir: &Path) -> Result<Outcome, AclError> {
    let d = dir.display();
    match std::fs::metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            crate::log::mkdir_private(dir).map_err(|e| failed(format!("acl: create {d}: {e}")))?;
            g.restrict(dir)
                .map_err(|e| failed(format!("acl: restrict {d}: {e}")))?;
            g.check(dir)
                .map_err(|e| failed(format!("acl: {d} is not restricted after creation: {e}")))?;
            Ok(Outcome::Created)
        }
        Err(e) => Err(failed(format!("acl: stat {d}: {e}"))),
        Ok(_) => ensure(g, dir),
    }
}

/// Checks `path` and repairs it when too open; a missing file is fine (the
/// configuration may come from the environment alone).
pub fn ensure_file(g: &dyn Guard, path: &Path) -> Result<Outcome, AclError> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Outcome::Unchanged),
        Err(e) => Err(failed(format!("acl: stat {}: {e}", path.display()))),
        Ok(_) => ensure(g, path),
    }
}

fn ensure(g: &dyn Guard, path: &Path) -> Result<Outcome, AclError> {
    let p = path.display();
    match g.check(path) {
        Ok(()) => return Ok(Outcome::Unchanged),
        Err(AclError::NotRestricted(_)) => {}
        Err(e) => return Err(failed(format!("acl: check {p}: {e}"))),
    }
    g.restrict(path).map_err(|e| {
        failed(format!(
            "acl: {p} is too open and could not be repaired: {e}"
        ))
    })?;
    g.check(path)
        .map_err(|e| failed(format!("acl: {p} is still too open after repair: {e}")))?;
    Ok(Outcome::Repaired)
}

/// The guard of this OS.
pub fn platform() -> Box<dyn Guard> {
    if cfg!(windows) {
        Box::new(DaclGuard)
    } else {
        Box::new(ModeGuard)
    }
}

/// The Windows guard: protected DACL for SYSTEM and Administrators. `restrict` needs an
/// elevated Administrator or LocalSystem (it also sets the owner).
pub struct DaclGuard;

impl Guard for DaclGuard {
    fn restrict(&self, path: &Path) -> Result<(), AclError> {
        dl_winsvc::acl::restrict(path).map_err(|e| {
            failed(format!(
                "SetNamedSecurityInfo (run as Administrator or LocalSystem): {e}"
            ))
        })
    }

    fn check(&self, path: &Path) -> Result<(), AclError> {
        let d =
            dl_winsvc::acl::read(path).map_err(|e| failed(format!("GetNamedSecurityInfo: {e}")))?;
        dl_winsvc::acl::check(&d)
            .map_err(|why| AclError::NotRestricted(format!("{}: {why}", path.display())))
    }
}

/// The non-Windows guard: 0700 for directories, 0600 for files.
pub struct ModeGuard;

impl Guard for ModeGuard {
    #[cfg(unix)]
    fn restrict(&self, path: &Path) -> Result<(), AclError> {
        use std::os::unix::fs::PermissionsExt;
        let md = std::fs::metadata(path).map_err(|e| failed(e.to_string()))?;
        let mode = if md.is_dir() { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| failed(e.to_string()))
    }

    #[cfg(unix)]
    fn check(&self, path: &Path) -> Result<(), AclError> {
        use std::os::unix::fs::PermissionsExt;
        let md = std::fs::metadata(path).map_err(|e| failed(e.to_string()))?;
        let perm = md.permissions().mode() & 0o777;
        if perm & 0o077 != 0 {
            let want = if md.is_dir() { 0o700 } else { 0o600 };
            return Err(AclError::NotRestricted(format!(
                "{} has mode {perm:04o}, want {want:04o}",
                path.display()
            )));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn restrict(&self, _: &Path) -> Result<(), AclError> {
        Ok(())
    }

    #[cfg(not(unix))]
    fn check(&self, _: &Path) -> Result<(), AclError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records calls; `open` paths fail the check until restricted.
    #[derive(Default)]
    struct Fake {
        open: Mutex<Vec<String>>,
        restricted: Mutex<Vec<String>>,
        broken: bool,
    }

    impl Guard for Fake {
        fn restrict(&self, path: &Path) -> Result<(), AclError> {
            let p = path.display().to_string();
            if !self.broken {
                self.open.lock().unwrap().retain(|x| *x != p);
            }
            self.restricted.lock().unwrap().push(p);
            Ok(())
        }
        fn check(&self, path: &Path) -> Result<(), AclError> {
            let p = path.display().to_string();
            if self.open.lock().unwrap().contains(&p) {
                return Err(AclError::NotRestricted(p));
            }
            Ok(())
        }
    }

    #[test]
    fn create_check_repair() {
        let base = std::env::temp_dir().join(format!("pms-acl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let g = Fake::default();
        let dir = base.join("state");
        assert_eq!(ensure_dir(&g, &dir).unwrap(), Outcome::Created);
        assert_eq!(ensure_dir(&g, &dir).unwrap(), Outcome::Unchanged);
        g.open.lock().unwrap().push(dir.display().to_string());
        assert_eq!(ensure_dir(&g, &dir).unwrap(), Outcome::Repaired);
        assert_eq!(
            ensure_file(&g, &base.join("missing.json")).unwrap(),
            Outcome::Unchanged
        );
        let bad = Fake {
            broken: true,
            ..Fake::default()
        };
        bad.open.lock().unwrap().push(dir.display().to_string());
        assert!(
            ensure_dir(&bad, &dir)
                .unwrap_err()
                .to_string()
                .contains("still too open")
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
