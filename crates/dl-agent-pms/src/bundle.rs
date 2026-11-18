//! Verifies a signed MM_PMS migration bundle, format v1: `manifest.json` (the signed
//! canonical bytes), `manifest.sig` (detached Ed25519 signature) and `sql/<name>` files.
//! The signature is checked against a pinned key set first; only then is anything the
//! manifest says trusted: the hash chain, contiguous versions, file order, and every
//! file's length and sha256. Same rules and error codes as the Go and TS verifiers (both
//! run on `internal/bundle/testdata/golden`).

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::Path;

use dl_json::{Map, Value};

use crate::json;
use crate::pat;

/// The only manifest format accepted.
pub const FORMAT: &str = "mm-pms-migration-bundle/1";
/// The chain value before the first migration.
pub const GENESIS_CHAIN: &str = "0000000000000000000000000000000000000000000000000000000000000000";

const MAX_MANIFEST_BYTES: u64 = 16 << 20;
const MAX_SIG_BYTES: u64 = 4096;
const MAX_FILE_BYTES: u64 = 64 << 20;
const MAX_LOCK_TIMEOUT_MS: i64 = 600_000;

/// Error codes, identical to the TS verifier's.
pub const CODE_UNSIGNED: &str = "unsigned";
/// See [`CODE_UNSIGNED`].
pub const CODE_KEY_ID: &str = "key_id";
/// See [`CODE_UNSIGNED`].
pub const CODE_SIGNATURE: &str = "signature";
/// See [`CODE_UNSIGNED`].
pub const CODE_FORMAT: &str = "format";
/// See [`CODE_UNSIGNED`].
pub const CODE_RANGE: &str = "range";
/// See [`CODE_UNSIGNED`].
pub const CODE_CHAIN: &str = "chain";
/// See [`CODE_UNSIGNED`].
pub const CODE_SEQUENCE: &str = "sequence";
/// See [`CODE_UNSIGNED`].
pub const CODE_NAME: &str = "name";
/// See [`CODE_UNSIGNED`].
pub const CODE_FILES: &str = "files";
/// See [`CODE_UNSIGNED`].
pub const CODE_HASH: &str = "hash";

/// A verification failure with a stable code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleError {
    /// The code.
    pub code: &'static str,
    /// What is wrong (names and numbers only).
    pub msg: String,
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.msg)
    }
}

impl std::error::Error for BundleError {}

fn fail<T>(code: &'static str, msg: impl Into<String>) -> Result<T, BundleError> {
    Err(BundleError {
        code,
        msg: msg.into(),
    })
}

/// One migration in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Entry {
    /// Version.
    pub seq: i64,
    /// The file's leading number.
    pub number: i64,
    /// File name.
    pub name: String,
    /// sha256 of the file.
    pub sha256: String,
    /// The chain link.
    pub chain: String,
    /// File length.
    pub bytes: i64,
    /// Needs a fresh backup first.
    pub requires_backup: bool,
    /// `SET LOCK_TIMEOUT` for the file.
    pub lock_timeout_ms: i64,
    /// An online index build (runs outside a transaction).
    pub online_index: bool,
}

/// The signed description of a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Manifest {
    /// Format.
    pub format: String,
    /// Content-derived id.
    pub bundle_id: String,
    /// Version before the bundle.
    pub from_version: i64,
    /// Version after it.
    pub to_version: i64,
    /// The chain at `from_version`.
    pub prev_chain: String,
    /// The chain after the last entry.
    pub chain_head: String,
    /// Build time.
    pub created_at: String,
    /// Oldest agent that may apply it.
    pub min_agent_version: String,
    /// Entries in order.
    pub migrations: Vec<Entry>,
}

/// A verified entry with its SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// The entry.
    pub entry: Entry,
    /// The file's bytes.
    pub sql: Vec<u8>,
}

