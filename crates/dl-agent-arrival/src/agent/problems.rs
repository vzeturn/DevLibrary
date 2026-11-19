//! `agent-problems.json` next to the identity file: the link's own warnings and errors, kept
//! across restarts, updates and reinstalls, so the cloud learns what went wrong while it could
//! not be reached (`AgentHello.RecentProblems`). The file, the fold rules and the bounds are
//! those of the .NET `ProblemJournal`: repeats of one key fold into a count, the folded entry
//! moves to the end, only the newest [`CAPACITY`] stay, and a run that never reached
//! [`ProblemJournal::mark_stopped`] is recorded as an unclean stop at the next start.
//!
//! Every change is read, applied and written through a temporary file under one process-wide
//! lock (the self-test reads the same file). A write failure is logged and never stops the
//! caller; the entries the cloud already saw are not lost by it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, PoisonError};

use dl_json::{Map, Value};
use dl_time::OffsetDateTime;

use crate::stj;

/// File name next to the identity.
pub const FILE_NAME: &str = "agent-problems.json";
/// Entries kept.
pub const CAPACITY: usize = 200;
/// Most entries one hello carries (`AgentProblems.MaxSent`).
pub const MAX_SENT: usize = 30;
/// `AgentProblems.MaxMessage`.
pub const MAX_MESSAGE: usize = 600;
/// `AgentProblems.MaxSource`.
pub const MAX_SOURCE: usize = 80;
/// A repeat folds into an entry this close to the newest one.
const FOLD_WINDOW: usize = 12;

static GATE: Mutex<()> = Mutex::new(());

/// One entry as the hello carries it.
pub use crate::contracts::messages::AgentProblem as Problem;

/// An event read from Windows' logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    /// When (round-trip UTC text).
    pub at: String,
    /// Level name.
    pub level: String,
    /// Where it came from.
    pub source: String,
    /// The condensed text.
    pub message: String,
}

#[derive(Debug, Clone)]
struct Entry {
    key: String,
    at: String,
    last_at: String,
    level: String,
    source: String,
    message: String,
    count: u32,
}

#[derive(Debug, Default)]
struct State {
    created_at: String,
    event_log_read_until: Option<String>,
    event_log_revision: i64,
    running_since: Option<String>,
    running_version: Option<String>,
    version: i64,
    entries: Vec<Entry>,
}

/// The journal file.
#[derive(Debug)]
pub struct ProblemJournal {
    path: PathBuf,
    version: AtomicI64,
}

/// Now, as the journal writes times (`2026-11-19T01:02:03.4567891Z`, sortable as text).
pub fn now() -> String {
    stj::num::format_utc_o(&OffsetDateTime::now_utc())
}

impl ProblemJournal {
    /// The journal next to `identity_file`.
    pub fn next_to(identity_file: &Path) -> ProblemJournal {
        let dir = identity_file
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        ProblemJournal {
            path: dir.join(FILE_NAME),
            version: AtomicI64::new(-1),
        }
    }

    /// The file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Changes on every recorded entry (read back on the first call).
    pub fn version(&self) -> i64 {
        let v = self.version.load(Ordering::SeqCst);
        if v >= 0 {
            return v;
        }
        let v = self.load().map_or(0, |s| s.version);
        self.version.store(v, Ordering::SeqCst);
        v
    }

    /// Records one occurrence at `now`. `key` groups repeats (a code, not the text); `None`
    /// uses the text with its digits masked.
    pub fn record(&self, level: &str, source: &str, message: &str, key: Option<&str>) {
        self.record_at(level, source, message, key, &now());
    }

    /// [`ProblemJournal::record`] at a given time.
    pub fn record_at(&self, level: &str, source: &str, message: &str, key: Option<&str>, at: &str) {
        let key = key.map_or_else(|| fold_key(message), str::to_owned);
        self.change(|s| add(s, level, source, message, &key, at));
    }

