//! Antivirus on the machine being set up (RT-0361).
//!
//! Behaviour-based engines (Kaspersky PDM, ESET, Bitdefender...) score an unsigned program that
//! starts a hidden PowerShell to add a Windows Defender exclusion as a trojan. When another
//! antivirus is active Defender is passive anyway (`Add-MpPreference` then fails with "Invalid
//! class"), so the installer leaves Defender alone and only tells the operator what to allow.
//! The machine setup detects the antivirus from the service list it already reads: no WMI, no
//! child process.

use std::path::Path;

use dl_winsvc::scm::{ServiceEntry, State};

/// Service name or display name prefix/fragment → product, lower case. Name prefixes match the
/// start of the service name; display fragments anywhere in the display name.
const NAME_PREFIXES: [(&str, &str); 14] = [
    ("avp", "Kaspersky"),
    ("klnagent", "Kaspersky"),
    ("ekrn", "ESET"),
    ("vsserv", "Bitdefender"),
    ("bdredline", "Bitdefender"),
    ("avast", "Avast"),
    ("avg", "AVG"),
    ("mcafee", "McAfee"),
    ("mfemms", "McAfee"),
    ("sophos", "Sophos"),
    ("sepmasterservice", "Symantec Endpoint Protection"),
    ("ntrtscan", "Trend Micro"),
    ("bkav", "Bkav"),
    ("csfalconservice", "CrowdStrike Falcon"),
];
const DISPLAY_FRAGMENTS: [(&str, &str); 14] = [
    ("kaspersky", "Kaspersky"),
    ("eset ", "ESET"),
    ("bitdefender", "Bitdefender"),
    ("avast", "Avast"),
    ("avg antivirus", "AVG"),
    ("mcafee", "McAfee"),
    ("sophos", "Sophos"),
    ("symantec", "Symantec"),
    ("norton", "Norton"),
    ("trend micro", "Trend Micro"),
    ("bkav", "Bkav"),
    ("cmc antivirus", "CMC Antivirus"),
    ("malwarebytes", "Malwarebytes"),
    ("sentinelone", "SentinelOne"),
];

/// The Windows Defender antivirus service.
const DEFENDER_SERVICE: &str = "WinDefend";

/// What the installer found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Antivirus {
    /// Running third-party products, deduplicated, in list order.
    pub others: Vec<String>,
    /// Windows Defender's service is running.
    pub defender_running: bool,
}

impl Antivirus {
    /// Classifies the services of the machine (only running ones count).
    pub fn from_services(services: &[ServiceEntry]) -> Antivirus {
        let mut av = Antivirus::default();
        for s in services.iter().filter(|s| s.status.state == State::Running) {
            if s.name.eq_ignore_ascii_case(DEFENDER_SERVICE) {
                av.defender_running = true;
                continue;
            }
            if let Some(p) = product(&s.name, &s.display_name)
                && !av.others.iter().any(|o| o == p)
            {
                av.others.push(p.to_string());
            }
        }
        av
    }

    /// Defender is the antivirus in charge: its service runs and no other product does.
    pub fn defender_in_charge(&self) -> bool {
        self.defender_running && self.others.is_empty()
    }
}

/// The third-party product a service belongs to, if any.
pub fn product(name: &str, display: &str) -> Option<&'static str> {
    let name = name.to_ascii_lowercase();
    let display = display.to_ascii_lowercase();
    if display.contains("defender") || display.contains("microsoft") {
        return None;
    }
    NAME_PREFIXES
        .iter()
        .find(|(p, _)| name.starts_with(p))
        .or_else(|| DISPLAY_FRAGMENTS.iter().find(|(f, _)| display.contains(f)))
        .map(|(_, p)| *p)
}

/// The lines asking the operator to trust the agent in `others` (not printed when empty).
pub fn trust_hint(others: &[String], exe: &Path, dir: &Path, sha: Option<&str>) -> Vec<String> {
    if others.is_empty() {
        return Vec::new();
    }
    let install = exe.parent().unwrap_or(exe);
    let mut lines = vec![
        format!(
            "Antivirus on this machine: {}. Windows Defender is left alone (it is passive).",
            others.join(", ")
        ),
        format!(
            "If it blocks the agent: add {} as a Trusted application{} and exclude {} and {} from scanning, then run the installer again.",
            exe.display(),
            sha.map(|s| format!(" (SHA-256 {s})")).unwrap_or_default(),
            install.display(),
            dir.display()
        ),
    ];
    if others.iter().any(|o| o == "Kaspersky") {
        lines.push(
            "Kaspersky: Settings > Threats and exclusions > Specify trusted applications > Add, and choose \"Do not monitor application activity\".".into(),
        );
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_winsvc::scm::Status;

    fn entry(name: &str, display: &str, state: State) -> ServiceEntry {
        ServiceEntry {
            name: name.into(),
            display_name: display.into(),
            status: Status {
                state,
                accepts_stop: false,
                win32_exit_code: 0,
                service_specific_exit_code: 0,
            },
        }
    }

    #[test]
    fn kaspersky_makes_defender_stand_aside() {
        let av = Antivirus::from_services(&[
            entry(
                "WinDefend",
                "Microsoft Defender Antivirus Service",
                State::Running,
            ),
            entry(
                "AVP21.3",
                "Kaspersky Small Office Security Service",
                State::Running,
            ),
            entry(
                "klnagent",
                "Kaspersky Security Center Network Agent",
                State::Running,
            ),
        ]);
        assert_eq!(av.others, vec!["Kaspersky".to_string()]);
        assert!(!av.defender_in_charge());
    }

    #[test]
    fn defender_alone_is_in_charge_and_stopped_products_do_not_count() {
        let av = Antivirus::from_services(&[
            entry(
                "WinDefend",
                "Microsoft Defender Antivirus Service",
                State::Running,
            ),
            entry("ekrn", "ESET Service", State::Stopped),
            entry("Spooler", "Print Spooler", State::Running),
        ]);
        assert!(av.others.is_empty());
        assert!(av.defender_in_charge());
        assert!(!Antivirus::from_services(&[]).defender_in_charge());
    }

    #[test]
    fn known_products_are_named_and_microsoft_services_are_not() {
        assert_eq!(product("ekrn", "ESET Service"), Some("ESET"));
        assert_eq!(product("x", "Bkav Pro Service"), Some("Bkav"));
        assert_eq!(product("avgsvc", "x"), Some("AVG"));
        assert_eq!(
            product(
                "WdNisSvc",
                "Microsoft Defender Antivirus Network Inspection"
            ),
            None
        );
        assert_eq!(product("Spooler", "Print Spooler"), None);
    }

    #[test]
    fn the_hint_names_the_paths_and_kaspersky_steps() {
        let lines = trust_hint(
            &["Kaspersky".into()],
            Path::new("C:/PF/dl-agent/dl-agent.exe"),
            Path::new("C:/PD/dl-agent"),
            Some("ab12"),
        );
        assert!(lines[0].contains("Kaspersky"));
        assert!(lines[1].contains("dl-agent.exe") && lines[1].contains("SHA-256 ab12"));
        assert!(lines[2].contains("Do not monitor application activity"));
        assert!(trust_hint(&[], Path::new("a"), Path::new("b"), None).is_empty());
    }
}
