//! The `service` command: stop or start a Windows service, or change its startup type, from
//! a hub command that is signed offline (RT-1105, part of RT-1054).
//!
//! Stopping a service is as strong as running code, so the hub alone cannot ask for it: the
//! command must carry an Ed25519 signature made with a release key this binary was built
//! with (the same keys that sign manifests and SQL bundles). An unsigned, badly signed,
//! expired or tampered command is refused before the service manager is touched.
//!
//! ```json
//! {"id":"c-17","kind":"service","expires_at":1763500600,
//!  "args":{"action":"stop","service":"Spooler","key_id":"rel-2026","signature":"<b64>"}}
//! ```
//!
//! `action` is `stop`, `start` or `set_startup` (then `startup` is `automatic`, `manual` or
//! `disabled`). The signature covers the compact JSON
//! `{"id","kind","action","service","startup"?,"expires_at"}` as `dl_json` writes it, so it is
//! bound to one command id and one expiry and cannot be replayed for another service.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dl_encoding::base64::STANDARD as B64;
use dl_json::{Value, json};

use crate::command::Command;
use crate::manifest::TrustedKeys;

/// The command kind, answered by the `sys` module.
pub const KIND: &str = "service";

/// Longest a signed command may live: a leaked signature stops working within a day.
pub const MAX_LIFETIME: Duration = Duration::from_secs(24 * 3600);

/// How long a stop or start waits for the service to reach its state.
const WAIT: Duration = Duration::from_secs(30);

/// Services no command may touch, whatever the signature says (compared case-insensitively):
/// the agent itself, whose stop would end the very session that asked for it.
pub const PROTECTED: &[&str] = &["dl-agent"];

/// What the command asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Stop the service and wait for it.
    Stop,
    /// Start the service and wait for it.
    Start,
    /// Change the startup type.
    SetStartup(Startup),
}

/// A startup type a command may set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Startup {
    /// At boot.
    Automatic,
    /// On demand.
    Manual,
    /// Never.
    Disabled,
}

/// The service manager, behind a trait so the tests need no Windows.
pub trait ServiceControl: Send + Sync {
    /// Stops `name` and waits until it is stopped.
    fn stop(&self, name: &str) -> Result<(), String>;
    /// Starts `name` and waits until it runs.
    fn start(&self, name: &str) -> Result<(), String>;
    /// Sets the startup type of `name`.
    fn set_startup(&self, name: &str, startup: Startup) -> Result<(), String>;
}

/// The real service control manager (an error on other platforms).
#[derive(Debug, Default)]
pub struct Scm;

impl ServiceControl for Scm {
    fn stop(&self, name: &str) -> Result<(), String> {
        let svc = open(name)?;
        svc.stop_and_wait(WAIT).map_err(|e| e.to_string())
    }

    fn start(&self, name: &str) -> Result<(), String> {
        use dl_winsvc::scm::State;
        let svc = open(name)?;
        if svc.status().map_err(|e| e.to_string())?.state == State::Running {
            return Ok(());
        }
        svc.start(&[]).map_err(|e| e.to_string())?;
        svc.wait_for_state(State::Running, WAIT)
            .map_err(|e| e.to_string())
    }

    fn set_startup(&self, name: &str, startup: Startup) -> Result<(), String> {
        use dl_winsvc::scm::StartType;
        let svc = open(name)?;
        let kind = match startup {
            Startup::Automatic => StartType::Automatic,
            Startup::Manual => StartType::Manual,
            Startup::Disabled => StartType::Disabled,
        };
        svc.set_start_type(kind).map_err(|e| e.to_string())
    }
}

fn open(name: &str) -> Result<dl_winsvc::scm::Service, String> {
    dl_winsvc::scm::Manager::connect()
        .and_then(|m| m.open(name))
        .map_err(|e| format!("cannot open service {name}: {e}"))
}

