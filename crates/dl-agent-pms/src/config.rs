//! Agent settings from a JSON file, then the environment (`PMS_AGENT_*`), which wins.
//! The enrollment code is used once: after a successful enrollment it is removed from
//! the config file ([`scrub_enrollment_code`]).
//!
//! The file is decoded like Go's `encoding/json` with `DisallowUnknownFields`: keys match
//! field names ignoring case (the last match wins) and an unknown key is an error.

use std::path::{Path, PathBuf};

use dl_json::{Map, Value};

use crate::json;

/// Config file path.
pub const ENV_CONFIG_PATH: &str = "PMS_AGENT_CONFIG";
/// Overrides `cloud_url`.
pub const ENV_CLOUD_URL: &str = "PMS_AGENT_CLOUD_URL";
/// Overrides `state_dir`.
pub const ENV_STATE_DIR: &str = "PMS_AGENT_STATE_DIR";
/// Overrides `enrollment_code`.
pub const ENV_ENROLLMENT_CODE: &str = "PMS_AGENT_ENROLLMENT_CODE";
/// Overrides `log_level`.
pub const ENV_LOG_LEVEL: &str = "PMS_AGENT_LOG_LEVEL";

/// The pre-migration backup proof of an old config (ignored: MM_PMS migrations run in
/// the cloud since the SMILE-only contract).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationBackup {
    /// `copy_only`, `attested` or empty (refuse files that need a backup).
    pub mode: String,
    /// Backup directory on the SQL Server host (copy_only).
    pub dir: String,
    /// Attestation marker file (attested).
    pub marker_file: String,
    /// Maximum attestation age (attested; 0 = 6 hours).
    pub max_age_minutes: i64,
}

/// The `migration` section of an old SmilePmsAgent config. Still parsed so such a file
/// keeps loading; nothing reads it, since no migration runs on a site any more.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Migration {
    /// The pinned keyring JSON, `{"keys":[{"key_id","public_key"}]}` (public keys only).
    pub keyring: Option<Value>,
    /// The backup gate.
    pub backup: MigrationBackup,
    /// Legacy FO database name (default SMILE_FO).
    pub legacy_fo_db: String,
    /// Legacy POS database name (default SMILE_POS).
    pub legacy_pos_db: String,
    /// Legacy BO database name (default SMILE_BO).
    pub legacy_bo_db: String,
}

/// The agent configuration.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    /// Tenant host, e.g. `https://acme.pms.example.com` (https, or http to loopback).
    pub cloud_url: String,
    /// Holds the protected key, the identity, databases.json and the logs.
    pub state_dir: PathBuf,
    /// One-time enrollment code (read only until enrolled).
    pub enrollment_code: String,
    /// debug, info, warn or error.
    pub log_level: String,
    /// Old migrator settings, parsed and ignored.
    pub migration: Migration,
    /// The file this config was read from (`None` when there was none).
    pub path: Option<PathBuf>,
    /// The enrollment code came from the file, not the environment.
    pub code_from_file: bool,
}

/// Where the service looks for its config when none is given.
pub fn default_path() -> PathBuf {
    if cfg!(windows) {
        let base = std::env::var("ProgramData")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| r"C:\ProgramData".to_owned());
        return Path::new(&base)
            .join("SmilePMS")
            .join("Agent")
            .join("config.json");
    }
    PathBuf::from("pms-agent.json")
}

fn default_state_dir(cfg_path: Option<&Path>) -> PathBuf {
    if cfg!(windows) {
        let p = default_path();
        return p.parent().unwrap_or(Path::new("")).join("state");
    }
    match cfg_path {
        Some(p) => p.parent().unwrap_or(Path::new("")).join(".pms-agent"),
        None => PathBuf::from(".pms-agent"),
    }
}

fn unknown_keys(obj: &Map, fields: &[&str]) -> Result<(), String> {
    for (k, _) in obj.iter() {
        if !fields.iter().any(|f| k.eq_ignore_ascii_case(f)) {
            return Err(format!("json: unknown field {}", json::quote(k)));
        }
    }
    Ok(())
}

