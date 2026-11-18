//! Remote configuration with last-known-good rollback (PROTOCOL.md §8).

use std::path::{Path, PathBuf};

use dl_json::{Value, json};

use crate::Error;
use crate::store;

/// Seconds a restarted module must survive before a new config becomes last-known-good.
pub const CONFIRM_AFTER_SECONDS: u64 = 120;

/// One configuration version from the hub.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteConfig {
    /// Monotonic version (0 = nothing received yet).
    pub version: u64,
    /// `{"modules":{"<name>":{"enabled":true,…}},"log_level":"info"}`.
    pub body: Value,
}

impl Default for RemoteConfig {
    fn default() -> RemoteConfig {
        RemoteConfig {
            version: 0,
            body: json!({"modules": {}}),
        }
    }
}

impl RemoteConfig {
    /// Parses `{version, body}`.
    pub fn from_json(v: &Value) -> Result<RemoteConfig, Error> {
        let version = v
            .get("version")
            .and_then(Value::as_u64)
            .ok_or_else(|| Error::Config("config without version".into()))?;
        let body = v
            .get("body")
            .cloned()
            .filter(Value::is_object)
            .ok_or_else(|| Error::Config("config body is not an object".into()))?;
        Ok(RemoteConfig { version, body })
    }

    fn to_json(&self) -> Value {
        json!({"version": self.version, "body": self.body.clone()})
    }

    /// The section of one module (`Null` when absent).
    pub fn module(&self, name: &str) -> Value {
        self.body
            .get("modules")
            .and_then(|m| m.get(name))
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// Modules run unless their section says `"enabled": false`.
    pub fn module_enabled(&self, name: &str) -> bool {
        self.module(name)
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// Log level, default `info`.
    pub fn log_level(&self) -> &str {
        self.body
            .get("log_level")
            .and_then(Value::as_str)
            .unwrap_or("info")
    }

    /// Module names whose section differs between two configs.
    pub fn changed_modules(&self, other: &RemoteConfig) -> Vec<String> {
        let names = |c: &RemoteConfig| -> Vec<String> {
            c.body
                .get("modules")
                .and_then(Value::as_object)
                .map(|o| o.keys().map(|k| k.to_string()).collect())
                .unwrap_or_default()
        };
        let mut all = names(self);
        for n in names(other) {
            if !all.contains(&n) {
                all.push(n);
            }
        }
        all.retain(|n| self.module(n) != other.module(n));
        all
    }
}

/// The applied config and its last-known-good, persisted in `config.json`.
#[derive(Debug)]
pub struct ConfigStore {
    path: PathBuf,
    /// Config in force.
    pub current: RemoteConfig,
    /// Config to go back to if `current` breaks a module.
    pub last_known_good: RemoteConfig,
    /// When `current` was applied, while it is still on trial; `None` once confirmed.
    pub trial_since: Option<u64>,
}

impl ConfigStore {
    /// Loads `<dir>/config.json` (missing → empty config).
    pub fn load(dir: &Path) -> Result<ConfigStore, Error> {
        let path = dir.join("config.json");
        let mut s = ConfigStore {
            path: path.clone(),
            current: RemoteConfig::default(),
            last_known_good: RemoteConfig::default(),
            trial_since: None,
        };
        if let Some(v) = store::read_json(&path)? {
            if let Some(c) = v.get("current") {
                s.current = RemoteConfig::from_json(c)?;
            }
            s.last_known_good = match v.get("last_known_good") {
                Some(c) => RemoteConfig::from_json(c)?,
                None => s.current.clone(),
            };
            s.trial_since = v.get("trial_since").and_then(Value::as_u64);
        }
        Ok(s)
    }

    fn save(&self) -> Result<(), Error> {
        let trial = self.trial_since.map_or(Value::Null, Value::from);
        store::write_json(
            &self.path,
            &json!({"current": self.current.to_json(), "last_known_good": self.last_known_good.to_json(), "trial_since": trial}),
        )
    }

    /// Applies a newer config; returns the modules to restart. Older or equal versions are
    /// ignored (empty list).
    pub fn apply(&mut self, new: RemoteConfig, now: u64) -> Result<Vec<String>, Error> {
        if new.version <= self.current.version {
            return Ok(Vec::new());
        }
        if self.trial_since.is_none() {
            self.last_known_good = self.current.clone();
        }
        let changed = new.changed_modules(&self.current);
        self.current = new;
        self.trial_since = Some(now);
        self.save()?;
        Ok(changed)
    }

    /// Promotes `current` to last-known-good once it ran long enough without a failure.
    pub fn confirm_if_due(&mut self, now: u64) -> Result<bool, Error> {
        match self.trial_since {
            Some(t) if now.saturating_sub(t) >= CONFIRM_AFTER_SECONDS => {
                self.last_known_good = self.current.clone();
                self.trial_since = None;
                self.save()?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Whether a module failure now should revert the config.
    pub fn in_trial(&self, now: u64) -> bool {
        self.trial_since
            .is_some_and(|t| now.saturating_sub(t) < CONFIRM_AFTER_SECONDS)
    }

    /// Goes back to last-known-good, keeping the bad version number so the hub does not
    /// send it again; returns the modules to restart.
    pub fn revert(&mut self) -> Result<Vec<String>, Error> {
        let bad_version = self.current.version;
        let changed = self.last_known_good.changed_modules(&self.current);
        self.current = RemoteConfig {
            version: bad_version,
            body: self.last_known_good.body.clone(),
        };
        self.trial_since = None;
        self.save()?;
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testdir::TestDir;

    fn cfg(version: u64, body: Value) -> RemoteConfig {
        RemoteConfig { version, body }
    }

    #[test]
    fn apply_restarts_only_changed_modules_and_ignores_old() {
        let d = TestDir::new("config");
        let mut s = ConfigStore::load(&d.0).unwrap();
        let v1 = cfg(1, json!({"modules": {"a": {"x": 1}, "b": {"y": 1}}}));
        let mut changed = s.apply(v1, 100).unwrap();
        changed.sort();
        assert_eq!(changed, ["a", "b"]);
        let v2 = cfg(
            2,
            json!({"modules": {"a": {"x": 2}, "b": {"y": 1}}, "log_level": "debug"}),
        );
        assert_eq!(s.apply(v2.clone(), 200).unwrap(), ["a"]);
        assert!(
            s.apply(cfg(2, json!({"modules": {}})), 300)
                .unwrap()
                .is_empty()
        );
        assert_eq!(s.current.log_level(), "debug");
        let back = ConfigStore::load(&d.0).unwrap();
        assert_eq!(back.current, v2);
    }

    #[test]
    fn revert_goes_to_last_known_good_and_keeps_version() {
        let d = TestDir::new("config-revert");
        let mut s = ConfigStore::load(&d.0).unwrap();
        s.apply(cfg(1, json!({"modules": {"a": {"x": 1}}})), 0)
            .unwrap();
        assert!(s.confirm_if_due(CONFIRM_AFTER_SECONDS).unwrap());
        s.apply(cfg(2, json!({"modules": {"a": {"x": 99}}})), 1000)
            .unwrap();
        assert!(s.in_trial(1010));
        assert_eq!(s.revert().unwrap(), ["a"]);
        assert_eq!(s.current.version, 2);
        assert_eq!(s.current.module("a"), json!({"x": 1}));
        assert!(!s.in_trial(1010));
    }

    #[test]
    fn modules_default_to_enabled() {
        let c = cfg(1, json!({"modules": {"off": {"enabled": false}}}));
        assert!(c.module_enabled("anything"));
        assert!(!c.module_enabled("off"));
    }
}
