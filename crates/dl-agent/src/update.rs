//! Self-update: decide, stage, swap, probation and rollback (PROTOCOL.md §6).

use std::fs;
use std::path::{Path, PathBuf};

use dl_hash::{Digest, Sha256};
use dl_json::{Value, json};

use crate::Error;
use crate::channel::in_rollout;
use crate::error::io;
use crate::manifest::{Artifact, Manifest};
use crate::store;
use crate::version::Version;

/// Exit code that tells the service wrapper "restart me, I installed an update".
pub const RESTART_EXIT_CODE: i32 = 75;
/// Starts without a commit after which the update is rolled back.
pub const MAX_PROBATION_BOOTS: u32 = 3;
/// Window over which failed attempts of one version are counted for the short cap.
pub const FAILURE_WINDOW_SECONDS: u64 = 24 * 3600;
/// Failed attempts of one version inside [`FAILURE_WINDOW_SECONDS`] before it waits.
pub const MAX_FAILURES_PER_WINDOW: usize = 3;
/// Failed attempts of one version ever before it stops until `retry_update`.
pub const MAX_FAILURES_LIFETIME: usize = 9;

/// An update waiting to prove itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    /// Version that was installed.
    pub version: Version,
    /// Version it replaced.
    pub previous_version: Version,
    /// Unix time the probation ends.
    pub probation_until: u64,
    /// Starts since the swap.
    pub boots: u32,
}

/// Persistent update state (`update.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateState {
    /// Newest `published_at` accepted per channel, against replayed old manifests.
    pub last_published: Vec<(String, u64)>,
    /// Update in probation.
    pub pending: Option<Pending>,
    /// Failed attempts (download, verification, install or rollback) as `(version, time)`,
    /// at most [`MAX_FAILURES_LIFETIME`] per version.
    pub failures: Vec<(Version, u64)>,
}

/// What to do with a manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Already on this version or newer.
    UpToDate,
    /// The agent is outside the stable rollout percentage for now.
    NotInRollout,
    /// Install this artifact.
    Install(Artifact, Version),
    /// Refused, with the reason (reported, never installed).
    Refused(String),
    /// Not now, with the reason: tried again at a later check, nothing is reported.
    Deferred(String),
}

impl UpdateState {
    fn last_for(&self, channel: &str) -> u64 {
        self.last_published
            .iter()
            .find(|(c, _)| c == channel)
            .map_or(0, |(_, t)| *t)
    }

    /// Failed attempts of `v`: `(inside the last 24 h at now, ever)`.
    pub fn failures_of(&self, v: &Version, now: u64) -> (usize, usize) {
        let mine = || self.failures.iter().filter(|(f, _)| f == v);
        let recent = mine()
            .filter(|(_, at)| at.saturating_add(FAILURE_WINDOW_SECONDS) > now)
            .count();
        (recent, mine().count())
    }

    /// Records a failed attempt of `v`; keeps the newest [`MAX_FAILURES_LIFETIME`] per version.
    pub fn record_failure(&mut self, v: &Version, now: u64) {
        self.failures.push((v.clone(), now));
        while self.failures.iter().filter(|(f, _)| f == v).count() > MAX_FAILURES_LIFETIME {
            if let Some(i) = self.failures.iter().position(|(f, _)| f == v) {
                self.failures.remove(i);
            }
        }
    }

    /// Versions with failed attempts, oldest first, each once.
    pub fn failed_versions(&self) -> Vec<Version> {
        let mut out: Vec<Version> = Vec::new();
        for (v, _) in &self.failures {
            if !out.contains(v) {
                out.push(v.clone());
            }
        }
        out
    }

    /// Remembers that a manifest was accepted.
    pub fn accept(&mut self, m: &Manifest) {
        let ch = m.channel.as_str();
        match self.last_published.iter_mut().find(|(c, _)| c == ch) {
            Some(e) => e.1 = e.1.max(m.published_at),
            None => self.last_published.push((ch.to_string(), m.published_at)),
        }
    }

    /// Decides about a manifest that already passed signature verification.
    pub fn decide(
        &self,
        m: &Manifest,
        current: &Version,
        agent_id: &str,
        target: &str,
        now: u64,
    ) -> Decision {
        if m.published_at < self.last_for(m.channel.as_str()) {
            return Decision::Refused(format!(
                "manifest {} is older than one already accepted",
                m.version
            ));
        }
        if m.version == *current || (m.version < *current && !m.allow_downgrade) {
            return Decision::UpToDate;
        }
        if let Some(min) = &m.min_version
            && current < min
        {
            return Decision::Refused(format!(
                "{current} is below the minimum {min} for {}",
                m.version
            ));
        }
        let (recent, ever) = self.failures_of(&m.version, now);
        if ever >= MAX_FAILURES_LIFETIME {
            return Decision::Refused(format!(
                "{} failed {ever} times on this machine; send retry_update to try it again",
                m.version
            ));
        }
        if recent >= MAX_FAILURES_PER_WINDOW {
            return Decision::Deferred(format!(
                "{} failed {recent} times in the last 24 h",
                m.version
            ));
        }
        if !in_rollout(
            agent_id,
            &m.product,
            &m.version.to_string(),
            m.rollout_percent,
        ) {
            return Decision::NotInRollout;
        }
        match m.artifact_for(target) {
            Some(a) => Decision::Install(a.clone(), m.version.clone()),
            None => Decision::Refused(format!("{} has no build for {target}", m.version)),
        }
    }

