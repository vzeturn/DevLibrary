//! Which agent copies this machine holds (`AgentHello.Installations`): Windows services whose
//! binary is the old .NET agent or dl-agent, and those processes running now. After the
//! rollout (old agents removed, dl-agent enrolled fresh) it reads
//! `services=1 [dl-agent -> ...] processes=MM.Arrival.Agent.exe:0 dl-agent.exe:1`.

/// `AgentInstallations.MaxLength`.
pub const MAX_LENGTH: usize = 600;
/// The old .NET agent's executable.
pub const OLD_BINARY: &str = "MM.Arrival.Agent.exe";
/// This agent's executable.
pub const BINARY: &str = "dl-agent.exe";

/// The one-line summary from what was found.
pub fn summarize(services: &[(String, String)], processes: &[String]) -> String {
    let count = |exe: &str| {
        processes
            .iter()
            .filter(|p| p.eq_ignore_ascii_case(exe))
            .count()
    };
    let list: Vec<String> = services
        .iter()
        .map(|(n, p)| format!("{n} -> {}", p.trim()))
        .collect();
    let text = format!(
        "services={} [{}] processes={OLD_BINARY}:{} {BINARY}:{}",
        services.len(),
        list.join("; "),
        count(OLD_BINARY),
        count(BINARY)
    );
    text.chars().take(MAX_LENGTH).collect()
}

/// Whether the old .NET agent is still installed or running.
pub fn old_agent_left(services: &[(String, String)], processes: &[String]) -> bool {
    let old = OLD_BINARY.to_lowercase();
    services
        .iter()
        .any(|(_, p)| p.to_lowercase().contains(&old))
        || processes.iter().any(|p| p.eq_ignore_ascii_case(OLD_BINARY))
}

/// Image names of `tasklist /FO CSV /NH`.
pub fn parse_tasklist(csv: &str) -> Vec<String> {
    csv.lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix('"')?;
            Some(rest[..rest.find('"')?].to_owned())
        })
        .collect()
}

/// Whether a service binary path is one of the agents.
pub fn is_agent(path: &str) -> bool {
    let p = path.to_lowercase();
    p.contains(&OLD_BINARY.to_lowercase()) || p.contains(&BINARY.to_lowercase())
}

/// Agent services (name, binary path) and the image names of running processes.
pub type Found = (Vec<(String, String)>, Vec<String>);

/// The services and processes (`None` off Windows).
///
/// # Errors
/// The service manager or `tasklist` cannot be read (the reason).
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
    let out = std::process::Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .output()
        .map_err(|e| format!("tasklist: {e}"))?;
    if !out.status.success() {
        return Err(format!("tasklist exited {}", out.status));
    }
    let processes = parse_tasklist(&String::from_utf8_lossy(&out.stdout));
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

    #[test]
    fn summary_counts_both_agents_and_flags_the_old_one() {
        let csv = "\"System Idle Process\",\"0\",\"Services\",\"0\",\"8 K\"\r\n\"dl-agent.exe\",\"4120\",\"Services\",\"0\",\"21,004 K\"\r\n\"MM.Arrival.Agent.exe\",\"5000\",\"Services\",\"0\",\"80,000 K\"\r\n";
        let p = parse_tasklist(csv);
        assert_eq!(p.len(), 3);
        let s = vec![(
            "dl-agent".to_owned(),
            "\"C:\\Program Files\\DevLibrary\\dl-agent.exe\" service".to_owned(),
        )];
        let text = summarize(&s, &p);
        assert_eq!(
            text,
            "services=1 [dl-agent -> \"C:\\Program Files\\DevLibrary\\dl-agent.exe\" service] processes=MM.Arrival.Agent.exe:1 dl-agent.exe:1"
        );
        assert!(old_agent_left(&s, &p));
        assert!(!old_agent_left(&s, &p[..2]));
        assert!(is_agent("C:\\x\\MM.Arrival.Agent.exe"));
        assert!(!is_agent("C:\\Windows\\svchost.exe -k netsvcs"));
        let long = vec![("x".to_owned(), "y".repeat(900))];
        assert_eq!(summarize(&long, &[]).chars().count(), MAX_LENGTH);
    }
}
