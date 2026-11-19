//! The agent's identity at the hub: `backup-enroll` trades the tenant's enroll key for an
//! agent id and API token (`POST /api/agent/self-register`) and keeps them sealed in
//! `<module data>/backup/enrollment.enc`. The Go agent's hwid is replaced by a random id
//! made at the first enrollment and kept across re-enrollments, so the hub still sees one
//! machine.
//!
//! On a machine the Go SRBackup agent ran on, its identity is adopted instead (see
//! [`goagent`]): the hub's servers, schedules and history hang off that agent id, so a new
//! registration would orphan them.

pub mod goagent;
pub mod tool;

#[cfg(test)]
mod adopt_tests;
#[cfg(test)]
mod goagent_tests;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use dl_json::{Map, Value};

use self::goagent::{GoAgent, GoSource};
use crate::Error;
use crate::hub::{HubClient, validate_origin};
use crate::seal;
use crate::state::{now_ms, s, state_dir, u};

/// The sealed enrollment file inside the backup state directory.
pub const ENROLLMENT_FILE: &str = "enrollment.enc";

/// A stored enrollment.
#[derive(Clone, PartialEq, Eq)]
pub struct Enrollment {
    /// The hub origin, e.g. `https://backupapi.sr.io.vn`.
    pub hub_url: String,
    /// The agent id the hub gave.
    pub agent_id: String,
    /// The agent API token (a secret).
    pub token: String,
    /// The machine id sent as `hwid`.
    pub hwid: String,
    /// When it was made (ms).
    pub enrolled_at: u64,
}

impl std::fmt::Debug for Enrollment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Enrollment")
            .field("hub_url", &self.hub_url)
            .field("agent_id", &self.agent_id)
            .finish_non_exhaustive()
    }
}

/// Where the enrollment lives for a module data directory.
pub fn enrollment_path(module_data_dir: &Path) -> PathBuf {
    state_dir(module_data_dir).join(ENROLLMENT_FILE)
}

impl Enrollment {
    /// Reads the enrollment; `Ok(None)` when there is none (or it lacks a token).
    pub fn load(module_data_dir: &Path) -> Result<Option<Enrollment>, Error> {
        let Some(doc) = seal::read_json(&enrollment_path(module_data_dir))? else {
            return Ok(None);
        };
        let e = Enrollment {
            hub_url: s(&doc, "hubUrl"),
            agent_id: s(&doc, "agentId"),
            token: s(&doc, "token"),
            hwid: s(&doc, "hwid"),
            enrolled_at: u(&doc, "enrolledAt"),
        };
        if e.hub_url.is_empty() || e.token.is_empty() {
            return Ok(None);
        }
        Ok(Some(e))
    }

    /// Seals the enrollment to disk.
    pub fn store(&self, module_data_dir: &Path) -> Result<(), Error> {
        let mut m = Map::new();
        m.insert("hubUrl", self.hub_url.as_str());
        m.insert("agentId", self.agent_id.as_str());
        m.insert("token", self.token.as_str());
        m.insert("hwid", self.hwid.as_str());
        m.insert("enrolledAt", self.enrolled_at);
        seal::write_json(&enrollment_path(module_data_dir), &Value::Object(m))
    }
}

/// What [`enroll`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A new agent id was issued.
    Enrolled {
        /// The new agent id.
        agent_id: String,
        /// Why an existing enrollment file was replaced without `force` (it did not open).
        replaced_unreadable: Option<String>,
        /// Why a Go agent config that was found was not adopted (no secret in it).
        not_adopted: Option<String>,
    },
    /// The Go agent's agent id and API key were verified at the hub and taken over.
    Adopted {
        /// The Go agent's id, now this agent's.
        agent_id: String,
        /// Why an existing enrollment file was replaced without `force` (it did not open).
        replaced_unreadable: Option<String>,
    },
    /// An enrollment for the same hub already existed and `force` was off.
    AlreadyEnrolled {
        /// The existing agent id.
        agent_id: String,
    },
}

