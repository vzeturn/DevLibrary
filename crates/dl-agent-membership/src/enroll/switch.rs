//! `membership-enroll --switch` and `--rollback`: taking over a site from the .NET
//! `MM.Membership.Agent`, and handing it back.
//!
//! **Switch.** The module reads the .NET identity file as is, so a switch normally writes
//! nothing: it finds the .NET configuration (`config.dpapi`, else the seed configuration or
//! the portable store, see [`super::legacy`]), checks with one `GET sync-config` that the hub
//! still accepts its token, stops the `MM.Membership.Agent` service and sets it to disabled
//! (one writer per hotel), and records what it did in the module's data folder
//! (`<data>/modules/membership/enrolled.json`, no secret). Only a seed configuration is
//! sealed into `config.dpapi` (what the .NET agent's own first start does), and only a
//! token the hub rejects (`AGENT_TOKEN_REJECTED`), an unreadable `config.dpapi` or no
//! configuration at all spends an enrollment code; that enrolment keeps the SMILE
//! connection string of the old configuration and backs the old file up
//! (`config.dpapi.bak-*`, `AgentSecureConfig.Save`). The installer may pass `--switch` on
//! every run: once recorded, later runs keep what is there and only make sure the .NET
//! service stays stopped; after a rollback they do nothing until `--force`.
//!
//! **Rollback.** Puts the recorded start type of the .NET service back and starts it,
//! restores the `.bak-*` copy the switch made (only with `--force` when the switch enrolled
//! again: that enrolment replaced the token at the hub, so the backup's token no longer
//! works and the current file, in the same format, is the one the .NET agent needs), and
//! marks the record rolled back. Turning the module off (`read_only: true` or
//! `enabled: false`) is a remote-config change the tool cannot make; it prints the step.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dl_json::{Map, Value};
use dl_time::OffsetDateTime;

use super::legacy::{Places, read_seed};
use super::service::{LegacyService, SERVICE_NAME, Started, Stopped};
use super::{SMILE_ENV, TOKEN_ENV, check_hub_url, enroll_and_save, read_code, secure};
use crate::config::{AgentConfig, ConfigStore, FILE_NAME, Sealer};
use crate::hub::gate::TokenGate;
use crate::hub::{HubApi, HubClient, HubError};
use crate::module::agent_version;

/// The record of the switch, in the module's data folder.
pub const RECORD_FILE: &str = "enrolled.json";
const SWITCHED: &str = "switched";
const ROLLED_BACK: &str = "rolled_back";

/// Checks the token of a configuration with the hub.
pub type Probe<'a> = &'a dyn Fn(&AgentConfig) -> Result<(), HubError>;

/// `GET api/agent/sync-config` with the configuration's token: the cheapest call that the
/// hub authenticates.
pub fn hub_probe(cfg: &AgentConfig) -> Result<(), HubError> {
    let gate = Arc::new(TokenGate::new(Duration::ZERO));
    HubClient::new(
        &cfg.hub_base_url,
        &cfg.agent_token,
        cfg.hotel_id,
        agent_version(),
        gate,
    )?
    .sync_config()
    .map(|_| ())
}

/// What the switch did, as kept in [`RECORD_FILE`]. No token, no connection string.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    /// `switched` or `rolled_back`.
    pub state: String,
    /// The hotel of the configuration kept or written.
    pub hotel_id: String,
    /// `config.dpapi`, `appsettings`, `portable` or `enrolled`.
    pub source: String,
    /// The identity file.
    pub config_path: String,
    /// Whether the switch wrote `config.dpapi`.
    pub rewrote: bool,
    /// Whether it spent an enrollment code (the hub replaced the token).
    pub reenrolled: bool,
    /// The copy of the file it replaced.
    pub backup: Option<String>,
    /// `not_windows`, `not_installed` or `disabled`.
    pub service: String,
    /// Whether the .NET service was running before.
    pub service_was_running: bool,
    /// Its start type before.
    pub service_start_before: Option<String>,
    /// Unix seconds of the last change.
    pub at: u64,
}

