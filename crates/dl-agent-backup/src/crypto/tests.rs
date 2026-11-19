use super::*;
use dl_json::Value;

fn hex(s: &str) -> Vec<u8> {
    dl_encoding::hex::decode(s).unwrap()
}

#[test]
fn hkdf_rfc5869_test_case_1() {
    let mut okm = [0u8; 42];
    hkdf_sha256(
        &[0x0b; 22],
        &hex("000102030405060708090a0b0c"),
        &hex("f0f1f2f3f4f5f6f7f8f9"),
        &mut okm,
    );
    assert_eq!(
        okm.to_vec(),
        hex("3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865")
    );
}

#[test]
fn hkdf_rfc5869_test_case_3_empty_salt_and_info() {
    let mut okm = [0u8; 42];
    hkdf_sha256(&[0x0b; 22], &[], &[], &mut okm);
    assert_eq!(
        okm.to_vec(),
        hex("8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8")
    );
}

/// Every fixture was produced by the hub's own `encryptCredential` under bun
/// (`tests/fixtures/make-credentials.ts`), with test-only keys.
#[test]
fn opens_every_value_the_hub_sealed() {
    let raw = include_str!("../../tests/fixtures/credentials.json");
    let doc = dl_json::parse(raw).unwrap();
    let cases = doc.as_array().unwrap();
    assert!(cases.len() >= 10);
    for case in cases {
        let key = case.get("key").and_then(Value::as_str).unwrap();
        let plain = case.get("plain").and_then(Value::as_str).unwrap();
        let stored = case.get("stored").and_then(Value::as_str).unwrap();
        assert_eq!(decrypt_credential(stored, key).unwrap(), plain);
    }
}

#[test]
fn wrong_key_is_a_credential_error() {
    let raw = include_str!("../../tests/fixtures/credentials.json");
    let doc = dl_json::parse(raw).unwrap();
    let stored = doc.as_array().unwrap()[0]
        .get("stored")
        .and_then(Value::as_str)
        .unwrap();
    let err = decrypt_credential(stored, "not-the-key").unwrap_err();
    assert_eq!(err.code(), "backup_credential_error");
    assert!(err.to_string().contains("authentication failed"));
}

#[test]
fn malformed_values_are_rejected() {
    assert!(decrypt_credential("a.b", "k").is_err());
    assert!(decrypt_credential("a.b.c.d", "k").is_err());
    assert!(decrypt_credential("!!.AAAA.AAAA", "k").is_err());
}

#[test]
fn empty_value_is_an_empty_secret() {
    assert_eq!(decrypt_credential("", "k").unwrap(), "");
}

#[test]
fn round_trip_with_random_salt() {
    let stored = encrypt_credential("p@ss'word", "agent-key").unwrap();
    assert_eq!(stored.split('.').count(), 3);
    assert_eq!(
        decrypt_credential(&stored, "agent-key").unwrap(),
        "p@ss'word"
    );
}
