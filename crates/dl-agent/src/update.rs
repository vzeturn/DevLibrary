//! Self-update: decide, stage, swap, probation and rollback (PROTOCOL.md §6).

use std::fs;
use std::path::{Path, PathBuf};

use dl_hash::{Digest, Sha256};
use dl_json::{Value, json};

use crate::channel::in_rollout;
use crate::error::io;
use crate::manifest::{Artifact, Manifest};
use crate::store;
use crate::version::Version;
use crate::Error;

/// Exit code that tells the service wrapper "restart me, I installed an update".
pub const RESTART_EXIT_CODE: i32 = 75;
/// Starts without a commit after which the update is rolled back.
pub const MAX_PROBATION_BOOTS: u32 = 3;

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
    /// Versions that were rolled back; never installed again.
    pub failed: Vec<Version>,
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
}

impl UpdateState {
    fn last_for(&self, channel: &str) -> u64 {
        self.last_published.iter().find(|(c, _)| c == channel).map_or(0, |(_, t)| *t)
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
    pub fn decide(&self, m: &Manifest, current: &Version, agent_id: &str, target: &str) -> Decision {
        if m.published_at < self.last_for(m.channel.as_str()) {
            return Decision::Refused(format!("manifest {} is older than one already accepted", m.version));
        }
        if self.failed.contains(&m.version) {
            return Decision::Refused(format!("{} was rolled back on this machine", m.version));
        }
        if m.version == *current || (m.version < *current && !m.allow_downgrade) {
            return Decision::UpToDate;
        }
        if let Some(min) = &m.min_version {
            if current < min {
                return Decision::Refused(format!("{current} is below the minimum {min} for {}", m.version));
            }
        }
        if !in_rollout(agent_id, &m.product, &m.version.to_string(), m.rollout_percent) {
            return Decision::NotInRollout;
        }
        match m.artifact_for(target) {
            Some(a) => Decision::Install(a.clone(), m.version.clone()),
            None => Decision::Refused(format!("{} has no build for {target}", m.version)),
        }
    }

    /// Loads `update.json` (missing → default).
    pub fn load(dir: &Path) -> Result<UpdateState, Error> {
        let Some(v) = store::read_json(&dir.join("update.json"))? else { return Ok(UpdateState::default()) };
        let mut s = UpdateState::default();
        if let Some(o) = v.get("last_published").and_then(Value::as_object) {
            for (k, t) in o.iter() {
                s.last_published.push((k.to_string(), t.as_u64().unwrap_or(0)));
            }
        }
        if let Some(f) = v.get("failed").and_then(Value::as_array) {
            s.failed = f.iter().filter_map(Value::as_str).filter_map(|x| x.parse().ok()).collect();
        }
        if let Some(p) = v.get("pending").filter(|p| p.is_object()) {
            let ver = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").parse::<Version>();
            s.pending = Some(Pending {
                version: ver("version")?,
                previous_version: ver("previous_version")?,
                probation_until: p.get("probation_until").and_then(Value::as_u64).unwrap_or(0),
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
        let failed: Vec<String> = self.failed.iter().map(Version::to_string).collect();
        let pending = match &self.pending {
            Some(p) => json!({"version": p.version.to_string(), "previous_version": p.previous_version.to_string(),
                              "probation_until": p.probation_until, "boots": u64::from(p.boots)}),
            None => Value::Null,
        };
        let v = json!({"last_published": Value::Object(last), "failed": failed, "pending": pending});
        store::write_json(&dir.join("update.json"), &v)
    }
}

/// Checks a downloaded artifact against its manifest entry.
pub fn verify_artifact(bytes: &[u8], a: &Artifact) -> Result<(), Error> {
    if bytes.len() as u64 != a.size {
        return Err(Error::Integrity(format!("size {} ≠ {}", bytes.len(), a.size)));
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
            io("mark update executable", fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)))?;
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
    let Some(p) = state.pending.as_mut() else { return BootAction::Normal };
    if p.version != *current {
        // The swap did not take (or someone replaced the binary): forget it.
        state.pending = None;
        return BootAction::Normal;
    }
    p.boots += 1;
    if p.boots > MAX_PROBATION_BOOTS { BootAction::RollBack } else { BootAction::Probation }
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
