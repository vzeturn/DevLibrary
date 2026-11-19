//! The HL agent's local config (Go `config.go`): `%ProgramData%\SMILE-HL-Agent\config.json`,
//! read where the Go service left it so a site switches with no re-provisioning.
//!
//! Secrets (`token`, `sql.password`, `sql.connectionStringOverride`) are stored as `enc:` +
//! base64 of a DPAPI machine-scope blob (no entropy). A plaintext secret (written by the
//! install script) is encrypted and written back on the first load. The file is edited as a
//! JSON value, so fields this module does not use (`ui`, `update`, `configToken`) survive.

use std::path::{Path, PathBuf};

use dl_encoding::base64::STANDARD as B64;
use dl_json::{Map, Value};

/// The prefix of an encrypted secret.
pub const ENC_PREFIX: &str = "enc:";
/// The env var that overrides the config path (Go `SMILEHL_AGENT_CONFIG`).
pub const CONFIG_ENV: &str = "SMILEHL_AGENT_CONFIG";
/// The JSON paths of the secrets.
const SECRETS: [&[&str]; 3] = [
    &["token"],
    &["sql", "password"],
    &["sql", "connectionStringOverride"],
];

/// The `sql` section.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SqlConfig {
    /// `server`.
    pub server: String,
    /// `database`.
    pub database: String,
    /// `trustedConnection`.
    pub trusted_connection: bool,
    /// `user`.
    pub user: String,
    /// `password` (secret, decrypted).
    pub password: String,
    /// `connectionStringOverride` (secret, decrypted).
    pub connection_string_override: String,
}

impl std::fmt::Debug for SqlConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlConfig")
            .field("server", &self.server)
            .field("database", &self.database)
            .field("trusted_connection", &self.trusted_connection)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

impl SqlConfig {
    /// `ConnString`: the override, or a go-mssqldb string (`dl-tds` reads the same keys).
    pub fn conn_string(&self) -> String {
        if !self.connection_string_override.is_empty() {
            return self.connection_string_override.clone();
        }
        let mut parts = vec![
            format!("server={}", self.server),
            format!("database={}", self.database),
            "encrypt=disable".to_owned(),
            "TrustServerCertificate=true".to_owned(),
            "connection timeout=15".to_owned(),
        ];
        if self.trusted_connection {
            parts.push("trusted_connection=yes".to_owned());
        } else {
            parts.push(format!("user id={}", self.user));
            parts.push(format!("password={}", self.password));
        }
        parts.join(";")
    }
}

/// `LocalConfig` (the fields the module uses).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct LocalConfig {
    /// `hubUrl`.
    pub hub_url: String,
    /// `hubUrlFallbacks`.
    pub hub_url_fallbacks: Vec<String>,
    /// `hotelId`.
    pub hotel_id: String,
    /// `token` (secret, decrypted).
    pub token: String,
    /// `instanceId`: this install's id for the hub's single-active check ("" = legacy).
    pub instance_id: String,
    /// `sql`.
    pub sql: SqlConfig,
}

impl std::fmt::Debug for LocalConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalConfig")
            .field("hub_url", &self.hub_url)
            .field("hub_url_fallbacks", &self.hub_url_fallbacks)
            .field("hotel_id", &self.hotel_id)
            .field("instance_id", &self.instance_id)
            .field("sql", &self.sql)
            .finish_non_exhaustive()
    }
}

impl LocalConfig {
    /// `Validate`: why the config cannot run, or `None`.
    pub fn problem(&self) -> Option<&'static str> {
        let s = &self.sql;
        let over = !s.connection_string_override.is_empty();
        if self.hub_url.is_empty() || self.hub_url.starts_with("https://hub.example.com") {
            Some("config has no hubUrl")
        } else if self.hotel_id.is_empty() {
            Some("config has no hotelId")
        } else if self.token.is_empty() {
            Some("config has no token (from the hotel's page on the hub dashboard)")
        } else if !over && (s.server.is_empty() || s.database.is_empty()) {
            Some("config has no SQL server/database")
        } else if !over && !s.trusted_connection && s.user.is_empty() {
            Some("config SQL has no user (or set trustedConnection)")
        } else {
            None
        }
    }

    fn from_value(v: &Value) -> LocalConfig {
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
        let sql = v.get("sql").cloned().unwrap_or(Value::Null);
        LocalConfig {
            hub_url: s(v, "hubUrl").trim_end_matches('/').to_owned(),
            hub_url_fallbacks: v
                .get("hubUrlFallbacks")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            hotel_id: s(v, "hotelId"),
            token: s(v, "token"),
            instance_id: s(v, "instanceId"),
            sql: SqlConfig {
                server: s(&sql, "server"),
                database: s(&sql, "database"),
                trusted_connection: sql
                    .get("trustedConnection")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                user: s(&sql, "user"),
                password: s(&sql, "password"),
                connection_string_override: s(&sql, "connectionStringOverride"),
            },
        }
    }
}

/// `DefaultConfigPath`.
pub fn default_config_path() -> PathBuf {
    if let Some(p) = std::env::var_os(CONFIG_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("ProgramData")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("SMILE-HL-Agent").join("config.json")
}

/// `stripUTF8BOM`.
fn strip_bom(raw: &[u8]) -> &[u8] {
    raw.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(raw)
}

/// Reads the file as a JSON object (`None` when it does not exist).
fn read_value(path: &Path) -> Result<Option<Value>, String> {
    let raw = match std::fs::read(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let v = dl_json::parse_slice(strip_bom(&raw))
        .map_err(|e| format!("config {} is not valid JSON: {e}", path.display()))?;
    if v.as_object().is_none() {
        return Err(format!("config {} is not a JSON object", path.display()));
    }
    Ok(Some(v))
}

fn field_mut<'a>(v: &'a mut Value, path: &[&str]) -> Option<&'a mut Value> {
    path.iter().try_fold(v, |cur, k| cur.get_mut(k))
}