impl Record {
    fn to_json(&self) -> String {
        let opt = |o: &Option<String>| o.as_ref().map_or(Value::Null, Value::from);
        let mut m = Map::new();
        m.insert("state", self.state.clone());
        m.insert("hotel_id", self.hotel_id.clone());
        m.insert("source", self.source.clone());
        m.insert("config_path", self.config_path.clone());
        m.insert("rewrote", self.rewrote);
        m.insert("reenrolled", self.reenrolled);
        m.insert("backup", opt(&self.backup));
        m.insert("service", self.service.clone());
        m.insert("service_was_running", self.service_was_running);
        m.insert("service_start_before", opt(&self.service_start_before));
        m.insert("at", self.at);
        dl_json::to_string(&Value::Object(m))
    }

    fn from_json(v: &Value) -> Record {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        Record {
            state: s("state").unwrap_or_default(),
            hotel_id: s("hotel_id").unwrap_or_default(),
            source: s("source").unwrap_or_default(),
            config_path: s("config_path").unwrap_or_default(),
            rewrote: b("rewrote"),
            reenrolled: b("reenrolled"),
            backup: s("backup"),
            service: s("service").unwrap_or_default(),
            service_was_running: b("service_was_running"),
            service_start_before: s("service_start_before"),
            at: v.get("at").and_then(Value::as_u64).unwrap_or(0),
        }
    }
}

/// The record in `dir`; `Ok(None)` without one.
pub fn read_record(dir: &Path) -> Result<Option<Record>, String> {
    let path = dir.join(RECORD_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let v = dl_json::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(Record::from_json(&v)))
}

fn write_record(dir: &Path, r: &Record) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(RECORD_FILE);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, r.to_json())
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// What `--switch` / `--rollback` act on.
pub struct Switch<'a> {
    /// `--hub-url` (may be empty: the .NET configuration names its hub).
    pub hub_url: String,
    /// Where `config.dpapi` lives.
    pub config_dir: PathBuf,
    /// The module's data folder, home of [`RECORD_FILE`].
    pub record_dir: PathBuf,
    /// `--force`: switch again after a rollback; restore a backup whose token was replaced.
    pub force: bool,
    /// DPAPI (tests: the plain sealer).
    pub sealer: Arc<dyn Sealer>,
    /// The .NET service.
    pub service: &'a dyn LegacyService,
    /// Where the .NET seed configuration may be.
    pub places: Places,
    /// The token check ([`hub_probe`]).
    pub probe: Probe<'a>,
}

/// Where the configuration to keep came from.
enum Source {
    Blob,
    Seed { portable: bool, files: Vec<PathBuf> },
}

