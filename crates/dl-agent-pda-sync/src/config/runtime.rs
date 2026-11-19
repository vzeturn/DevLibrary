//! Cloud-pushed tunables (`RuntimeConfig` in `Config/AgentOptions.cs`) and their store
//! (port of `Config/RuntimeConfigStore.cs`), persisted in `runtime-config.json` so a
//! restart keeps the last good one. JSON with `JsonSerializerDefaults.Web` (camelCase,
//! case-insensitive, numbers also from strings), written indented with nulls.

use super::health::read_exception;
use super::paths::{AgentPaths, write_atomic};
use super::status::{ReadError, read_text};
use super::stj_read::{self, JsonError};
use crate::reporting::log::{Exception, Logger};
use crate::util::stj::{Style, Writer};
use dl_json::Value;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};

/// The logger category (`ILogger<RuntimeConfigStore>`).
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Config.RuntimeConfigStore";

/// Cloud-pushed tunables: poll intervals, history scope, table allowlist.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Open-check poll interval in ms (2000).
    pub open_check_poll_ms: i32,
    /// Master-data scan interval in seconds (120).
    pub master_scan_seconds: i32,
    /// Days of closed checks re-read (3).
    pub closed_rescan_days: i32,
    /// Months of history captured (24).
    pub history_months: i32,
    /// Tables to capture, `None` for all. A JSON `null` entry is kept as `None`, as the C#
    /// list keeps it.
    pub table_allowlist: Option<Vec<Option<String>>>,
}

impl Default for RuntimeConfig {
    fn default() -> RuntimeConfig {
        RuntimeConfig {
            open_check_poll_ms: 2000,
            master_scan_seconds: 120,
            closed_rescan_days: 3,
            history_months: 24,
            table_allowlist: None,
        }
    }
}

impl PartialEq for RuntimeConfig {
    /// C# record equality: the allowlist compares by reference, so two configs are equal
    /// only when their numbers match and neither has an allowlist.
    fn eq(&self, other: &RuntimeConfig) -> bool {
        self.open_check_poll_ms == other.open_check_poll_ms
            && self.master_scan_seconds == other.master_scan_seconds
            && self.closed_rescan_days == other.closed_rescan_days
            && self.history_months == other.history_months
            && self.table_allowlist.is_none()
            && other.table_allowlist.is_none()
    }
}

impl RuntimeConfig {
    /// The allowlisted table names (null entries skipped), `None` for all tables.
    pub fn allowlist(&self) -> Option<Vec<&str>> {
        self.table_allowlist
            .as_ref()
            .map(|l| l.iter().filter_map(|t| t.as_deref()).collect())
    }

    /// `JsonSerializer.Serialize(config, Web + WriteIndented)`.
    pub fn to_json(&self) -> String {
        let mut w = Writer::new(Style::Indented);
        w.begin_object();
        w.key("openCheckPollMs")
            .int(i64::from(self.open_check_poll_ms));
        w.key("masterScanSeconds")
            .int(i64::from(self.master_scan_seconds));
        w.key("closedRescanDays")
            .int(i64::from(self.closed_rescan_days));
        w.key("historyMonths").int(i64::from(self.history_months));
        w.key("tableAllowlist");
        match &self.table_allowlist {
            None => {
                w.null();
            }
            Some(items) => {
                w.begin_array();
                for t in items {
                    match t {
                        Some(s) => w.string(s),
                        None => w.null(),
                    };
                }
                w.end_array();
            }
        }
        w.end_object();
        w.finish()
    }

    /// `JsonElement.Deserialize<RuntimeConfig>(Web)`; `Ok(None)` for `null`.
    pub fn from_value(v: &Value) -> Result<Option<RuntimeConfig>, JsonError> {
        let Some(m) = stj_read::root_object(v)? else {
            return Ok(None);
        };
        let mut c = RuntimeConfig::default();
        let int = |name: &str, target: &mut i32| -> Result<(), JsonError> {
            if let Some(v) = stj_read::get_ci(m, name) {
                *target = stj_read::int32(v, name)?;
            }
            Ok(())
        };
        int("openCheckPollMs", &mut c.open_check_poll_ms)?;
        int("masterScanSeconds", &mut c.master_scan_seconds)?;
        int("closedRescanDays", &mut c.closed_rescan_days)?;
        int("historyMonths", &mut c.history_months)?;
        c.table_allowlist = match stj_read::get_ci(m, "tableAllowlist") {
            None | Some(Value::Null) => None,
            Some(Value::Array(items)) => Some(
                items
                    .iter()
                    .enumerate()
                    .map(|(i, t)| stj_read::opt_string(Some(t), &format!("tableAllowlist[{i}]")))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            Some(_) => {
                return Err(JsonError(
                    "The JSON value could not be converted to System.Collections.Generic.IReadOnlyList`1[System.String]. Path: $.tableAllowlist".to_owned(),
                ));
            }
        };
        Ok(Some(c))
    }

    /// Parses `runtime-config.json` content; `Ok(None)` for `null`.
    pub fn from_json(text: &str) -> Result<Option<RuntimeConfig>, JsonError> {
        RuntimeConfig::from_value(&stj_read::parse(text)?)
    }
}

/// Holds the cloud-pushed [`RuntimeConfig`], persisted so a restart keeps the last good one.
pub struct RuntimeConfigStore {
    paths: AgentPaths,
    log: Logger,
    current: Mutex<Arc<RuntimeConfig>>,
}

impl std::fmt::Debug for RuntimeConfigStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeConfigStore")
            .field("current", &self.current())
            .finish_non_exhaustive()
    }
}

