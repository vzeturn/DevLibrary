//! Legal vault continuity across a fresh enrol: the new install must open every file the
//! .NET agent sealed, so it must hold the same tenant master key (TMK).
//!
//! Nothing in the Check-In cloud hands an old key to a new agent (SmileCheckin
//! `TenantKeyEscrowService`: the escrow copy is revealed only by a platform root, by hand,
//! and `EnsureBackedUpAsync` never re-escrows a new key), so the key has to come from the
//! disk. The .NET agent ran as a service whose working directory is `%SystemRoot%\System32`
//! (Install.ps1: "a service's working directory is System32") and Install.ps1 never sets
//! `TenantKey`, `TenantKeyFile` or `VaultDirectory`, so a stock install keeps both in
//! `System32`, which the bench never renames and [`Layout::resolve`] already prefers. This
//! module covers the rest and refuses whatever it cannot prove:
//!
//! * the key each legacy install used, the way `TenantKeyStore` loaded it (`TenantKey`
//!   first, else `TenantKeyFile` against System32, else the install folder);
//! * one distinct readable key that the new layout would not find → that file alone is
//!   imported (opened, sealed again for this machine) into `<install>\data`;
//! * a key that exists but cannot be read, two different keys, or sealed vault files with
//!   no key or with another key's fingerprint → stop, create nothing;
//! * an operator-supplied key (`DL_AGENT_ARRIVAL_TENANT_KEY`, e.g. from the escrow reveal)
//!   is imported the same way.
//!
//! [`Layout::resolve`]: crate::agent::layout::Layout::resolve

use std::path::{Path, PathBuf};

use crate::agent::layout::Layout;
use crate::agent::secrets::{TENANT_KEY_ENTROPY, protect, unprotect, write_secret};
use crate::agent::settings::Settings;
use crate::contracts::key_vault::TenantKeyVault;
use crate::contracts::values::parse_base64;
use crate::legacy::installs::LegacyInstall;

/// Where an imported key is written, under the install directory.
pub const IMPORTED_KEY_FILE: &str = "data/agent-tenant-key.bin";

/// What happened to the tenant key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// The key the new settings resolve to is the old agent's (or the install's own).
    Kept,
    /// The old agent's key was copied into the install.
    Imported,
    /// The operator supplied the key.
    Supplied,
    /// No key existed anywhere: a new one is created.
    Created,
}

impl KeySource {
    /// The word written to `dl-enroll.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            KeySource::Kept => "kept",
            KeySource::Imported => "imported",
            KeySource::Supplied => "supplied",
            KeySource::Created => "created",
        }
    }
}

/// Where a key was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyOrigin {
    /// `Agent:TenantKey` in the settings of a folder.
    Configured(PathBuf),
    /// A key file.
    File(PathBuf),
    /// `DL_AGENT_ARRIVAL_TENANT_KEY`.
    Supplied,
}

impl std::fmt::Display for KeyOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyOrigin::Configured(dir) => write!(f, "Agent:TenantKey in {}", dir.display()),
            KeyOrigin::File(p) => write!(f, "{}", p.display()),
            KeyOrigin::Supplied => f.write_str("the supplied key"),
        }
    }
}

/// A key found on the disk (the key itself stays in memory, never printed).
pub struct FoundKey {
    /// Where it came from.
    pub origin: KeyOrigin,
    /// `TenantKeyVault::fingerprint`.
    pub fingerprint: String,
    key: Vec<u8>,
}

impl std::fmt::Debug for FoundKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FoundKey")
            .field("origin", &self.origin)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

/// Sealed vault files of one directory, by key fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultScan {
    /// The directory.
    pub dir: PathBuf,
    /// `(fingerprint, files)`, most files first.
    pub by_fingerprint: Vec<(String, usize)>,
    /// Files shorter than a sealed header.
    pub malformed: usize,
    /// Files seen: every file under the directory (no cap: a file left out would be a legal
    /// document nobody proved this install can open).
    pub files: usize,
}

/// The decision for the new install.
#[derive(Debug)]
pub struct KeyPlan {
    /// Kept, imported, supplied or created.
    pub source: KeySource,
    /// The key to write before the settings name it (import / supplied).
    pub import: Option<FoundKey>,
    /// The key the install will hold (`None` when one is created).
    pub fingerprint: Option<String>,
    /// Where it came from (`None` when one is created).
    pub from: Option<KeyOrigin>,
    /// `Agent:TenantKeyFile` to write (absolute), when the key is imported.
    pub tenant_key_file: Option<PathBuf>,
    /// `Agent:VaultDirectory` to write (absolute), when the old vault is not where the new
    /// settings would look.
    pub vault_directory: Option<PathBuf>,
    /// Sealed files seen in the old vault directories.
    pub vault: Vec<VaultScan>,
    /// Lines for the operator (no secrets).
    pub notes: Vec<String>,
}

