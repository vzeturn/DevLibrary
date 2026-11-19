//! Where the .NET agent is installed and how its relative paths resolve.
//!
//! The module reads the existing install in place (settings, identity, tenant key, vault,
//! journal), so a site switches over without enrolling again. The install directory is
//! `modules.arrival.install_dir` when set; on Windows otherwise
//! `%ProgramW6432%\MM-Arrival Agent`, or the one other `MM-Arrival Agent` folder under
//! Program Files that holds `data\agent-identity.bin` (what Install.ps1 adopts); elsewhere
//! the module's own data directory.
//!
//! A relative path in the settings resolved against the service's working directory,
//! `%SystemRoot%\System32`, under .NET: an existing file there is used, then the same path
//! under the install directory; new files go under the install directory.

use std::path::{Path, PathBuf};

/// The folder Install.ps1 creates under Program Files.
pub const FOLDER: &str = "MM-Arrival Agent";
/// The identity file Install.ps1 points `IdentityFile` at, relative to the install directory.
pub const IDENTITY_IN_INSTALL: &str = "data/agent-identity.bin";

/// The install directory and the legacy working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    install_dir: PathBuf,
    legacy_cwd: Option<PathBuf>,
}

impl Layout {
    /// A layout over `install_dir`, with `legacy_cwd` tried first for relative paths.
    pub fn new(install_dir: PathBuf, legacy_cwd: Option<PathBuf>) -> Layout {
        Layout {
            install_dir,
            legacy_cwd,
        }
    }

    /// Finds the install: `configured` when given, else the platform default (see the
    /// module docs), else `fallback`.
    pub fn detect(configured: Option<&Path>, fallback: &Path) -> Layout {
        let legacy_cwd = std::env::var_os("SystemRoot").map(|r| PathBuf::from(r).join("System32"));
        if let Some(dir) = configured {
            return Layout::new(dir.to_path_buf(), legacy_cwd);
        }
        if cfg!(windows) {
            let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
            let primary = env("ProgramW6432").or_else(|| env("ProgramFiles"));
            let others: Vec<PathBuf> = [env("ProgramFiles"), env("ProgramFiles(x86)")]
                .into_iter()
                .flatten()
                .map(|p| p.join(FOLDER))
                .collect();
            if let Some(primary) = primary.map(|p| p.join(FOLDER)) {
                return Layout::new(choose(&primary, &others), legacy_cwd);
            }
        }
        Layout::new(fallback.to_path_buf(), None)
    }

    /// The install directory.
    pub fn install_dir(&self) -> &Path {
        &self.install_dir
    }

    /// Resolves a configured path for reading: absolute as is; relative under the legacy
    /// working directory when it exists there, else under the install directory.
    pub fn resolve(&self, configured: &str) -> PathBuf {
        let p = Path::new(configured);
        if p.is_absolute() {
            return p.to_path_buf();
        }
        if let Some(cwd) = &self.legacy_cwd {
            let legacy = cwd.join(p);
            if legacy.exists() {
                return legacy;
            }
        }
        self.install_dir.join(p)
    }
}

/// Install.ps1's choice: the 64-bit folder, unless it has no identity and exactly one other
/// folder does.
fn choose(primary: &Path, others: &[PathBuf]) -> PathBuf {
    let has_identity = |d: &Path| d.join(IDENTITY_IN_INSTALL).is_file();
    if has_identity(primary) {
        return primary.to_path_buf();
    }
    let with: Vec<&PathBuf> = others
        .iter()
        .filter(|d| d.as_path() != primary && has_identity(d))
        .collect();
    match with.as_slice() {
        [only] => (*only).clone(),
        _ => primary.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("dl-arrival-layout-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn relative_paths_prefer_the_legacy_working_directory_when_the_file_is_there() {
        let root = temp("resolve");
        let (install, sys32) = (root.join("install"), root.join("System32"));
        std::fs::create_dir_all(&sys32).unwrap();
        let layout = Layout::new(install.clone(), Some(sys32.clone()));
        assert_eq!(
            layout.resolve("agent-tenant-key.bin"),
            install.join("agent-tenant-key.bin")
        );
        std::fs::write(sys32.join("agent-tenant-key.bin"), b"k").unwrap();
        assert_eq!(
            layout.resolve("agent-tenant-key.bin"),
            sys32.join("agent-tenant-key.bin")
        );
        let abs = root.join("abs.bin");
        assert_eq!(layout.resolve(abs.to_str().unwrap()), abs);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn adopts_the_single_other_install_with_an_identity() {
        let root = temp("choose");
        let (primary, x86, other) = (root.join("p"), root.join("x86"), root.join("o"));
        assert_eq!(choose(&primary, &[x86.clone(), other.clone()]), primary);
        std::fs::create_dir_all(x86.join("data")).unwrap();
        std::fs::write(x86.join(IDENTITY_IN_INSTALL), b"id").unwrap();
        assert_eq!(choose(&primary, &[x86.clone(), other.clone()]), x86);
        std::fs::create_dir_all(other.join("data")).unwrap();
        std::fs::write(other.join(IDENTITY_IN_INSTALL), b"id").unwrap();
        assert_eq!(choose(&primary, &[x86.clone(), other.clone()]), primary);
        std::fs::create_dir_all(primary.join("data")).unwrap();
        std::fs::write(primary.join(IDENTITY_IN_INSTALL), b"id").unwrap();
        assert_eq!(choose(&primary, &[x86, other]), primary);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
