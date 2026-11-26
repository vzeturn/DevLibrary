//! `RowHasherTests.cs`, `RowKeyTests.cs`, `ValueTransformTests.cs`, `Tcvn3Tests.cs` and the
//! staff PIN golden vector (`MM.PdaOrder.SyncAgent.Tests/Staff/StaffPinHasherTests.cs`).

use super::*;
use crate::contracts::hashing::{canonical_value, row_hasher, row_key};
use crate::contracts::mapping::{MappingError, tcvn3, transforms};
use crate::contracts::staff;
use crate::contracts::time::DateTimeKind;

fn h(cols: &[(&str, Option<&str>)]) -> String {
    row_hasher::hash_pairs(cols.iter().copied())
}

#[test]
fn empty_row_is_sha256_of_empty_input() {
    assert_eq!(
        h(&[]),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn single_column_vectors() {
    for (k, v, expected) in [
        (
            "a",
            Some("1"),
            "1c370cfb4b8664891ba67384263480f1d642eec8a2da7b572783ec1594a0314f",
        ),
        (
            "a",
            None,
            "1242cbbdaa5264fed92898c7fca7269cf6ae650e415e4d3ea04259fec2348507",
        ),
        (
            "a",
            Some(""),
            "3609a603acb93d08718a2e29207b8485b44260f4ac964b1bcffb0a0425d997c6",
        ),
    ] {
        assert_eq!(h(&[(k, v)]), expected);
    }
}

#[test]
fn multi_column_unicode_vector() {
    assert_eq!(
        h(&[
            ("check_no", Some("500001234")),
            ("total_paid", Some("125000.5")),
            ("trn_desc", Some("Phở bò")),
            ("note", None),
        ]),
        "00365837b576fcfa09ce189474fe5186beacd84342965dc8ba71a37878561bb3"
    );
}

#[test]
fn column_insertion_order_does_not_matter() {
    let a = h(&[("b", Some("x")), ("a", Some("yz"))]);
    assert_eq!(a, h(&[("a", Some("yz")), ("b", Some("x"))]));
    assert_eq!(
        a,
        "0f09f26d88a027bc1e8ad934d849fd99a3a8527246aa4285b009949ee79d24bd"
    );
    // The map form hashes the same.
    assert_eq!(
        row_hasher::hash(&smap([("b", Some("x")), ("a", Some("yz"))])),
        a
    );
}

#[test]
fn length_prefixes_prevent_boundary_ambiguity() {
    let a = h(&[("a", Some("b")), ("c", Some("d"))]);
    let b = h(&[("ab", Some("")), ("c", Some("d"))]);
    assert_eq!(
        a,
        "f72dbbb3f0f7f70bcdf88c7ae06f327a097b69fc2b0451ca45e9fe8bec4dd9f3"
    );
    assert_eq!(
        b,
        "21597d481627a277b41646ee90cec449e27c363689282ca9352a6fbdf074a884"
    );
    assert_ne!(a, b);
}

#[test]
fn null_and_empty_differ() {
    assert_ne!(h(&[("a", None)]), h(&[("a", Some(""))]));
}

#[test]
fn hash_is_lower_case_hex_64() {
    let x = h(&[("a", Some("1"))]);
    assert!(
        x.len() == 64
            && x.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
}

// Hash_is_culture_independent: Rust has no current culture; the vector above covers it.

#[test]
fn a_key_without_nulls_keeps_its_form() {
    assert_eq!(
        row_key::of(&smap([("b", Some("2")), ("a", Some("1"))])),
        "a=1\u{1f}b=2"
    );
}

#[test]
fn a_null_key_value_never_collides_with_an_empty_string() {
    let with_null = row_key::of(&smap([("CheckNo", Some("7")), ("Notes", None)]));
    let with_empty = row_key::of(&smap([("CheckNo", Some("7")), ("Notes", Some(""))]));
    assert_eq!(with_null, "CheckNo=7\u{1f}Notes");
    assert_ne!(with_null, with_empty);
    assert!(row_key::bucket(&with_null, 256) < 256);
}

// ---- ValueTransformTests ---------------------------------------------------------------

#[test]
fn money_decimal_is_invariant_and_trimmed() {
    for (input, expected) in [
        ("10.50", "10.5"),
        ("0.00", "0"),
        ("-0.00", "0"),
        ("100", "100"),
        ("1000000.000000", "1000000"),
        ("-125000.50", "-125000.5"),
        ("1234567890123.4567", "1234567890123.4567"),
        (
            "0.0000000000000000000000000001",
            "0.0000000000000000000000000001",
        ),
        (
            "79228162514264337593543950335",
            "79228162514264337593543950335",
        ),
    ] {
        assert_eq!(
            transforms::money(&dec(input), "c").unwrap().as_deref(),
            Some(expected)
        );
        assert_eq!(
            transforms::money(&input.into(), "c").unwrap().as_deref(),
            Some(expected)
        );
    }
}

// Money_is_culture_independent: no current culture in Rust (covered by the cases above).

#[test]
fn money_accepts_integers_null_and_rejects_garbage() {
    assert_eq!(
        transforms::money(&RawValue::I32(42), "c")
            .unwrap()
            .as_deref(),
        Some("42")
    );
    assert_eq!(
        transforms::money(&RawValue::I64(42), "c")
            .unwrap()
            .as_deref(),
        Some("42")
    );
    assert_eq!(transforms::money(&RawValue::Null, "c").unwrap(), None);
    assert!(matches!(
        transforms::money(&"".into(), "c"),
        Err(MappingError::ValueFormat { .. })
    ));
}

#[test]
fn money_unparseable_text_throws_with_column_name() {
    let e = transforms::money(&"12,5abc".into(), "TotalPaid").unwrap_err();
    assert!(e.to_string().contains("TotalPaid"));
}

#[test]
fn money_from_floats_like_convert_to_decimal() {
    // Convert.ToDecimal(double/float) as .NET 10 prints it (probe.out "CD"/"CF").
    for (v, expected) in [
        (0.1, "0.1"),
        (1.0 / 3.0, "0.333333333333333"),
        (123_456_789.123_456_79, "123456789.123457"),
        (1e-20, "0.00000000000000000001"),
        (12_345_678_901_234_568.0, "12345678901234600"),
        (1e28, "10000000000000000000000000000"),
    ] {
        assert_eq!(
            transforms::money(&RawValue::F64(v), "c")
                .unwrap()
                .as_deref(),
            Some(expected)
        );
    }
    for (v, expected) in [
        (0.1f32, "0.1"),
        (1.0f32 / 3.0, "0.3333333"),
        (123_456.79f32, "123456.8"),
    ] {
        assert_eq!(
            transforms::money(&RawValue::F32(v), "c")
                .unwrap()
                .as_deref(),
            Some(expected)
        );
    }
}

#[test]
fn decimal_or_null_like_decimal_try_parse_float() {
    // decimal.TryParse(s, NumberStyles.Float, Invariant) canonicalised (probe.out "DP").
    for (s, expected) in [
        (
            "1.23456789012345678901234567891",
            Some("1.2345678901234567890123456789"),
        ),
        ("0.00000000000000000000000000001", Some("0")),
        ("79228162514264337593543950336", None),
        (" 12 ", Some("12")),
        ("+5", Some("5")),
        ("1e3", Some("1000")),
        ("1E-2", Some("0.01")),
        ("-0.00", Some("0")),
        ("12,5", None),
        ("", None),
        ("1.", Some("1")),
        (".5", Some("0.5")),
        ("1_0", None),
        ("0x10", None),
        ("\t7\n", Some("7")),
        ("1e400", None),
        ("1e-400", Some("0")),
    ] {
        assert_eq!(
            canonical_value::decimal_or_null(Some(s)).as_deref(),
            expected,
            "[{s}]"
        );
    }
}

#[test]
fn vietnam_local_time_is_utc_plus_seven() {
    assert_eq!(
        transforms::local_time(&dt(2026, 10, 24, 12, 0, 0), "c", &vietnam())
            .unwrap()
            .as_deref(),
        Some("2026-10-24T05:00:00.0000000Z")
    );
}

#[test]
fn fractional_seconds_survive() {
    let t = DateTime::new(2026, 1, 1, 0, 0, 0)
        .unwrap()
        .checked_add_ticks(1_234_567)
        .unwrap();
    assert_eq!(
        transforms::local_time(&RawValue::DateTime(t), "c", &vietnam())
            .unwrap()
            .as_deref(),
        Some("2025-12-31T17:00:00.1234567Z")
    );
}

#[test]
fn utc_kind_is_trusted_and_not_shifted() {
    let t = DateTime::new(2026, 1, 1, 8, 0, 0)
        .unwrap()
        .with_kind(DateTimeKind::Utc);
    assert_eq!(
        transforms::local_time(&RawValue::DateTime(t), "c", &vietnam())
            .unwrap()
            .as_deref(),
        Some("2026-01-01T08:00:00.0000000Z")
    );
}

#[test]
fn dst_gap_time_moves_forward_by_the_gap() {
    let ny = TimeZone::find_system_time_zone_by_id("America/New_York").unwrap();
    // 2026-03-08 02:30 does not exist; becomes 03:30 EDT (UTC-4) = 07:30Z.
    assert_eq!(
        transforms::local_time(&dt(2026, 3, 8, 2, 30, 0), "c", &ny)
            .unwrap()
            .as_deref(),
        Some("2026-03-08T07:30:00.0000000Z")
    );
}

#[test]
fn dst_ambiguous_time_resolves_to_standard_offset() {
    let ny = TimeZone::find_system_time_zone_by_id("America/New_York").unwrap();
    // 2026-11-01 01:30 happens twice; standard (EST, UTC-5) = 06:30Z.
    assert_eq!(
        transforms::local_time(&dt(2026, 11, 1, 1, 30, 0), "c", &ny)
            .unwrap()
            .as_deref(),
        Some("2026-11-01T06:30:00.0000000Z")
    );
}

#[test]
fn time_null_and_garbage() {
    assert_eq!(
        transforms::local_time(&RawValue::Null, "c", &vietnam()).unwrap(),
        None
    );
    assert!(transforms::local_time(&"nope".into(), "c", &vietnam()).is_err());
}

#[test]
fn capture_text_loses_nul_characters_and_char_padding() {
    // RT-1309: PostgreSQL refuses U+0000; a padded CHAR(n) code still matches once trimmed.
    let z = vietnam();
    let g = |v: RawValue| transforms::generic(&v, "c", &z).unwrap();
    assert_eq!(g("C\0\0\0".into()).as_deref(), Some("C"));
    assert_eq!(g("100001      ".into()).as_deref(), Some("100001"));
    assert_eq!(
        transforms::tcvn3(&RawValue::Bytes(b"C\0\0  ".to_vec()), "IName")
            .unwrap()
            .as_deref(),
        Some("C")
    );
}

#[test]
fn generic_covers_common_ado_types() {
    let z = vietnam();
    let g = |v: RawValue| transforms::generic(&v, "c", &z).unwrap();
    assert_eq!(g(RawValue::Bool(true)).as_deref(), Some("true"));
    assert_eq!(g(RawValue::I16(7)).as_deref(), Some("7"));
    assert_eq!(g(RawValue::F64(1.5)).as_deref(), Some("1.5"));
    assert_eq!(g(RawValue::Bytes(vec![1, 2, 3])).as_deref(), Some("AQID"));
    assert_eq!(g("".into()).as_deref(), Some(""));
    assert_eq!(g(RawValue::Null), None);
    assert_eq!(
        g(dt(2026, 10, 24, 7, 0, 0)).as_deref(),
        Some("2026-10-24T00:00:00.0000000Z")
    );
    let id = dl_uuid::Uuid::parse("11111111-2222-3333-4444-555555555555").unwrap();
    assert_eq!(
        g(RawValue::Guid(id)).as_deref(),
        Some("11111111-2222-3333-4444-555555555555")
    );
}

#[test]
fn snake_case() {
    for (vendor, pg) in [
        ("PNo", "p_no"),
        ("CheckNo", "check_no"),
        ("ITotalR", "i_total_r"),
        ("nSeq", "n_seq"),
        ("RVC", "rvc"),
        ("CGName", "cg_name"),
        ("StsCode1", "sts_code1"),
        ("TrnSeq", "trn_seq"),
        // .NET 10 SnakeCase.From (probe3 "SNAKE").
        ("A_B", "a_b"),
        ("__X__", "x"),
        ("ABCDef", "abc_def"),
        ("a1B2", "a1_b2"),
        ("IDCode", "id_code"),
        ("Đơn Giá", "đơn_giá"),
    ] {
        assert_eq!(transforms::snake_case(vendor), pg);
    }
}

#[test]
fn canonical_utc_is_fixed_width() {
    let t = DateTime::new(2026, 1, 2, 3, 4, 5)
        .unwrap()
        .with_kind(DateTimeKind::Utc);
    assert_eq!(canonical_value::utc(t), "2026-01-02T03:04:05.0000000Z");
}

// ---- Tcvn3Tests ------------------------------------------------------------------------

#[test]
fn decodes_lowercase_and_plain_ascii() {
    assert_eq!(tcvn3::decode(&PHO_BO_TCVN3), "Phở bò");
}

#[test]
fn decodes_uppercase_base_plus_tone_pair_to_one_char() {
    assert_eq!(tcvn3::decode(&[0x41, 0xB8]), "Á");
}

#[test]
fn decodes_standalone_d_stroke_both_cases() {
    assert_eq!(tcvn3::decode(&[0xA7, 0xAE]), "Đđ");
}

#[test]
fn null_stays_null_and_empty_stays_empty() {
    assert_eq!(tcvn3::decode(&[]), "");
    assert_eq!(transforms::tcvn3(&RawValue::Null, "c").unwrap(), None);
    assert_eq!(
        transforms::tcvn3(&RawValue::Bytes(vec![]), "c")
            .unwrap()
            .as_deref(),
        Some("")
    );
}

#[test]
fn latin1_materialised_string_is_decoded_as_bytes() {
    let latin1: String = PHO_BO_TCVN3.iter().map(|b| char::from(*b)).collect();
    assert_eq!(
        transforms::tcvn3(&latin1.as_str().into(), "c")
            .unwrap()
            .as_deref(),
        Some("Phở bò")
    );
}

#[test]
fn already_unicode_string_passes_through() {
    assert_eq!(
        transforms::tcvn3(&"Phở bò".into(), "c").unwrap().as_deref(),
        Some("Phở bò")
    );
}

#[test]
fn rejects_non_text_values() {
    assert!(matches!(
        transforms::tcvn3(&RawValue::I32(42), "c"),
        Err(MappingError::ValueFormat { .. })
    ));
}

/// `Copy_matches_infrastructure_original_for_every_one_and_two_byte_input`: the C# test
/// compares the Mapping copy with the Infrastructure original; here the Rust decoder is
/// compared with the .NET output of the same 65 792 inputs (SHA-256 of every decode
/// followed by `\n`, printed by probe3 "TCVN3").
#[test]
fn matches_dotnet_for_every_one_and_two_byte_input() {
    use dl_hash::{Digest, Sha256};
    let mut sha = Sha256::default();
    for a in 0..=255u8 {
        sha.update(tcvn3::decode(&[a]).as_bytes());
        sha.update(b"\n");
        for b in 0..=255u8 {
            sha.update(tcvn3::decode(&[a, b]).as_bytes());
            sha.update(b"\n");
        }
    }
    assert_eq!(
        dl_encoding::hex::encode(sha.finalize()),
        "1ed3128be6a1208af21bf0cda097502f1d7fee476ffb6268c8d3c4fa98f9ff23"
    );
}

// ---- Staff PIN (golden vector shared with the cloud) -----------------------------------

const STAFF_PIN_TESTS: &str = include_str!("../../../testdata/cs/Staff/StaffPinHasherTests.cs");

/// The value of a C# string literal that follows `marker` in the test source.
fn literal_after<'a>(src: &'a str, marker: &str) -> &'a str {
    let at = src.find(marker).expect("marker") + marker.len();
    let rest = &src[at..];
    let start = rest.find('"').expect("open quote") + 1;
    let len = rest[start..].find('"').expect("close quote");
    &rest[start..start + len]
}

#[test]
fn staff_pin_golden_vector() {
    let key_hex = literal_after(STAFF_PIN_TESTS, "GoldenKey = Convert.FromHexString(");
    let key = dl_encoding::hex::decode(key_hex).unwrap();
    let key_id = literal_after(STAFF_PIN_TESTS, "GoldenKeyId =");
    let pin = literal_after(STAFF_PIN_TESTS, "GoldenPin =");
    let lookup = literal_after(STAFF_PIN_TESTS, "GoldenLookup =");
    assert_eq!(key_id, "e346975cd4c277eb");
    assert_eq!(staff::key_id(&key), key_id);
    let mac = staff::lookup(&key, pin).unwrap();
    assert_eq!(dl_encoding::base64::STANDARD.encode(mac), lookup);
    assert_eq!(lookup, "FnBxjjdDkD4NU/2PPVfVQ+rDq2ELINJH2Z63OYZEh64=");
    assert!(staff::lookup(&key[..31], pin).is_err());
    assert!(staff::is_usable(Some("323")) && staff::is_usable(Some("123456789012")));
    assert!(!staff::is_usable(Some("1234567890123")) && !staff::is_usable(Some("")));
    assert!(
        !staff::is_usable(Some("12a")) && !staff::is_usable(None) && !staff::is_usable(Some("١٢"))
    );
}

/// `Text_loses_trailing_spaces_as_the_store_database_compares_it` (ADR 0123).
#[test]
fn text_loses_trailing_spaces_as_the_store_database_compares_it() {
    let z = vietnam();
    for (stored, expected) in [
        ("100001      ", "100001"),
        (
            "  keeps leading and inner  spaces  ",
            "  keeps leading and inner  spaces",
        ),
        ("   ", ""),
        ("tab stays\t", "tab stays\t"),
        ("a b\t ", "a b\t"),
        ("plain", "plain"),
        ("", ""),
    ] {
        let latin1 = RawValue::Bytes(stored.bytes().collect());
        let cases = [
            transforms::generic(&stored.into(), "c", &z),
            transforms::tcvn3(&stored.into(), "c"),
            transforms::tcvn3(&latin1, "c"),
        ];
        for got in cases {
            assert_eq!(got.unwrap().as_deref(), Some(expected), "{stored:?}");
        }
    }
    assert_eq!(
        transforms::tcvn3(&"Phở bò   ".into(), "c")
            .unwrap()
            .as_deref(),
        Some("Phở bò"),
        "an already-Unicode value is trimmed too"
    );
}

/// `A_fixed_width_code_maps_to_the_same_key_and_hash_as_its_trimmed_form` (ADR 0123).
#[test]
fn a_fixed_width_code_maps_to_the_same_key_and_hash_as_its_trimmed_form() {
    let item = |code: &str, name: &str| {
        row([
            ("ICode", code.into()),
            ("IName", name.into()),
            ("Price", dec("50000.00")),
            ("IClass", RawValue::I32(901)),
            ("IGroup", "1".into()),
            ("IActive", RawValue::Bool(true)),
            ("RVC", RawValue::I32(1)),
        ])
    };
    let padded = mapper()
        .map("ItemDef", &item("100001      ", "Pho bo    "))
        .unwrap();
    let trimmed = mapper().map("ItemDef", &item("100001", "Pho bo")).unwrap();

    assert_eq!(
        padded.key.get("item_code").cloned().flatten().as_deref(),
        Some("100001")
    );
    assert_eq!(padded.key, trimmed.key);
    assert_eq!(padded.columns, trimmed.columns);
    assert_eq!(padded.hash, trimmed.hash);
}