/// What [`plan`] works from.
#[derive(Debug)]
pub struct PlanInput<'a> {
    /// The new install's layout (its legacy working directory is System32).
    pub layout: &'a Layout,
    /// The new install's settings as they stand before the enrol writes them.
    pub target: &'a Settings,
    /// Legacy folders, newest first, with their settings.
    pub legacy: &'a [(LegacyInstall, Settings)],
    /// `%SystemRoot%\System32`, the .NET service's working directory.
    pub system32: Option<&'a Path>,
    /// A base64 key the operator supplied.
    pub supplied: Option<&'a str>,
}

const RECOVER: &str = "Nothing was changed. Recover the old key from the Check-In key escrow (platform key-escrow reveal, root only) and pass it in DL_AGENT_ARRIVAL_TENANT_KEY, or name the folder that holds it with --legacy-dir.";

/// Decides how the new install gets the old agent's tenant key (see the module docs).
///
/// # Errors
/// Why continuity cannot be proven (paths and fingerprints only, never a key).
pub fn plan(input: &PlanInput<'_>) -> Result<KeyPlan, String> {
    let layout = input.layout;
    let target_file = layout.resolve(&input.target.tenant_key_file);
    let imported_file = layout.install_dir().join(IMPORTED_KEY_FILE);
    let current = read_target(input, &target_file)?;

    let mut used: Vec<FoundKey> = Vec::new();
    let mut vault_dirs: Vec<PathBuf> = Vec::new();
    for (install, settings) in input.legacy {
        if let Some(k) = effective_key(install, settings, input.system32)? {
            used.push(k);
        }
        let v = dotnet_path(install, &settings.vault_directory, input.system32);
        if !vault_dirs.contains(&v) {
            vault_dirs.push(v);
        }
    }
    // A stock .NET install's key and vault, even when no folder of it is left.
    if let Some(sys) = input.system32 {
        let default = sys.join(Settings::default().tenant_key_file);
        if input.legacy.is_empty()
            && let Some(k) = read_file(&default)?
        {
            used.push(k);
        }
        let v = sys.join(Settings::default().vault_directory);
        if !vault_dirs.contains(&v) {
            vault_dirs.push(v);
        }
    }
    let vault: Vec<VaultScan> = vault_dirs
        .iter()
        .filter(|d| d.is_dir())
        .map(|d| scan_vault(d))
        .collect::<Result<_, _>>()?;
    let sealed: usize = vault.iter().map(|s| s.files).sum();

    let mut notes = Vec::new();
    let chosen: Option<FoundKey> = if let Some(b64) = input.supplied {
        let k = supplied_key(b64)?;
        for other in used.iter().filter(|u| u.fingerprint != k.fingerprint) {
            notes.push(format!(
                "warning: {} holds another key ({}); the supplied key ({}) wins",
                other.origin, other.fingerprint, k.fingerprint
            ));
        }
        Some(k)
    } else {
        let mut distinct: Vec<FoundKey> = Vec::new();
        for k in used {
            if !distinct.iter().any(|d| d.fingerprint == k.fingerprint) {
                distinct.push(k);
            }
        }
        if distinct.len() > 1 {
            let list: Vec<String> = distinct
                .iter()
                .map(|d| format!("{} at {}", d.fingerprint, d.origin))
                .collect();
            return Err(format!(
                "the old agent installs hold {} different tenant keys ({}). {RECOVER}",
                distinct.len(),
                list.join("; ")
            ));
        }
        distinct.pop()
    };

    // Every sealed vault file must carry the fingerprint of the key the install keeps.
    let keep_fp = chosen
        .as_ref()
        .map(|k| k.fingerprint.clone())
        .or_else(|| current.as_ref().map(|c| c.fingerprint.clone()));
    for scan in &vault {
        let foreign: Vec<String> = scan
            .by_fingerprint
            .iter()
            .filter(|(fp, _)| Some(fp) != keep_fp.as_ref())
            .map(|(fp, n)| format!("{n} sealed with {fp}"))
            .collect();
        if !foreign.is_empty() || scan.malformed > 0 {
            let held = keep_fp.as_deref().unwrap_or("no key");
            return Err(format!(
                "the legal vault {} has files this install could not open ({}{}; the install would hold {held}). {RECOVER}",
                scan.dir.display(),
                foreign.join(", "),
                if scan.malformed > 0 {
                    format!(", {} malformed", scan.malformed)
                } else {
                    String::new()
                }
            ));
        }
    }

    let vault_directory = carry_vault(layout, input.target, &vault);
    if let Some(dir) = &vault_directory {
        notes.push(format!(
            "the old legal vault stays at {}; the new settings point there",
            dir.display()
        ));
    }

    let Some(key) = chosen else {
        return Ok(match current {
            Some(c) => KeyPlan {
                source: KeySource::Kept,
                fingerprint: Some(c.fingerprint),
                from: Some(c.origin),
                import: None,
                tenant_key_file: None,
                vault_directory,
                vault,
                notes,
            },
            None if sealed > 0 => {
                return Err(format!(
                    "{sealed} sealed vault files exist but no tenant key was found. {RECOVER}"
                ));
            }
            None => KeyPlan {
                source: KeySource::Created,
                fingerprint: None,
                from: None,
                import: None,
                tenant_key_file: None,
                vault_directory,
                vault,
                notes,
            },
        });
    };

    if let Some(c) = &current {
        if c.fingerprint == key.fingerprint {
            return Ok(KeyPlan {
                source: if matches!(key.origin, KeyOrigin::Supplied) {
                    KeySource::Supplied
                } else {
                    KeySource::Kept
                },
                fingerprint: Some(c.fingerprint.clone()),
                from: Some(c.origin.clone()),
                import: None,
                tenant_key_file: None,
                vault_directory,
                vault,
                notes,
            });
        }
        // The new settings already reach another key: never replace a key file.
        return Err(format!(
            "this install already holds tenant key {} at {}, but the old agent used {} ({}). {RECOVER}",
            c.fingerprint, c.origin, key.fingerprint, key.origin
        ));
    }
    if imported_file.exists() && imported_file != target_file {
        return Err(format!(
            "{} exists but the settings do not name it; move it aside first. {RECOVER}",
            imported_file.display()
        ));
    }
    Ok(KeyPlan {
        source: if matches!(key.origin, KeyOrigin::Supplied) {
            KeySource::Supplied
        } else {
            KeySource::Imported
        },
        fingerprint: Some(key.fingerprint.clone()),
        from: Some(key.origin.clone()),
        import: Some(key),
        tenant_key_file: Some(imported_file),
        vault_directory,
        vault,
        notes,
    })
}

