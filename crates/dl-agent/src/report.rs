//! Structured problem reports with fingerprint coalescing and a durable spool (PROTOCOL.md §7).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use dl_hash::{Digest, Sha256};
use dl_json::{Map, Value, json};

use crate::Error;
use crate::store;

/// Longest `command` kept: 100 characters stay within the hub's 200 UTF-16 unit limit.
pub const MAX_COMMAND_CHARS: usize = 100;

/// Most distinct reports kept on disk while the hub is unreachable; the oldest go first.
pub const MAX_SPOOLED: usize = 500;

/// How bad a problem is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth knowing, nothing broken.
    Info,
    /// Degraded but working.
    Warning,
    /// A feature failed.
    Error,
    /// The agent or a module cannot do its job (also: update rolled back).
    Critical,
}

impl Severity {
    /// Wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
            Severity::Critical => "critical",
        }
    }

    fn parse(s: &str) -> Severity {
        match s {
            "critical" => Severity::Critical,
            "error" => Severity::Error,
            "warning" => Severity::Warning,
            _ => Severity::Info,
        }
    }
}

/// One coalesced problem.
#[derive(Clone, Debug, PartialEq)]
pub struct ProblemReport {
    /// `hex(sha256(module|code|message without digits))[0..16]`.
    pub fingerprint: String,
    /// Severity (the highest seen for this fingerprint).
    pub severity: Severity,
    /// Module name, or `"agent"` for the runtime itself.
    pub module: String,
    /// Stable machine code, e.g. `sql_login_failed`.
    pub code: String,
    /// Human message (latest occurrence).
    pub message: String,
    /// The command or operation that failed (`update.install`, `hub.heartbeat`, or a hub
    /// command kind such as `restart_module`); empty when no command was running.
    pub command: String,
    /// Extra facts (latest occurrence). Never secrets.
    pub context: Map,
    /// First occurrence, Unix seconds.
    pub first_seen: u64,
    /// Latest occurrence, Unix seconds.
    pub last_seen: u64,
    /// Occurrences since the report was last sent.
    pub count: u64,
}

/// The fingerprint of a problem: digits are removed from the message so `timeout after 31s`
/// and `timeout after 30s` coalesce.
pub fn fingerprint(module: &str, code: &str, message: &str) -> String {
    let msg: String = message.chars().filter(|c| !c.is_ascii_digit()).collect();
    let h = Sha256::digest(format!("{module}|{code}|{msg}"));
    dl_encoding::hex::encode(&h[..8])
}

impl ProblemReport {
    /// A new report seen once at `now`.
    pub fn new(
        severity: Severity,
        module: &str,
        code: &str,
        message: &str,
        now: u64,
    ) -> ProblemReport {
        ProblemReport {
            fingerprint: fingerprint(module, code, message),
            severity,
            module: module.to_string(),
            code: code.to_string(),
            message: message.to_string(),
            command: String::new(),
            context: Map::new(),
            first_seen: now,
            last_seen: now,
            count: 1,
        }
    }

    /// Names the command or operation that failed (kept to 100 characters).
    pub fn with_command(mut self, command: &str) -> ProblemReport {
        self.command = command.chars().take(MAX_COMMAND_CHARS).collect();
        self
    }

    /// Adds a context fact.
    pub fn with(mut self, key: &str, value: impl Into<Value>) -> ProblemReport {
        self.context.insert(key, value);
        self
    }

    /// Wire JSON, with the agent facts the hub expects on every report.
    pub fn to_json(&self, agent_version: &str, os: &str, hostname: &str) -> Value {
        json!({
            "fingerprint": self.fingerprint, "severity": self.severity.as_str(),
            "module": self.module, "code": self.code, "command": self.command,
            "message": self.message,
            "context": Value::Object(self.context.clone()),
            "first_seen": self.first_seen, "last_seen": self.last_seen, "count": self.count,
            "agent_version": agent_version, "os": os, "hostname": hostname,
        })
    }

    fn to_disk(&self) -> Value {
        self.to_json("0.0.0", "", "")
    }

    fn from_disk(v: &Value) -> Option<ProblemReport> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let n = |k: &str| v.get(k).and_then(Value::as_u64);
        Some(ProblemReport {
            fingerprint: s("fingerprint")?,
            severity: Severity::parse(&s("severity")?),
            module: s("module")?,
            code: s("code")?,
            message: s("message")?,
            // Spools written before `command` existed have none.
            command: s("command").unwrap_or_default(),
            context: v
                .get("context")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
            first_seen: n("first_seen")?,
            last_seen: n("last_seen")?,
            count: n("count")?.max(1),
        })
    }
}

/// A shared, bounded queue of unsent reports, persisted after every change.
#[derive(Clone, Debug)]
pub struct Spool {
    inner: Arc<Mutex<Vec<ProblemReport>>>,
    path: Option<PathBuf>,
}

impl Spool {
    /// A spool kept only in memory (tests, `run --once`).
    pub fn in_memory() -> Spool {
        Spool {
            inner: Arc::default(),
            path: None,
        }
    }