    /// The start of a run: an earlier run that never stopped cleanly is recorded first.
    pub fn mark_started(&self, version: &str, machine: &str) {
        let at = now();
        self.change(|s| {
            if let Some(since) = s.running_since.clone() {
                let text = format!(
                    "The previous run (version {}, started {since}) ended without a clean stop: \
                     the process crashed, was killed, or the server lost power.",
                    s.running_version.as_deref().unwrap_or("?")
                );
                add(
                    s,
                    "Warning",
                    "Lifecycle",
                    &text,
                    "lifecycle|unclean-stop",
                    &at,
                );
            }
            let text = format!("Agent {version} started on {machine}.");
            add(
                s,
                "Information",
                "Lifecycle",
                &text,
                &format!("lifecycle|started|{version}"),
                &at,
            );
            s.running_since = Some(at.clone());
            s.running_version = Some(version.to_owned());
        });
    }

    /// The run stops normally.
    pub fn mark_stopped(&self) {
        self.change(|s| s.running_since = None);
    }

    /// Where the event-log reader stopped: `(read_until, revision, created_at)`.
    ///
    /// # Errors
    /// The file exists but cannot be read.
    pub fn event_log_mark(&self) -> Result<(Option<String>, i64, String), String> {
        let s = self.load()?;
        Ok((s.event_log_read_until, s.event_log_revision, s.created_at))
    }

    /// Records events read from Windows' logs and moves the read mark in one write; an event
    /// already inside an entry's span is not counted twice.
    pub fn import(&self, events: &[Imported], read_until: &str, revision: i64) {
        let mut sorted: Vec<&Imported> = events.iter().collect();
        sorted.sort_by(|a, b| a.at.cmp(&b.at));
        self.change(|s| {
            for e in &sorted {
                let key = clip_key(&format!("{}|{}", e.source, fold_key(&e.message)));
                let seen = s
                    .entries
                    .iter()
                    .any(|x| x.key == key && e.at >= x.at && e.at <= x.last_at);
                if !seen {
                    add(s, &e.level, &e.source, &e.message, &key, &e.at);
                }
            }
            s.event_log_read_until = Some(read_until.to_owned());
            s.event_log_revision = revision;
        });
    }

    /// The newest `max` entries, oldest first.
    ///
    /// # Errors
    /// The file exists but cannot be read.
    pub fn recent(&self, max: usize) -> Result<Vec<Problem>, String> {
        let s = self.load()?;
        let skip = s.entries.len().saturating_sub(max);
        Ok(s.entries.iter().skip(skip).map(problem).collect())
    }

    /// The newest warning, error or critical entry.
    ///
    /// # Errors
    /// The file exists but cannot be read.
    pub fn latest_problem(&self) -> Result<Option<Problem>, String> {
        let s = self.load()?;
        Ok(s.entries
            .iter()
            .filter(|e| e.level != "Information")
            .max_by(|a, b| a.last_at.cmp(&b.last_at))
            .map(problem))
    }

    fn change(&self, f: impl FnOnce(&mut State)) {
        let _gate = GATE.lock().unwrap_or_else(PoisonError::into_inner);
        let mut state = match self.load() {
            Ok(s) => s,
            Err(e) => {
                // A file cut by a power loss: start over rather than stop the link.
                dl_log::warn!("problem journal unreadable; starting a new one"; path = %self.path.display(), error = %e);
                State {
                    created_at: now(),
                    ..State::default()
                }
            }
        };
        f(&mut state);
        self.version.store(state.version, Ordering::SeqCst);
        if let Err(e) = save(&self.path, &state) {
            dl_log::warn!("problem journal not written"; path = %self.path.display(), error = %e);
        }
    }

    fn load(&self) -> Result<State, String> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(State {
                    created_at: now(),
                    ..State::default()
                });
            }
            Err(e) => return Err(format!("read {}: {e}", self.path.display())),
        };
        let v = stj::parse(text.trim_start_matches('\u{feff}'))
            .map_err(|_| format!("{} does not parse", self.path.display()))?;
        Ok(from_json(&v))
    }
}

