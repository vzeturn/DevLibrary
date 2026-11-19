//! Reading the Go SRBackup agent's `agent.json` (read only), so `backup-enroll` can continue
//! as the same hub agent instead of registering a new one. The port of the Go agent's
//! `internal/config` discovery, `internal/secret` (at-rest sealing) and `internal/hwid`.
//!
//! Sealed values are `enc:v1:` + base64(nonce‖ciphertext‖tag), AES-256-GCM under
//! HKDF-SHA256(salt = 32 zero bytes, ikm = machine id, info = `srbackup-agent-at-rest-v1`).
//! The machine id is the Windows `MachineGuid`, or `host-` + hex of the first 8 bytes of
//! SHA-256(hostname) when the registry cannot be read. A value without the prefix is a
//! plaintext from an older agent and is used as it is, as Go's `MustDecrypt` does.

use std::path::{Path, PathBuf};

use dl_encoding::base64::STANDARD;
use dl_hash::{Digest, Sha256};
use dl_json::Value;

use crate::crypto::{gcm, hkdf_sha256};

/// The marker of a value sealed by the Go agent.
const PREFIX: &str = "enc:v1:";
/// HKDF info binding the key to the Go agent's at-rest use.
const KEY_INFO: &[u8] = b"srbackup-agent-at-rest-v1";
/// AES-GCM standard nonce size (Go's `gcm.NonceSize()`).
const NONCE_LEN: usize = 12;

/// The machine id the Go agent derived its at-rest key from.
#[derive(Clone, PartialEq, Eq)]
pub struct MachineId {
    /// The id itself (a GUID, or `host-<16 hex>`).
    pub id: String,
    /// Why the hostname fallback was used, when it was.
    pub fallback: Option<String>,
}

impl std::fmt::Debug for MachineId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MachineId")
            .field("fallback", &self.fallback)
            .finish_non_exhaustive()
    }
}

impl MachineId {
    /// This machine's id, read the way Go's `hwid.Get()` does.
    pub fn current() -> MachineId {
        match machine_guid() {
            Ok(id) => MachineId { id, fallback: None },
            Err(why) => MachineId {
                id: hostname_id(&dl_agent::hub::hostname()),
                fallback: Some(why),
            },
        }
    }
}

/// Go's fallback id: `host-` + hex of the first 8 bytes of SHA-256(hostname).
pub fn hostname_id(host: &str) -> String {
    let sum = Sha256::digest(host.as_bytes());
    let hex: String = sum.as_ref()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("host-{hex}")
}

#[cfg(windows)]
fn machine_guid() -> Result<String, String> {
    use dl_winos::registry::{Hive, Key, View};
    // The 64-bit view: the Go agent ships as amd64 and `reg query` from it reads that view.
    let key = Key::open(
        Hive::LocalMachine,
        r"SOFTWARE\Microsoft\Cryptography",
        View::Wow64_64,
    )
    .map_err(|e| format!("open HKLM\\SOFTWARE\\Microsoft\\Cryptography: {e}"))?;
    match key.string("MachineGuid") {
        Ok(Some(id)) if !id.trim().is_empty() => Ok(id.trim().to_owned()),
        Ok(_) => Err("MachineGuid is missing or empty".into()),
        Err(e) => Err(format!("read MachineGuid: {e}")),
    }
}

#[cfg(not(windows))]
fn machine_guid() -> Result<String, String> {
    Err("no MachineGuid off Windows".into())
}

/// The Go agent's at-rest key for `machine_id`.
fn derive_key(machine_id: &str) -> [u8; 32] {
    let mut key = [0u8; 32];
    hkdf_sha256(machine_id.as_bytes(), &[0u8; 32], KEY_INFO, &mut key);
    key
}

/// Opens a value the Go agent stored: sealed (`enc:v1:`) under `machine_id`, or a plaintext
/// left by an older agent. Errors never contain the value.
pub fn open_value(stored: &str, machine_id: &str) -> Result<String, String> {
    // Go's IsEncrypted: the prefix followed by at least one byte.
    let Some(b64) = stored.strip_prefix(PREFIX).filter(|r| !r.is_empty()) else {
        return Ok(stored.to_owned());
    };
    let raw = STANDARD
        .decode(b64)
        .map_err(|e| format!("decode sealed value: {e}"))?;
    if raw.len() < NONCE_LEN {
        return Err("sealed value is too short to contain a nonce".into());
    }
    let (nonce, sealed) = raw.split_at(NONCE_LEN);
    let plain = gcm::open(&derive_key(machine_id), nonce, &[], sealed).map_err(|_| {
        "cannot decrypt: the value was sealed on a different machine, or has been altered"
            .to_owned()
    })?;
    String::from_utf8(plain).map_err(|_| "sealed value is not UTF-8".to_owned())
}

