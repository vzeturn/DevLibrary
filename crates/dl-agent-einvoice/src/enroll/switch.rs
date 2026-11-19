//! `einvoice-enroll --switch`: the unattended switch from the MM-EInvoice Go agent.
//!
//! The Go agent and this module share the folder and format of `config.enc`, so "a
//! `config.enc` opens" cannot tell a site this dl-agent already enrolled from one the Go
//! agent still runs. After each enrolment the tool records the device id it sealed in the
//! module's data folder (`<data>/modules/einvoice/enrolled.json`); `--switch` keeps a
//! `config.enc` with that device id and enrols afresh over any other (new key, new device,
//! procedures reinstalled, the old SQL connection and collect window kept: see
//! [`super::previous`]). So the installer can pass it on every run: the first run switches,
//! later runs keep what they made.

use std::path::{Path, PathBuf};

use dl_json::{Map, Value};

use crate::agent_config::AgentConfig;

/// The record of the last enrolment, in the module's data folder.
pub const ENROLLED_FILE: &str = "enrolled.json";

/// What `--switch` finds in the config folder.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Found {
    /// No `config.enc`: enrol.
    Nothing,
    /// The `config.enc` this dl-agent sealed: keep it.
    Ours,
    /// A `config.enc` of another agent (or one that does not open): enrol afresh.
    Other(String),
}

/// Compares `config.enc` in `config_dir` with the record in `record_dir`.
pub(super) fn found(config_dir: &Path, record_dir: Option<&Path>) -> Found {
    let cfg = match AgentConfig::load(config_dir) {
        Ok(None) => return Found::Nothing,
        Ok(Some(cfg)) => cfg,
        Err(e) => return Found::Other(format!("its config.enc does not open: {e}")),
    };
    let recorded = record_dir.and_then(|d| recorded_device(d).ok().flatten());
    if recorded.as_deref() == Some(cfg.device_id.to_string().as_str()) {
        Found::Ours
    } else {
        Found::Other("config.enc was not sealed by this dl-agent".into())
    }
}

/// The device id of the last recorded enrolment; `Ok(None)` without a record.
fn recorded_device(record_dir: &Path) -> Result<Option<String>, String> {
    let path = record_dir.join(ENROLLED_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let v = dl_json::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(v.get("device_id")
        .and_then(Value::as_str)
        .map(str::to_owned))
}

/// Records the enrolment just sealed in `config_dir` (no secret: device id, folders, time).
pub(super) fn record(
    config_dir: &Path,
    record_dir: &Path,
    previous: Option<&PathBuf>,
) -> Result<(), String> {
    let cfg = AgentConfig::load(config_dir)?
        .ok_or_else(|| format!("no config.enc in {}", config_dir.display()))?;
    let mut m = Map::new();
    m.insert("device_id", cfg.device_id.to_string());
    m.insert("config_dir", config_dir.display().to_string());
    m.insert(
        "previous",
        previous.map_or(Value::Null, |p| Value::from(p.display().to_string())),
    );
    m.insert("at", dl_agent::now());
    std::fs::create_dir_all(record_dir).map_err(|e| format!("{}: {e}", record_dir.display()))?;
    let path = record_dir.join(ENROLLED_FILE);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, dl_json::to_string(&Value::Object(m)))
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("dl-einvoice-switch-{tag}-{nanos}"))
    }

    #[test]
    fn keeps_only_the_config_it_sealed() {
        let root = scratch("found");
        let (cfg_dir, rec_dir) = (root.join("MM-EInvoice"), root.join("modules/einvoice"));
        assert_eq!(found(&cfg_dir, Some(&rec_dir)), Found::Nothing);

        let go = AgentConfig {
            config_dir: cfg_dir.clone(),
            device_id: dl_uuid::Uuid::new_v4(),
            ..AgentConfig::default()
        };
        go.save().unwrap();
        assert!(
            matches!(found(&cfg_dir, Some(&rec_dir)), Found::Other(_)),
            "the Go agent's"
        );
        assert!(matches!(found(&cfg_dir, None), Found::Other(_)));

        record(&cfg_dir, &rec_dir, Some(&root.join("old"))).unwrap();
        assert_eq!(found(&cfg_dir, Some(&rec_dir)), Found::Ours);
        let text = std::fs::read_to_string(rec_dir.join(ENROLLED_FILE)).unwrap();
        assert!(
            !text.contains("api_key") && text.contains("\"previous\""),
            "{text}"
        );

        let again = AgentConfig {
            device_id: dl_uuid::Uuid::new_v4(),
            ..go
        };
        again.save().unwrap();
        assert!(
            matches!(found(&cfg_dir, Some(&rec_dir)), Found::Other(_)),
            "re-sealed elsewhere"
        );

        std::fs::write(cfg_dir.join(crate::agent_config::CONFIG_FILE), b"x").unwrap();
        match found(&cfg_dir, Some(&rec_dir)) {
            Found::Other(why) => assert!(why.contains("does not open"), "{why}"),
            other => panic!("{other:?}"),
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}