fn problem(e: &Entry) -> Problem {
    Problem {
        at: e.at.clone(),
        level: e.level.clone(),
        source: e.source.clone(),
        message: e.message.clone(),
        count: e.count,
        last_at: (e.count > 1).then(|| e.last_at.clone()),
    }
}

fn add(s: &mut State, level: &str, source: &str, message: &str, key: &str, at: &str) {
    s.version += 1;
    let key = clip_key(key);
    let start = s.entries.len().saturating_sub(FOLD_WINDOW);
    for i in (start..s.entries.len()).rev() {
        if s.entries[i].key != key || s.entries[i].level != level {
            continue;
        }
        let mut e = s.entries.remove(i);
        e.count = e.count.saturating_add(1);
        if at >= e.last_at.as_str() {
            e.last_at = at.to_owned();
            e.message = trim(message, MAX_MESSAGE);
        }
        s.entries.push(e);
        return;
    }
    s.entries.push(Entry {
        key,
        at: at.to_owned(),
        last_at: at.to_owned(),
        level: level.to_owned(),
        source: trim(source, MAX_SOURCE),
        message: trim(message, MAX_MESSAGE),
        count: 1,
    });
    if s.entries.len() > CAPACITY {
        let extra = s.entries.len() - CAPACITY;
        s.entries.drain(..extra);
    }
}

/// Digits vary between repeats (attempts, delays, ids): they do not make a new problem.
pub fn fold_key(message: &str) -> String {
    message
        .chars()
        .take(160)
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect()
}

fn clip_key(key: &str) -> String {
    key.chars().take(200).collect()
}

/// `AgentProblems.Trim`.
pub fn trim(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut s: String = text.chars().take(max - 1).collect();
    s.push('…');
    s
}

fn from_json(v: &Value) -> State {
    let text = |m: &Value, k: &str| m.get(k).and_then(Value::as_str).map(str::to_owned);
    let entries = match v.get("entries") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|e| {
                let at = text(e, "at")?;
                Some(Entry {
                    key: text(e, "key").unwrap_or_default(),
                    last_at: text(e, "lastAt").unwrap_or_else(|| at.clone()),
                    at,
                    level: text(e, "level").unwrap_or_default(),
                    source: text(e, "source").unwrap_or_default(),
                    message: text(e, "message").unwrap_or_default(),
                    count: e
                        .get("count")
                        .and_then(Value::as_i64)
                        .and_then(|n| u32::try_from(n).ok())
                        .unwrap_or(1),
                })
            })
            .collect(),
        _ => Vec::new(),
    };
    State {
        created_at: text(v, "createdAt").unwrap_or_else(now),
        event_log_read_until: text(v, "eventLogReadUntil"),
        event_log_revision: v
            .get("eventLogRevision")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        running_since: text(v, "runningSince"),
        running_version: text(v, "runningVersion"),
        version: v.get("version").and_then(Value::as_i64).unwrap_or(0),
        entries,
    }
}