/// `--switch`.
pub fn run_switch(
    sw: &Switch<'_>,
    input: Option<&mut dyn BufRead>,
    out: &mut dyn Write,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<(), String> {
    let store = ConfigStore::new(sw.config_dir.clone(), Arc::clone(&sw.sealer));
    let record = match read_record(&sw.record_dir) {
        Ok(r) => r,
        Err(e) => {
            say!(out, "switch record unreadable ({e}); switching again\n");
            None
        }
    };
    let loaded = store.load();
    if let Some(r) = record.as_ref().filter(|_| !sw.force) {
        if r.state == ROLLED_BACK {
            say!(
                out,
                "switched back to the .NET agent earlier; nothing done (pass --force to switch again)\n"
            );
            return Ok(());
        }
        if let Ok(Some(cfg)) = &loaded
            && r.state == SWITCHED
            && r.hotel_id == cfg.hotel_id.to_string()
        {
            say!(
                out,
                "already switched for hotel {}; {} kept\n",
                cfg.hotel_id,
                store.path().display()
            );
            return stop_service(sw.service, out).map(|_| ());
        }
    }
    let mut smile_hint = None;
    let (found, source) = match loaded {
        Ok(Some(cfg)) => (Some(cfg), Source::Blob),
        Ok(None) => seed(sw, out, env, &mut smile_hint),
        Err(e) => {
            say!(out, "{} does not open ({e})\n", store.path().display());
            let (_, s) = seed(sw, out, env, &mut smile_hint);
            (None, s)
        }
    };
    let mut need_code = found.is_none();
    if let Some(cfg) = &found {
        if !sw.hub_url.trim().is_empty() && check_hub_url(&sw.hub_url, &cfg.hub_base_url).is_err() {
            say!(
                out,
                "warning: the .NET configuration names another hub than --hub-url; keeping it\n"
            );
        }
        match (sw.probe)(cfg) {
            Ok(()) => say!(out, "the hub accepts the .NET agent's token\n"),
            Err(HubError::TokenRejected { .. }) => {
                say!(
                    out,
                    "the hub rejected the .NET agent's token: enrolling with a fresh code, keeping its SMILE connection\n"
                );
                need_code = true;
            }
            Err(e) => say!(
                out,
                "could not check the token with the hub ({e}); keeping it\n"
            ),
        }
    }
    let mut r = Record {
        state: SWITCHED.into(),
        config_path: store.path().display().to_string(),
        ..Record::default()
    };
    if need_code {
        let hub_url = Some(sw.hub_url.trim())
            .filter(|u| !u.is_empty())
            .or_else(|| found.as_ref().map(|c| c.hub_base_url.as_str()))
            .ok_or("--hub-url is needed to enroll: no readable .NET configuration names a hub")?
            .to_owned();
        let parsed = dl_http::Url::parse(&hub_url)
            .map_err(|_| "the hub address is not an absolute URL".to_owned())?;
        if !secure(&parsed) {
            return Err("the hub address must use https (http only to this machine)".into());
        }
        let code = read_code(input, out, env).map_err(|e| {
            format!("{e}; an enrollment code is needed because the .NET configuration cannot be used ({TOKEN_ENV})")
        })?;
        let smile = found
            .as_ref()
            .map(|c| c.smile_connection_string.clone())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                env(SMILE_ENV)
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
            })
            .or(smile_hint)
            .unwrap_or_default();
        let done = enroll_and_save(&hub_url, &store, code, smile, out, env)?;
        r.hotel_id = done.hotel_id.to_string();
        r.source = "enrolled".into();
        r.rewrote = true;
        r.reenrolled = true;
        r.backup = done.backup.map(|b| b.display().to_string());
    } else if let Some(cfg) = &found {
        r.hotel_id = cfg.hotel_id.to_string();
        match source {
            Source::Blob => {
                r.source = FILE_NAME.into();
                say!(
                    out,
                    "keeping the .NET agent's {} (hotel {})\n",
                    store.path().display(),
                    cfg.hotel_id
                );
            }
            Source::Seed { portable, files } => {
                r.source = if portable { "portable" } else { "appsettings" }.into();
                r.rewrote = true;
                r.backup = store
                    .save(cfg)
                    .map_err(|e| format!("{}: {e}", store.path().display()))?
                    .map(|b| b.display().to_string());
                say!(
                    out,
                    "sealed {} from the .NET configuration in {} (hotel {})\n",
                    store.path().display(),
                    list(&files),
                    cfg.hotel_id
                );
            }
        }
    }
    let stopped = stop_service(sw.service, out);
    if let Ok(s) = &stopped {
        match s {
            Stopped::NotWindows => r.service = "not_windows".into(),
            Stopped::NotInstalled => r.service = "not_installed".into(),
            Stopped::Disabled {
                was_running,
                start_before,
            } => {
                r.service = "disabled".into();
                r.service_was_running = *was_running;
                r.service_start_before = Some(start_before.clone());
            }
        }
    }
    r.at = dl_agent::now();
    if let Err(e) = write_record(&sw.record_dir, &r) {
        say!(
            out,
            "warning: could not record the switch ({e}); a later --switch run checks again\n"
        );
    }
    stopped.map(|_| ())
}

/// Reads the seed configuration for [`run_switch`]; keeps its SMILE string as a hint.
fn seed(
    sw: &Switch<'_>,
    out: &mut dyn Write,
    env: &dyn Fn(&str) -> Option<String>,
    smile_hint: &mut Option<String>,
) -> (Option<AgentConfig>, Source) {
    let none = Source::Seed {
        portable: false,
        files: Vec::new(),
    };
    let seed = match read_seed(&sw.places, env) {
        Ok(Some(s)) => s,
        Ok(None) => return (None, none),
        Err(e) => {
            say!(out, "the .NET seed configuration does not open ({e})\n");
            return (None, none);
        }
    };
    *smile_hint = seed.section.smile();
    let cfg = match seed.section.config() {
        Ok(c) => c,
        Err(e) => {
            say!(out, "the .NET seed configuration is unusable ({e})\n");
            None
        }
    };
    let source = Source::Seed {
        portable: seed.portable,
        files: seed.files,
    };
    (cfg, source)
}