/// A verified bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    /// The manifest.
    pub manifest: Manifest,
    /// The signing key id.
    pub key_id: String,
    /// The migrations in order.
    pub migrations: Vec<Migration>,
}

/// The pinned public keys, by key id.
pub type Keyring = BTreeMap<String, [u8; 32]>;

/// Reads `{"keys":[{"key_id":"…","public_key":"<base64 of 32 raw bytes>"}]}`. An empty set,
/// a malformed entry or a repeated id is an error.
pub fn parse_keyring(v: &Value) -> Result<Keyring, String> {
    let e = |m: String| format!("bundle keyring: {m}");
    let keys = match v {
        Value::Object(o) => match json::field_ci(o, "keys") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a.clone(),
            Some(_) => return Err(e("keys is not an array".into())),
        },
        Value::Null => Vec::new(),
        _ => return Err(e("not an object".into())),
    };
    let mut ring = Keyring::new();
    for k in &keys {
        let empty = Map::new();
        let o = match k {
            Value::Object(o) => o,
            Value::Null => &empty,
            _ => return Err(e("bad entry".into())),
        };
        let s = |name: &str| match json::field_ci(o, name) {
            None | Some(Value::Null) => Ok(String::new()),
            Some(Value::String(s)) => Ok(s.clone()),
            Some(_) => Err(e(format!("{name} is not a string"))),
        };
        let id = s("key_id")?;
        let pk = s("public_key")?;
        let clean: String = pk.chars().filter(|c| *c != '\r' && *c != '\n').collect();
        let raw = dl_encoding::base64::STANDARD.decode(&clean);
        let key: Option<[u8; 32]> = raw.ok().and_then(|r| r.try_into().ok());
        let (true, Some(key)) = (pat::is_key_id(&id), key) else {
            return Err(e(format!("bad entry {}", json::quote(&id))));
        };
        if ring.insert(id.clone(), key).is_some() {
            return Err(e(format!("duplicate key id {}", json::quote(&id))));
        }
    }
    if ring.is_empty() {
        return Err(e("no keys".into()));
    }
    Ok(ring)
}

/// One link of the hash chain; it binds position, name and content.
pub fn chain_step(prev: &str, seq: i64, name: &str, content_sha256: &str) -> String {
    crate::sha256_hex(format!("mm-pms-chain/1\n{prev}\n{seq}\n{name}\n{content_sha256}").as_bytes())
}

/// The content-derived bundle id.
pub fn bundle_id_for(from: i64, to: i64, head: &str) -> String {
    format!("pms-{from}-{to}-{}", head.get(..16).unwrap_or(head))
}

/// The object `raw` must be with exactly `fields` as keys.
fn strict_object(raw: &Value, fields: &[&str], what: &str) -> Result<Map, BundleError> {
    let Value::Object(o) = raw else {
        return fail(CODE_FORMAT, format!("{what} is not a JSON object"));
    };
    let mut got: Vec<&str> = o.keys().map(String::as_str).collect();
    got.sort_unstable();
    got.dedup();
    if got != fields {
        return fail(
            CODE_FORMAT,
            format!("{what} must have exactly the fields {}", fields.join(", ")),
        );
    }
    Ok(o.clone())
}

fn type_err(what: &str, field: &str) -> BundleError {
    BundleError {
        code: CODE_FORMAT,
        msg: format!("{what}: json: cannot unmarshal {field}"),
    }
}

fn get_i(o: &Map, k: &str, what: &str) -> Result<i64, BundleError> {
    json::int(o, k, k).map_err(|_| type_err(what, k))
}

fn get_s(o: &Map, k: &str, what: &str) -> Result<String, BundleError> {
    json::string(o, k, k).map_err(|_| type_err(what, k))
}

fn get_b(o: &Map, k: &str, what: &str) -> Result<bool, BundleError> {
    json::boolean(o, k, k).map_err(|_| type_err(what, k))
}

