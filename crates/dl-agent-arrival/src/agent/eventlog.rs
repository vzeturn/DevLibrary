//! What Windows recorded about the agent while nobody could see it, imported into the
//! problem journal once per start (the .NET `EventLogHistory`): crashes of the agent's
//! process, its service stopping, starting and failing, the server shutting down, restarting
//! or losing power, and the old .NET agent's own warnings from before the journal existed.
//!
//! Read with `wevtutil qe <log> /q:<XPath> /f:RenderedXml /e:Events` (no event-log API in
//! dl-winsvc); the parse and the rules are pure, so they are tested off Windows.

use dl_time::{Duration, OffsetDateTime};

use crate::agent::problems::{Imported, ProblemJournal, now};
use crate::stj;

/// Raised when the reader learns to find more, so the look-back is read again.
pub const REVISION: i64 = 1;
/// How far back the first read goes.
pub const FIRST_LOOK_BACK_DAYS: i64 = 14;
/// Events kept per log (newest first), so a flapping service never floods the journal.
pub const MAX_PER_LOG: usize = 2_000;
/// The old .NET agent's event source.
pub const OLD_OWN_SOURCE: &str = "MM.Arrival.Agent";
/// Names Windows uses for the agents: executables, services, display names.
pub const AGENT_NAMES: [&str; 4] = [
    "dl-agent",
    "DevLibrary Agent",
    "MM.Arrival.Agent",
    "MM-Arrival",
];
/// Application-log sources that report crashes.
pub const CRASH_SOURCES: [&str; 4] = [
    ".NET Runtime",
    "Application Error",
    "Application Hang",
    "Windows Error Reporting",
];
/// `Service Control Manager`.
pub const SCM: &str = "Service Control Manager";
/// Server start (6005), clean shutdown (6006), unexpected shutdown (6008), restart by someone
/// (1074), power loss (41).
pub const SERVER_RESTARTS: [(&str, &[u32]); 3] = [
    ("EventLog", &[6005, 6006, 6008]),
    ("User32", &[1074]),
    ("Microsoft-Windows-Kernel-Power", &[41]),
];

/// Service exit code of a restart the agent asks for after installing an update
/// (`dl_agent::update::RESTART_EXIT_CODE`): SCM logs it as error 7024, but it is expected.
pub const RESTART_EXIT: &str = "75";

/// SCM event "the service terminated unexpectedly" (with the recovery action taken).
const UNEXPECTED_STOP: u32 = 7031;

/// Whether an SCM event is the service ending with the update-restart exit code.
fn is_update_restart(e: &RawEvent) -> bool {
    e.id == 7024
        && e.message
            .trim_end_matches(['.', ' ', '\r', '\n'])
            .rsplit([' ', '%', ':'])
            .next()
            .is_some_and(|code| code == RESTART_EXIT)
}

/// One event as `wevtutil` rendered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEvent {
    /// Provider name.
    pub provider: String,
    /// Event id.
    pub id: u32,
    /// 1 critical, 2 error, 3 warning, 4/0 information.
    pub level: u8,
    /// Round-trip UTC text (7 fraction digits, `Z`).
    pub at: String,
    /// The rendered message, else the data values joined.
    pub message: String,
}

/// The XPath that selects `providers`' events in `[from, to)` (UTC, millisecond text).
pub fn query(providers: &[&str], from: &str, to: &str) -> String {
    let names: Vec<String> = providers.iter().map(|p| format!("@Name='{p}'")).collect();
    format!(
        "*[System[Provider[{}] and TimeCreated[@SystemTime>='{from}' and @SystemTime<'{to}']]]",
        names.join(" or ")
    )
}

