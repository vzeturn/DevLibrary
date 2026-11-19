//! The sealed backup file format.

use std::collections::HashMap;

use super::*;
use crate::testutil::temp_dir;

/// Keys by id, one map per tenant: what the hub would give that tenant's agent.
struct Tenant(HashMap<String, DataKey>);

impl Tenant {
    fn new(keys: &[&DataKey]) -> Tenant {
        Tenant(keys.iter().map(|k| (k.id.clone(), (*k).clone())).collect())
    }
}

impl BackupKeys for Tenant {
    fn current(&self) -> Result<Option<DataKey>, Error> {
        Ok(self.0.values().next().cloned())
    }
    fn by_id(&self, id: &str) -> Result<DataKey, Error> {
        self.0.get(id).cloned().ok_or_else(|| {
            Error::Credential(format!("the hub does not give this agent backup key {id}"))
        })
    }
}

fn key(id: &str, byte: u8) -> DataKey {
    DataKey {
        id: id.into(),
        key: [byte; 32],
    }
}

fn data(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
}

/// Seals `plain` with a small chunk size so the multi-chunk paths are exercised.
fn sealed(dir: &Path, name: &str, plain: &[u8], k: &DataKey, chunk: usize) -> PathBuf {
    let src = dir.join(format!("{name}.bak"));
    std::fs::write(&src, plain).unwrap();
    let dst = dir.join(format!("{name}.bak.dlbk"));
    seal_with(&src, &dst, k, &[9u8; SALT_LEN], chunk, &CancelToken::new()).unwrap();
    dst
}

#[test]
fn round_trips_every_length_around_the_chunk_boundary() {
    let dir = temp_dir("env-round");
    let k = key("tenant-a:1", 1);
    let keys = Tenant::new(&[&k]);
    for len in [0, 1, 63, 64, 65, 128, 129, 1000] {
        let plain = data(len);
        let enc = sealed(&dir, &format!("r{len}"), &plain, &k, 64);
        let raw = std::fs::read(&enc).unwrap();
        assert_eq!(&raw[..8], MAGIC);
        assert!(
            !raw.windows(16)
                .any(|w| plain.len() >= 16 && w == &plain[..16]),
            "no plaintext in the sealed file ({len})"
        );
        assert_eq!(key_id_of(&enc).unwrap().as_deref(), Some("tenant-a:1"));
        let out = dir.join(format!("r{len}.out"));
        let id = open_file(&enc, &out, &keys, &CancelToken::new()).unwrap();
        assert_eq!(id, "tenant-a:1");
        assert_eq!(std::fs::read(&out).unwrap(), plain, "{len}");
    }
}

#[test]
fn seal_file_uses_a_fresh_salt_and_the_default_chunk() {
    let dir = temp_dir("env-salt");
    let src = dir.join("a.bak");
    std::fs::write(&src, data(CHUNK + 10)).unwrap();
    let k = key("t:1", 2);
    let (a, b) = (dir.join("a1.dlbk"), dir.join("a2.dlbk"));
    let n = seal_file(&src, &a, &k, &CancelToken::new()).unwrap();
    seal_file(&src, &b, &k, &CancelToken::new()).unwrap();
    assert_eq!(n, std::fs::metadata(&a).unwrap().len());
    assert_ne!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    let out = dir.join("a.out");
    open_file(&a, &out, &Tenant::new(&[&k]), &CancelToken::new()).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), data(CHUNK + 10));
}

#[test]
fn another_tenants_agent_cannot_open_the_file() {
    let dir = temp_dir("env-tenant");
    let (a, b) = (key("tenant-a:1", 1), key("tenant-b:1", 2));
    let enc = sealed(&dir, "b", &data(300), &b, 64);
    let out = dir.join("b.out");

    // Tenant A's agent: the hub refuses B's key id.
    let err = open_file(&enc, &out, &Tenant::new(&[&a]), &CancelToken::new()).unwrap_err();
    assert!(matches!(err, Error::Credential(_)), "{err}");
    assert!(err.to_string().contains("tenant-b:1"), "{err}");
    assert!(!out.exists() && !part_path(&out).exists());

    // Even if A's key were handed out under B's id, nothing opens and nothing is written.
    let forged = DataKey {
        id: "tenant-b:1".into(),
        key: a.key,
    };
    let err = open_file(&enc, &out, &Tenant::new(&[&forged]), &CancelToken::new()).unwrap_err();
    assert!(err.to_string().contains("authentication failed"), "{err}");
    assert!(!out.exists() && !part_path(&out).exists());
}

