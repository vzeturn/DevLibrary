//! `AgentOptions`, read in place from the .NET install: `appsettings.json`, then
//! `appsettings.Production.json` on top, `Agent` section. Keys match case-insensitively and
//! arrays merge by index, as `Microsoft.Extensions.Configuration` binds them.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use dl_json::Value;

use crate::contracts::Params;
use crate::error::Error;
use crate::stj;

/// The configuration section.
pub const SECTION: &str = "Agent";
/// Shipped defaults.
pub const BASE_FILE: &str = "appsettings.json";
/// What Install.ps1 writes (cloud URL, PMS connection, data paths).
pub const PRODUCTION_FILE: &str = "appsettings.Production.json";

/// `AgentOptions` (update and service-host settings are read by nobody: dl-agent owns both).
#[derive(Clone, PartialEq)]
pub struct Settings {
    /// `CloudBaseUrl`.
    pub cloud_base_url: String,
    /// `PmsConnectionString` (ADO form, plain text in the ACL-protected file).
    pub pms_connection_string: String,
    /// `IdentityFile`.
    pub identity_file: String,
    /// `TenantKey` (base64), wins over the key file when set.
    pub tenant_key: String,
    /// `TenantKeyFile`.
    pub tenant_key_file: String,
    /// `VaultDirectory`.
    pub vault_directory: String,
    /// `IdempotencyFile`.
    pub idempotency_file: String,
    /// `MaxConcurrency`: PMS calls at once.
    pub max_concurrency: i32,
    /// `SnapshotCapacity`.
    pub snapshot_capacity: i32,
    /// `CommandTimeoutSeconds`.
    pub command_timeout_seconds: i32,
    /// `LockTimeoutSeconds`.
    pub lock_timeout_seconds: i32,
    /// `AutoInstallProcedures`.
    pub auto_install_procedures: bool,
    /// `Watches`.
    pub watches: Vec<Watch>,
}

/// `WatchOptions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    /// A catalog read.
    pub operation: String,
    /// Parameters as text; exactly `{today}` becomes the hotel date.
    pub parameters: Params,
    /// Seconds between runs (at least 5 is used).
    pub interval_seconds: i32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            cloud_base_url: String::new(),
            pms_connection_string: String::new(),
            identity_file: "agent-identity.bin".into(),
            tenant_key: String::new(),
            tenant_key_file: "agent-tenant-key.bin".into(),
            vault_directory: "vault".into(),
            idempotency_file: "agent-idempotency.jsonl".into(),
            max_concurrency: 4,
            snapshot_capacity: 2000,
            command_timeout_seconds: 15,
            lock_timeout_seconds: 8,
            auto_install_procedures: true,
            watches: Vec::new(),
        }
    }
}

impl fmt::Debug for Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redact = |s: &str| if s.is_empty() { "" } else { "<redacted>" };
        f.debug_struct("Settings")
            .field("cloud_base_url", &self.cloud_base_url)
            .field(
                "pms_connection_string",
                &redact(&self.pms_connection_string),
            )
            .field("identity_file", &self.identity_file)
            .field("tenant_key", &redact(&self.tenant_key))
            .field("tenant_key_file", &self.tenant_key_file)
            .field("vault_directory", &self.vault_directory)
            .field("idempotency_file", &self.idempotency_file)
            .field("max_concurrency", &self.max_concurrency)
            .field("snapshot_capacity", &self.snapshot_capacity)
            .field("command_timeout_seconds", &self.command_timeout_seconds)
            .field("lock_timeout_seconds", &self.lock_timeout_seconds)
            .field("auto_install_procedures", &self.auto_install_procedures)
            .field("watches", &self.watches)
            .finish()
    }
}

/// Configuration flattened to `a:b:0:c` paths, as the JSON configuration provider keeps it.
#[derive(Debug, Default, Clone)]
pub struct Flat {
    /// Lower-cased path → (path as written, value).
    entries: BTreeMap<String, (String, String)>,
}

impl Flat {
    /// Nothing yet.
    pub fn new() -> Flat {
        Flat::default()
    }