/// `2026-11-19T01:02:03.4567891Z` → `2026-11-19T01:02:03.456Z` (what the XPath compares).
pub fn xpath_time(at: &str) -> String {
    let (main, frac) = at
        .trim_end_matches('Z')
        .split_once('.')
        .unwrap_or((at.trim_end_matches('Z'), ""));
    let mut ms: String = frac.chars().take(3).collect();
    while ms.len() < 3 {
        ms.push('0');
    }
    format!("{main}.{ms}Z")
}

/// A `SystemTime` attribute (any fraction length) in the journal's form.
pub fn journal_time(system_time: &str) -> String {
    let t = system_time.trim().trim_end_matches('Z');
    let (main, frac) = t.split_once('.').unwrap_or((t, ""));
    let mut f: String = frac.chars().filter(char::is_ascii_digit).take(7).collect();
    while f.len() < 7 {
        f.push('0');
    }
    format!("{main}.{f}Z")
}

/// The events of a `wevtutil qe ... /f:RenderedXml /e:Events` output.
pub fn parse_rendered(xml: &str) -> Vec<RawEvent> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<Event ").or_else(|| rest.find("<Event>")) {
        let body = &rest[start..];
        let Some(end) = body.find("</Event>") else {
            break;
        };
        let event = &body[..end];
        rest = &body[end + "</Event>".len()..];
        let provider = attr(event, "Provider", "Name").unwrap_or_default();
        let at = attr(event, "TimeCreated", "SystemTime").map(|t| journal_time(&t));
        let Some(at) = at else { continue };
        let id = element(event, "EventID")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let level = element(event, "Level")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(4);
        let message = element(event, "Message")
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| data_values(event).join(" | "));
        out.push(RawEvent {
            provider,
            id,
            level,
            at,
            message,
        });
    }
    out
}

fn attr(xml: &str, tag: &str, name: &str) -> Option<String> {
    let open = format!("<{tag} ");
    let s = &xml[xml.find(&open)? + open.len()..];
    let s = &s[..s.find('>')?];
    let key = format!("{name}=");
    let v = &s[s.find(&key)? + key.len()..];
    let quote = v.chars().next()?;
    let v = &v[1..];
    Some(unescape(&v[..v.find(quote)?]))
}

fn element(xml: &str, tag: &str) -> Option<String> {
    let mut search = xml;
    loop {
        let i = search.find(&format!("<{tag}"))?;
        let after = &search[i + 1 + tag.len()..];
        match after.chars().next() {
            Some('>') | Some(' ') => {
                let gt = after.find('>')?;
                if after[..gt].ends_with('/') {
                    return Some(String::new());
                }
                let content = &after[gt + 1..];
                let close = content.find(&format!("</{tag}>"))?;
                return Some(unescape(&content[..close]));
            }
            _ => search = after,
        }
    }
}

fn data_values(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<Data") {
        let after = &rest[i + 5..];
        let Some(gt) = after.find('>') else { break };
        if after[..gt].ends_with('/') {
            rest = &after[gt + 1..];
            continue;
        }
        let content = &after[gt + 1..];
        let Some(close) = content.find("</Data>") else {
            break;
        };
        let v = unescape(&content[..close]);
        if !v.trim().is_empty() {
            out.push(v);
        }
        rest = &content[close..];
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(semi) = tail.find(';').filter(|n| *n <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..semi];
        let ch = match entity {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=semi]),
        }
        rest = &tail[semi + 1..];
    }
    out.push_str(rest);
    out
}

fn level_name(level: u8) -> &'static str {
    match level {
        1 => "Critical",
        2 => "Error",
        3 => "Warning",
        _ => "Information",
    }
}

fn mentions(message: &str) -> bool {
    let lower = message.to_lowercase();
    AGENT_NAMES
        .iter()
        .any(|n| lower.contains(&n.to_lowercase()))
}

