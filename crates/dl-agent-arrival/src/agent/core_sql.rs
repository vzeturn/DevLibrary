//! The core's SQL settings for the PMS database. `dl-agent sql-setup` keeps them in
//! `<data dir>/sql.json` under `databases.SMILE_FO`: the connection without its password, the
//! login mode (`auth`: `windows` or `sql`), the DPAPI-sealed password and the TLS rung setup
//! proved (`transport`). When that entry exists the link uses it instead of the old install's
//! `PmsConnectionString`, so one SQL setup serves every module; without it the appsettings
//! value stays in use. The unsealed connection lives in memory only and is never written back.

use std::path::Path;

use dl_agent::sql::transport::Rung;
use dl_agent::sql::{SQL_FILE, secret};
use dl_json::Value;

use crate::agent::settings::Settings;

/// The `sql.json` database the link reads and installs its procedures in.
pub const DATABASE: &str = "SMILE_FO";
/// Problem code: `sql.json` declares SMILE_FO but the entry cannot be used.
pub const UNUSABLE: &str = "arrival_core_sql_unusable";

/// Where the PMS connection came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PmsSource {
    /// `PmsConnectionString` of the old install, read the way SqlClient reads it.
    #[default]
    AppSettings,
    /// The core's `sql.json` entry, read as the core reads it, with its pinned rung (`None`:
    /// the automatic ladder).
    Core {
        /// `transport` of the entry.
        pin: Option<Rung>,
    },
}

impl PmsSource {
    /// `appsettings` or `sql.json`, for status output.
    pub fn name(self) -> &'static str {
        match self {
            PmsSource::AppSettings => "appsettings",
            PmsSource::Core { .. } => SQL_FILE,
        }
    }

    /// The pinned rung (always `None` for appsettings).
    pub fn pin(self) -> Option<Rung> {
        match self {
            PmsSource::AppSettings => None,
            PmsSource::Core { pin } => pin,
        }
    }
}

/// The full connection (password unsealed) and pinned rung of the `SMILE_FO` entry in a
/// `sql.json` document; `Ok(None)` when the document has no such entry.
///
/// # Errors
/// The entry is unusable: no connection, a bad `auth` or `transport`, or a password that
/// does not unseal. The message never echoes the connection string.
pub fn entry(doc: &Value) -> Result<Option<(String, Option<Rung>)>, String> {
    let Some(entry) = doc.get("databases").and_then(|d| d.get(DATABASE)) else {
        return Ok(None);
    };
    let bad = |m: &str| format!("{SQL_FILE}: database {DATABASE}: {m}");
    let (connection, _) = secret::resolve(entry).map_err(|e| bad(&e.to_string()))?;
    let pin = match entry.get("transport") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let s = v
                .as_str()
                .ok_or_else(|| bad("transport must be a string"))?;
            Rung::parse(s).map_err(|m| bad(&m))?
        }
    };
    Ok(Some((connection, pin)))
}

/// The agent's data directory, where `sql.json` lives, from a module's own directory
/// (`<data>/modules/<name>`, `ModuleContext::data_dir`); `None` for any other shape.
pub fn agent_data_dir(module_dir: &Path) -> Option<&Path> {
    let modules = module_dir.parent()?;
    if modules.file_name()? != "modules" {
        return None;
    }
    modules.parent()
}

/// Points `settings` at the core's `SMILE_FO` when the agent's `sql.json` declares it, and
/// returns the source now in use. `module_dir` is the module's `ModuleContext::data_dir`.
///
/// # Errors
/// `sql.json` cannot be read or its `SMILE_FO` entry is unusable; `settings` then keep the
/// appsettings connection and the caller reports the message (never a secret).
pub fn apply(settings: &mut Settings, module_dir: &Path) -> Result<PmsSource, String> {
    if let Some((connection, pin)) = core_entry(module_dir)? {
        settings.pms_connection_string = connection;
        settings.pms_source = PmsSource::Core { pin };
    }
    Ok(settings.pms_source)
}