/// Writes the key of a plan that imports one (sealed for this machine, owner-only), then
/// reads it back; the caller writes the settings only after this.
///
/// # Errors
/// The write or the read-back failed, or the read-back has another fingerprint.
pub fn import(plan: &KeyPlan) -> Result<(), String> {
    let (Some(key), Some(file)) = (&plan.import, &plan.tenant_key_file) else {
        return Ok(());
    };
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let sealed = protect(&key.key, TENANT_KEY_ENTROPY).map_err(|e| e.to_string())?;
    let mut tmp = file.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    write_secret(&tmp, &sealed, "tenant key").map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, file).map_err(|e| format!("replace {}: {e}", file.display()))?;
    match read_file(file)? {
        Some(back) if back.fingerprint == key.fingerprint => Ok(()),
        _ => Err(format!(
            "the imported key at {} does not read back as {}",
            file.display(),
            key.fingerprint
        )),
    }
}

/// The key the target settings reach now, if any.
fn read_target(input: &PlanInput<'_>, file: &Path) -> Result<Option<FoundKey>, String> {
    let t = input.target;
    if !t.tenant_key.trim().is_empty() {
        let dir = input.layout.install_dir().to_path_buf();
        return configured(&t.tenant_key, KeyOrigin::Configured(dir)).map(Some);
    }
    read_file(file)
}

/// The key the .NET agent of `install` used, the way `TenantKeyStore` loaded it.
fn effective_key(
    install: &LegacyInstall,
    settings: &Settings,
    system32: Option<&Path>,
) -> Result<Option<FoundKey>, String> {
    if !settings.tenant_key.trim().is_empty() {
        let origin = KeyOrigin::Configured(install.dir.clone());
        return configured(&settings.tenant_key, origin).map(Some);
    }
    let file = Path::new(&settings.tenant_key_file);
    if file.is_absolute() {
        return read_file(&install.relocate(file));
    }
    // Relative: the service's working directory first; the install folder covers an agent
    // run from its own folder.
    let mut tries: Vec<PathBuf> = system32.iter().map(|s| s.join(file)).collect();
    tries.push(install.dir.join(file));
    for p in tries {
        if let Some(k) = read_file(&p)? {
            return Ok(Some(k));
        }
    }
    Ok(None)
}