const MANIFEST_FIELDS: [&str; 9] = [
    "bundle_id",
    "chain_head",
    "created_at",
    "format",
    "from_version",
    "migrations",
    "min_agent_version",
    "prev_chain",
    "to_version",
];
const ENTRY_FIELDS: [&str; 9] = [
    "bytes",
    "chain",
    "lock_timeout_ms",
    "name",
    "number",
    "online_index",
    "requires_backup",
    "seq",
    "sha256",
];
const SIG_FIELDS: [&str; 3] = ["alg", "key_id", "sig"];

/// Decodes manifest bytes and checks every structural rule. Does not check the
/// signature: call it only on bytes whose signature verified ([`open`] does both).
pub fn parse_manifest(raw: &[u8]) -> Result<Manifest, BundleError> {
    let Ok(v) = json::parse(raw) else {
        return fail(CODE_FORMAT, "manifest is not JSON");
    };
    let format = match &v {
        Value::Object(o) => match o.get("format") {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        },
        _ => return fail(CODE_FORMAT, "manifest is not JSON"),
    };
    if format != FORMAT {
        return fail(
            CODE_FORMAT,
            format!("unsupported bundle format {}", json::quote(&format)),
        );
    }
    let o = strict_object(&v, &MANIFEST_FIELDS, "manifest")?;
    let w = "manifest";
    let raw_entries = match o.get("migrations") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Null) => Vec::new(),
        _ => return Err(type_err(w, "migrations")),
    };
    let mut m = Manifest {
        format,
        bundle_id: get_s(&o, "bundle_id", w)?,
        from_version: get_i(&o, "from_version", w)?,
        to_version: get_i(&o, "to_version", w)?,
        prev_chain: get_s(&o, "prev_chain", w)?,
        chain_head: get_s(&o, "chain_head", w)?,
        created_at: get_s(&o, "created_at", w)?,
        min_agent_version: get_s(&o, "min_agent_version", w)?,
        migrations: Vec::with_capacity(raw_entries.len()),
    };
    for (i, r) in raw_entries.iter().enumerate() {
        let what = format!("migration {i}");
        let e = strict_object(r, &ENTRY_FIELDS, &what)?;
        m.migrations.push(Entry {
            seq: get_i(&e, "seq", &what)?,
            number: get_i(&e, "number", &what)?,
            name: get_s(&e, "name", &what)?,
            sha256: get_s(&e, "sha256", &what)?,
            chain: get_s(&e, "chain", &what)?,
            bytes: get_i(&e, "bytes", &what)?,
            requires_backup: get_b(&e, "requires_backup", &what)?,
            lock_timeout_ms: get_i(&e, "lock_timeout_ms", &what)?,
            online_index: get_b(&e, "online_index", &what)?,
        });
    }
    check_manifest(&m)?;
    Ok(m)
}

