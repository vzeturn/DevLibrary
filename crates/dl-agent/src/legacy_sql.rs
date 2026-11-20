//! The SQL logins the agents this one replaces already use, so a machine setup reuses one
//! instead of asking (RT-0303).
//!
//! Each product module knows where its old agent kept its connection and how it was
//! protected; it registers a [`Source`] with the app (`App::sql_source`). The installer gathers
//! every source's [`Candidate`]s, in the order the sources were registered, and tests them one
//! by one with `sql-setup` until one signs in to every SMILE database.
//!
//! An old agent's folder may still be in place, renamed by the hub bench's `remove_legacy`
//! (`<dir>.bench-old-<yyyyMMddHHmmss>`), or moved to a folder the bench recorded in
//! `%ProgramData%\dl-agent\bench-removed-*.json`; [`copies`] lists all three.

use std::path::{Path, PathBuf};

use crate::cli_install_sql::Secret;
use crate::sql::secret::AuthMode;

/// The suffix the bench gives a renamed folder (`<dir>.bench-old-<stamp>`).
pub const RENAMED_MARK: &str = ".bench-old-";
/// The bench's record of what it removed, under `%ProgramData%\dl-agent`.
pub const BENCH_RECORD_PREFIX: &str = "bench-removed-";

/// One SQL login an old agent used.
pub struct Candidate {
    /// Where it came from, in words with the file (`the old PMS site agent (C:\…\config.json)`).
    /// Shown and saved as `login_source`; never a secret.
    pub source: String,
    /// Windows or SQL login.
    pub auth: AuthMode,
    /// `host`, `host\instance` or `host,port`.
    pub server: String,
    /// SQL login name (empty for Windows).
    pub user: String,
    /// SQL login password (empty for Windows).
    pub password: Secret,
}

impl std::fmt::Debug for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.describe())
    }
}

impl Candidate {
    /// `SQL login smile on srv\SMILE (the old … agent (path))`, never the password.
    pub fn describe(&self) -> String {
        match self.auth {
            AuthMode::Windows => format!("Windows login on {} ({})", self.server, self.source),
            AuthMode::Sql => format!(
                "SQL login {} on {} ({})",
                self.user, self.server, self.source
            ),
        }
    }

    fn key(&self) -> (bool, String, String) {
        let server = normal_server(&self.server);
        match self.auth {
            AuthMode::Windows => (true, server, String::new()),
            AuthMode::Sql => (false, server, self.user.to_lowercase()),
        }
    }
}

/// `.`, `(local)`, `localhost`, `127.0.0.1` and the machine name are the same host.
fn normal_server(server: &str) -> String {
    let s = server.trim().to_lowercase();
    let (host, rest) = match s.find(['\\', ',']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s.as_str(), ""),
    };
    let me = std::env::var("COMPUTERNAME")
        .unwrap_or_default()
        .to_lowercase();
    let host = match host.trim() {
        "" | "." | "(local)" | "localhost" | "127.0.0.1" | "tcp:." => ".",
        h if !me.is_empty() && h == me => ".",
        h => h,
    };
    let rest = if rest == ",1433" { "" } else { rest };
    format!("{host}{rest}")
}

/// A module's finder: the candidates its old agent left on this machine. It reports what it
/// could not read on stderr (a warning) and never fails the setup.
pub type Source = fn() -> Vec<Candidate>;

/// A candidate from an ADO.NET connection string (`Server=…;User ID=…;Password=…`).
pub fn from_connection_string(conn: &str, source: &str) -> Option<Candidate> {
    let s = crate::cli_install_sql::legacy_saved(conn, source)?;
    let auth = AuthMode::parse(&s.auth).ok()?;
    if auth == AuthMode::Sql && s.user.trim().is_empty() {
        return None;
    }
    Some(Candidate {
        source: source.to_string(),
        auth,
        server: s.server,
        user: s.user,
        password: Secret::new(s.password.unwrap_or_default()),
    })
}

/// Every candidate from `sources`, in order, without repeats: the same login on the same
/// server is tried once (the first one found wins; a second password for it is tried too).
pub fn gather(found: Vec<Candidate>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for c in found {
        let same = out
            .iter()
            .any(|o| o.key() == c.key() && o.password.expose() == c.password.expose());
        if !same && !c.server.trim().is_empty() {
            out.push(c);
        }
    }
    out
}

/// Where the folder `dir` is now: in place, renamed by the bench (newest first), or moved to
/// a folder a bench record lists for it. Folders that do not exist are left out.
pub fn copies(dir: &Path) -> Vec<PathBuf> {
    let program_data = std::env::var("ProgramData").unwrap_or_default();
    let records = (!program_data.trim().is_empty())
        .then(|| PathBuf::from(program_data.trim()).join("dl-agent"));
    copies_in(dir, records.as_deref())
}

fn key(p: &Path) -> String {
    p.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

/// [`copies`] with the bench records' folder given (tests).
pub fn copies_in(dir: &Path, records: Option<&Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if dir.is_dir() {
        out.push(dir.to_path_buf());
    }
    let mut renamed: Vec<(String, PathBuf)> = Vec::new();
    if let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) {
        let prefix = format!("{}{RENAMED_MARK}", name.to_string_lossy()).to_lowercase();
        if let Ok(entries) = std::fs::read_dir(parent) {
            for e in entries.flatten() {
                let n = e.file_name().to_string_lossy().to_lowercase();
                if let Some(stamp) = n.strip_prefix(&prefix)
                    && e.path().is_dir()
                {
                    renamed.push((stamp.to_string(), e.path()));
                }
            }
        }
    }
    if let Some(records) = records {
        for (from, to) in bench_moves(records) {
            if key(&from) == key(dir) && to.is_dir() {
                let stamp = to
                    .file_name()
                    .map(|n| n.to_string_lossy().to_lowercase())
                    .and_then(|n| {
                        n.find(RENAMED_MARK)
                            .map(|i| n[i + RENAMED_MARK.len()..].to_string())
                    })
                    .unwrap_or_default();
                renamed.push((stamp, to));
            }
        }
    }
    renamed.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, p) in renamed {
        if !out.iter().any(|o| key(o) == key(&p)) {
            out.push(p);
        }
    }
    out
}

