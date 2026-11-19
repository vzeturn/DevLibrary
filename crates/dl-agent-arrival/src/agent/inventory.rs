//! Which agent copies this machine holds (`AgentHello.Installations`): Windows services whose
//! binary is the old .NET agent or dl-agent, and those processes running now. After the
//! rollout (old agents removed, dl-agent enrolled fresh) it reads
//! `services=1 [dl-agent -> ...] processes=1`: the old .NET agent's format, which the cloud parses
//! (`processes` counts both agents' processes, so an old agent still running reads as 2).
//! Processes come from `dl_winsvc::process` (one system call, no `tasklist` to start or parse).

use dl_winsvc::process::{self, Process};

/// `AgentInstallations.MaxLength`.
pub const MAX_LENGTH: usize = 600;
/// The old .NET agent's executable.
pub const OLD_BINARY: &str = "MM.Arrival.Agent.exe";
/// This agent's executable.
pub const BINARY: &str = "dl-agent.exe";

/// The one-line summary from what was found.
pub fn summarize(services: &[(String, String)], processes: &[Process]) -> String {
    let count = |exe: &str| processes.iter().filter(|p| p.is(exe)).count();
    let list: Vec<String> = services
        .iter()
        .map(|(n, p)| format!("{n} -> {}", p.trim()))
        .collect();
    let text = format!(
        "services={} [{}] processes={}",
        services.len(),
        list.join("; "),
        count(OLD_BINARY) + count(BINARY)
    );
    text.chars().take(MAX_LENGTH).collect()
}

/// Whether the old .NET agent is still installed or running.
pub fn old_agent_left(services: &[(String, String)], processes: &[Process]) -> bool {
    let old = OLD_BINARY.to_lowercase();
    services
        .iter()
        .any(|(_, p)| p.to_lowercase().contains(&old))
        || processes.iter().any(|p| p.is(OLD_BINARY))
}

/// Whether a service binary path is one of the agents.
pub fn is_agent(path: &str) -> bool {
    let p = path.to_lowercase();
    p.contains(&OLD_BINARY.to_lowercase()) || p.contains(&BINARY.to_lowercase())
}

/// Agent services (name, binary path) and the running processes.
pub type Found = (Vec<(String, String)>, Vec<Process>);

/// The services and processes (`None` off Windows).
///
/// # Errors
/// The service manager or the process list cannot be read (the reason).
pub fn scan() -> Result<Option<Found>, String> {
    if !cfg!(windows) {
        return Ok(None);
    }
    let scm = dl_winsvc::scm::Manager::connect_read_only().map_err(|e| format!("services: {e}"))?;
    let mut services = Vec::new();
    let mut unreadable = 0usize;
    for entry in scm.list_services().map_err(|e| format!("services: {e}"))? {
        match scm.open_read_only(&entry.name).and_then(|s| s.config()) {
            Ok(c) if is_agent(&c.binary_path_name) => {
                services.push((entry.name, c.binary_path_name))
            }
            Ok(_) => {}
            Err(_) => unreadable += 1,
        }
    }
    if unreadable > 0 {
        dl_log::debug!("services whose configuration could not be read"; count = unreadable);
    }
    let processes = process::list().map_err(|e| format!("processes: {e}"))?;
    Ok(Some((services, processes)))
}

/// The hello's `Installations` text (`None` off Windows; `unavailable: ...` when it cannot be
/// read), and whether the old agent is still there.
pub fn describe() -> (Option<String>, bool) {
    match scan() {
        Ok(Some((s, p))) => (Some(summarize(&s, &p)), old_agent_left(&s, &p)),
        Ok(None) => (None, false),
        Err(e) => {
            dl_log::warn!("could not list the agent installations"; error = %e);
            (
                Some(
                    format!("unavailable: {e}")
                        .chars()
                        .take(MAX_LENGTH)
                        .collect(),
                ),
                false,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, name: &str) -> Process {
        Process {
            pid,
            parent_pid: 0,
            session_id: 0,
            name: name.to_owned(),
        }
    }

    #[test]
    fn summary_counts_both_agents_and_flags_the_old_one() {
        // Image names as the system reports them; the case of `.exe` does not matter.
        let p = vec![
            proc(0, ""),
            proc(4120, "dl-agent.exe"),
            proc(5000, "MM.Arrival.Agent.EXE"),
        ];
        let s = vec![(
            "dl-agent".to_owned(),
            "\"C:\\Program Files\\DevLibrary\\dl-agent.exe\" service".to_owned(),
        )];
        let text = summarize(&s, &p);
        assert_eq!(
            text,
            "services=1 [dl-agent -> \"C:\\Program Files\\DevLibrary\\dl-agent.exe\" service] processes=2"
        );
        assert!(old_agent_left(&s, &p));
        assert!(!old_agent_left(&s, &p[..2]));
        assert!(is_agent("C:\\x\\MM.Arrival.Agent.exe"));
        assert!(!is_agent("C:\\Windows\\svchost.exe -k netsvcs"));
        let long = vec![("x".to_owned(), "y".repeat(900))];
        assert_eq!(summarize(&long, &[]).chars().count(), MAX_LENGTH);
    }
}