impl RuntimeConfigStore {
    /// Loads the stored config (defaults when absent or corrupt). Fails only when the file
    /// exists but cannot be read, as the C# constructor throws then.
    pub fn new(paths: AgentPaths, log: &Logger) -> io::Result<RuntimeConfigStore> {
        let log = log.for_category(CATEGORY);
        let current = Self::load(&paths, &log)?;
        Ok(RuntimeConfigStore {
            paths,
            log,
            current: Mutex::new(Arc::new(current)),
        })
    }

    fn load(paths: &AgentPaths, log: &Logger) -> io::Result<RuntimeConfig> {
        let file = paths.runtime_config_file();
        if !file.is_file() {
            return Ok(RuntimeConfig::default());
        }
        let text = read_text(&file)?;
        match RuntimeConfig::from_json(&text) {
            Ok(c) => Ok(c.unwrap_or_default()),
            Err(e) => {
                log.warn_ex(
                    read_exception(&ReadError::Json(e)),
                    "Stored cloud config is corrupt; using defaults",
                    &[],
                );
                Ok(RuntimeConfig::default())
            }
        }
    }

    /// The config in force.
    pub fn current(&self) -> Arc<RuntimeConfig> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Applies the `config` object of a heartbeat response (parse it with
    /// [`crate::util::stj::parse`] so numbers keep their text); invalid payloads are
    /// logged and ignored. Fails when the new config cannot be written.
    pub fn apply(&self, config: &Value) -> io::Result<()> {
        let next = match RuntimeConfig::from_value(config) {
            Ok(n) => n,
            Err(e) => {
                self.log.warn_ex(
                    Exception::new("System.Text.Json.JsonException", e.to_string()),
                    "Ignoring malformed cloud config",
                    &[],
                );
                return Ok(());
            }
        };
        let Some(next) = next else { return Ok(()) };
        {
            let mut cur = self.current.lock().unwrap_or_else(PoisonError::into_inner);
            if **cur == next {
                return Ok(());
            }
            *cur = Arc::new(next.clone());
        }
        self.paths.ensure_created()?;
        write_atomic(&self.paths.runtime_config_file(), &next.to_json())?;
        self.log.info("Cloud config updated", &[]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporting::log::{Level, LoggerConfig, MemorySink};
    use crate::util::stj::NEWLINE;

    fn parse(s: &str) -> Value {
        crate::util::stj::parse(s).unwrap()
    }

    /// Expected: .NET 10 (`sa-core` check, "RUNTIME" lines).
    #[test]
    fn store_matches_the_csharp_store() {
        let dir = std::env::temp_dir().join(format!("pda-rt-{}", dl_uuid::Uuid::new_v4().simple()));
        let paths = AgentPaths::new(&dir);
        let sink = std::sync::Arc::new(MemorySink::default());
        let log = Logger::new(LoggerConfig {
            sinks: vec![(sink.clone(), Level::Verbose)],
            ..LoggerConfig::default()
        });
        let store = RuntimeConfigStore::new(paths.clone(), &log).unwrap();
        assert_eq!(store.current().open_check_poll_ms, 2000);
        store
            .apply(&parse(
                r#"{"openCheckPollMs":500,"TableAllowlist":["a","b"],"unknown":1}"#,
            ))
            .unwrap();
        let nl = NEWLINE;
        assert_eq!(
            std::fs::read_to_string(paths.runtime_config_file()).unwrap(),
            format!(
                "{{{nl}  \"openCheckPollMs\": 500,{nl}  \"masterScanSeconds\": 120,{nl}  \"closedRescanDays\": 3,{nl}  \"historyMonths\": 24,{nl}  \"tableAllowlist\": [{nl}    \"a\",{nl}    \"b\"{nl}  ]{nl}}}"
            )
        );
        for bad in [
            r#"{"openCheckPollMs":"x"}"#,
            r#"{"openCheckPollMs":1.5}"#,
            "[]",
            "null",
            r#"{"openCheckPollMs":null}"#,
        ] {
            store.apply(&parse(bad)).unwrap();
            assert_eq!(store.current().open_check_poll_ms, 500, "{bad}");
        }
        store.apply(&parse(r#"{"historyMonths":"5"}"#)).unwrap();
        assert_eq!(store.current().history_months, 5);
        assert_eq!(store.current().open_check_poll_ms, 2000);
        assert_eq!(
            sink.messages_at(Level::Warning),
            vec!["Ignoring malformed cloud config"; 4]
        );
        // Same numbers without an allowlist: no rewrite.
        let infos = sink.messages_at(Level::Information).len();
        store.apply(&parse(r#"{"historyMonths":5}"#)).unwrap();
        assert_eq!(sink.messages_at(Level::Information).len(), infos);
        // With an allowlist every apply rewrites (C# record equality on a list reference).
        store.apply(&parse(r#"{"tableAllowlist":["a"]}"#)).unwrap();
        store.apply(&parse(r#"{"tableAllowlist":["a"]}"#)).unwrap();
        assert_eq!(sink.messages_at(Level::Information).len(), infos + 2);
        // A restart reads it back.
        let again = RuntimeConfigStore::new(paths.clone(), &log).unwrap();
        assert_eq!(again.current().allowlist(), Some(vec!["a"]));
        std::fs::write(paths.runtime_config_file(), "{ corrupt").unwrap();
        let corrupt = RuntimeConfigStore::new(paths.clone(), &log).unwrap();
        assert_eq!(corrupt.current().history_months, 24);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