#[test]
fn altered_cut_or_extended_files_are_refused() {
    let dir = temp_dir("env-alter");
    let k = key("t:1", 3);
    let keys = Tenant::new(&[&k]);
    let enc = sealed(&dir, "x", &data(200), &k, 64);
    let good = std::fs::read(&enc).unwrap();
    let header = 8 + 1 + 3 + SALT_LEN + 4;
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("flipped byte", {
            let mut v = good.clone();
            v[header + 5] ^= 1;
            v
        }),
        ("altered salt", {
            let mut v = good.clone();
            v[8 + 1 + 3] ^= 1;
            v
        }),
        // 200 bytes in chunks of 64: three full chunks (80 sealed bytes each) and a final one.
        ("cut at a chunk boundary", good[..header + 3 * 80].to_vec()),
        ("cut inside the last chunk", good[..good.len() - 1].to_vec()),
        ("trailing data", {
            let mut v = good.clone();
            v.extend_from_slice(&[0u8; 20]);
            v
        }),
        ("chunks swapped", {
            let mut v = good.clone();
            let (c0, c1) = (header, header + 80);
            let first = v[c0..c0 + 80].to_vec();
            let second = v[c1..c1 + 80].to_vec();
            v[c0..c0 + 80].copy_from_slice(&second);
            v[c1..c1 + 80].copy_from_slice(&first);
            v
        }),
    ];
    for (what, bytes) in cases {
        let f = dir.join("bad.dlbk");
        std::fs::write(&f, &bytes).unwrap();
        let out = dir.join("bad.out");
        let err = open_file(&f, &out, &keys, &CancelToken::new()).unwrap_err();
        assert!(matches!(err, Error::Credential(_)), "{what}: {err}");
        assert!(!out.exists() && !part_path(&out).exists(), "{what}");
    }
}

#[test]
fn plaintext_backups_are_recognised_as_legacy() {
    let dir = temp_dir("env-legacy");
    let f = dir.join("old.bak");
    std::fs::write(&f, b"TAPE\0\0\0\0 a plaintext SQL Server backup").unwrap();
    assert_eq!(key_id_of(&f).unwrap(), None);
    let short = dir.join("short.bak");
    std::fs::write(&short, b"DLB").unwrap();
    assert_eq!(key_id_of(&short).unwrap(), None);
    let err = open_file(&f, &dir.join("o"), &NoKeys, &CancelToken::new()).unwrap_err();
    assert!(err.to_string().contains("not an encrypted backup"), "{err}");
}

#[test]
fn keys_from_the_hub_are_checked() {
    let b64 = dl_encoding::base64::URL_SAFE_NO_PAD.encode([5u8; 32]);
    let k = DataKey::from_hub("t-1:v2", &b64).unwrap();
    assert_eq!(k.key, [5u8; 32]);
    assert!(!format!("{k:?}").contains('5'), "Debug hides the key");
    assert!(DataKey::from_hub("t-1", "AAAA").is_err(), "too short");
    assert!(DataKey::from_hub("bad id", &b64).is_err());
    assert!(DataKey::from_hub(&"x".repeat(65), &b64).is_err());
    assert!(NoKeys.current().unwrap().is_none());
    assert!(matches!(NoKeys.by_id("k"), Err(Error::Credential(_))));
}

#[test]
fn cancel_stops_sealing_and_leaves_nothing() {
    let dir = temp_dir("env-cancel");
    let src = dir.join("c.bak");
    std::fs::write(&src, data(10)).unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    let dst = dir.join("c.dlbk");
    let err = seal_file(&src, &dst, &key("t:1", 1), &cancel).unwrap_err();
    assert_eq!(err, Error::Cancelled);
    assert!(!dst.exists());
}