    /// Adds one file's JSON on top of what is there (a UTF-8 BOM is skipped).
    ///
    /// # Errors
    /// [`Error::Config`] naming `file` when the text is not a JSON object (never echoing it).
    pub fn add_json(&mut self, file: &str, text: &str) -> Result<(), Error> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let v = stj::parse(text).map_err(|_| Error::Config(format!("{file} is not valid JSON")))?;
        if !matches!(v, Value::Object(_)) {
            return Err(Error::Config(format!("{file} is not a JSON object")));
        }
        self.visit(String::new(), &v);
        Ok(())
    }

    fn visit(&mut self, path: String, v: &Value) {
        let join = |seg: &str| {
            if path.is_empty() {
                seg.to_string()
            } else {
                format!("{path}:{seg}")
            }
        };
        let leaf = match v {
            Value::Object(m) => {
                for (k, child) in m.iter() {
                    self.visit(join(k), child);
                }
                return;
            }
            Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    self.visit(join(&i.to_string()), child);
                }
                return;
            }
            // `JsonElement.ToString()`: booleans as `True`/`False`, null as empty.
            Value::Bool(true) => "True".to_string(),
            Value::Bool(false) => "False".to_string(),
            Value::Null => String::new(),
            Value::Number(n) => n.to_string(),
            Value::String(s) => s.clone(),
        };
        self.entries.insert(path.to_lowercase(), (path, leaf));
    }

    /// The value at `path` (case-insensitive).
    pub fn get(&self, path: &str) -> Option<&str> {
        self.entries
            .get(&path.to_lowercase())
            .map(|(_, v)| v.as_str())
    }

    /// Distinct next segments under `prefix` (as first written), in key order.
    fn children(&self, prefix: &str) -> Vec<String> {
        let lower = format!("{}:", prefix.to_lowercase());
        let mut out: Vec<String> = Vec::new();
        for (key, (orig, _)) in self.entries.range(lower.clone()..) {
            let Some(rest) = key.strip_prefix(&lower) else {
                break;
            };
            let len = rest.split(':').next().map_or(0, str::len);
            let seg = &orig[lower.len()..lower.len() + len];
            if !out.iter().any(|s| s.eq_ignore_ascii_case(seg)) {
                out.push(seg.to_string());
            }
        }
        out
    }
}

impl Settings {
    /// Reads `dir/appsettings.json` and `dir/appsettings.Production.json` (each optional).
    ///
    /// # Errors
    /// [`Error::Io`] for an unreadable file, [`Error::Config`] for invalid JSON or values.
    pub fn load(dir: &Path) -> Result<Settings, Error> {
        let mut flat = Flat::new();
        for file in [BASE_FILE, PRODUCTION_FILE] {
            let path = dir.join(file);
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let text = String::from_utf8(bytes)
                        .map_err(|_| Error::Config(format!("{file} is not UTF-8")))?;
                    flat.add_json(file, &text)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::io(file, e)),
            }
        }
        Settings::bind(&flat)
    }

    /// Binds the `Agent` section of `flat` over the defaults.
    ///
    /// # Errors
    /// [`Error::Config`] naming the key whose value does not parse.
    pub fn bind(flat: &Flat) -> Result<Settings, Error> {
        let mut s = Settings::default();
        let key = |name: &str| format!("{SECTION}:{name}");
        let text = |name: &str, slot: &mut String| {
            if let Some(v) = flat.get(&key(name)) {
                *slot = v.to_string();
            }
        };
        text("CloudBaseUrl", &mut s.cloud_base_url);
        text("PmsConnectionString", &mut s.pms_connection_string);
        text("IdentityFile", &mut s.identity_file);
        text("TenantKey", &mut s.tenant_key);
        text("TenantKeyFile", &mut s.tenant_key_file);
        text("VaultDirectory", &mut s.vault_directory);
        text("IdempotencyFile", &mut s.idempotency_file);
        for (name, slot) in [
            ("MaxConcurrency", &mut s.max_concurrency),
            ("SnapshotCapacity", &mut s.snapshot_capacity),
            ("CommandTimeoutSeconds", &mut s.command_timeout_seconds),
            ("LockTimeoutSeconds", &mut s.lock_timeout_seconds),
        ] {
            if let Some(v) = flat.get(&key(name)) {
                *slot = int(&key(name), v)?;
            }
        }
        if let Some(v) = flat.get(&key("AutoInstallProcedures")) {
            s.auto_install_procedures = boolean(&key("AutoInstallProcedures"), v)?;
        }
        let watches = key("Watches");
        let mut indexes: Vec<(usize, String)> = Vec::new();
        for seg in flat.children(&watches) {
            let n = seg
                .parse::<usize>()
                .map_err(|_| Error::Config(format!("{watches}:{seg} is not an array index")))?;
            indexes.push((n, seg));
        }
        indexes.sort();
        for (_, seg) in indexes {
            let at = format!("{watches}:{seg}");
            let mut w = Watch {
                operation: flat
                    .get(&format!("{at}:Operation"))
                    .unwrap_or("")
                    .to_string(),
                parameters: Params::new(),
                interval_seconds: 30,
            };
            if let Some(v) = flat.get(&format!("{at}:IntervalSeconds")) {
                w.interval_seconds = int(&format!("{at}:IntervalSeconds"), v)?;
            }
            let params = format!("{at}:Parameters");
            for name in flat.children(&params) {
                let value = flat.get(&format!("{params}:{name}")).map(str::to_string);
                w.parameters.insert(name, value);
            }
            s.watches.push(w);
        }
        Ok(s)
    }
}

