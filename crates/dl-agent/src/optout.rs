//! Local update controls (PROTOCOL.md §6.5), all in the data directory or the service's
//! environment:
//!
//! - opt-out: a `no-update` file or `DL_AGENT_DISABLE_AUTOUPDATE=1` stops the agent installing on
//!   the hub's `update_check`; an explicit `update_now` (hub command or `dl-agent update`) still
//!   runs, as the operator's override;
//! - pin: `dl-agent install x.y.z` writes `update-pin`, which holds automatic updates until
//!   `dl-agent install stable|canary` removes it;
//! - channel: `dl-agent install stable|canary` writes `update-channel`, which the agent follows
//!   instead of the channel the hub assigned (`install auto` removes it);
//! - `dl-agent update`: the CLI drops `update-now.request`, the running agent takes it within a
//!   second, runs the `update_now` path and answers in `update-now.result.json`.

use std::path::Path;

use dl_json::{Value, json};

use crate::{Error, Version, store};

/// Opt-out file name inside the data directory.
pub const FILE: &str = "no-update";
/// Environment variable that turns automatic updates off (`1` or `true`).
pub const ENV: &str = "DL_AGENT_DISABLE_AUTOUPDATE";
/// Pinned version file name inside the data directory.
pub const PIN_FILE: &str = "update-pin";
/// Local channel choice file name inside the data directory.
pub const CHANNEL_FILE: &str = "update-channel";
/// `dl-agent update` request file name inside the data directory.
pub const REQUEST_FILE: &str = "update-now.request";
/// The agent's answer to [`REQUEST_FILE`].
pub const RESULT_FILE: &str = "update-now.result.json";

/// Value of `update_blocked` in the heartbeat while the `no-update` file exists.
pub const BLOCKED: &str = "local";
/// Value of `update_blocked` while [`ENV`] is set.
pub const BLOCKED_ENV: &str = "env";
/// Value of `update_blocked` while a version is pinned.
pub const BLOCKED_PIN: &str = "pinned";

/// Whether automatic updates are off for any local reason.
pub fn blocked(data_dir: &Path) -> bool {
    reason(data_dir).is_some()
}

/// Why the agent does not update by itself, when it does not.
pub fn reason(data_dir: &Path) -> Option<&'static str> {
    reason_from(data_dir, std::env::var(ENV).ok().as_deref())
}

/// [`reason`] with the value of [`ENV`] given (tests).
pub fn reason_from(data_dir: &Path, env: Option<&str>) -> Option<&'static str> {
    if data_dir.join(FILE).exists() {
        Some(BLOCKED)
    } else if env.is_some_and(env_on) {
        Some(BLOCKED_ENV)
    } else if pin(data_dir).is_some() {
        Some(BLOCKED_PIN)
    } else {
        None
    }
}

fn env_on(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes")
}

/// One sentence for reports and the self-test, for a [`reason`] value.
pub fn describe(data_dir: &Path, reason: &str) -> String {
    match reason {
        BLOCKED_ENV => format!("{ENV} is set for the service"),
        BLOCKED_PIN => format!(
            "pinned to {} (dl-agent install x.y.z)",
            pin(data_dir).map_or_else(|| "?".into(), |v| v.to_string())
        ),
        _ => format!("the {FILE} file exists in the data directory"),
    }
}

