//! The golden bundle of `internal/bundle/testdata` (built by
//! packages/db/scripts/build-migration-bundle.mjs and verified by the Go and TS agents).

use std::collections::BTreeMap;
use std::path::PathBuf;

use dl_agent_pms::bundle::{self, Keyring};
use dl_agent_pms::migrate;

fn testdata() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/bundle")
}

fn dev_keyring() -> Keyring {
    let raw = std::fs::read(testdata().join("dev-keyring.json")).unwrap();
    bundle::parse_keyring(&dl_agent_pms::json::parse(&raw).unwrap()).unwrap()
}

fn golden_in_memory() -> (Vec<u8>, Vec<u8>, BTreeMap<String, Vec<u8>>) {
    let dir = testdata().join("golden");
    let manifest = std::fs::read(dir.join("manifest.json")).unwrap();
    let sig = std::fs::read(dir.join("manifest.sig")).unwrap();
    let mut files = BTreeMap::new();
    for e in std::fs::read_dir(dir.join("sql")).unwrap() {
        let e = e.unwrap();
        files.insert(
            e.file_name().to_string_lossy().into_owned(),
            std::fs::read(e.path()).unwrap(),
        );
    }
    (manifest, sig, files)
}

#[test]
fn open_and_verify_agree_on_the_golden_bundle() {
    let keys = dev_keyring();
    let opened = bundle::open(&testdata().join("golden"), &keys).unwrap();
    let (m, s, f) = golden_in_memory();
    let verified = bundle::verify(&m, &s, &f, &keys).unwrap();
    assert_eq!(opened, verified);
    assert_eq!(opened.key_id, "dev-test-1");
    assert_eq!(
        opened.manifest.bundle_id,
        bundle::bundle_id_for(
            opened.manifest.from_version,
            opened.manifest.to_version,
            &opened.manifest.chain_head
        )
    );
    assert_eq!(opened.migrations.len(), opened.manifest.migrations.len());
    // The golden fixture carries nothing forbidden.
    assert!(migrate::scan(&opened).is_empty());
}

#[test]
fn in_memory_refusals() {
    type Edit = fn(&mut Keyring, &mut Vec<u8>, &mut Vec<u8>, &mut BTreeMap<String, Vec<u8>>);
    let cases: [(&str, Edit, &str); 6] = [
        ("unpinned key", |k, _, _, _| k.clear(), bundle::CODE_KEY_ID),
        (
            "tampered manifest",
            |_, m, _, _| {
                m.pop();
                m.extend_from_slice(b" }");
            },
            bundle::CODE_SIGNATURE,
        ),
        (
            "missing file",
            |_, _, _, f| {
                let k = f.keys().next().unwrap().clone();
                f.remove(&k);
            },
            bundle::CODE_FILES,
        ),
        (
            "extra file",
            |_, _, _, f| {
                f.insert("999_extra.sql".into(), b"x".to_vec());
            },
            bundle::CODE_FILES,
        ),
        (
            "changed file",
            |_, _, _, f| {
                f.values_mut().next().unwrap().push(b'\n');
            },
            bundle::CODE_HASH,
        ),
        (
            "oversized signature",
            |_, _, s, _| *s = vec![0; 4097],
            bundle::CODE_UNSIGNED,
        ),
    ];
    for (name, edit, code) in cases {
        let mut keys = dev_keyring();
        let (mut m, mut s, mut f) = golden_in_memory();
        edit(&mut keys, &mut m, &mut s, &mut f);
        let err = bundle::verify(&m, &s, &f, &keys).unwrap_err();
        assert_eq!(err.code, code, "{name}: {err}");
    }
}

#[test]
fn keyring_rules() {
    let p = |s: &str| bundle::parse_keyring(&dl_agent_pms::json::parse(s.as_bytes()).unwrap());
    assert!(p(r#"{"keys":[]}"#).is_err());
    assert!(p(r#"{"keys":[{"key_id":"Bad","public_key":"6y3usY1FgaeDokVPUzLsN0ZArylH7D89fxuduKLMQfY="}]}"#).is_err());
    assert!(p(r#"{"keys":[{"key_id":"a","public_key":"AAAA"}]}"#).is_err());
    let dup = r#"{"keys":[{"key_id":"a","public_key":"6y3usY1FgaeDokVPUzLsN0ZArylH7D89fxuduKLMQfY="},{"key_id":"a","public_key":"6y3usY1FgaeDokVPUzLsN0ZArylH7D89fxuduKLMQfY="}]}"#;
    assert!(p(dup).unwrap_err().contains("duplicate"));
}