fn get_str(obj: &Map, name: &str, owner: &str) -> Result<String, String> {
    match json::field_ci(obj, name) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(v) => Err(format!(
            "json: cannot unmarshal {} into Go struct field {owner}.{name} of type string",
            v.type_name()
        )),
    }
}

fn get_obj<'a>(obj: &'a Map, name: &str, owner: &str) -> Result<Option<&'a Map>, String> {
    match json::field_ci(obj, name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(m)) => Ok(Some(m)),
        Some(v) => Err(format!(
            "json: cannot unmarshal {} into Go struct field {owner}.{name} of type object",
            v.type_name()
        )),
    }
}

fn decode(raw: &[u8]) -> Result<Config, String> {
    let v = json::parse(raw).map_err(|e| e.to_string())?;
    let obj = match v {
        Value::Object(m) => m,
        Value::Null => return Ok(Config::default()),
        other => {
            return Err(format!(
                "json: cannot unmarshal {} into Go value of type config.Config",
                other.type_name()
            ));
        }
    };
    unknown_keys(
        &obj,
        &[
            "cloud_url",
            "state_dir",
            "enrollment_code",
            "log_level",
            "migration",
        ],
    )?;
    let mut cfg = Config {
        cloud_url: get_str(&obj, "cloud_url", "Config")?,
        state_dir: PathBuf::from(get_str(&obj, "state_dir", "Config")?),
        enrollment_code: get_str(&obj, "enrollment_code", "Config")?,
        log_level: get_str(&obj, "log_level", "Config")?,
        ..Config::default()
    };
    if let Some(m) = get_obj(&obj, "migration", "Config")? {
        unknown_keys(
            m,
            &[
                "keyring",
                "backup",
                "legacy_fo_db",
                "legacy_pos_db",
                "legacy_bo_db",
            ],
        )?;
        cfg.migration.keyring = json::field_ci(m, "keyring").cloned();
        cfg.migration.legacy_fo_db = get_str(m, "legacy_fo_db", "Migration")?;
        cfg.migration.legacy_pos_db = get_str(m, "legacy_pos_db", "Migration")?;
        cfg.migration.legacy_bo_db = get_str(m, "legacy_bo_db", "Migration")?;
        if let Some(b) = get_obj(m, "backup", "Migration")? {
            unknown_keys(b, &["mode", "dir", "marker_file", "max_age_minutes"])?;
            cfg.migration.backup = MigrationBackup {
                mode: get_str(b, "mode", "MigrationBackup")?,
                dir: get_str(b, "dir", "MigrationBackup")?,
                marker_file: get_str(b, "marker_file", "MigrationBackup")?,
                max_age_minutes: match json::field_ci(b, "max_age_minutes") {
                    None | Some(Value::Null) => 0,
                    Some(v) => json::as_int(v).ok_or_else(|| {
                        format!(
                            "json: cannot unmarshal {} into Go struct field MigrationBackup.max_age_minutes of type int",
                            v.type_name()
                        )
                    })?,
                },
            };
        }
    }
    Ok(cfg)
}

/// The config file [`load`] reads: `path`, else `$PMS_AGENT_CONFIG`, else [`default_path`].
pub fn resolve_path(path: Option<&Path>, getenv: &dyn Fn(&str) -> String) -> PathBuf {
    match path {
        Some(p) if !p.as_os_str().is_empty() => p.to_owned(),
        _ => {
            let env = getenv(ENV_CONFIG_PATH);
            if env.is_empty() {
                default_path()
            } else {
                PathBuf::from(env)
            }
        }
    }
}