/// Verifies and runs one `service` command: `(ok, output)` for the command result.
pub fn execute(
    cmd: &Command,
    trusted: &TrustedKeys,
    control: &dyn ServiceControl,
    own_service: Option<&str>,
    now: SystemTime,
) -> (bool, String) {
    let request = match verify(cmd, trusted, own_service, now) {
        Ok(r) => r,
        Err(why) => return (false, format!("service: refused: {why}")),
    };
    let name = &request.service;
    let done = match request.action {
        Action::Stop => control.stop(name).map(|()| format!("{name}: stopped")),
        Action::Start => control.start(name).map(|()| format!("{name}: running")),
        Action::SetStartup(s) => control
            .set_startup(name, s)
            .map(|()| format!("{name}: startup {}", startup_word(s))),
    };
    match done {
        Ok(out) => (true, out),
        Err(e) => (false, format!("service: {name}: {e}")),
    }
}

/// A command that passed every check.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    /// What to do.
    pub action: Action,
    /// The service's short name.
    pub service: String,
}

fn startup_word(s: Startup) -> &'static str {
    match s {
        Startup::Automatic => "automatic",
        Startup::Manual => "manual",
        Startup::Disabled => "disabled",
    }
}

/// The bytes the signature covers.
pub fn signing_bytes(cmd: &Command) -> Option<Vec<u8>> {
    let a = &cmd.args;
    let mut body = json!({
        "id": cmd.id,
        "kind": KIND,
        "action": a.get("action").and_then(Value::as_str)?,
        "service": a.get("service").and_then(Value::as_str)?,
    });
    if let Some(o) = body.as_object_mut() {
        if let Some(s) = a.get("startup").and_then(Value::as_str) {
            o.insert("startup", s);
        }
        o.insert("expires_at", cmd.expires_at);
    }
    Some(dl_json::to_string(&body).into_bytes())
}

/// Checks the signature, the expiry and the arguments; nothing is run here.
pub fn verify(
    cmd: &Command,
    trusted: &TrustedKeys,
    own_service: Option<&str>,
    now: SystemTime,
) -> Result<Request, String> {
    let a = &cmd.args;
    let key_id = a.get("key_id").and_then(Value::as_str);
    let sig_b64 = a.get("signature").and_then(Value::as_str);
    let (Some(key_id), Some(sig_b64)) = (key_id, sig_b64) else {
        return Err("unsigned command".into());
    };
    let key = trusted
        .get(key_id)
        .ok_or_else(|| format!("unknown key {key_id:?}"))?;
    let sig: [u8; dl_ed25519::SIGNATURE_LEN] = B64
        .decode(sig_b64)
        .ok()
        .and_then(|raw| raw.try_into().ok())
        .ok_or("malformed signature")?;
    let bytes = signing_bytes(cmd).ok_or("missing action or service")?;
    key.verify(&bytes, &sig).map_err(|_| "bad signature")?;

    let now_s = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if cmd.expires_at == u64::MAX {
        return Err("no expiry".into());
    }
    if cmd.expires_at <= now_s {
        return Err("expired".into());
    }
    if cmd.expires_at > now_s + MAX_LIFETIME.as_secs() {
        return Err("expiry too far ahead".into());
    }

    let service = a.get("service").and_then(Value::as_str).unwrap_or_default();
    if service.is_empty()
        || service.len() > 256
        || service
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err("bad service name".into());
    }
    if PROTECTED
        .iter()
        .copied()
        .chain(own_service)
        .any(|p| p.eq_ignore_ascii_case(service))
    {
        return Err(format!("{service} is protected"));
    }
    let action = match a.get("action").and_then(Value::as_str).unwrap_or_default() {
        "stop" => Action::Stop,
        "start" => Action::Start,
        "set_startup" => Action::SetStartup(match a.get("startup").and_then(Value::as_str) {
            Some("automatic") => Startup::Automatic,
            Some("manual") => Startup::Manual,
            Some("disabled") => Startup::Disabled,
            _ => return Err("startup must be automatic, manual or disabled".into()),
        }),
        other => return Err(format!("unknown action {other:?}")),
    };
    Ok(Request {
        action,
        service: service.to_string(),
    })
}

#[cfg(test)]
#[path = "service_cmd_tests.rs"]
mod tests;