fn int(key: &str, v: &str) -> Result<i32, Error> {
    v.trim()
        .parse::<i32>()
        .map_err(|_| Error::Config(format!("{key} is not a whole number")))
}

fn boolean(key: &str, v: &str) -> Result<bool, Error> {
    match v.trim() {
        t if t.eq_ignore_ascii_case("true") => Ok(true),
        t if t.eq_ignore_ascii_case("false") => Ok(false),
        _ => Err(Error::Config(format!("{key} is not true or false"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIPPED: &str = r#"{"Logging":{"LogLevel":{"Default":"Information"}},
      "Agent":{"CloudBaseUrl":"https://sync.example.com","PmsConnectionString":"REPLACE",
      "MaxConcurrency":4,"AutoUpdate":true,
      "Watches":[{"Operation":"dashboard.fo-command-center","Parameters":{"HotelDate":"{today}"},"IntervalSeconds":20}]}}"#;

    #[test]
    fn production_overrides_case_insensitively_and_arrays_merge_by_index() {
        let mut flat = Flat::new();
        flat.add_json("a", SHIPPED).unwrap();
        let prod = "\u{feff}{\"agent\":{\"cloudbaseurl\":\"https://arrival.example\",\"PmsConnectionString\":\"Server=.;Database=SMILE_FO\",\"IdentityFile\":\"C:\\\\x\\\\data\\\\agent-identity.bin\",\"Watches\":[{\"IntervalSeconds\":40}],\"AutoInstallProcedures\":false}}";
        flat.add_json("b", prod).unwrap();
        let s = Settings::bind(&flat).unwrap();
        assert_eq!(s.cloud_base_url, "https://arrival.example");
        assert_eq!(s.identity_file, "C:\\x\\data\\agent-identity.bin");
        assert_eq!(s.max_concurrency, 4);
        assert!(!s.auto_install_procedures);
        assert_eq!(s.watches.len(), 1);
        assert_eq!(s.watches[0].operation, "dashboard.fo-command-center");
        assert_eq!(s.watches[0].interval_seconds, 40);
        assert_eq!(
            s.watches[0].parameters.get("HotelDate"),
            Some(Some("{today}"))
        );
        assert_eq!(s.snapshot_capacity, 2000);
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("SMILE_FO"), "{dbg}");
    }

    #[test]
    fn bad_values_name_the_key_not_the_value() {
        let mut flat = Flat::new();
        flat.add_json("a", r#"{"Agent":{"MaxConcurrency":"lots-of-secret"}}"#)
            .unwrap();
        let e = Settings::bind(&flat).unwrap_err().to_string();
        assert!(
            e.contains("Agent:MaxConcurrency") && !e.contains("secret"),
            "{e}"
        );
        assert!(
            Flat::new()
                .add_json("x", "{secret")
                .unwrap_err()
                .to_string()
                .ends_with("x is not valid JSON")
        );
    }
}