fn check_manifest(m: &Manifest) -> Result<(), BundleError> {
    if m.from_version < 0 || m.migrations.is_empty() {
        return fail(
            CODE_RANGE,
            "from_version must be >= 0 and migrations non-empty",
        );
    }
    if m.to_version != m.from_version + m.migrations.len() as i64 {
        return fail(CODE_RANGE, "to_version != from_version + migrations");
    }
    if !pat::is_hex64(&m.prev_chain) || (m.from_version == 0 && m.prev_chain != GENESIS_CHAIN) {
        return fail(
            CODE_CHAIN,
            "prev_chain must be 64 hex (genesis when from_version is 0)",
        );
    }
    if !pat::is_utc_second(&m.created_at) || !pat::is_short_semver(&m.min_agent_version) {
        return fail(CODE_FORMAT, "bad created_at or min_agent_version");
    }
    let (mut prev, mut prev_name, mut prev_number) = (m.prev_chain.clone(), String::new(), -1i64);
    for (i, e) in m.migrations.iter().enumerate() {
        let want = m.from_version + i as i64 + 1;
        if e.seq != want {
            return fail(
                CODE_SEQUENCE,
                format!(
                    "entry {i} has seq {}, expected {want} (gap, duplicate or reorder)",
                    e.seq
                ),
            );
        }
        let Some(num) = pat::migration_file(&e.name) else {
            return fail(
                CODE_NAME,
                format!("entry {}: bad name {}", e.seq, json::quote(&e.name)),
            );
        };
        if num.parse::<i64>().ok() != Some(e.number) {
            return fail(CODE_NAME, format!("entry {}: name/number mismatch", e.seq));
        }
        if e.name <= prev_name || e.number < prev_number {
            return fail(
                CODE_SEQUENCE,
                format!(
                    "entry {}: {} is not after {prev_name} (duplicate or reorder)",
                    e.seq, e.name
                ),
            );
        }
        if !pat::is_hex64(&e.sha256) || e.bytes < 0 || e.bytes > MAX_FILE_BYTES as i64 {
            return fail(CODE_FORMAT, format!("entry {}: bad sha256/bytes", e.seq));
        }
        if e.lock_timeout_ms < 1 || e.lock_timeout_ms > MAX_LOCK_TIMEOUT_MS {
            return fail(CODE_FORMAT, format!("entry {}: bad flags", e.seq));
        }
        let link = chain_step(&prev, e.seq, &e.name, &e.sha256);
        if e.chain != link {
            return fail(CODE_CHAIN, format!("entry {}: chain mismatch", e.seq));
        }
        prev = link;
        prev_name = e.name.clone();
        prev_number = e.number;
    }
    if m.chain_head != prev {
        return fail(CODE_CHAIN, "chain_head does not match the last entry");
    }
    if m.bundle_id != bundle_id_for(m.from_version, m.to_version, &m.chain_head) {
        return fail(CODE_FORMAT, "bundle_id does not match its content");
    }
    Ok(())
}

fn read_capped(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let md = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !md.is_file() || md.len() > limit {
        return Err(format!(
            "{}: not a regular file or larger than {limit} bytes",
            path.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        ));
    }
    std::fs::read(path).map_err(|e| e.to_string())
}

fn verify_manifest(
    manifest_raw: &[u8],
    sig_raw: &[u8],
    keys: &Keyring,
) -> Result<(Manifest, String), BundleError> {
    let unsigned = |e: BundleError| BundleError {
        code: CODE_UNSIGNED,
        msg: e.to_string(),
    };
    let sig_v = json::parse(sig_raw).map_err(|_| BundleError {
        code: CODE_UNSIGNED,
        msg: "format: manifest.sig is not a JSON object".into(),
    })?;
    let so = strict_object(&sig_v, &SIG_FIELDS, "manifest.sig").map_err(unsigned)?;
    let w = "manifest.sig";
    let alg = get_s(&so, "alg", w).map_err(unsigned)?;
    let key_id = get_s(&so, "key_id", w).map_err(unsigned)?;
    let sig = get_s(&so, "sig", w).map_err(unsigned)?;
    if alg != "ed25519" {
        return fail(
            CODE_UNSIGNED,
            format!("unsupported signature alg {}", json::quote(&alg)),
        );
    }
    let Some(pubkey) = keys.get(&key_id) else {
        return fail(
            CODE_KEY_ID,
            format!("signing key {} is not pinned", json::quote(&key_id)),
        );
    };
    // Go's base64 decoder skips CR and LF anywhere in the text.
    let clean: String = sig.chars().filter(|c| *c != '\r' && *c != '\n').collect();
    let ok = dl_encoding::base64::STANDARD
        .decode(&clean)
        .ok()
        .filter(|s| s.len() == 64)
        .is_some_and(|s| dl_ed25519::verify(pubkey, manifest_raw, &s));
    if !ok {
        return fail(CODE_SIGNATURE, "signature does not verify");
    }
    Ok((parse_manifest(manifest_raw)?, key_id))
}