/// The `SMILE_FO` entry of the agent's `sql.json` for a module directory: `None` when the
/// directory is not `<data>/modules/<name>`, the file is missing or has no such entry.
///
/// # Errors
/// `sql.json` cannot be read or the entry is unusable (the message never echoes a secret).
pub fn core_entry(module_dir: &Path) -> Result<Option<(String, Option<Rung>)>, String> {
    let Some(data_dir) = agent_data_dir(module_dir) else {
        return Ok(None);
    };
    let doc = dl_agent::store::read_json(&data_dir.join(SQL_FILE))
        .map_err(|e| format!("{SQL_FILE}: {e}"))?;
    Ok(doc.as_ref().map(entry).transpose()?.flatten())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_json::json;

    const WINDOWS: &str =
        "Server=\"pms\";Database=\"SMILE_FO\";Integrated Security=SSPI;TrustServerCertificate=true";

    fn module(root: &Path) -> std::path::PathBuf {
        root.join("modules").join("arrival")
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("arrival-core-sql-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        d
    }

    #[test]
    fn the_smile_fo_entry_and_its_rung_are_read() {
        let doc = json!({"databases": {"SMILE_FO": {
            "connection": WINDOWS, "auth": "windows", "transport": "tls1.0"}}});
        assert_eq!(
            entry(&doc).unwrap(),
            Some((WINDOWS.to_string(), Some(Rung::Tls10)))
        );
        let auto = json!({"databases": {"SMILE_FO": {"connection": WINDOWS, "transport": "auto"}}});
        assert_eq!(entry(&auto).unwrap(), Some((WINDOWS.to_string(), None)));
        let other = json!({"databases": {"SMILE_BO": {"connection": WINDOWS}}});
        assert_eq!(entry(&other).unwrap(), None);
        assert_eq!(entry(&Value::Null).unwrap(), None);
    }

    #[test]
    fn an_unusable_entry_is_an_error_that_never_echoes_the_connection() {
        for e in [
            json!({"connection": WINDOWS, "transport": "tls9"}),
            json!({"connection": WINDOWS, "auth": "sql"}),
            json!({"connection": "Server=pms;User Id=u", "password_sealed": "not-a-seal"}),
            json!({"auth": "windows"}),
        ] {
            let doc = json!({"databases": {"SMILE_FO": e}});
            let err = entry(&doc).expect_err("unusable");
            assert!(err.starts_with("sql.json: database SMILE_FO: "), "{err}");
            assert!(!err.contains("Server="), "{err}");
        }
    }

    #[test]
    fn apply_prefers_the_core_entry_and_keeps_appsettings_without_one() {
        let base = Settings {
            pms_connection_string: "Server=old;Database=PMS;User Id=u;Password=p".into(),
            ..Settings::default()
        };

        let empty = dir("empty");
        let mut s = base.clone();
        assert_eq!(apply(&mut s, &module(&empty)), Ok(PmsSource::AppSettings));
        assert_eq!(s, base);

        let core = dir("core");
        let doc = json!({"databases": {"SMILE_FO": {
            "connection": WINDOWS, "auth": "windows", "transport": "unencrypted"}}});
        std::fs::write(core.join(SQL_FILE), dl_json::to_string(&doc)).expect("write");
        let mut s = base.clone();
        assert_eq!(
            apply(&mut s, &core),
            Ok(PmsSource::AppSettings),
            "not a module dir"
        );
        let source = apply(&mut s, &module(&core)).expect("apply");
        assert_eq!(
            source,
            PmsSource::Core {
                pin: Some(Rung::Unencrypted)
            }
        );
        assert_eq!(source.name(), "sql.json");
        assert_eq!(s.pms_connection_string, WINDOWS);
        assert_eq!(s.pms_source, source);

        let broken = dir("broken");
        let doc = json!({"databases": {"SMILE_FO": {"connection": WINDOWS, "auth": "sql"}}});
        std::fs::write(broken.join(SQL_FILE), dl_json::to_string(&doc)).expect("write");
        let mut s = base.clone();
        assert!(apply(&mut s, &module(&broken)).is_err());
        assert_eq!(s, base, "a broken entry leaves the appsettings connection");

        for d in [empty, core, broken] {
            std::fs::remove_dir_all(d).expect("clean up");
        }
    }
}
