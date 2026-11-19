//! The agent-side .NET fixtures: the identity a site already has on disk, the link status
//! and heartbeat files, the journal time stamp and the procedure bundle.

mod common;

use common::{TempDir, field, i, s};
use dl_agent_arrival::agent::installer::{self, LEGACY_PICTURE_HASH, PICTURE_PHASE};
use dl_agent_arrival::agent::journal::entry_line;
use dl_agent_arrival::agent::link_status::{self, LinkStatus, heartbeat_line};
use dl_agent_arrival::agent::secrets::Identity;
use dl_agent_arrival::contracts::{PmsResponse, Status};
use dl_time::OffsetDateTime;

fn at(text: &str) -> OffsetDateTime {
    OffsetDateTime::parse_rfc3339(text).expect("time")
}

#[test]
fn identity_written_by_the_dotnet_agent_loads_in_place() {
    let g = common::golden();
    let id = field(&g, "identity");
    let json = s(id, "json");

    // The token the console shows and the stored JSON name the same identity.
    let from_token = Identity::from_token(s(id, "token")).expect("token");
    let from_json = Identity::from_json_bytes(json.as_bytes()).expect("json");
    assert_eq!(from_token.to_json(), json);
    assert_eq!(from_json.to_json(), json);
    assert_eq!(from_json.tenant_n(), s(id, "tenant_n"));
    assert!(from_json.cipher().is_ok());

    // Off Windows `AgentIdentityStore.Save` writes the JSON itself (DPAPI is Windows only).
    let dir = TempDir::new("golden-identity");
    let path = dir.path().join("agent-identity.bin");
    std::fs::write(&path, json).expect("write fixture");
    let loaded = Identity::load(&path).expect("load").expect("present");
    assert_eq!(loaded.agent_id, "agt-7Hq");
    assert_eq!(loaded.to_json(), json);
    assert!(
        Identity::load(&dir.path().join("missing.bin"))
            .expect("missing")
            .is_none()
    );

    // What this agent saves, the .NET agent reads back byte for byte.
    let again = dir.path().join("again.bin");
    loaded.save(&again).expect("save");
    if cfg!(not(windows)) {
        assert_eq!(std::fs::read_to_string(&again).expect("read"), json);
    }
}

#[test]
fn link_status_and_heartbeat_match() {
    let g = common::golden();
    let ls = field(&g, "link_status");
    let connected = LinkStatus {
        state: link_status::CONNECTED,
        agent_id: "agt-7Hq".into(),
        kid: "k-2026-11".into(),
        at_utc: at("2026-11-19T01:02:03.4567891+00:00"),
        problem: None,
    };
    assert_eq!(connected.to_json(), s(ls, "connected"));
    let failed = LinkStatus {
        state: link_status::FAILED,
        at_utc: at("2026-11-19T01:02:03+00:00"),
        problem: Some("Cannot reach the cloud: <x> & \"y\"".into()),
        ..connected.clone()
    };
    assert_eq!(failed.to_json(), s(ls, "failed"));
    assert_eq!(
        heartbeat_line(&at("2026-11-19T01:02:03.4567891+00:00"), "1.2.3"),
        s(ls, "heartbeat")
    );
    let line = entry_line(
        "k",
        &at("2026-11-19T01:02:03.4567+00:00"),
        &PmsResponse::bare("r", Status::Ok, None),
    );
    assert!(
        line.contains(&format!("\"at\":{}", s(ls, "journal_at"))),
        "{line}"
    );
}

#[test]
fn procedure_bundle_matches_the_dotnet_one() {
    let g = common::golden();
    let b = field(&g, "bundle");
    let bundle = installer::bundle();
    assert_eq!(bundle.text.len() as i64, i(b, "bytes"));
    assert_eq!(bundle.hash, s(b, "bundle_hash"));
    assert_eq!(bundle.picture_hash, s(b, "picture_hash"));
    assert_eq!(bundle.picture_hash, LEGACY_PICTURE_HASH);
    assert_eq!(bundle.batches.len() as i64, i(b, "batches"));
    let phases = field(b, "phases");
    let picture = bundle
        .batches
        .iter()
        .filter(|x| x.phase == PICTURE_PHASE)
        .count();
    assert_eq!(picture as i64, i(phases, "picture"));
    assert_eq!((bundle.batches.len() - picture) as i64, i(phases, "pms"));
    let expected: Vec<&str> = field(b, "batch_hashes")
        .as_array()
        .expect("batch_hashes")
        .iter()
        .map(|v| v.as_str().expect("hash"))
        .collect();
    let actual: Vec<String> = bundle
        .batches
        .iter()
        .map(|x| format!("{}:{}", x.phase, installer::short_hash(&x.sql)))
        .collect();
    assert_eq!(actual, expected);
}
