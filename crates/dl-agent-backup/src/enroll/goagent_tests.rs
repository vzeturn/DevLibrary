//! The Go agent's `agent.json`: values sealed by the real Go `secret` package
//! (`tests/fixtures/go-agent/make-go-agent.go.txt`), plaintext keys, discovery order and
//! origin matching.

use std::path::{Path, PathBuf};

use super::goagent::*;
use crate::testutil::temp_dir;

/// The machine id the fixtures were sealed under.
pub(super) const FIXTURE_MACHINE: &str = "3f6c1a2e-9b4d-4e8f-a7c5-12d0e9b64f3a";
/// What the fixtures hold (test-only values).
pub(super) const GO_AGENT_ID: &str = "0b9f5a52-6a4e-4c47-9a0e-3f1d2c7b8e61";
pub(super) const GO_API_KEY: &str = "go-agent-token-7f3a91c2";
const GO_MASTER_KEY: &str = "go-master-key-5d2e84b0";
const PLAIN_AGENT_ID: &str = "5c1e7d40-2b8a-4f39-b6d2-9e0a4c3f7b18";
const PLAIN_API_KEY: &str = "go-agent-plain-token-1e6b";

pub(super) const SEALED: &str = include_str!("../../tests/fixtures/go-agent/sealed.json");
const PLAINTEXT: &str = include_str!("../../tests/fixtures/go-agent/plaintext.json");

pub(super) fn machine(id: &str) -> MachineId {
    MachineId {
        id: id.into(),
        fallback: None,
    }
}

/// Writes `fixture` with `server_url` replaced into a fresh dir; returns the file.
pub(super) fn write_config(tag: &str, fixture: &str, server_url: &str) -> PathBuf {
    let mut doc = dl_json::parse(fixture).unwrap();
    doc.as_object_mut()
        .unwrap()
        .insert("server_url", server_url);
    let path = temp_dir(tag).join("agent.json");
    std::fs::write(&path, dl_json::to_string(&doc)).unwrap();
    path
}

pub(super) fn source(paths: &[&Path], id: &str) -> GoSource {
    GoSource {
        paths: paths.iter().map(|p| p.to_path_buf()).collect(),
        machine: machine(id),
    }
}

fn field(fixture: &str, k: &str) -> String {
    dl_json::parse(fixture)
        .unwrap()
        .get(k)
        .and_then(dl_json::Value::as_str)
        .unwrap()
        .to_owned()
}

#[test]
fn opens_the_values_go_sealed_byte_for_byte() {
    let api = field(SEALED, "api_key");
    let master = field(SEALED, "master_key");
    assert!(api.starts_with("enc:v1:") && master.starts_with("enc:v1:"));
    assert_eq!(open_value(&api, FIXTURE_MACHINE).unwrap(), GO_API_KEY);
    assert_eq!(open_value(&master, FIXTURE_MACHINE).unwrap(), GO_MASTER_KEY);
}

#[test]
fn another_machine_id_fails_cleanly_without_the_secret() {
    let api = field(SEALED, "api_key");
    let err = open_value(&api, "some-other-machine").unwrap_err();
    assert!(err.contains("different machine"), "{err}");
    for secret in [GO_API_KEY, FIXTURE_MACHINE, api.as_str()] {
        assert!(!err.contains(secret), "{err}");
    }
}

#[test]
fn plaintext_and_odd_values_follow_go() {
    assert_eq!(open_value(PLAIN_API_KEY, "any").unwrap(), PLAIN_API_KEY);
    // Go's IsEncrypted needs at least one byte after the prefix.
    assert_eq!(open_value("enc:v1:", "any").unwrap(), "enc:v1:");
    assert!(
        open_value("enc:v1:@@@", "any")
            .unwrap_err()
            .contains("decode")
    );
    assert!(
        open_value("enc:v1:AAAA", "any")
            .unwrap_err()
            .contains("nonce")
    );
}

#[test]
fn the_hostname_fallback_matches_go() {
    // Printed by Go: fmt.Sprintf("host-%x", sha256.Sum256([]byte("SQL-HOST"))[:8]).
    assert_eq!(hostname_id("SQL-HOST"), "host-90b07b80f2ff6f69");
}