/// What lifts it, for a [`reason`] value.
pub fn remedy(reason: &str) -> String {
    match reason {
        BLOCKED_ENV => format!("remove {ENV} from the service's environment and restart it"),
        BLOCKED_PIN => "run dl-agent install stable (or canary) to follow a channel again".into(),
        _ => format!("delete {FILE} in the data directory to let the hub's updates through"),
    }
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let t = text.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn write_or_remove(path: &Path, value: Option<&str>) -> Result<(), Error> {
    match value {
        Some(v) => store::write_bytes(path, format!("{v}\n").as_bytes()),
        None if path.exists() => crate::error::io("remove", std::fs::remove_file(path)),
        None => Ok(()),
    }
}

/// The pinned version, when one is pinned (an unreadable pin file counts as none).
pub fn pin(data_dir: &Path) -> Option<Version> {
    read_trimmed(&data_dir.join(PIN_FILE)).and_then(|v| v.parse().ok())
}

/// Pins `version`, or removes the pin.
pub fn set_pin(data_dir: &Path, version: Option<&Version>) -> Result<(), Error> {
    let v = version.map(Version::to_string);
    write_or_remove(&data_dir.join(PIN_FILE), v.as_deref())
}

/// The channel chosen on this machine (`stable` or `canary`), when one is.
pub fn channel(data_dir: &Path) -> Option<String> {
    read_trimmed(&data_dir.join(CHANNEL_FILE)).filter(|c| c == "stable" || c == "canary")
}

/// Chooses `channel` on this machine, or goes back to the hub's choice.
pub fn set_channel(data_dir: &Path, channel: Option<&str>) -> Result<(), Error> {
    if let Some(c) = channel
        && c != "stable"
        && c != "canary"
    {
        return Err(Error::Config(format!("unknown channel {c}")));
    }
    write_or_remove(&data_dir.join(CHANNEL_FILE), channel)
}

/// Heartbeat fields for the local controls: `update_blocked`, `update_pin`, `channel_choice`.
pub fn heartbeat_fields(data_dir: &Path) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some(why) = reason(data_dir) {
        out.push(("update_blocked", why.to_string()));
    }
    if let Some(v) = pin(data_dir) {
        out.push(("update_pin", v.to_string()));
    }
    if let Some(c) = channel(data_dir) {
        out.push(("channel_choice", c));
    }
    out
}

/// Writes the `dl-agent update` request, stamped `at`.
pub fn request_update(data_dir: &Path, at: u64) -> Result<(), Error> {
    let result = data_dir.join(RESULT_FILE);
    if result.exists() {
        crate::error::io(
            "remove the previous update result",
            std::fs::remove_file(result),
        )?;
    }
    store::write_bytes(&data_dir.join(REQUEST_FILE), format!("{at}\n").as_bytes())
}

/// Takes a pending `dl-agent update` request: `true` once, then the file is gone.
pub fn take_update_request(data_dir: &Path) -> Result<bool, Error> {
    let path = data_dir.join(REQUEST_FILE);
    if !path.exists() {
        return Ok(false);
    }
    crate::error::io("remove the update request", std::fs::remove_file(path))?;
    Ok(true)
}

/// The agent's answer to a `dl-agent update` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateResult {
    /// When the agent answered, Unix seconds.
    pub at: u64,
    /// `false`: the check or the install failed (`output` says why).
    pub ok: bool,
    /// A new build was installed; the agent restarts on it.
    pub installed: bool,
    /// The version that answered.
    pub from: String,
    /// The version installed (empty when none was).
    pub to: String,
    /// Channel checked.
    pub channel: String,
    /// What happened, in words.
    pub output: String,
}

impl UpdateResult {
    fn to_json(&self) -> Value {
        json!({"at": self.at, "ok": self.ok, "installed": self.installed, "from": self.from,
            "to": self.to, "channel": self.channel, "output": self.output})
    }

    fn from_json(v: &Value) -> Option<UpdateResult> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        Some(UpdateResult {
            at: v.get("at")?.as_u64()?,
            ok: v.get("ok")?.as_bool()?,
            installed: v.get("installed").and_then(Value::as_bool).unwrap_or(false),
            from: s("from"),
            to: s("to"),
            channel: s("channel"),
            output: s("output"),
        })
    }
}

/// Writes the answer for the CLI.
pub fn write_update_result(data_dir: &Path, r: &UpdateResult) -> Result<(), Error> {
    store::write_json(&data_dir.join(RESULT_FILE), &r.to_json())
}