fn save(path: &Path, s: &State) -> Result<(), String> {
    let mut m = Map::new();
    m.insert("createdAt", s.created_at.as_str());
    if let Some(t) = &s.event_log_read_until {
        m.insert("eventLogReadUntil", t.as_str());
    }
    m.insert("eventLogRevision", s.event_log_revision);
    if let Some(t) = &s.running_since {
        m.insert("runningSince", t.as_str());
    }
    if let Some(t) = &s.running_version {
        m.insert("runningVersion", t.as_str());
    }
    m.insert("version", s.version);
    let entries: Vec<Value> = s
        .entries
        .iter()
        .map(|e| {
            let mut x = Map::new();
            x.insert("key", e.key.as_str());
            x.insert("at", e.at.as_str());
            x.insert("lastAt", e.last_at.as_str());
            x.insert("level", e.level.as_str());
            x.insert("source", e.source.as_str());
            x.insert("message", e.message.as_str());
            x.insert("count", i64::from(e.count));
            Value::Object(x)
        })
        .collect();
    m.insert("entries", Value::Array(entries));
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let tmp = crate::agent::vault::tmp_path(path);
    std::fs::write(&tmp, dl_json::to_string(&Value::Object(m)))
        .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("replace {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal(tag: &str) -> (PathBuf, ProblemJournal) {
        let dir = std::env::temp_dir().join(format!(
            "arr-pj-{tag}-{}-{:016x}",
            std::process::id(),
            dl_rand::u64()
        ));
        let j = ProblemJournal::next_to(&dir.join("agent-identity.bin"));
        (dir, j)
    }

    #[test]
    fn repeats_fold_and_the_newest_stay() {
        let (dir, j) = journal("fold");
        j.record_at(
            "Error",
            "pms",
            "login failed 1",
            None,
            "2026-11-19T01:00:00.0000000Z",
        );
        j.record_at(
            "Warning",
            "cloud",
            "timeout",
            Some("cloud"),
            "2026-11-19T01:00:01.0000000Z",
        );
        j.record_at(
            "Error",
            "pms",
            "login failed 2",
            None,
            "2026-11-19T01:00:02.0000000Z",
        );
        let r = j.recent(MAX_SENT).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[1].message, "login failed 2");
        assert_eq!(r[1].count, 2);
        assert_eq!(
            r[1].last_at.as_deref(),
            Some("2026-11-19T01:00:02.0000000Z")
        );
        assert_eq!(j.latest_problem().unwrap().unwrap().source, "pms");
        for i in 0..(CAPACITY + 5) {
            j.record(
                "Warning",
                "x",
                &format!("distinct {}", "y".repeat(i % 7 + 1)),
                Some(&format!("k{i}")),
            );
        }
        assert_eq!(j.recent(usize::MAX).unwrap().len(), CAPACITY);
        assert_eq!(j.recent(MAX_SENT).unwrap().len(), MAX_SENT);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unclean_stop_is_recorded_at_the_next_start() {
        let (dir, j) = journal("life");
        j.mark_started("1.0", "HOTEL");
        j.mark_started("1.1", "HOTEL");
        let p = j.latest_problem().unwrap().unwrap();
        assert!(p.message.contains("without a clean stop"), "{}", p.message);
        assert!(p.message.contains("1.0"), "{}", p.message);
        j.mark_stopped();
        j.mark_started("1.1", "HOTEL");
        let warnings = j
            .recent(usize::MAX)
            .unwrap()
            .into_iter()
            .filter(|p| p.level == "Warning")
            .count();
        assert_eq!(warnings, 1, "a clean stop adds no warning");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn imported_events_move_the_mark_and_are_not_counted_twice() {
        let (dir, j) = journal("import");
        let e = Imported {
            at: "2026-11-18T01:00:00.0000000Z".into(),
            level: "Error".into(),
            source: "Application Error (Windows log)".into(),
            message: "Faulting application name: dl-agent.exe".into(),
        };
        j.import(std::slice::from_ref(&e), "2026-11-19T00:00:00.0000000Z", 1);
        j.import(std::slice::from_ref(&e), "2026-11-19T00:00:00.0000000Z", 1);
        assert_eq!(j.recent(10).unwrap().len(), 1);
        assert_eq!(j.recent(10).unwrap()[0].count, 1);
        let (until, rev, _) = j.event_log_mark().unwrap();
        assert_eq!(until.as_deref(), Some("2026-11-19T00:00:00.0000000Z"));
        assert_eq!(rev, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_cut_file_starts_over_and_texts_are_bounded() {
        let (dir, j) = journal("cut");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(j.path(), "{\"entries\":[{").unwrap();
        assert!(j.recent(1).is_err());
        j.record("Error", &"s".repeat(200), &"m".repeat(2000), None);
        let p = &j.recent(1).unwrap()[0];
        assert_eq!(p.source.chars().count(), MAX_SOURCE);
        assert_eq!(p.message.chars().count(), MAX_MESSAGE);
        let mut out = String::new();
        p.write(&mut out);
        assert!(
            out.starts_with("{\"at\":") && out.ends_with("\"lastAt\":null}"),
            "{out}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