#[test]
fn finds_the_sealed_config_and_debug_hides_the_key() {
    let path = write_config("go-find", SEALED, "https://backupapi.example.test");
    let a = find(&source(&[&path], FIXTURE_MACHINE)).unwrap().unwrap();
    assert_eq!(
        (a.agent_id.as_str(), a.api_key.as_str()),
        (GO_AGENT_ID, GO_API_KEY)
    );
    assert!(!format!("{a:?}").contains(GO_API_KEY));
    assert!(!format!("{:?}", machine(FIXTURE_MACHINE)).contains(FIXTURE_MACHINE));
}

#[test]
fn finds_a_plaintext_config() {
    let path = write_config("go-plain", PLAINTEXT, "https://backupapi.example.test");
    let a = find(&source(&[&path], "whatever")).unwrap().unwrap();
    assert_eq!(
        (a.agent_id.as_str(), a.api_key.as_str()),
        (PLAIN_AGENT_ID, PLAIN_API_KEY)
    );
}

#[test]
fn discovery_takes_the_first_file_with_a_server_url_like_go() {
    let dir = temp_dir("go-order");
    let missing = dir.join("missing.json");
    let broken = dir.join("broken.json");
    std::fs::write(&broken, "{ not json").unwrap();
    let no_url = dir.join("no-url.json");
    std::fs::write(&no_url, r#"{"agent_id":"x","api_key":"y"}"#).unwrap();
    let good = write_config("go-order-good", PLAINTEXT, "https://hub.example.test");
    let a = find(&source(&[&missing, &broken, &no_url, &good], "m"))
        .unwrap()
        .unwrap();
    assert_eq!(a.path, good);

    assert!(find(&source(&[&missing], "m")).unwrap().is_none());
    let err = find(&source(&[&broken], "m")).unwrap_err();
    assert!(err.contains("broken.json"), "{err}");
}

#[test]
fn a_wrong_machine_id_is_an_error_naming_the_fallback() {
    let path = write_config("go-wrong", SEALED, "https://backupapi.example.test");
    let mut src = source(&[&path], "not-this-machine");
    src.machine.fallback = Some("MachineGuid is missing or empty".into());
    let err = find(&src).unwrap_err();
    assert!(
        err.contains("api_key") && err.contains("hostname fallback"),
        "{err}"
    );
    assert!(!err.contains(GO_API_KEY) && !err.contains("not-this-machine"));
}

#[test]
fn origins_compare_scheme_host_and_port() {
    let o = "https://backupapi.sr.io.vn";
    assert!(same_origin("https://BackupAPI.sr.io.vn/", o));
    assert!(same_origin("https://backupapi.sr.io.vn:443", o));
    assert!(!same_origin("http://backupapi.sr.io.vn", o));
    assert!(!same_origin("https://backupapi.sr.io.vn:8443", o));
    assert!(!same_origin("https://other.sr.io.vn", o));
    assert!(!same_origin("https://backupapi.sr.io.vn/hub", o));
    assert!(!same_origin("not a url", o));
}

#[test]
fn windows_candidates_follow_go_order() {
    let env = |k: &str| match k {
        "PROGRAMDATA" => Some("PD".to_owned()),
        "APPDATA" => Some("AP".to_owned()),
        "LOCALAPPDATA" => Some("LA".to_owned()),
        "HOME" => Some("HM".to_owned()),
        _ => None,
    };
    let got = candidates(&env);
    let join = |root: &str| Path::new(root).join("srbackup").join("agent.json");
    if cfg!(windows) {
        assert_eq!(got, [join("PD"), join("AP"), join("LA")]);
        let only = candidates(&|_| None);
        assert_eq!(only, [PathBuf::from(r"C:\ProgramData\srbackup\agent.json")]);
    } else {
        let home = Path::new("HM").join(".srbackup").join("agent.json");
        assert_eq!(got, [home, PathBuf::from("/etc/srbackup/agent.json")]);
    }
}
