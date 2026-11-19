//! Where the .NET agent (`MM.Arrival.Agent`) was installed on this machine, before or after
//! the bench removed it.
//!
//! A pilot site is switched by removing the old agents (the hub bench's `remove_legacy`:
//! stop, delete the service, rename the folder to `<dir>.bench-old-<yyyyMMddHHmmss>`, never
//! delete) and enrolling dl-agent afresh. The tenant master key and the legal vault of the
//! old agent stay on the disk; this module finds the folders that may say where:
//!
//! * `<Program Files>\MM-Arrival Agent` (still installed),
//! * `<Program Files>\MM-Arrival Agent.bench-old-*` (renamed by the bench),
//! * every `dirs[].to` of `%ProgramData%\dl-agent\bench-removed-*.json` that held the .NET
//!   agent (an install in a folder of the operator's choosing),
//! * a folder the operator names (`--legacy-dir`).
//!
//! Newest first: the folder still in place, then by the rename stamp. Only paths and file
//! names are read here; the settings are read by [`LegacyInstall::settings`].

use std::path::{Path, PathBuf};

use crate::agent::layout::FOLDER;
use crate::agent::settings::Settings;
use crate::stj;

/// The suffix the bench gives a renamed folder (`<dir>.bench-old-<stamp>`).
pub const RENAMED_MARK: &str = ".bench-old-";
/// The bench's record of what it removed, under `%ProgramData%\dl-agent`.
pub const BENCH_RECORD_PREFIX: &str = "bench-removed-";
/// Files that mark a folder as a .NET agent install.
const DOTNET_FILES: &[&str] = &["MM.Arrival.Agent.exe", "MM.Arrival.Agent.dll"];

/// How a legacy folder was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// Named by the operator (`--legacy-dir`).
    Named,
    /// `MM-Arrival Agent` under Program Files, not removed.
    InPlace,
    /// A `.bench-old-*` sibling of the default folder.
    Renamed,
    /// A folder listed in a bench removal record.
    Recorded,
}

impl Found {
    /// The word written to `dl-enroll.json` and printed.
    pub fn as_str(self) -> &'static str {
        match self {
            Found::Named => "named",
            Found::InPlace => "in_place",
            Found::Renamed => "renamed",
            Found::Recorded => "bench_record",
        }
    }
}

/// One folder that held (or holds) the .NET agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyInstall {
    /// Where the folder is now.
    pub dir: PathBuf,
    /// Where it was while the .NET agent ran (absolute paths in its settings start here).
    pub original: PathBuf,
    /// How it was found.
    pub found: Found,
    /// The rename stamp (`yyyyMMddHHmmss`), empty for a folder in place.
    pub stamp: String,
}

impl LegacyInstall {
    /// The .NET agent's settings in this folder (`appsettings.json` +
    /// `appsettings.Production.json`).
    ///
    /// # Errors
    /// Why they cannot be read (never a value).
    pub fn settings(&self) -> Result<Settings, String> {
        Settings::load(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))
    }

    /// An absolute path the .NET agent used, moved to where the folder is now when it lay
    /// inside the folder (case-insensitive, either separator, as Windows compares).
    pub fn relocate(&self, used: &Path) -> PathBuf {
        if self.dir == self.original {
            return used.to_path_buf();
        }
        let (u, o) = (components(used), components(&self.original));
        let inside = u.len() >= o.len()
            && u.iter()
                .zip(&o)
                .all(|(a, b)| a.to_lowercase() == b.to_lowercase());
        if !inside {
            return used.to_path_buf();
        }
        u[o.len()..].iter().fold(self.dir.clone(), |p, c| p.join(c))
    }
}

/// The parts of a path split on either separator.
fn components(p: &Path) -> Vec<String> {
    p.to_string_lossy()
        .split(['/', '\\'])
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .collect()
}