/// Whether an event concerns the agent, as what (`None`: not wanted).
pub fn classify(log: &str, e: &RawEvent, own_before: &str) -> Option<Imported> {
    let name = level_name(e.level);
    let problem = matches!(e.level, 1..=3);
    let make = |level: &str, source: String, message: String| Imported {
        at: e.at.clone(),
        level: level.to_owned(),
        source,
        message,
    };
    match log {
        "Application" if e.provider == OLD_OWN_SOURCE => (problem && e.at.as_str() < own_before)
            .then(|| {
                make(
                    name,
                    format!("{OLD_OWN_SOURCE} (Windows log)"),
                    condense(&e.message),
                )
            }),
        "Application" => (problem
            && CRASH_SOURCES.contains(&e.provider.as_str())
            && mentions(&e.message))
        .then(|| {
            make(
                name,
                format!("{} (Windows log)", e.provider),
                condense(&e.message),
            )
        }),
        // Every state change of an agent's service: "entered the stopped state" answers
        // "when did it stop", and the removal of the old agent shows here too.
        "System" if e.provider == SCM => mentions(&e.message).then(|| {
            // Exit 75 is the agent restarting itself into a new version, not a failure.
            let level = if is_update_restart(e) {
                "Information"
            } else {
                name
            };
            make(level, format!("{SCM} (Windows log)"), condense(&e.message))
        }),
        "System" => SERVER_RESTARTS
            .iter()
            .any(|(p, ids)| *p == e.provider && ids.contains(&e.id))
            .then(|| {
                let level = if problem { name } else { "Information" };
                make(level, "Server (Windows log)".into(), condense(&e.message))
            }),
        _ => None,
    }
}

/// One line: no stack frames, no blank lines, single spaces, at most ~2000 characters.
pub fn condense(text: &str) -> String {
    let mut out = String::new();
    for line in text.replace('\r', "").split('\n').map(str::trim) {
        if line.is_empty()
            || line.starts_with("at ")
            || line.starts_with("--- End of")
            || line == "Exception:"
        {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line);
        if out.len() > 2_000 {
            break;
        }
    }
    out
}

/// The providers read from each log.
pub fn providers(log: &str) -> Vec<&'static str> {
    match log {
        "Application" => std::iter::once(OLD_OWN_SOURCE)
            .chain(CRASH_SOURCES)
            .collect(),
        _ => std::iter::once(SCM)
            .chain(SERVER_RESTARTS.iter().map(|(p, _)| *p))
            .collect(),
    }
}

/// Reads both logs in `[from, to)`; nothing off Windows.
///
/// # Errors
/// `wevtutil` could not run or failed (its message).
pub fn read(from: &str, to: &str, own_before: &str) -> Result<Vec<Imported>, String> {
    if !cfg!(windows) {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for log in ["Application", "System"] {
        let q = query(&providers(log), &xpath_time(from), &xpath_time(to));
        let out = std::process::Command::new("wevtutil")
            .args([
                "qe",
                log,
                &format!("/q:{q}"),
                "/f:RenderedXml",
                "/e:Events",
                "/rd:true",
            ])
            .arg(format!("/c:{MAX_PER_LOG}"))
            .output()
            .map_err(|e| format!("wevtutil: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "wevtutil qe {log} exited {}: {}",
                out.status,
                condense(&String::from_utf8_lossy(&out.stderr))
            ));
        }
        let xml = String::from_utf8_lossy(&out.stdout);
        found.extend(classify_all(log, &parse_rendered(&xml), own_before));
    }
    Ok(found)
}

/// [`classify`] over one log's events, plus the pairing a single event cannot see: SCM logs
/// an update restart twice at the same instant (7024 with exit 75, then 7031 "terminated
/// unexpectedly ... Restart the service"), and the 7031 half is no problem either.
pub fn classify_all(log: &str, events: &[RawEvent], own_before: &str) -> Vec<Imported> {
    let restarts: Vec<&str> = events
        .iter()
        .filter(|e| e.provider == SCM && is_update_restart(e))
        .map(|e| e.at.as_str())
        .collect();
    events
        .iter()
        .filter_map(|e| {
            let mut found = classify(log, e, own_before)?;
            if e.provider == SCM && e.id == UNEXPECTED_STOP && restarts.contains(&e.at.as_str()) {
                "Information".clone_into(&mut found.level);
            }
            Some(found)
        })
        .collect()
}

