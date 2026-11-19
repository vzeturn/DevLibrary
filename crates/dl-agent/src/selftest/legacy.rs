//! `legacy.services`: the per-product agents dl-agent replaces. One still running next to
//! the dl-agent module that does its work means every job runs twice (two e-invoice
//! uploads, two key cards, two backups), so it is a `warn` until the old one is stopped.

use std::time::Instant;

use super::{CORE, Check, Outcome};

/// The old agents, as the hub's bench script finds them (`LEGACY_SERVICE_PATTERN`): needles
/// matched case-insensitively inside a service's name or display name, first entry wins,
/// and the dl-agent module that took the agent's work (`None`: no module yet, listed for
/// information). Exact names of the Go E-Invoice and SRBackup services are not confirmed,
/// hence substrings rather than names.
pub(crate) const LEGACY: [(&[&str], Option<&str>); 8] = [
    (&["mmei", "einvoice", "e-invoice"], Some("einvoice")),
    (&["smilepmsdevice", "smile pms device"], Some("pms-device")),
    (&["smilepms", "smile pms"], Some("pms-site")),
    (&["smilehl", "smile hl"], Some("hotellink")),
    (&["mm.arrival", "arrival.agent"], Some("arrival")),
    (&["mm.membership", "membership.agent"], Some("membership")),
    (&["srbackup"], Some("backup")),
    (&["pdasync", "pda sync"], None),
];

/// `Some(module)` when the service is one of the old agents (`Some(None)`: an old agent
/// without a dl-agent module), `None` otherwise. dl-agent's own service never matches.
pub(crate) fn classify(name: &str, display: &str) -> Option<Option<&'static str>> {
    let (name, display) = (name.to_lowercase(), display.to_lowercase());
    if name.starts_with("dl-agent") {
        return None;
    }
    LEGACY
        .iter()
        .find(|(needles, _)| {
            needles
                .iter()
                .any(|n| name.contains(n) || display.contains(n))
        })
        .map(|(_, module)| *module)
}

/// One old agent found on the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    /// Its service name, as listed.
    pub name: String,
    /// Its display name.
    pub display: String,
    /// `running`, `stopped`, ...
    pub state: String,
    /// Whether it is running (or starting).
    pub running: bool,
}

/// The verdict for `found` given the dl-agent modules that are enabled.
pub(crate) fn assess(found: &[Found], enabled: &[String]) -> Outcome {
    if found.is_empty() {
        return Outcome::pass("no old per-product agent installed");
    }
    let mut doubled = Vec::new();
    let mut listed = Vec::new();
    for f in found {
        let module = classify(&f.name, &f.display).flatten();
        let tag = match module {
            Some(m) => format!("{} ({}, module {m})", f.name, f.state),
            None => format!("{} ({})", f.name, f.state),
        };
        if f.running && module.is_some_and(|m| enabled.iter().any(|e| e == m)) {
            doubled.push(f.name.clone());
        }
        listed.push(tag);
    }
    if doubled.is_empty() {
        return Outcome::pass(listed.join("; "));
    }
    Outcome::warn(format!(
        "{}; running next to the enabled dl-agent module (work done twice): {}",
        listed.join("; "),
        doubled.join(", ")
    ))
    .remedy(format!(
        "once the dl-agent module works, stop and disable the old service: sc stop \"{0}\" && sc config \"{0}\" start= disabled",
        doubled[0]
    ))
}

#[cfg(windows)]
fn installed() -> Result<Vec<Found>, String> {
    use dl_winsvc::scm::{Manager, State};
    let services = Manager::connect_read_only()
        .and_then(|m| m.list_services())
        .map_err(|e| e.to_string())?;
    Ok(services
        .into_iter()
        .filter_map(|s| {
            classify(&s.name, &s.display_name)?;
            let state = s.status.state;
            Some(Found {
                name: s.name.clone(),
                display: s.display_name.clone(),
                state: format!("{state:?}").to_lowercase(),
                running: matches!(
                    state,
                    State::Running | State::StartPending | State::ContinuePending
                ),
            })
        })
        .collect())
}

/// The core check: which old agents exist, their state, and whether one doubles an enabled
/// module. Windows only (the old agents are Windows services).
pub(crate) fn check(enabled: &[String]) -> Check {
    let title = "Old per-product agents";
    if !cfg!(windows) {
        return Check::skip(
            CORE,
            "legacy.services",
            title,
            "the old agents are Windows services",
        );
    }
    let started = Instant::now();
    #[cfg(windows)]
    let outcome = match installed() {
        Ok(found) => assess(&found, enabled),
        Err(e) => Outcome::warn(format!("cannot list services: {e}")),
    };
    #[cfg(not(windows))]
    let outcome = assess(&[], enabled);
    Check::new(CORE, "legacy.services", title, outcome, started.elapsed())
}