/// The answer, once the agent wrote one.
pub fn read_update_result(data_dir: &Path) -> Result<Option<UpdateResult>, Error> {
    Ok(store::read_json(&data_dir.join(RESULT_FILE))?
        .as_ref()
        .and_then(UpdateResult::from_json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testdir::TestDir;

    #[test]
    fn the_file_turns_the_opt_out_on_and_removing_it_turns_it_off() {
        let d = TestDir::new("optout");
        assert_eq!(reason_from(&d.0, None), None);
        std::fs::write(d.0.join(FILE), b"").unwrap();
        assert_eq!(reason_from(&d.0, None), Some("local"));
        std::fs::remove_file(d.0.join(FILE)).unwrap();
        assert_eq!(reason_from(&d.0, None), None);
    }

    #[test]
    fn the_env_opt_out_counts_only_when_switched_on() {
        let d = TestDir::new("optout-env");
        assert_eq!(reason_from(&d.0, Some("1")), Some("env"));
        assert_eq!(reason_from(&d.0, Some("TRUE")), Some("env"));
        assert_eq!(reason_from(&d.0, Some("0")), None);
        assert_eq!(reason_from(&d.0, Some("")), None);
        assert!(remedy("env").contains(ENV));
    }

    #[test]
    fn a_pin_holds_updates_until_it_is_removed() {
        let d = TestDir::new("optout-pin");
        let v: Version = "1.4.2".parse().unwrap();
        set_pin(&d.0, Some(&v)).unwrap();
        assert_eq!(pin(&d.0), Some(v));
        assert_eq!(reason_from(&d.0, None), Some("pinned"));
        assert!(describe(&d.0, "pinned").contains("1.4.2"));
        let fields = heartbeat_fields(&d.0);
        assert!(fields.contains(&("update_pin", "1.4.2".into())));
        set_pin(&d.0, None).unwrap();
        assert_eq!(pin(&d.0), None);
        assert_eq!(reason_from(&d.0, None), None);
        set_pin(&d.0, None).unwrap(); // removing twice is fine
    }

    #[test]
    fn a_garbled_pin_file_pins_nothing() {
        let d = TestDir::new("optout-pin-bad");
        std::fs::write(d.0.join(PIN_FILE), b"<html>").unwrap();
        assert_eq!(pin(&d.0), None);
    }

    #[test]
    fn the_channel_choice_takes_only_known_channels() {
        let d = TestDir::new("optout-channel");
        assert_eq!(channel(&d.0), None);
        set_channel(&d.0, Some("canary")).unwrap();
        assert_eq!(channel(&d.0).as_deref(), Some("canary"));
        assert!(set_channel(&d.0, Some("beta")).is_err());
        std::fs::write(d.0.join(CHANNEL_FILE), b"beta\n").unwrap();
        assert_eq!(channel(&d.0), None);
        set_channel(&d.0, None).unwrap();
        assert!(!d.0.join(CHANNEL_FILE).exists());
    }

    #[test]
    fn an_update_request_is_taken_once_and_answered() {
        let d = TestDir::new("optout-request");
        assert!(!take_update_request(&d.0).unwrap());
        request_update(&d.0, 100).unwrap();
        assert!(take_update_request(&d.0).unwrap());
        assert!(!take_update_request(&d.0).unwrap());
        assert_eq!(read_update_result(&d.0).unwrap(), None);
        let r = UpdateResult {
            at: 101,
            ok: true,
            installed: true,
            from: "1.0.0".into(),
            to: "1.1.0".into(),
            channel: "stable".into(),
            output: "update installed, restarting".into(),
        };
        write_update_result(&d.0, &r).unwrap();
        assert_eq!(read_update_result(&d.0).unwrap(), Some(r));
        request_update(&d.0, 200).unwrap(); // a new request clears the old answer
        assert_eq!(read_update_result(&d.0).unwrap(), None);
    }
}