    /// Loads `update.json` (missing → default).
    pub fn load(dir: &Path) -> Result<UpdateState, Error> {
        let Some(v) = store::read_json(&dir.join("update.json"))? else {
            return Ok(UpdateState::default());
        };
        let mut s = UpdateState::default();
        if let Some(o) = v.get("last_published").and_then(Value::as_object) {
            for (k, t) in o.iter() {
                s.last_published
                    .push((k.to_string(), t.as_u64().unwrap_or(0)));
            }
        }
        if let Some(f) = v.get("failures").and_then(Value::as_array) {
            for e in f {
                let ver = e.get("version").and_then(Value::as_str).map(str::parse);
                if let Some(Ok(ver)) = ver {
                    s.failures
                        .push((ver, e.get("at").and_then(Value::as_u64).unwrap_or(0)));
                }
            }
        }
        // Older state files refused a rolled-back version forever: count it as one old failure.
        if let Some(f) = v.get("failed").and_then(Value::as_array) {
            for ver in f.iter().filter_map(Value::as_str) {
                if let Ok(ver) = ver.parse() {
                    s.failures.push((ver, 0));
                }
            }
        }
        if let Some(p) = v.get("pending").filter(|p| p.is_object()) {
            let ver = |k: &str| {
                p.get(k)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .parse::<Version>()
            };
            s.pending = Some(Pending {
                version: ver("version")?,
                previous_version: ver("previous_version")?,
                probation_until: p
                    .get("probation_until")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                boots: p.get("boots").and_then(Value::as_u64).unwrap_or(0) as u32,
            });
        }
        Ok(s)
    }

    /// Saves `update.json` atomically.
    pub fn save(&self, dir: &Path) -> Result<(), Error> {
        let mut last = dl_json::Map::new();
        for (c, t) in &self.last_published {
            last.insert(c.clone(), *t);
        }
        let failures: Vec<Value> = self
            .failures
            .iter()
            .map(|(v, at)| json!({"version": v.to_string(), "at": *at}))
            .collect();
        let pending = match &self.pending {
            Some(p) => {
                json!({"version": p.version.to_string(), "previous_version": p.previous_version.to_string(),
                              "probation_until": p.probation_until, "boots": u64::from(p.boots)})
            }
            None => Value::Null,
        };
        let v = json!({"last_published": Value::Object(last), "failures": failures, "pending": pending});
        store::write_json(&dir.join("update.json"), &v)
    }
}

/// When the site lets updates install: the hub config's `update` section,
/// `{"auto": true, "window": {"from": "22:00", "to": "05:00", "utc_offset_minutes": 420}}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// `false`: only releases marked `required` install by themselves.
    pub auto: bool,
    /// Local time span updates may install in; `None` means any time.
    pub window: Option<Window>,
}

impl Default for Policy {
    fn default() -> Policy {
        Policy {
            auto: true,
            window: None,
        }
    }
}

/// A daily span of local time, `from` inclusive, `to` exclusive; may cross midnight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// Minutes after local midnight the window opens.
    pub from: u32,
    /// Minutes after local midnight it closes.
    pub to: u32,
    /// Local time minus UTC, in minutes (Vietnam: 420).
    pub utc_offset_minutes: i32,
}

/// Default offset when the window does not name one (UTC+7).
pub const DEFAULT_UTC_OFFSET_MINUTES: i32 = 420;

fn minutes_of(hhmm: &str) -> Option<u32> {
    let (h, m) = hhmm.split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

impl Window {
    /// Parses `{"from": "HH:MM", "to": "HH:MM", "utc_offset_minutes"?: n}`.
    pub fn parse(v: &Value) -> Result<Window, Error> {
        let t = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .and_then(minutes_of)
                .ok_or_else(|| Error::Config(format!("update window: {k} must be HH:MM")))
        };
        let offset = match v.get("utc_offset_minutes") {
            None | Some(Value::Null) => DEFAULT_UTC_OFFSET_MINUTES,
            Some(o) => o
                .as_i64()
                .filter(|o| o.abs() <= 14 * 60)
                .ok_or_else(|| Error::Config("update window: bad utc_offset_minutes".into()))?
                as i32, // bounded above
        };
        Ok(Window {
            from: t("from")?,
            to: t("to")?,
            utc_offset_minutes: offset,
        })
    }

    /// Whether Unix time `now` falls inside the window. `from == to` means all day.
    pub fn contains(&self, now: u64) -> bool {
        let local = now as i64 + i64::from(self.utc_offset_minutes) * 60;
        let minute = (local.rem_euclid(86_400) / 60) as u32;
        match self.from.cmp(&self.to) {
            std::cmp::Ordering::Equal => true,
            std::cmp::Ordering::Less => (self.from..self.to).contains(&minute),
            std::cmp::Ordering::Greater => minute >= self.from || minute < self.to,
        }
    }

    fn label(&self) -> String {
        let f = |m: u32| format!("{:02}:{:02}", m / 60, m % 60);
        format!("{}–{}", f(self.from), f(self.to))
    }
}