fn list(files: &[PathBuf]) -> String {
    if files.is_empty() {
        return "the environment".into();
    }
    files
        .iter()
        .map(|f| f.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn stop_service(service: &dyn LegacyService, out: &mut dyn Write) -> Result<Stopped, String> {
    match service.stop_and_disable() {
        Ok(s) => {
            match &s {
                Stopped::NotWindows => {}
                Stopped::NotInstalled => {
                    say!(out, "no {SERVICE_NAME} service on this machine\n");
                }
                Stopped::Disabled { .. } => {
                    say!(out, "{SERVICE_NAME} service stopped and disabled\n");
                }
            }
            Ok(s)
        }
        Err(e) => Err(format!(
            "{e}; stop and disable the {SERVICE_NAME} service by hand before turning writes on"
        )),
    }
}

fn stamp() -> String {
    let n = OffsetDateTime::now_utc().datetime();
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        n.year(),
        n.month().number(),
        n.day(),
        n.hour(),
        n.minute(),
        n.second()
    )
}

/// Copies `from` over `to` through a temporary file.
fn replace(from: &Path, to: &Path) -> Result<(), String> {
    let tmp = to.with_extension("dpapi.restore");
    std::fs::copy(from, &tmp)
        .and_then(|_| std::fs::rename(&tmp, to))
        .map_err(|e| format!("restore {} from {}: {e}", to.display(), from.display()))
}

/// `--rollback`.
pub fn run_rollback(sw: &Switch<'_>, out: &mut dyn Write) -> Result<(), String> {
    let path = sw.record_dir.join(RECORD_FILE);
    let mut r = read_record(&sw.record_dir)?.ok_or_else(|| {
        format!(
            "no switch recorded in {}: nothing to roll back",
            path.display()
        )
    })?;
    let config = sw.config_dir.join(FILE_NAME);
    if r.state == ROLLED_BACK {
        say!(
            out,
            "already rolled back; making sure the .NET service runs\n"
        );
    } else if let Some(b) = r.backup.clone() {
        if r.reenrolled && !sw.force {
            say!(
                out,
                "keeping {}: the switch enrolled again, so the token in {b} no longer works; \
                 the .NET agent reads the current file (pass --force to restore {b} anyway)\n",
                config.display()
            );
        } else {
            let backup = PathBuf::from(&b);
            if !backup.is_file() {
                return Err(format!("{b} is gone; restore {FILE_NAME} by hand"));
            }
            let kept = sw
                .config_dir
                .join(format!("{FILE_NAME}.switch-{}", stamp()));
            if config.is_file() {
                std::fs::copy(&config, &kept)
                    .map_err(|e| format!("keep {}: {e}", config.display()))?;
                say!(out, "the switch's {FILE_NAME} kept as {}\n", kept.display());
            }
            replace(&backup, &config)?;
            say!(out, "restored {} from {b}\n", config.display());
        }
    } else if r.rewrote {
        say!(
            out,
            "{} was sealed from the .NET configuration; the .NET agent reads it as is\n",
            config.display()
        );
    } else {
        say!(out, "{} was not changed by the switch\n", config.display());
    }
    if r.service == "disabled" {
        let start = r
            .service_start_before
            .clone()
            .unwrap_or_else(|| "automatic".into());
        match sw.service.restore_and_start(&start) {
            Ok(Started::Restored { running: true }) => {
                say!(out, "{SERVICE_NAME} service set to {start} and running\n");
            }
            Ok(Started::Restored { running: false }) => {
                say!(
                    out,
                    "{SERVICE_NAME} service set back to {start} (not started)\n"
                );
            }
            Ok(Started::NotInstalled) => {
                say!(out, "no {SERVICE_NAME} service on this machine any more\n");
            }
            Ok(Started::NotWindows) => {}
            Err(e) => {
                return Err(format!(
                    "{e}; start the {SERVICE_NAME} service by hand (services.msc)"
                ));
            }
        }
    } else {
        say!(out, "the switch did not touch the {SERVICE_NAME} service\n");
    }
    r.state = ROLLED_BACK.into();
    r.at = dl_agent::now();
    write_record(&sw.record_dir, &r)?;
    say!(
        out,
        "now turn the module off in the remote config: \
         {{\"modules\":{{\"membership\":{{\"read_only\":true}}}}}} (or \"enabled\":false)\n"
    );
    Ok(())
}

#[cfg(test)]
#[path = "switch_tests.rs"]
mod tests;
