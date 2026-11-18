//! Release signing for dl-agent builds (run on the offline signing machine).
//!
//! ```text
//! cargo run -p dl-agent --example release -- keygen <seed-file> <key-id>
//! cargo run -p dl-agent --example release -- sign <seed-file> <key-id> <product> <channel> \
//!     <version> <target> <artifact-url> <binary> [rollout_percent] [notes]
//! cargo run -p dl-agent --example release -- sign-sql <seed-file> <key-id> <bundle.json>
//! ```
//!
//! `keygen` writes a new Ed25519 seed (base64) to `<seed-file>` and prints the
//! `key-id:public-key` pair to bake into agents with `DL_AGENT_RELEASE_KEYS`.
//! `sign` prints a signed manifest (PROTOCOL §5) for one binary; publish it on the hub
//! with `PUT /admin/releases/<product>/<version>` (canary) or `POST …/promote` (stable).
//! `sign-sql` prints a signed SQL bundle (PROTOCOL §9.7); an object may name a `file`
//! (relative to the bundle) instead of carrying `sql` inline.

use std::process::ExitCode;

use dl_agent::manifest::{Artifact, Manifest};
use dl_agent::sql::bundle::Bundle;
use dl_ed25519::SigningKey;
use dl_encoding::base64::STANDARD as B64;
use dl_hash::{Digest, Sha256};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("keygen") if args.len() == 3 => keygen(&args[1], &args[2]),
        Some("sign") if (9..=11).contains(&args.len()) => sign(&args[1..]),
        Some("sign-sql") if args.len() == 4 => sign_sql(&args[1], &args[2], &args[3]),
        _ => Err("usage: release keygen <seed-file> <key-id> | release sign <seed-file> <key-id> <product> <channel> <version> <target> <url> <binary> [rollout] [notes] | release sign-sql <seed-file> <key-id> <bundle.json>".into()),
    };
    match result {
        Ok(out) => {
            println!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn keygen(path: &str, key_id: &str) -> Result<String, String> {
    if std::path::Path::new(path).exists() {
        return Err(format!(
            "{path} exists; refusing to overwrite a release key"
        ));
    }
    let seed: [u8; 32] = dl_rand::secure_bytes().map_err(|e| e.to_string())?;
    write_private(path, B64.encode(seed).as_bytes()).map_err(|e| format!("{path}: {e}"))?;
    let public = SigningKey::from_seed(&seed).verifying_key();
    Ok(format!("{key_id}:{}", B64.encode(public.as_bytes())))
}

#[cfg(unix)]
fn write_private(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

fn read_seed(path: &str) -> Result<[u8; 32], String> {
    let seed_text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    B64.decode(seed_text.trim())
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "the seed must be 32 bytes".to_string())
}

fn sign_sql(seed_path: &str, key_id: &str, bundle_path: &str) -> Result<String, String> {
    let seed = read_seed(seed_path)?;
    let text = std::fs::read_to_string(bundle_path).map_err(|e| format!("{bundle_path}: {e}"))?;
    let mut v = dl_json::parse(&text).map_err(|e| format!("{bundle_path}: {e}"))?;
    let dir = std::path::Path::new(bundle_path)
        .parent()
        .unwrap_or(std::path::Path::new("."));
    for o in v
        .get_mut("objects")
        .and_then(dl_json::Value::as_array_mut)
        .ok_or("the bundle has no objects")?
    {
        let file = o
            .get("file")
            .and_then(dl_json::Value::as_str)
            .map(str::to_string);
        if let (Some(f), Some(obj)) = (file, o.as_object_mut()) {
            let sql = std::fs::read_to_string(dir.join(&f)).map_err(|e| format!("{f}: {e}"))?;
            obj.insert("sql", sql.trim_start_matches('\u{feff}'));
            obj.remove("file");
        }
    }
    let mut b = Bundle::from_json(&v).map_err(|e| e.to_string())?;
    b.published_at = dl_agent::now();
    b.sign(key_id, &SigningKey::from_seed(&seed));
    Ok(dl_json::to_string(&b.to_json()))
}

fn sign(a: &[String]) -> Result<String, String> {
    let seed = read_seed(&a[0])?;
    let binary = std::fs::read(&a[7]).map_err(|e| format!("{}: {e}", a[7]))?;
    let mut m = Manifest {
        product: a[2].clone(),
        channel: a[3].parse().map_err(|e: dl_agent::Error| e.to_string())?,
        version: a[4].parse().map_err(|e: dl_agent::Error| e.to_string())?,
        published_at: dl_agent::now(),
        rollout_percent: match a.get(8) {
            Some(p) => p.parse().map_err(|_| format!("bad rollout percent {p}"))?,
            None => 100,
        },
        min_version: None,
        allow_downgrade: false,
        notes: a.get(9).cloned().unwrap_or_default(),
        artifacts: vec![Artifact {
            target: a[5].clone(),
            url: a[6].clone(),
            sha256: Sha256::digest(&binary),
            size: binary.len() as u64,
        }],
        key_id: String::new(),
        signature: [0; 64],
    };
    m.sign(&a[1], &SigningKey::from_seed(&seed));
    Ok(m.to_json())
}