/// `LoadConfig` without the template: `Ok(None)` when the file is missing. Plaintext
/// secrets are encrypted and written back (seed-once). The config is not validated.
pub fn load(path: &Path) -> Result<Option<LocalConfig>, String> {
    let Some((cfg, v, plaintext)) = decode(path)? else {
        return Ok(None);
    };
    if plaintext {
        let mut enc = v;
        encrypt_secrets(&mut enc)?;
        write_atomic(path, dl_json::to_string_pretty(&enc).as_bytes())
            .map_err(|e| format!("re-encrypt config {}: {e}", path.display()))?;
    }
    Ok(Some(cfg))
}

/// [`load`] without the seed-once write: plaintext secrets stay as they are on disk (the
/// self-test reads the config, it never rewrites it).
pub fn read(path: &Path) -> Result<Option<LocalConfig>, String> {
    Ok(decode(path)?.map(|(cfg, _, _)| cfg))
}

/// The config with its secrets decrypted, the decrypted JSON, and whether any secret was
/// stored in plaintext.
fn decode(path: &Path) -> Result<Option<(LocalConfig, Value, bool)>, String> {
    let Some(mut v) = read_value(path)? else {
        return Ok(None);
    };
    let mut plaintext = false;
    for p in SECRETS {
        let Some(slot) = field_mut(&mut v, p) else {
            continue;
        };
        let Some(text) = slot.as_str().filter(|t| !t.is_empty()).map(str::to_owned) else {
            continue;
        };
        let plain = match text.strip_prefix(ENC_PREFIX) {
            Some(enc) => decrypt(enc).map_err(|e| {
                format!(
                    "decrypt {} in {} failed (config from another machine?): {e}",
                    p.join("."),
                    path.display()
                )
            })?,
            None => {
                plaintext = true;
                text
            }
        };
        *slot = Value::from(plain);
    }
    let cfg = LocalConfig::from_value(&v);
    Ok(Some((cfg, v, plaintext)))
}

/// `encryptField` over every secret of `v` (already-encrypted and empty values stay).
fn encrypt_secrets(v: &mut Value) -> Result<(), String> {
    for p in SECRETS {
        if let Some(slot) = field_mut(v, p)
            && let Some(text) = slot.as_str()
            && !text.is_empty()
            && !text.starts_with(ENC_PREFIX)
        {
            *slot = Value::from(encrypt(text)?);
        }
    }
    Ok(())
}

/// What `hl-enroll` writes (the hub's install script fields).
#[derive(Clone, Default)]
pub struct Enrollment {
    /// `hubUrl`.
    pub hub_url: String,
    /// `hotelId`.
    pub hotel_id: String,
    /// `token`.
    pub token: String,
    /// `instanceId`.
    pub instance_id: String,
    /// SQL login; empty = Windows authentication.
    pub sql_user: String,
    /// SQL password (with `sql_user`).
    pub sql_password: String,
}

/// Writes `e` into the config at `path` the way the hub's install script does: keeps the
/// other fields of an existing (readable) file, forces `sql` to the local `SMILE_FO`, and
/// stores the secrets encrypted.
pub fn write_enrollment(path: &Path, e: &Enrollment) -> Result<(), String> {
    let mut obj = match read_value(path) {
        Ok(Some(Value::Object(m))) => m,
        // The install script starts over on an unreadable file, too.
        Ok(_) | Err(_) => Map::new(),
    };
    obj.insert("hubUrl", e.hub_url.trim_end_matches('/'));
    obj.insert("hotelId", e.hotel_id.as_str());
    obj.insert("token", e.token.as_str());
    obj.insert("instanceId", e.instance_id.as_str());
    let mut sql = Map::new();
    sql.insert("server", ".");
    sql.insert("database", "SMILE_FO");
    sql.insert("trustedConnection", e.sql_user.is_empty());
    sql.insert("user", e.sql_user.as_str());
    let password = if e.sql_user.is_empty() {
        ""
    } else {
        e.sql_password.as_str()
    };
    sql.insert("password", password);
    obj.insert("sql", Value::Object(sql));
    let mut v = Value::Object(obj);
    encrypt_secrets(&mut v)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    write_atomic(path, dl_json::to_string_pretty(&v).as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))
}

fn encrypt(plain: &str) -> Result<String, String> {
    let blob = protect(plain.as_bytes())?;
    Ok(format!("{ENC_PREFIX}{}", B64.encode(blob)))
}

fn decrypt(b64: &str) -> Result<String, String> {
    let raw = B64.decode(b64).map_err(|e| format!("base64: {e}"))?;
    let plain = unprotect(&raw)?;
    String::from_utf8(plain).map_err(|_| "secret is not UTF-8".to_owned())
}

#[cfg(windows)]
fn protect(p: &[u8]) -> Result<Vec<u8>, String> {
    dl_winsvc::dpapi::protect(p, dl_winsvc::dpapi::Scope::LocalMachine).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn unprotect(b: &[u8]) -> Result<Vec<u8>, String> {
    dl_winsvc::dpapi::unprotect(b, dl_winsvc::dpapi::Scope::LocalMachine).map_err(|e| e.to_string())
}

/// Off Windows there is no DPAPI: the blob is the plaintext (tests and dev runs only).
#[cfg(not(windows))]
fn protect(p: &[u8]) -> Result<Vec<u8>, String> {
    Ok(p.to_vec())
}

#[cfg(not(windows))]
fn unprotect(b: &[u8]) -> Result<Vec<u8>, String> {
    Ok(b.to_vec())
}

/// Writes through a temp file and a rename (owner-only on Unix).
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests;