/// Enrolls this machine with the hub at `hub_url`.
///
/// Without `force` an existing enrollment for the same hub is kept, and one for another hub
/// is an error (re-enrolling moves the site, so it must be asked for). An enrollment file
/// that no longer opens (another machine's DPAPI blob) is replaced.
///
/// Before registering, a Go agent config from `go` for the same hub origin is adopted when
/// the hub accepts its API key; a rejected key, a config that does not open or another
/// origin falls back to self-registration. The Go `agent.json` is never written.
pub fn enroll(
    module_data_dir: &Path,
    hub_url: &str,
    enroll_key: &str,
    machine_name: &str,
    force: bool,
    go: &GoSource,
) -> Result<Outcome, Error> {
    let origin = validate_origin(hub_url)?;
    let enroll_key = enroll_key.trim();
    if enroll_key.is_empty() {
        return Err(Error::Auth("the enroll key is empty".into()));
    }
    let (previous, replaced_unreadable) = match Enrollment::load(module_data_dir) {
        Ok(p) => (p, None),
        Err(e) => (None, Some(e.to_string())),
    };
    if let Some(prev) = &previous
        && !force
    {
        if prev.hub_url == origin {
            return Ok(Outcome::AlreadyEnrolled {
                agent_id: prev.agent_id.clone(),
            });
        }
        return Err(Error::Hub(format!(
            "already enrolled with {}; pass --force to enroll with {origin}",
            prev.hub_url
        )));
    }
    let hwid = previous
        .map(|p| p.hwid)
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| dl_uuid::Uuid::new_v4().to_string());
    let not_adopted = match try_adopt(&origin, go)? {
        Adopt::Found(agent) => {
            let e = Enrollment {
                hub_url: origin,
                agent_id: agent.agent_id.clone(),
                token: agent.api_key,
                hwid,
                enrolled_at: now_ms(),
            };
            e.store(module_data_dir)?;
            return Ok(Outcome::Adopted {
                agent_id: agent.agent_id,
                replaced_unreadable,
            });
        }
        Adopt::None => None,
        Adopt::Skip(why) => Some(why),
    };
    let reg = HubClient::new(&origin, "").self_register(enroll_key, machine_name, &hwid)?;
    let e = Enrollment {
        hub_url: origin,
        agent_id: reg.agent_id.clone(),
        token: reg.token,
        hwid,
        enrolled_at: now_ms(),
    };
    e.store(module_data_dir)?;
    Ok(Outcome::Enrolled {
        agent_id: reg.agent_id,
        replaced_unreadable,
        not_adopted,
    })
}

enum Adopt {
    /// Verified at the hub.
    Found(GoAgent),
    /// No Go agent config on this machine.
    None,
    /// One was found but is not usable; the reason names no secret.
    Skip(String),
}

/// Looks for the Go agent's identity and checks it at the hub. A hub that cannot be reached
/// or answers 5xx is an error, not a fallback: registering anew would orphan the agent.
fn try_adopt(origin: &str, go: &GoSource) -> Result<Adopt, Error> {
    let agent = match goagent::find(go) {
        Ok(Some(a)) => a,
        Ok(None) => return Ok(Adopt::None),
        Err(why) => {
            return Ok(Adopt::Skip(format!(
                "the Go agent config did not open: {why}"
            )));
        }
    };
    if !goagent::same_origin(&agent.server_url, origin) {
        return Ok(Adopt::Skip(format!(
            "the Go agent in {} belongs to another hub ({})",
            agent.path.display(),
            describe_origin(&agent.server_url)
        )));
    }
    match HubClient::new(origin, &agent.api_key).check_token() {
        Ok(()) => Ok(Adopt::Found(agent)),
        Err(Error::Auth(m)) => Ok(Adopt::Skip(format!(
            "the hub rejected the Go agent {}'s key ({m})",
            agent.agent_id
        ))),
        Err(e) => Err(Error::Hub(format!(
            "could not check the Go agent {} at the hub, nothing changed: {e}",
            agent.agent_id
        ))),
    }
}

/// `scheme://host:port` of a URL, without any user info or path, for messages.
fn describe_origin(url: &str) -> String {
    match dl_http::Url::parse(url.trim()) {
        Ok(u) => format!("{}://{}:{}", u.scheme().as_str(), u.host(), u.port()),
        Err(_) => "an unparseable server_url".into(),
    }
}