/// Every legacy folder on this machine, newest first; `named` (when given) comes first and
/// alone decides when the operator passes it.
///
/// # Errors
/// A bench record that exists but cannot be read or parsed (the folders it lists would be
/// missed, so the caller stops instead).
pub fn discover(
    getenv: &dyn Fn(&str) -> String,
    named: Option<&Path>,
) -> Result<Vec<LegacyInstall>, String> {
    if let Some(dir) = named {
        return Ok(vec![LegacyInstall {
            dir: dir.to_path_buf(),
            original: original_of(dir),
            found: Found::Named,
            stamp: stamp_of(dir),
        }]);
    }
    let mut out: Vec<LegacyInstall> = Vec::new();
    for base in program_dirs(getenv) {
        let default = base.join(FOLDER);
        if default.is_dir() {
            push(&mut out, default.clone(), default.clone(), Found::InPlace);
        }
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        let prefix = format!("{FOLDER}{RENAMED_MARK}").to_lowercase();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if name.starts_with(&prefix) && entry.path().is_dir() {
                push(&mut out, entry.path(), default.clone(), Found::Renamed);
            }
        }
    }
    let program_data = getenv("ProgramData");
    if !program_data.trim().is_empty() {
        for (from, to) in bench_records(&PathBuf::from(program_data.trim()).join("dl-agent"))? {
            let named_like = from
                .file_name()
                .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(FOLDER));
            let holds_agent = DOTNET_FILES.iter().any(|f| to.join(f).is_file());
            if to.is_dir() && (named_like || holds_agent) {
                push(&mut out, to, from, Found::Recorded);
            }
        }
    }
    out.sort_by(|a, b| {
        let rank = |i: &LegacyInstall| u8::from(i.found != Found::InPlace);
        rank(a).cmp(&rank(b)).then_with(|| b.stamp.cmp(&a.stamp))
    });
    Ok(out)
}

fn push(out: &mut Vec<LegacyInstall>, dir: PathBuf, original: PathBuf, found: Found) {
    let key = |p: &Path| p.to_string_lossy().replace('\\', "/").to_lowercase();
    if out.iter().any(|i| key(&i.dir) == key(&dir)) {
        return;
    }
    let stamp = stamp_of(&dir);
    out.push(LegacyInstall {
        dir,
        original,
        found,
        stamp,
    });
}

/// `ProgramW6432`, `ProgramFiles`, `ProgramFiles(x86)`, without repeats.
fn program_dirs(getenv: &dyn Fn(&str) -> String) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for key in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
        let v = getenv(key);
        let v = v.trim();
        if v.is_empty() {
            continue;
        }
        let p = PathBuf::from(v);
        if !out.iter().any(|o| {
            o.to_string_lossy()
                .eq_ignore_ascii_case(&p.to_string_lossy())
        }) {
            out.push(p);
        }
    }
    out
}

/// `<dir>` without its `.bench-old-<stamp>` suffix.
fn original_of(dir: &Path) -> PathBuf {
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned());
    match name.as_deref().and_then(|n| {
        n.to_lowercase()
            .find(RENAMED_MARK)
            .map(|at| n[..at].to_string())
    }) {
        Some(base) => dir.with_file_name(base),
        None => dir.to_path_buf(),
    }
}

/// The stamp after `.bench-old-`, empty when there is none.
fn stamp_of(dir: &Path) -> String {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name.find(RENAMED_MARK)
        .map(|at| name[at + RENAMED_MARK.len()..].to_string())
        .unwrap_or_default()
}