/// Reads `path` (or `$PMS_AGENT_CONFIG`, or [`default_path`]); a missing file is fine when
/// the environment supplies the values. Applies the environment, fills defaults and
/// validates.
pub fn load(path: Option<&Path>, getenv: &dyn Fn(&str) -> String) -> Result<Config, String> {
    let path = resolve_path(path, getenv);
    let mut cfg = match std::fs::read(&path) {
        Ok(raw) => {
            let mut c = decode(&raw).map_err(|e| format!("config {}: {e}", path.display()))?;
            c.code_from_file = !c.enrollment_code.is_empty();
            c.path = Some(path.clone());
            c
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => return Err(format!("config {}: {e}", path.display())),
    };
    let env = |k: &str| Some(getenv(k)).filter(|v| !v.is_empty());
    if let Some(v) = env(ENV_CLOUD_URL) {
        cfg.cloud_url = v;
    }
    if let Some(v) = env(ENV_STATE_DIR) {
        cfg.state_dir = PathBuf::from(v);
    }
    if let Some(v) = env(ENV_ENROLLMENT_CODE) {
        cfg.enrollment_code = v;
        cfg.code_from_file = false;
    }
    if let Some(v) = env(ENV_LOG_LEVEL) {
        cfg.log_level = v;
    }
    if cfg.state_dir.as_os_str().is_empty() {
        cfg.state_dir = default_state_dir(cfg.path.as_deref());
    }
    cfg.cloud_url = cfg.cloud_url.trim().trim_end_matches('/').to_owned();
    validate_cloud_url(&cfg.cloud_url)?;
    Ok(cfg)
}

/// The parts of an absolute URL Go's `url.Parse` would report.
struct UrlParts<'a> {
    scheme: String,
    /// Go's `u.Host` (authority without user info).
    host_port: &'a str,
    /// Go's `u.Hostname()`.
    host: &'a str,
    has_user: bool,
    query: &'a str,
    fragment: &'a str,
}

fn split_url(raw: &str) -> Option<UrlParts<'_>> {
    let (scheme, rest) = raw.split_once("://")?;
    if scheme.is_empty()
        || !scheme.as_bytes()[0].is_ascii_alphabetic()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.')
        || raw.bytes().any(|b| b < 0x20 || b == 0x7f)
    {
        return None;
    }
    let (before_frag, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (before_query, query) = before_frag.split_once('?').unwrap_or((before_frag, ""));
    let authority = &before_query[..before_query.find('/').unwrap_or(before_query.len())];
    let (has_user, host_port) = match authority.rfind('@') {
        Some(i) => (true, &authority[i + 1..]),
        None => (false, authority),
    };
    let host = if let Some(s) = host_port.strip_prefix('[') {
        let end = s.find(']')?;
        let after = &s[end + 1..];
        if !(after.is_empty() || after.starts_with(':')) {
            return None;
        }
        &s[..end]
    } else {
        match host_port.rfind(':') {
            Some(i) => {
                if !host_port[i + 1..].bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                &host_port[..i]
            }
            None => host_port,
        }
    };
    if host.contains(' ') {
        return None;
    }
    Some(UrlParts {
        scheme: scheme.to_ascii_lowercase(),
        host_port,
        host,
        has_user,
        query,
        fragment,
    })
}

fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Checks a cloud URL the way [`load`] does (https, or http to loopback; no query,
/// fragment or credentials).
pub fn validate_cloud_url(raw: &str) -> Result<(), String> {
    if raw.is_empty() {
        return Err(format!("cloud_url is required (file or {ENV_CLOUD_URL})"));
    }
    let bad =
        || "cloud_url must be an absolute URL without query, fragment or credentials".to_owned();
    let u = split_url(raw).ok_or_else(bad)?;
    if u.host_port.is_empty() || !u.query.is_empty() || !u.fragment.is_empty() || u.has_user {
        return Err(bad());
    }
    match u.scheme.as_str() {
        "https" => return Ok(()),
        "http" if is_loopback(u.host) => return Ok(()),
        _ => {}
    }
    Err("cloud_url must use https (http is allowed for loopback hosts only)".into())
}

/// Removes `enrollment_code` from the config file after it was redeemed, keeping every
/// other key (written like Go's `json.MarshalIndent` of a map: keys sorted). The file is
/// replaced atomically.
pub fn scrub_enrollment_code(path: &Path) -> Result<(), String> {
    let e = |x: &dyn std::fmt::Display| format!("scrub enrollment code: {x}");
    let raw = std::fs::read(path).map_err(|x| e(&x))?;
    let Value::Object(obj) = json::parse(&raw).map_err(|x| e(&x))? else {
        return Err(e(&"config is not a JSON object"));
    };
    if !obj.contains_key("enrollment_code") {
        return Ok(());
    }
    let mut entries: Vec<(String, Value)> = obj
        .iter()
        .filter(|(k, _)| k.as_str() != "enrollment_code")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Map::new();
    for (k, v) in entries {
        out.insert(k, v);
    }
    let mut text = json::to_pretty(&Value::Object(out), "  ");
    text.push('\n');
    let perms = std::fs::metadata(path).map_err(|x| e(&x))?.permissions();
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).map_err(|x| e(&x))?;
    let _ = std::fs::set_permissions(&tmp, perms);
    std::fs::rename(&tmp, path).map_err(|x| e(&x))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pms-cfg-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn no_env(_: &str) -> String {
        String::new()
    }

    #[test]
    fn file_then_environment() {
        let d = tmp("load");
        let p = d.join("config.json");
        std::fs::write(
            &p,
            r#"{"cloud_url":" https://acme.pms.example.com/ ","Enrollment_Code":"ABC","migration":{"backup":{"mode":"attested","max_age_minutes":30}}}"#,
        )
        .unwrap();
        let c = load(Some(&p), &no_env).unwrap();
        assert_eq!(c.cloud_url, "https://acme.pms.example.com");
        assert_eq!(c.enrollment_code, "ABC");
        assert!(c.code_from_file);
        assert_eq!(c.migration.backup.max_age_minutes, 30);
        assert_eq!(c.path.as_deref(), Some(p.as_path()));
        let env = |k: &str| match k {
            ENV_ENROLLMENT_CODE => "ENV".to_owned(),
            ENV_STATE_DIR => "/var/x".to_owned(),
            _ => String::new(),
        };
        let c = load(Some(&p), &env).unwrap();
        assert_eq!(c.enrollment_code, "ENV");
        assert!(!c.code_from_file);
        assert_eq!(c.state_dir, PathBuf::from("/var/x"));
        std::fs::write(&p, r#"{"cloud_url":"https://x","extra":1}"#).unwrap();
        assert!(
            load(Some(&p), &no_env)
                .unwrap_err()
                .contains("unknown field \"extra\"")
        );
        std::fs::write(&p, r#"{"cloud_url":5}"#).unwrap();
        assert!(load(Some(&p), &no_env).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cloud_url_rules() {
        for ok in [
            "https://acme.pms.example.com",
            "https://acme.pms.example.com:8443/base",
            "http://localhost:8080",
            "http://127.0.0.1:9",
            "http://[::1]:9",
            "HTTPS://acme.example.com",
        ] {
            assert!(validate_cloud_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "acme.example.com",
            "http://acme.example.com",
            "https://acme.example.com?x=1",
            "https://acme.example.com#f",
            "https://u:p@acme.example.com",
            "ftp://acme.example.com",
            "https://",
            "http://10.0.0.1",
        ] {
            assert!(validate_cloud_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn scrub_keeps_other_keys_sorted() {
        let d = tmp("scrub");
        let p = d.join("config.json");
        std::fs::write(
            &p,
            r#"{"enrollment_code":"X","cloud_url":"https://a","migration":{"legacy_fo_db":"FO"}}"#,
        )
        .unwrap();
        scrub_enrollment_code(&p).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "{\n  \"cloud_url\": \"https://a\",\n  \"migration\": {\n    \"legacy_fo_db\": \"FO\"\n  }\n}\n"
        );
        scrub_enrollment_code(&p).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }
}
