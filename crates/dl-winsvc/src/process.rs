//! The running processes, read in one `NtQuerySystemInformation(SystemProcessInformation)`
//! call: what `tasklist` prints, without starting a program or parsing its localised output.

use crate::{Error, sys};

/// One running process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    /// Process id.
    pub pid: u32,
    /// The process that started it (it may have exited since and its id been reused).
    pub parent_pid: u32,
    /// Terminal Services session: services run in session 0.
    pub session_id: u32,
    /// Image file name without its folder, such as `MM.Arrival.Agent.exe`; empty for the idle
    /// process.
    pub name: String,
}

impl Process {
    /// Whether this process runs the image `name`: ASCII case ignored, `.exe` optional.
    pub fn is(&self, name: &str) -> bool {
        let want = stem(name);
        !want.is_empty() && stem(&self.name).eq_ignore_ascii_case(want)
    }
}

/// `name` without a trailing `.exe` in any case.
fn stem(name: &str) -> &str {
    match name.len().checked_sub(4) {
        Some(i) if name.is_char_boundary(i) && name[i..].eq_ignore_ascii_case(".exe") => &name[..i],
        _ => name,
    }
}

/// Every running process. Off Windows it fails with [`Error::Unsupported`].
pub fn list() -> Result<Vec<Process>, Error> {
    sys::processes()
}

/// The running processes whose image is `name` (see [`Process::is`]).
pub fn named(name: &str) -> Result<Vec<Process>, Error> {
    Ok(list()?.into_iter().filter(|p| p.is(name)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str) -> Process {
        Process {
            pid: 7,
            parent_pid: 4,
            session_id: 0,
            name: name.into(),
        }
    }

    #[test]
    fn image_names_match_without_case_or_exe() {
        let agent = p("MM.Arrival.Agent.EXE");
        for name in [
            "MM.Arrival.Agent.exe",
            "mm.arrival.agent",
            "MM.ARRIVAL.AGENT.Exe",
        ] {
            assert!(agent.is(name), "{name}");
        }
        for name in ["MM.Arrival", "Agent.exe", "", ".exe"] {
            assert!(!agent.is(name), "{name}");
        }
        assert!(!p("").is(""));
        assert!(p("System").is("system"));
        assert!(!p("System").is("system.exe.exe"));
    }

    #[cfg(not(windows))]
    #[test]
    fn list_is_unsupported_off_windows() {
        assert!(matches!(list(), Err(Error::Unsupported)));
        assert!(matches!(named("x.exe"), Err(Error::Unsupported)));
    }

    #[cfg(windows)]
    #[test]
    fn list_holds_this_test_process() {
        let me = std::process::id();
        let all = list().unwrap();
        let mine = all.iter().find(|p| p.pid == me).expect("this process");
        assert!(mine.name.to_ascii_lowercase().ends_with(".exe"), "{mine:?}");
        assert!(named(&mine.name).unwrap().iter().any(|p| p.pid == me));
        assert!(all.len() > 1, "{all:?}");
        assert_eq!(all.iter().filter(|p| p.pid == me).count(), 1);
    }
}