fn assemble(
    m: Manifest,
    key_id: String,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<Bundle, BundleError> {
    let mut out = Vec::with_capacity(m.migrations.len());
    for e in &m.migrations {
        let Some(data) = files.get(&e.name) else {
            return fail(CODE_FILES, format!("{} missing", e.name));
        };
        if data.len() as i64 != e.bytes || crate::sha256_hex(data) != e.sha256 {
            return fail(CODE_HASH, format!("{} does not match its sha256", e.name));
        }
        out.push(Migration {
            entry: e.clone(),
            sql: data.clone(),
        });
    }
    Ok(Bundle {
        manifest: m,
        key_id,
        migrations: out,
    })
}

/// Verifies the bundle in `dir` against the pinned key set.
pub fn open(dir: &Path, keys: &Keyring) -> Result<Bundle, BundleError> {
    let manifest_raw =
        read_capped(&dir.join("manifest.json"), MAX_MANIFEST_BYTES).map_err(|e| BundleError {
            code: CODE_FORMAT,
            msg: format!("manifest.json: {e}"),
        })?;
    let sig_raw =
        read_capped(&dir.join("manifest.sig"), MAX_SIG_BYTES).map_err(|e| BundleError {
            code: CODE_UNSIGNED,
            msg: format!("manifest.sig: {e}"),
        })?;
    let (m, key_id) = verify_manifest(&manifest_raw, &sig_raw, keys)?;
    let sql_dir = dir.join("sql");
    let listed = std::fs::read_dir(&sql_dir).map_err(|e| BundleError {
        code: CODE_FILES,
        msg: format!("sql/: {e}"),
    })?;
    let expected: HashSet<&str> = m.migrations.iter().map(|e| e.name.as_str()).collect();
    for f in listed {
        let f = f.map_err(|e| BundleError {
            code: CODE_FILES,
            msg: format!("sql/: {e}"),
        })?;
        let name = f.file_name().to_string_lossy().into_owned();
        let is_dir = f.file_type().map(|t| t.is_dir()).unwrap_or(true);
        if !expected.contains(name.as_str()) || is_dir {
            return fail(CODE_FILES, format!("unexpected entry in sql/: {name}"));
        }
    }
    let mut files = BTreeMap::new();
    for e in &m.migrations {
        // Names passed migration_file, so they hold no path separator.
        let data =
            read_capped(&sql_dir.join(&e.name), MAX_FILE_BYTES).map_err(|x| BundleError {
                code: CODE_FILES,
                msg: format!("{} missing: {x}", e.name),
            })?;
        files.insert(e.name.clone(), data);
    }
    assemble(m, key_id, &files)
}

/// Verifies a bundle held in memory (as the cloud serves it) exactly like [`open`].
pub fn verify(
    manifest_raw: &[u8],
    sig_raw: &[u8],
    files: &BTreeMap<String, Vec<u8>>,
    keys: &Keyring,
) -> Result<Bundle, BundleError> {
    if manifest_raw.len() as u64 > MAX_MANIFEST_BYTES {
        return fail(
            CODE_FORMAT,
            format!("manifest.json larger than {MAX_MANIFEST_BYTES} bytes"),
        );
    }
    if sig_raw.len() as u64 > MAX_SIG_BYTES {
        return fail(
            CODE_UNSIGNED,
            format!("manifest.sig larger than {MAX_SIG_BYTES} bytes"),
        );
    }
    let (m, key_id) = verify_manifest(manifest_raw, sig_raw, keys)?;
    let expected: HashSet<&str> = m.migrations.iter().map(|e| e.name.as_str()).collect();
    if let Some(extra) = files.keys().find(|k| !expected.contains(k.as_str())) {
        return fail(CODE_FILES, format!("unexpected file {extra}"));
    }
    assemble(m, key_id, files)
}