impl Policy {
    /// The policy in a config body; a malformed section falls back to the default, with
    /// the error so it can be reported.
    pub fn from_config(body: &Value) -> (Policy, Option<Error>) {
        let Some(u) = body.get("update").filter(|u| u.is_object()) else {
            return (Policy::default(), None);
        };
        let auto = u.get("auto").and_then(Value::as_bool).unwrap_or(true);
        match u.get("window") {
            None | Some(Value::Null) => (Policy { auto, window: None }, None),
            Some(w) => match Window::parse(w) {
                Ok(w) => (
                    Policy {
                        auto,
                        window: Some(w),
                    },
                    None,
                ),
                Err(e) => (Policy { auto, window: None }, Some(e)),
            },
        }
    }

    /// `Some(reason)` when an install of `m` must wait. `forced` (the `update_now` command)
    /// skips the policy; `required` releases skip `auto: false`; `urgent` ones skip the window.
    pub fn hold(&self, m: &Manifest, now: u64, forced: bool) -> Option<String> {
        if forced {
            return None;
        }
        if !self.auto && !m.required {
            return Some("automatic updates are off on this site".into());
        }
        match &self.window {
            Some(w) if !m.urgent && !w.contains(now) => {
                Some(format!("outside the update window {}", w.label()))
            }
            _ => None,
        }
    }
}

/// Checks a downloaded artifact against its manifest entry.
pub fn verify_artifact(bytes: &[u8], a: &Artifact) -> Result<(), Error> {
    if bytes.len() as u64 != a.size {
        return Err(Error::Integrity(format!(
            "size {} ≠ {}",
            bytes.len(),
            a.size
        )));
    }
    if !dl_hash::ct_eq(&Sha256::digest(bytes), &a.sha256) {
        return Err(Error::Integrity("SHA-256 mismatch".into()));
    }
    Ok(())
}

/// Binary swapping inside the install directory: `<dir>/<exe>`, `<dir>/previous/<exe>`.
#[derive(Clone, Debug)]
pub struct Installer {
    /// The running executable.
    pub exe: PathBuf,
}

impl Installer {
    fn previous(&self) -> PathBuf {
        let dir = self.exe.parent().unwrap_or(Path::new(".")).join("previous");
        dir.join(self.exe.file_name().unwrap_or_default())
    }

    fn staged(&self) -> PathBuf {
        let mut s = self.exe.clone().into_os_string();
        s.push(".new");
        PathBuf::from(s)
    }

    /// Verifies and installs a new binary; the old one is kept for rollback. The running
    /// process keeps working (a running file can be renamed on Windows) until it exits.
    pub fn install(&self, bytes: &[u8], a: &Artifact) -> Result<(), Error> {
        verify_artifact(bytes, a)?;
        let staged = self.staged();
        io("write staged update", fs::write(&staged, bytes))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            io(
                "mark update executable",
                fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)),
            )?;
        }
        let prev = self.previous();
        if let Some(d) = prev.parent() {
            io("create previous dir", fs::create_dir_all(d))?;
        }
        let _ = fs::remove_file(&prev); // absent on the first update; a stale one is replaced
        io("keep previous binary", fs::rename(&self.exe, &prev))?;
        if let Err(e) = fs::rename(&staged, &self.exe) {
            let _ = fs::rename(&prev, &self.exe); // put the running binary back before failing
            return Err(Error::Io("activate update", e));
        }
        Ok(())
    }

    /// Restores the previous binary.
    pub fn rollback(&self) -> Result<(), Error> {
        let prev = self.previous();
        if !prev.exists() {
            return Err(Error::Config("no previous binary to roll back to".into()));
        }
        let failed = self.staged();
        let _ = fs::remove_file(&failed);
        io("set failed binary aside", fs::rename(&self.exe, &failed))?;
        io("restore previous binary", fs::rename(&prev, &self.exe))
    }
}

/// What to do at start-up about a pending update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BootAction {
    /// No update in probation.
    Normal,
    /// Still in probation; count this start and run.
    Probation,
    /// Too many starts without a commit: roll back now.
    RollBack,
}

/// Called once at every start, before modules run; persists the boot count.
pub fn on_boot(state: &mut UpdateState, current: &Version) -> BootAction {
    let Some(p) = state.pending.as_mut() else {
        return BootAction::Normal;
    };
    if p.version != *current {
        // The swap did not take (or someone replaced the binary): forget it.
        state.pending = None;
        return BootAction::Normal;
    }
    p.boots += 1;
    if p.boots > MAX_PROBATION_BOOTS {
        BootAction::RollBack
    } else {
        BootAction::Probation
    }
}

/// Commits the pending update once probation has passed with a healthy agent.
pub fn try_commit(state: &mut UpdateState, now: u64, healthy: bool, hub_ok: bool) -> bool {
    match &state.pending {
        Some(p) if now >= p.probation_until && healthy && hub_ok => {
            state.pending = None;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
