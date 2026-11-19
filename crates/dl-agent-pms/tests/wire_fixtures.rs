//! The golden frames of `packages/shared/src/agent-protocol/fixtures`, the same files the
//! TypeScript and Go protocol tests read.

use std::path::PathBuf;

use dl_agent_pms::wire::{self, Frame};
use dl_json::Value;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/agent-protocol")
}

fn read(file: &str) -> Vec<u8> {
    std::fs::read(fixture_dir().join(file)).unwrap_or_else(|e| panic!("{file}: {e}"))
}

/// Object keys sorted recursively, so two JSON texts compare like Go's `reflect.DeepEqual`
/// over `json.Unmarshal` into `any`. Numbers compare by value.
fn semantic(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut items: Vec<(String, String)> =
                m.iter().map(|(k, v)| (k.clone(), semantic(v))).collect();
            items.sort();
            let parts: Vec<String> = items
                .into_iter()
                .map(|(k, v)| format!("{k:?}:{v}"))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(semantic).collect::<Vec<_>>().join(",")),
        Value::Number(n) => format!("{}", n.as_f64().unwrap_or(f64::NAN)),
        other => dl_json::to_string(other),
    }
}

fn parse(data: &[u8]) -> Value {
    dl_agent_pms::json::parse(data).unwrap()
}

#[test]
fn fixtures_round_trip() {
    let manifest = parse(&read("manifest.json"));
    assert_eq!(manifest["protocol"].as_i64(), Some(wire::PROTOCOL_VERSION));
    let (mut valid, mut retired, mut invalid) = (0, 0, 0);
    for fx in manifest["fixtures"].as_array().unwrap() {
        let file = fx["file"].as_str().unwrap();
        let cloud_sent = fx["direction"].as_str() == Some("cloud_to_agent");
        let raw = read(file);
        let decoded = wire::decode_json(&raw);
        let typ = parse(&raw)["type"].as_str().unwrap_or_default().to_owned();
        if wire::RETIRED_FRAMES.contains(&typ.as_str()) {
            // The SMILE-only contract retired the migrate family: refused, never run.
            retired += 1;
            assert!(
                matches!(decoded, Err(wire::WireError::Retired(_))),
                "{file}: a retired frame must be refused as retired, got {decoded:?}"
            );
            continue;
        }
        if fx["valid"].as_bool() != Some(true) {
            invalid += 1;
            assert!(decoded.is_err(), "{file}: invalid fixture decoded");
            continue;
        }
        valid += 1;
        let frame = decoded.unwrap_or_else(|e| panic!("{file}: {e}"));
        let out = wire::marshal_frame(&frame).unwrap();
        assert_eq!(
            semantic(&parse(&raw)),
            semantic(&parse(out.as_bytes())),
            "{file}: round trip differs\n in: {}\nout: {out}",
            String::from_utf8_lossy(&raw)
        );
        assert_eq!(
            wire::decode_cloud_frame(&raw, false).is_ok(),
            cloud_sent,
            "{file}: direction"
        );
        if !cloud_sent {
            let msg = wire::encode(&frame).unwrap();
            assert!(!msg.binary, "{file}");
        }
    }
    assert_eq!((valid, retired, invalid), (16, 8, 8));
}

#[test]
fn manifest_codes_match_the_agent() {
    let manifest = parse(&read("manifest.json"));
    let codes: Vec<&str> = manifest["error_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(codes, wire::ERROR_CODES);
    assert_eq!(
        manifest["schema_version_unknown"].as_i64(),
        Some(wire::SCHEMA_VERSION_UNKNOWN)
    );
    for (file, code) in [
        ("result-failed-busy.json", wire::ERR_AGENT_BUSY),
        ("result-failed-timeout.json", wire::ERR_OP_TIMEOUT),
    ] {
        match wire::decode_json(&read(file)).unwrap() {
            Frame::Result(r) => {
                assert_eq!(r.status, wire::STATUS_FAILED);
                assert_eq!(r.error.unwrap().code, code);
            }
            other => panic!("{file}: {other:?}"),
        }
    }
    match wire::decode_json(&read("hello-schema-unknown.json")).unwrap() {
        Frame::Hello(h) => assert_eq!(h.schema_version, wire::SCHEMA_VERSION_UNKNOWN),
        other => panic!("{other:?}"),
    }
}

#[test]
fn hello_ops_and_replay_fixtures() {
    match wire::decode_json(&read("hello-ops.json")).unwrap() {
        Frame::Hello(h) => {
            let m = dl_agent_pms::ops::Manifest::load().unwrap();
            assert_eq!(
                h.ops,
                Some(m.implemented_ops()),
                "this build advertises the fixture's ops"
            );
            assert_eq!(h.contract_max, m.contract_max());
            assert_eq!(h.schema_version, 0);
        }
        other => panic!("{other:?}"),
    }
    match wire::decode_json(&read("op-replay.json")).unwrap() {
        Frame::Op(op) => {
            assert!(op.is_replay());
            assert_eq!(op.target_db, "SMILE_FO");
        }
        other => panic!("{other:?}"),
    }
    match wire::decode_json(&read("op-read.json")).unwrap() {
        Frame::Op(op) => {
            assert!(!op.is_replay());
            assert_eq!(op.request_key, None, "reads carry no request key");
        }
        other => panic!("{other:?}"),
    }
    for file in [
        "invalid/op-missing-target-db.json",
        "invalid/op-target-db-mm-pms.json",
        "invalid/hello-ops-bad-entry.json",
    ] {
        assert!(
            matches!(
                wire::decode_json(&read(file)),
                Err(wire::WireError::Frame(_))
            ),
            "{file}"
        );
    }
}
