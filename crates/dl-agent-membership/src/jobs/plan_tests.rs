use super::*;

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

#[test]
fn reward_reference_is_the_dotnet_guid_bytes_in_base64url() {
    // Guid("00112233-4455-6677-8899-aabbccddeeff").ToByteArray() is little-endian in the
    // first three groups: 33 22 11 00 55 44 77 66 88 99 aa bb cc dd ee ff.
    let g: Uuid = "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap();
    assert_eq!(reward_ref(g), "RWD:MyIRAFVEd2aImaq7zN3u_w");
    assert!(reward_ref(g).len() <= 30);
}

#[test]
fn percent_off_rounds_caps_and_clamps_like_the_voucher_rule() {
    assert_eq!(percent_discount(d("0.1"), None, d("1234565")), d("123457"));
    assert_eq!(
        percent_discount(d("0.1"), Some(d("50000.9")), d("1000000")),
        d("50000")
    );
    assert_eq!(
        percent_discount(d("0.1"), Some(d("-5")), d("1000000")),
        d("0")
    );
    assert_eq!(percent_discount(d("1.5"), None, d("1000")), d("1000"));
    assert_eq!(percent_discount(d("0.1"), None, d("-10")), d("0"));
}

#[test]
fn rule_strings_are_read_by_exact_name() {
    let rule = Some(r#"{"creditFolioCode":"SPA","x":1}"#);
    assert_eq!(rule_string(rule, "creditFolioCode").as_deref(), Some("SPA"));
    assert_eq!(rule_string(rule, "x"), None);
    assert_eq!(rule_string(Some("{"), "creditFolioCode"), None);
    assert_eq!(rule_string(None, "creditFolioCode"), None);
}

#[test]
fn failures_get_the_hub_codes_and_classes() {
    let deadlock = WriteError::Smile(SmileError::Server(1205));
    assert_eq!(
        classify(&deadlock),
        ("SQL_1205".into(), ErrorClass::Transient)
    );
    assert_eq!(summary(&deadlock), "SQL 1205");
    let down = WriteError::Smile(SmileError::Unavailable("refused".into()));
    assert_eq!(classify(&down).0, NETWORK);
    let profile = profile_unavailable(208);
    assert_eq!(
        classify(&profile),
        (
            PROFILE_UNAVAILABLE.into(),
            class_of_code(PROFILE_UNAVAILABLE).1
        )
    );
    assert_eq!(summary(&profile), "PROFILE_UNAVAILABLE: SQL 208");
    assert_eq!(
        classify(&profile_unavailable(1222)).1,
        ErrorClass::Transient
    );
    let lock = WriteError::Coded {
        code: "APPLOCK_TIMEOUT".into(),
        message: "busy".into(),
    };
    assert_eq!(classify(&lock).1, ErrorClass::Transient);
    let long = WriteError::Hub("x".repeat(900));
    assert_eq!(summary(&long).chars().count(), 500);
}