/// Every `{from, to}` of the bench records in `dir`; an unreadable record is a warning.
fn bench_moves(dir: &Path) -> Vec<(PathBuf, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_lowercase();
        if !(name.starts_with(BENCH_RECORD_PREFIX) && name.ends_with(".json")) {
            continue;
        }
        let doc = match crate::store::read_json(&e.path()) {
            Ok(Some(d)) => d,
            Ok(None) => continue,
            Err(err) => {
                eprintln!(
                    "Warning: bench record {} unreadable: {err}",
                    e.path().display()
                );
                continue;
            }
        };
        let dirs = doc.get("dirs").and_then(dl_json::Value::as_array);
        for d in dirs.into_iter().flatten() {
            let get = |k: &str| d.get(k).and_then(dl_json::Value::as_str).map(PathBuf::from);
            if let (Some(from), Some(to)) = (get("from"), get("to")) {
                out.push((from, to));
            }
        }
    }
    out
}

/// `%ProgramData%\<rel>` and `%ProgramFiles%\<rel>` (both 64- and 32-bit), without repeats.
pub fn machine_dirs(program_data: &[&str], program_files: &[&str]) -> Vec<PathBuf> {
    let env = |k: &str| {
        std::env::var(k)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let mut out: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        if !out.iter().any(|o| key(o) == key(&p)) {
            out.push(p);
        }
    };
    if let Some(pd) = env("ProgramData") {
        for rel in program_data {
            add(PathBuf::from(&pd).join(rel));
        }
    }
    for base in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
        .into_iter()
        .filter_map(env)
    {
        for rel in program_files {
            add(PathBuf::from(&base).join(rel));
        }
    }
    out
}

/// Every copy of every folder in `dirs` that holds `file`: the files to read, newest first.
pub fn files(dirs: &[PathBuf], file: &str) -> Vec<PathBuf> {
    dirs.iter()
        .flat_map(|d| copies(d))
        .map(|d| d.join(file))
        .filter(|f| f.is_file())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(auth: AuthMode, server: &str, user: &str, pw: &str) -> Candidate {
        Candidate {
            source: "t".into(),
            auth,
            server: server.into(),
            user: user.into(),
            password: Secret::new(pw.into()),
        }
    }

    #[test]
    fn the_same_login_on_the_same_server_is_tried_once() {
        let got = gather(vec![
            cand(AuthMode::Sql, "(local)", "SA", "a"),
            cand(AuthMode::Sql, ".", "sa", "a"),
            cand(AuthMode::Sql, "localhost,1433", "sa", "b"),
            cand(AuthMode::Windows, ".", "", ""),
            cand(AuthMode::Windows, "127.0.0.1", "", ""),
            cand(AuthMode::Windows, ".\\SMILE", "", ""),
            cand(AuthMode::Sql, " ", "sa", "a"),
        ]);
        let shown: Vec<_> = got
            .iter()
            .map(|c| (c.server.as_str(), c.password.expose()))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("(local)", "a"),
                ("localhost,1433", "b"),
                (".", ""),
                (".\\SMILE", "")
            ]
        );
    }

    #[test]
    fn a_candidate_never_shows_its_password() {
        let c = from_connection_string(
            "Server=srv\\SMILE;User ID=smile;Password=hunter2;Database=SMILE_FO",
            "the old PMS site agent (x)",
        )
        .unwrap();
        assert_eq!(
            (c.auth, c.server.as_str(), c.user.as_str()),
            (AuthMode::Sql, "srv\\SMILE", "smile")
        );
        assert_eq!(c.password.expose(), "hunter2");
        let shown = format!("{c:?} {}", c.describe());
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("the old PMS site agent"), "{shown}");
    }

    #[test]
    fn a_folder_is_found_in_place_renamed_and_where_the_bench_moved_it() {
        let d = crate::store::testdir::TestDir::new("legacy-sql-copies");
        let base = d.0.join("ProgramData");
        let agent = base.join("SmilePMS");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::create_dir_all(base.join("SmilePMS.bench-old-20261101000000")).unwrap();
        std::fs::create_dir_all(base.join("smilepms.bench-old-20261119000000")).unwrap();
        std::fs::create_dir_all(base.join("SmilePMSX")).unwrap();
        let moved = d.0.join("elsewhere").join("pms.bench-old-20261120000000");
        std::fs::create_dir_all(&moved).unwrap();
        let records = d.0.join("dl-agent");
        std::fs::create_dir_all(&records).unwrap();
        let rec = format!(
            r#"{{"removed_at":"x","services":[],"tasks":[],"dirs":[{{"from":"{}","to":"{}"}}]}}"#,
            agent.display().to_string().replace('\\', "\\\\"),
            moved.display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(records.join("bench-removed-20261120000000.json"), rec).unwrap();
        std::fs::write(records.join("bench-removed-bad.json"), "{").unwrap();
        let got: Vec<String> = copies_in(&agent, Some(&records))
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            got,
            vec![
                "SmilePMS",
                "pms.bench-old-20261120000000",
                "smilepms.bench-old-20261119000000",
                "SmilePMS.bench-old-20261101000000",
            ]
        );
        std::fs::remove_dir_all(&agent).unwrap();
        assert_eq!(copies_in(&agent, None).len(), 2);
    }
}