/// `(from, to)` of every folder the bench renamed, from its records in `dir`.
fn bench_records(dir: &Path) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if !(name.starts_with(BENCH_RECORD_PREFIX) && name.ends_with(".json")) {
            continue;
        }
        let path = entry.path();
        let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let text = String::from_utf8_lossy(&bytes);
        let text = text.trim_start_matches('\u{feff}');
        let v = stj::parse(text).map_err(|_| format!("{} is not valid JSON", path.display()))?;
        let dirs = v
            .as_object()
            .and_then(|m| m.get("dirs"))
            .and_then(dl_json::Value::as_array);
        for d in dirs.into_iter().flatten() {
            let field = |k: &str| {
                d.as_object()
                    .and_then(|m| m.get(k))
                    .and_then(dl_json::Value::as_str)
                    .map(PathBuf::from)
            };
            if let (Some(from), Some(to)) = (field("from"), field("to")) {
                out.push((from, to));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn temp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("dl-arrival-installs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn finds_in_place_renamed_and_recorded_folders_newest_first() {
        let root = temp("discover");
        let pf = root.join("Program Files");
        let pd = root.join("ProgramData");
        std::fs::create_dir_all(pf.join("MM-Arrival Agent.bench-old-20261101090000")).unwrap();
        std::fs::create_dir_all(pf.join("MM-Arrival Agent.bench-old-20261119080000")).unwrap();
        std::fs::create_dir_all(pf.join("Other.bench-old-20261119080000")).unwrap();
        let custom = root.join("D").join("Checkin.bench-old-20261110000000");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(custom.join("MM.Arrival.Agent.exe"), b"").unwrap();
        std::fs::create_dir_all(pd.join("dl-agent")).unwrap();
        let record = format!(
            r#"{{"removed_at":"x","services":[],"tasks":[],"dirs":[{{"from":{},"to":{}}},{{"from":"C:/Nope","to":"C:/Nope.bench-old-1"}}]}}"#,
            dl_json::to_string(&dl_json::Value::from(
                root.join("D").join("Checkin").to_string_lossy().as_ref()
            )),
            dl_json::to_string(&dl_json::Value::from(custom.to_string_lossy().as_ref())),
        );
        std::fs::write(
            pd.join("dl-agent/bench-removed-20261110000000.json"),
            record,
        )
        .unwrap();
        let env: HashMap<&str, String> = [
            ("ProgramW6432", pf.to_string_lossy().into_owned()),
            ("ProgramFiles", pf.to_string_lossy().into_owned()),
            ("ProgramData", pd.to_string_lossy().into_owned()),
        ]
        .into_iter()
        .collect();
        let getenv = |k: &str| env.get(k).cloned().unwrap_or_default();

        let found = discover(&getenv, None).unwrap();
        let names: Vec<(String, Found)> = found
            .iter()
            .map(|i| {
                (
                    i.dir.file_name().unwrap().to_string_lossy().into_owned(),
                    i.found,
                )
            })
            .collect();
        assert_eq!(
            names,
            vec![
                (
                    "MM-Arrival Agent.bench-old-20261119080000".into(),
                    Found::Renamed
                ),
                ("Checkin.bench-old-20261110000000".into(), Found::Recorded),
                (
                    "MM-Arrival Agent.bench-old-20261101090000".into(),
                    Found::Renamed
                ),
            ]
        );
        assert_eq!(found[0].original, pf.join("MM-Arrival Agent"));

        std::fs::create_dir_all(pf.join("MM-Arrival Agent")).unwrap();
        let found = discover(&getenv, None).unwrap();
        assert_eq!(found[0].found, Found::InPlace, "the folder in place leads");

        std::fs::write(pd.join("dl-agent/bench-removed-bad.json"), b"{").unwrap();
        assert!(discover(&getenv, None).is_err(), "a broken record stops");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn relocates_paths_inside_the_renamed_folder_only() {
        let i = LegacyInstall {
            dir: PathBuf::from("/pf/MM-Arrival Agent.bench-old-1"),
            original: PathBuf::from("C:\\Program Files\\MM-Arrival Agent"),
            found: Found::Renamed,
            stamp: "1".into(),
        };
        assert_eq!(
            i.relocate(Path::new(
                "c:\\program files\\mm-arrival agent\\data\\Key.bin"
            )),
            PathBuf::from("/pf/MM-Arrival Agent.bench-old-1/data/Key.bin")
        );
        assert_eq!(
            i.relocate(Path::new("C:\\Program Files\\MM-Arrival Agent")),
            i.dir
        );
        let elsewhere = Path::new("C:\\Program Files\\MM-Arrival Agent2\\k.bin");
        assert_eq!(i.relocate(elsewhere), elsewhere);
        let named = discover(&|_| String::new(), Some(&i.dir)).unwrap();
        assert_eq!(named[0].original, PathBuf::from("/pf/MM-Arrival Agent"));
        assert_eq!(named[0].stamp, "1");
    }
}
