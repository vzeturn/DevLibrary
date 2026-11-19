//! AES-GCM, HKDF, envelopes and the tenant key against the .NET golden fixtures.

mod common;

use common::{cases, field, golden, i, s, unb64, unhex};
use dl_agent_arrival::contracts::key_vault::TenantKeyVault;
use dl_agent_arrival::contracts::messages::{
    AgentHello, AgentPollRequest, AgentPollResponse, PmsChangeNotice, PmsResponse,
};
use dl_agent_arrival::contracts::{Direction, EnvelopeCipher, SyncEnvelope};
use dl_agent_arrival::crypto::{self, AesGcm};
use dl_agent_arrival::stj;
use dl_json::Value;

#[test]
fn aes_gcm_matches() {
    let g = golden();
    for c in cases(&g, "aes_gcm") {
        let key: [u8; 32] = unhex(s(c, "key")).try_into().expect("key");
        let nonce: [u8; 12] = unhex(s(c, "nonce")).try_into().expect("nonce");
        let aad = unhex(s(c, "aad"));
        let plain = unhex(s(c, "plain"));
        let out = unhex(s(c, "out"));
        let gcm = AesGcm::new(&key);
        assert_eq!(gcm.seal(&nonce, &plain, &aad), out);
        assert_eq!(gcm.open(&nonce, &out, &aad), Some(plain));
        let mut bad = out.clone();
        bad[0] ^= 1;
        assert_eq!(gcm.open(&nonce, &bad, &aad), None);
    }
}

#[test]
fn hkdf_direction_keys_match() {
    let g = golden();
    for c in cases(&g, "hkdf") {
        let salt = format!("mm-arrival/sync/{}/{}", s(c, "tenant"), s(c, "agent"));
        let secret = unb64(s(c, "secret"));
        let a2c = crypto::hkdf_sha256(&secret, salt.as_bytes(), b"v1 agent->cloud");
        let c2a = crypto::hkdf_sha256(&secret, salt.as_bytes(), b"v1 cloud->agent");
        assert_eq!(crypto::hex(&a2c), s(c, "a2c"));
        assert_eq!(crypto::hex(&c2a), s(c, "c2a"));
    }
}

fn hello_of(v: &Value) -> Option<AgentHello> {
    let m = v.as_object()?;
    let text = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_string);
    Some(AgentHello {
        agent_version: text("agentVersion").expect("agentVersion"),
        catalog_version: i32::try_from(m.get("catalogVersion")?.as_i64()?).ok()?,
        machine_name: text("machineName").expect("machineName"),
        pms_schema_fingerprint: text("pmsSchemaFingerprint"),
        procedures_report: text("proceduresReport"),
    })
}

/// Rebuilds the poll request from the fixture's plain text and serializes it again.
fn poll_request_text(plain: &str) -> String {
    let v = stj::parse(plain).expect("plain");
    let m = v.as_object().expect("object");
    let responses = m["responses"]
        .as_array()
        .expect("responses")
        .iter()
        .map(|r| PmsResponse::from_json(r).expect("response"))
        .collect();
    let notices = m["notices"]
        .as_array()
        .expect("notices")
        .iter()
        .map(|n| PmsChangeNotice {
            operation: s(n, "operation").into(),
            parameters_hash: s(n, "parametersHash").into(),
            new_etag: s(n, "newETag").into(),
            changed_rows: i32::try_from(i(n, "changedRows")).expect("rows"),
        })
        .collect();
    AgentPollRequest {
        hello: hello_of(&m["hello"]),
        responses,
        notices,
    }
    .to_json_string()
}

#[test]
fn envelopes_match() {
    let g = golden();
    for c in cases(&g, "envelopes") {
        let cipher = EnvelopeCipher::new(
            s(c, "tenant"),
            s(c, "agent"),
            s(c, "kid"),
            &unb64(s(c, "secret")),
        )
        .expect("cipher");
        let plain = s(c, "plain");
        let ty = s(c, "type");
        let nonce: [u8; 12] = unb64(s(c, "nonce")).try_into().expect("nonce");
        let d = if s(c, "direction") == "a2c" {
            assert_eq!(poll_request_text(plain), plain, "poll request text");
            Direction::AgentToCloud
        } else {
            let parsed =
                AgentPollResponse::from_json(&stj::parse(plain).expect("plain")).expect("response");
            assert_eq!(parsed.to_json_string(), plain, "poll response text");
            Direction::CloudToAgent
        };
        let env = cipher.seal_with(d, ty, plain, i(c, "timestamp"), s(c, "message_id"), nonce);
        assert_eq!(env.to_json_string(), s(c, "envelope"));
        let parsed = SyncEnvelope::parse(s(c, "envelope")).expect("envelope");
        assert_eq!(cipher.open_text(d, ty, &parsed).expect("opens"), plain);
        assert!(cipher.open_text(d, "other.type", &parsed).is_err());
    }
}

#[test]
fn tenant_key_wraps_match() {
    let g = golden();
    let tk = field(&g, "tenant_key");
    let vault = TenantKeyVault::new(&unb64(s(tk, "tmk"))).expect("tmk");
    assert_eq!(vault.fingerprint(), s(tk, "fingerprint"));
    for w in field(tk, "wraps").as_array().expect("wraps") {
        let wrapped = unb64(s(w, "wrapped"));
        let key = unb64(s(w, "key"));
        assert_eq!(
            vault.unwrap(&wrapped, s(w, "context")).expect("unwraps"),
            key
        );
        let nonce: [u8; 12] = wrapped[8..20].try_into().expect("nonce");
        assert_eq!(vault.wrap_with(&key, s(w, "context"), nonce), wrapped);
    }
}