/// A directory setting of the .NET agent as it resolved it.
fn dotnet_path(install: &LegacyInstall, configured: &str, system32: Option<&Path>) -> PathBuf {
    let p = Path::new(configured);
    if p.is_absolute() {
        return install.relocate(p);
    }
    match system32 {
        Some(s) => s.join(p),
        None => install.dir.join(p),
    }
}

/// `VaultDirectory` for the new settings: the old vault holding files, when the new
/// settings would look elsewhere.
fn carry_vault(layout: &Layout, target: &Settings, vault: &[VaultScan]) -> Option<PathBuf> {
    let would = layout.resolve(&target.vault_directory);
    vault
        .iter()
        .find(|s| s.files > 0)
        .map(|s| s.dir.clone())
        .filter(|d| *d != would)
}

fn configured(b64: &str, origin: KeyOrigin) -> Result<FoundKey, String> {
    let key =
        parse_base64(b64.trim()).ok_or_else(|| format!("{origin} is not base64. {RECOVER}"))?;
    let fingerprint = TenantKeyVault::new(&key)
        .map_err(|e| format!("{origin}: {e} {RECOVER}"))?
        .fingerprint();
    Ok(FoundKey {
        origin,
        fingerprint,
        key,
    })
}

fn supplied_key(b64: &str) -> Result<FoundKey, String> {
    configured(b64, KeyOrigin::Supplied)
}

/// A key file opened for this machine; `None` when there is no file.
fn read_file(path: &Path) -> Result<Option<FoundKey>, String> {
    let blob = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}. {RECOVER}", path.display())),
    };
    let key = unprotect(&blob, TENANT_KEY_ENTROPY).map_err(|e| {
        format!(
            "{} cannot be opened on this machine ({e}). {RECOVER}",
            path.display()
        )
    })?;
    let fingerprint = TenantKeyVault::new(&key)
        .map_err(|e| format!("{}: {e} {RECOVER}", path.display()))?
        .fingerprint();
    Ok(Some(FoundKey {
        origin: KeyOrigin::File(path.to_path_buf()),
        fingerprint,
        key,
    }))
}

/// Counts the sealed files of a vault directory by the key fingerprint in their header: every
/// file, read one at a time and only its header (36 bytes), so memory stays flat however many
/// there are.
///
/// # Errors
/// A directory or file that cannot be read.
pub fn scan_vault(dir: &Path) -> Result<VaultScan, String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    let (mut files, mut malformed) = (0usize, 0usize);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = std::fs::read_dir(&d).map_err(|e| format!("read {}: {e}", d.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("read {}: {e}", d.display()))?;
            let path = entry.path();
            let kind = entry
                .file_type()
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            if kind.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_some_and(|x| x == "tmp") {
                continue;
            }
            files += 1;
            match header(&path)? {
                Some(fp) => match counts.iter_mut().find(|(f, _)| *f == fp) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((fp, 1)),
                },
                None => malformed += 1,
            }
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(VaultScan {
        dir: dir.to_path_buf(),
        by_fingerprint: counts,
        malformed,
        files,
    })
}

/// The fingerprint at the head of a sealed file (`None` when it is too short to be one).
fn header(path: &Path) -> Result<Option<String>, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut head = [0u8; 8 + 12 + 16];
    let mut got = 0;
    while got < head.len() {
        let n = f
            .read(&mut head[got..])
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        got += n;
    }
    Ok((got == head.len()).then(|| crate::crypto::hex(&head[..8])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scan_reads_every_file_past_ten_thousand() {
        let dir = std::env::temp_dir().join(format!(
            "arr-vault-scan-{}-{:016x}",
            std::process::id(),
            dl_rand::u64()
        ));
        let mut head = [0u8; 8 + 12 + 16];
        head[..8].copy_from_slice(&[0xAA; 8]);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..10_000 {
            std::fs::write(dir.join(format!("{i:05}")), head).unwrap();
        }
        // Read last: the scan takes a folder's files before its subfolders.
        let deep = dir.join("z").join("guests");
        std::fs::create_dir_all(&deep).unwrap();
        head[..8].copy_from_slice(&[0xBB; 8]);
        std::fs::write(deep.join("late"), head).unwrap();
        std::fs::write(deep.join("short"), b"x").unwrap();
        let scan = scan_vault(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(scan.files, 10_002);
        assert_eq!(scan.malformed, 1);
        assert_eq!(
            scan.by_fingerprint,
            vec![
                (crate::crypto::hex(&[0xAA; 8]), 10_000),
                (crate::crypto::hex(&[0xBB; 8]), 1)
            ]
        );
    }
}