/// Imports what Windows recorded since the journal's mark (14 days on the first run).
///
/// # Errors
/// The journal or the event logs cannot be read; recorded in the journal too, so the cloud
/// learns the history is missing and why.
pub fn import(journal: &ProblemJournal) -> Result<usize, String> {
    let (until, revision, created) = journal.event_log_mark()?;
    let to = now();
    let from = match until {
        Some(u) if revision >= REVISION => u,
        _ => stj::num::format_utc_o(
            &(OffsetDateTime::now_utc() - Duration::from_days(FIRST_LOOK_BACK_DAYS)),
        ),
    };
    if from >= to {
        return Ok(0);
    }
    match read(&from, &to, &created) {
        Ok(events) => {
            let n = events.len();
            journal.import(&events, &to, REVISION);
            if cfg!(windows) {
                journal.record(
                    "Information",
                    "EventLogHistory",
                    &format!("Read the Windows event logs from {from} to {to}: {n} event(s) about the agent or server restarts."),
                    Some("eventlog|read"),
                );
            }
            Ok(n)
        }
        Err(e) => {
            journal.record(
                "Warning",
                "EventLogHistory",
                &format!("Could not read the Windows event logs for earlier problems: {e}"),
                Some("eventlog|failed"),
            );
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<Events><Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System><Provider Name='Service Control Manager' Guid='{555908d1}' EventSourceName='Service Control Manager'/><EventID Qualifiers='16384'>7036</EventID><Level>4</Level><TimeCreated SystemTime='2026-11-18T03:04:05.1234567Z'/></System><EventData><Data Name='param1'>dl-agent</Data><Data Name='param2'>stopped</Data></EventData><RenderingInfo Culture='en-US'><Message>The DevLibrary Agent service entered the stopped state.</Message><Level>Information</Level></RenderingInfo></Event><Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System><Provider Name='Application Error'/><EventID Qualifiers='0'>1000</EventID><Level>2</Level><TimeCreated SystemTime="2026-11-18T05:00:00.123456789Z"/></System><EventData><Data>dl-agent.exe</Data><Data>1.3.8</Data><Data/></EventData></Event><Event><System><Provider Name='EventLog'/><EventID>6008</EventID><Level>2</Level><TimeCreated SystemTime='2026-11-18T06:00:00.5Z'/></System><RenderingInfo><Message>The previous system shutdown at 1:00:00 AM on &#x200E;11/&#x200E;18/&#x200E;2026 was unexpected.&#13;
</Message></RenderingInfo></Event></Events>"#;

    #[test]
    fn rendered_xml_parses_with_or_without_a_message() {
        let e = parse_rendered(XML);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].provider, "Service Control Manager");
        assert_eq!(e[0].id, 7036);
        assert_eq!(e[0].level, 4);
        assert_eq!(e[0].at, "2026-11-18T03:04:05.1234567Z");
        assert!(
            e[0].message
                .contains("DevLibrary Agent service entered the stopped")
        );
        assert_eq!(e[1].at, "2026-11-18T05:00:00.1234567Z");
        assert_eq!(e[1].message, "dl-agent.exe | 1.3.8");
        assert_eq!(e[2].at, "2026-11-18T06:00:00.5000000Z");
        assert!(e[2].message.contains("was unexpected."), "{}", e[2].message);
    }

    #[test]
    fn the_rules_keep_what_concerns_the_agent() {
        let e = parse_rendered(XML);
        let before = "2026-11-19T00:00:00.0000000Z";
        let scm = classify("System", &e[0], before).unwrap();
        assert_eq!(scm.level, "Information");
        assert_eq!(scm.source, "Service Control Manager (Windows log)");
        let crash = classify("Application", &e[1], before).unwrap();
        assert_eq!(crash.level, "Error");
        let power = classify("System", &e[2], before).unwrap();
        assert_eq!(power.source, "Server (Windows log)");
        let other = RawEvent {
            provider: "Application Error".into(),
            id: 1000,
            level: 2,
            at: e[1].at.clone(),
            message: "notepad.exe".into(),
        };
        assert!(classify("Application", &other, before).is_none());
        let own = RawEvent {
            provider: OLD_OWN_SOURCE.into(),
            id: 0,
            level: 3,
            at: e[1].at.clone(),
            message: "Category: X\nEventId: 1\n\nPMS down\n   at Foo()".into(),
        };
        assert!(classify("Application", &own, before).is_some());
        assert!(classify("Application", &own, "2026-11-18T00:00:00.0000000Z").is_none());
    }

    #[test]
    fn the_update_restart_exit_is_not_a_problem() {
        let ev = |message: &str| RawEvent {
            provider: SCM.into(),
            id: 7024,
            level: 2,
            at: "2026-11-19T13:06:44.6804764Z".into(),
            message: message.into(),
        };
        let before = "2026-11-19T00:00:00.0000000Z";
        for m in [
            "The DevLibrary Agent service terminated with the following service-specific error: %%75",
            "The DevLibrary Agent service terminated with the following service-specific error: 75.",
        ] {
            assert_eq!(
                classify("System", &ev(m), before).unwrap().level,
                "Information"
            );
        }
        let real = "The DevLibrary Agent service terminated with the following service-specific error: %%1";
        assert_eq!(
            classify("System", &ev(real), before).unwrap().level,
            "Error"
        );
        let other = "The DevLibrary Agent service terminated with the following service-specific error: %%175";
        assert_eq!(
            classify("System", &ev(other), before).unwrap().level,
            "Error"
        );
    }

    #[test]
    fn the_unexpected_stop_paired_with_an_update_restart_is_not_a_problem() {
        let ev = |id: u32, at: &str, message: &str| RawEvent {
            provider: SCM.into(),
            id,
            level: 2,
            at: at.into(),
            message: message.into(),
        };
        let t = "2026-11-19T14:38:27.8649581Z";
        let crash = "The DevLibrary Agent service terminated unexpectedly.  It has done this 13 time(s).  The following corrective action will be taken in 60000 milliseconds: Restart the service.";
        let events = [
            ev(
                7024,
                t,
                "The DevLibrary Agent service terminated with the following service-specific error: %%75",
            ),
            ev(7031, t, crash),
            ev(7031, "2026-11-19T15:00:00.0000000Z", crash),
        ];
        let got = classify_all("System", &events, "2026-11-19T00:00:00.0000000Z");
        let levels: Vec<&str> = got.iter().map(|i| i.level.as_str()).collect();
        assert_eq!(levels, ["Information", "Information", "Error"]);
    }

    #[test]
    fn query_and_times() {
        let q = query(
            &["A", "B"],
            "2026-11-18T00:00:00.000Z",
            "2026-11-19T00:00:00.000Z",
        );
        assert_eq!(
            q,
            "*[System[Provider[@Name='A' or @Name='B'] and TimeCreated[@SystemTime>='2026-11-18T00:00:00.000Z' and @SystemTime<'2026-11-19T00:00:00.000Z']]]"
        );
        assert_eq!(
            xpath_time("2026-11-19T01:02:03.4567891Z"),
            "2026-11-19T01:02:03.456Z"
        );
        assert_eq!(
            journal_time("2026-11-19T01:02:03Z"),
            "2026-11-19T01:02:03.0000000Z"
        );
        assert_eq!(condense("a\n\n   at X.Y()\n--- End of stack\nb"), "a b");
        assert!(providers("Application").contains(&".NET Runtime"));
        assert!(providers("System").contains(&"User32"));
    }
}