    /// Opens `<dir>/reports.json`. A corrupt file is set aside, not fatal: losing old
    /// reports must never stop the agent.
    pub fn open(path: PathBuf) -> Spool {
        let items = match store::read_json(&path) {
            Ok(Some(v)) => v
                .as_array()
                .map(|a| a.iter().filter_map(ProblemReport::from_disk).collect())
                .unwrap_or_default(),
            Ok(None) => Vec::new(),
            Err(_) => {
                let mut bad = path.clone().into_os_string();
                bad.push(".corrupt");
                let _ = std::fs::rename(&path, bad); // keep it for diagnostics
                Vec::new()
            }
        };
        Spool {
            inner: Arc::new(Mutex::new(items)),
            path: Some(path),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<ProblemReport>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records a problem, coalescing with an unsent report of the same fingerprint.
    pub fn record(&self, r: ProblemReport) -> Result<(), Error> {
        let mut items = self.lock();
        match items.iter_mut().find(|x| x.fingerprint == r.fingerprint) {
            Some(x) => {
                x.count += r.count;
                x.last_seen = x.last_seen.max(r.last_seen);
                x.severity = x.severity.max(r.severity);
                x.message = r.message;
                x.context = r.context;
            }
            None => {
                items.push(r);
                if items.len() > MAX_SPOOLED {
                    let extra = items.len() - MAX_SPOOLED;
                    items.drain(..extra);
                }
            }
        }
        self.persist(&items)
    }

    /// The oldest `max` unsent reports (they stay queued until [`Spool::ack`]).
    pub fn peek(&self, max: usize) -> Vec<ProblemReport> {
        self.lock().iter().take(max).cloned().collect()
    }

    /// Drops reports the hub accepted. A report that coalesced more occurrences while the
    /// upload was in flight keeps only the new ones.
    pub fn ack(&self, sent: &[ProblemReport]) -> Result<(), Error> {
        let mut items = self.lock();
        for s in sent {
            if let Some(i) = items.iter().position(|x| x.fingerprint == s.fingerprint) {
                if items[i].count <= s.count {
                    items.remove(i);
                } else {
                    items[i].count -= s.count;
                    items[i].first_seen = s.last_seen;
                }
            }
        }
        self.persist(&items)
    }

    /// Unsent reports.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether nothing is waiting.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn persist(&self, items: &[ProblemReport]) -> Result<(), Error> {
        match &self.path {
            Some(p) => store::write_json(
                p,
                &Value::Array(items.iter().map(ProblemReport::to_disk).collect()),
            ),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testdir::TestDir;

    #[test]
    fn fingerprint_ignores_digits_and_has_16_hex() {
        let a = fingerprint("pda-sync", "timeout", "timed out after 30s");
        assert_eq!(
            a,
            fingerprint("pda-sync", "timeout", "timed out after 125s")
        );
        assert_ne!(a, fingerprint("einvoice", "timeout", "timed out after 30s"));
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn records_coalesce_and_ack_keeps_new_occurrences() {
        let s = Spool::in_memory();
        s.record(ProblemReport::new(Severity::Warning, "m", "c", "x 1", 10))
            .unwrap();
        s.record(ProblemReport::new(Severity::Error, "m", "c", "x 2", 20))
            .unwrap();
        let sent = s.peek(10);
        assert_eq!(sent.len(), 1);
        assert_eq!(
            (sent[0].count, sent[0].severity, sent[0].last_seen),
            (2, Severity::Error, 20)
        );
        s.record(ProblemReport::new(Severity::Error, "m", "c", "x 3", 30))
            .unwrap();
        s.ack(&sent).unwrap();
        assert_eq!(s.peek(10)[0].count, 1);
        s.ack(&s.peek(10)).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn spool_survives_restart_and_is_bounded() {
        let d = TestDir::new("spool");
        let p = d.0.join("reports.json");
        let s = Spool::open(p.clone());
        for i in 0..(MAX_SPOOLED + 5) {
            let r = ProblemReport::new(Severity::Info, "m", &format!("c{i}"), "y", i as u64);
            s.record(r).unwrap();
        }
        assert_eq!(s.len(), MAX_SPOOLED);
        let again = Spool::open(p);
        assert_eq!(again.len(), MAX_SPOOLED);
        assert_eq!(again.peek(1)[0].code, "c5", "oldest dropped first");
    }

    #[test]
    fn command_is_on_the_wire_survives_the_spool_and_old_spools_read() {
        let r = ProblemReport::new(Severity::Error, "agent", "update_failed", "bad hash", 5)
            .with_command("update.install");
        let wire = r.to_json("1.4.0", "windows", "FO");
        assert_eq!(wire["command"].as_str(), Some("update.install"));
        assert_eq!(wire["code"].as_str(), Some("update_failed"));
        assert!(wire["context"].is_object());
        let none = ProblemReport::new(Severity::Info, "m", "c", "x", 1).to_json("1", "", "");
        assert_eq!(
            none["command"].as_str(),
            Some(""),
            "the field is always there"
        );
        let long =
            ProblemReport::new(Severity::Info, "m", "c", "x", 1).with_command(&"y".repeat(300));
        assert_eq!(long.command.len(), MAX_COMMAND_CHARS);

        let d = TestDir::new("spool-command");
        let p = d.0.join("reports.json");
        Spool::open(p.clone()).record(r).unwrap();
        assert_eq!(Spool::open(p.clone()).peek(1)[0].command, "update.install");
        // A spool written before `command` existed still loads, with an empty command.
        std::fs::write(
            &p,
            r#"[{"fingerprint":"ab","severity":"error","module":"m","code":"c","message":"x","context":{},"first_seen":1,"last_seen":1,"count":1}]"#,
        )
        .unwrap();
        assert_eq!(Spool::open(p).peek(1)[0].command, "");
    }

    #[test]
    fn corrupt_spool_is_set_aside() {
        let d = TestDir::new("spool-bad");
        let p = d.0.join("reports.json");
        std::fs::write(&p, "[{").unwrap();
        assert!(Spool::open(p).is_empty());
        assert!(d.0.join("reports.json.corrupt").exists());
    }
}