/// The Go agent's identity at the hub.
#[derive(Clone, PartialEq, Eq)]
pub struct GoAgent {
    /// Where `agent.json` was read.
    pub path: PathBuf,
    /// `server_url` as stored.
    pub server_url: String,
    /// `agent_id`.
    pub agent_id: String,
    /// The opened `api_key` (a secret).
    pub api_key: String,
}

impl std::fmt::Debug for GoAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoAgent")
            .field("path", &self.path)
            .field("server_url", &self.server_url)
            .field("agent_id", &self.agent_id)
            .finish_non_exhaustive()
    }
}

/// Where to look for the Go agent's config, and the machine id to open it with.
#[derive(Debug, Clone)]
pub struct GoSource {
    /// Candidate `agent.json` paths, most preferred first.
    pub paths: Vec<PathBuf>,
    /// The machine id the values were sealed under.
    pub machine: MachineId,
}

impl GoSource {
    /// The Go agent's own search order on this machine.
    pub fn current() -> GoSource {
        GoSource {
            paths: candidates(&|k| std::env::var(k).ok().filter(|v| !v.is_empty())),
            machine: MachineId::current(),
        }
    }

    /// No Go agent to look for (tests, and callers that must not adopt).
    pub fn none() -> GoSource {
        GoSource {
            paths: Vec::new(),
            machine: MachineId {
                id: String::new(),
                fallback: None,
            },
        }
    }
}

/// Go's `windowsConfigCandidates` (or its non-Windows list), minus the Go executable's own
/// directory, which this binary cannot know (the installer keeps the file in ProgramData).
pub fn candidates(env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if cfg!(windows) {
        match env("PROGRAMDATA") {
            Some(pd) => paths.push(Path::new(&pd).join("srbackup").join("agent.json")),
            None => paths.push(PathBuf::from(r"C:\ProgramData\srbackup\agent.json")),
        }
        if let Some(ap) = env("APPDATA") {
            paths.push(Path::new(&ap).join("srbackup").join("agent.json"));
        }
        if let Some(la) = env("LOCALAPPDATA") {
            paths.push(Path::new(&la).join("srbackup").join("agent.json"));
        }
    } else {
        if let Some(home) = env("HOME") {
            paths.push(Path::new(&home).join(".srbackup").join("agent.json"));
        }
        paths.push(PathBuf::from("/etc/srbackup/agent.json"));
    }
    paths
}

/// Finds the config the Go agent would load (the first candidate that reads, parses and has
/// a `server_url`, as Go's auto-discovery does) and opens its identity.
///
/// `Ok(None)`: no Go config on this machine. `Err`: one was found but cannot be adopted
/// (the reason names no secret).
pub fn find(source: &GoSource) -> Result<Option<GoAgent>, String> {
    let mut skipped: Vec<String> = Vec::new();
    for path in &source.paths {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                skipped.push(format!("read {}: {e}", path.display()));
                continue;
            }
        };
        let doc = match dl_json::parse_slice(&bytes) {
            Ok(d) => d,
            Err(e) => {
                skipped.push(format!("parse {}: {e}", path.display()));
                continue;
            }
        };
        let field = |k: &str| doc.get(k).and_then(Value::as_str).unwrap_or_default();
        if field("server_url").is_empty() {
            continue;
        }
        return open_agent(path, &doc, &source.machine).map(Some);
    }
    if skipped.is_empty() {
        Ok(None)
    } else {
        Err(skipped.join("; "))
    }
}

fn open_agent(path: &Path, doc: &Value, machine: &MachineId) -> Result<GoAgent, String> {
    let field = |k: &str| {
        doc.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let at = path.display();
    let agent_id = field("agent_id");
    if agent_id.is_empty() {
        return Err(format!("{at} has no agent_id"));
    }
    let api_key = open_value(&field("api_key"), &machine.id).map_err(|e| {
        let why = machine
            .fallback
            .as_deref()
            .map(|f| format!(" (machine id from the hostname fallback: {f})"))
            .unwrap_or_default();
        format!("{at}: api_key: {e}{why}")
    })?;
    if api_key.is_empty() {
        return Err(format!("{at} has no api_key"));
    }
    Ok(GoAgent {
        path: path.to_path_buf(),
        server_url: field("server_url"),
        agent_id,
        api_key,
    })
}

/// Whether the Go agent's `server_url` names the same origin as `origin` (already
/// validated): same scheme, host (any case) and effective port, no path.
pub fn same_origin(server_url: &str, origin: &str) -> bool {
    let parse = |u: &str| dl_http::Url::parse(u.trim().trim_end_matches('/')).ok();
    let (Some(a), Some(b)) = (parse(server_url), parse(origin)) else {
        return false;
    };
    let path_ok = |u: &dl_http::Url| u.path().is_empty() || u.path() == "/";
    path_ok(&a)
        && path_ok(&b)
        && a.scheme() == b.scheme()
        && a.host().eq_ignore_ascii_case(b.host())
        && a.port() == b.port()
}
