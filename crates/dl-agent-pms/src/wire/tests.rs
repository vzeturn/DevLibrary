use super::*;

const ID: &str = "6f1c2b7e-8d4a-4c3b-9e2f-1a2b3c4d5e6f";

#[test]
fn refuses_contract_drift() {
    let cases = [
        r#"{"type":"nope"}"#,
        r#"[1]"#,
        r#"{"type":"ping","ts":null}"#,
        r#"{"type":"op","id":"6f1c2b7e-8d4a-4c3b-9e2f-1a2b3c4d5e6f","op":"x","contract_ver":1,"args":{},"deadline_ms":1}"#,
        r#"{"type":"ping","ts":1.5}"#,
        r#"{"type":"op","id":"6f1c2b7e-8d4a-4c3b-9e2f-1a2b3c4d5e6f","op":"x","contract_ver":1,"args":[],"request_key":null,"deadline_ms":1}"#,
        r#"{"type":"op","id":"nope","op":"x","contract_ver":1,"args":{},"request_key":null,"deadline_ms":1}"#,
        r#"{"type":"cfg","protocol":1,"poll":{"interval_ms":1,"x":1},"limits":{"max_frame_bytes":1,"gzip_threshold_bytes":1,"inflight_cap":1,"heartbeat_ms":1},"allowlist_hash":null}"#,
        r#"{"type":"cfg","protocol":2,"poll":{"interval_ms":1},"limits":{"max_frame_bytes":1,"gzip_threshold_bytes":1,"inflight_cap":1,"heartbeat_ms":1},"allowlist_hash":null}"#,
        r#"{"type":"result","id":"6f1c2b7e-8d4a-4c3b-9e2f-1a2b3c4d5e6f","status":"failed","sets":[],"rowcounts":[],"outputs":{},"error":{"code":"X","message":"m"}}"#,
        r#"{"type":"result","id":"6f1c2b7e-8d4a-4c3b-9e2f-1a2b3c4d5e6f","status":"applied","sets":[[null]],"rowcounts":[],"outputs":{},"error":null}"#,
        r#"{"type":"ping","Ts":1,"ts":1}"#,
    ];
    for js in cases {
        assert!(decode_json(js.as_bytes()).is_err(), "{js}");
    }
}

#[test]
fn errors_never_echo_values() {
    let err = decode_json(br#"{"type":"op","id":"6f1c2b7e-8d4a-4c3b-9e2f-1a2b3c4d5e6f","op":"x","contract_ver":"SECRET-VALUE","target_db":"SMILE_FO","args":{},"request_key":null,"deadline_ms":1}"#).unwrap_err();
    assert_eq!(err.to_string(), "bad op frame at contract_ver");
}

#[test]
fn migrate_frames_are_retired_and_op_frames_name_a_smile_database() {
    for t in RETIRED_FRAMES {
        let js = format!(r#"{{"type":"{t}","id":"{ID}"}}"#);
        assert_eq!(
            decode_json(js.as_bytes()),
            Err(WireError::Retired(t)),
            "{t}"
        );
    }
    let op = |db: &str, extra: &str| {
        format!(
            r#"{{"type":"op","id":"{ID}","op":"fo.hotelDate.get","contract_ver":1,"target_db":"{db}","args":{{}},"request_key":null,"deadline_ms":1{extra}}}"#
        )
    };
    for db in ["SMILE_FO", "SMILE_POS", "SMILE_BO"] {
        let Frame::Op(o) = decode_json(op(db, "").as_bytes()).unwrap() else {
            panic!("not an op");
        };
        assert_eq!(o.target_db, db);
        assert!(!o.is_replay());
    }
    for db in ["MM_PMS", "smile_fo", ""] {
        assert!(decode_json(op(db, "").as_bytes()).is_err(), "{db}");
    }
    let Frame::Op(o) = decode_json(op("SMILE_FO", r#","replay":true"#).as_bytes()).unwrap() else {
        panic!("not an op");
    };
    assert!(o.is_replay());
    let back = to_json(&Frame::Op(o));
    assert!(
        back.contains(r#""target_db":"SMILE_FO""#) && back.contains(r#""replay":true"#),
        "{back}"
    );
    assert!(decode_json(op("SMILE_FO", r#","replay":"yes""#).as_bytes()).is_err());
}

#[test]
fn gzips_large_frames_and_limits_cloud_frames() {
    let mut row = Row::new();
    row.insert("v".into(), Value::String("x".repeat(GZIP_THRESHOLD_BYTES)));
    let big = Frame::Result(OpResult {
        id: ID.into(),
        status: STATUS_APPLIED.into(),
        sets: vec![vec![row]],
        rowcounts: vec![1],
        outputs: BTreeMap::new(),
        error: None,
        replayed: None,
    });
    let msg = encode(&big).unwrap();
    assert!(msg.binary);
    let plain = dl_flate::gzip::decompress(&msg.data, MAX_FRAME_BYTES).unwrap();
    assert_eq!(decode_json(&plain).unwrap(), big);

    let ping = dl_flate::gzip::compress(br#"{"type":"ping","ts":5}"#, dl_flate::Level::Default);
    assert_eq!(
        decode_cloud_frame(&ping, true).unwrap(),
        Frame::Ping(Ping { ts: 5 })
    );
    let bomb = dl_flate::gzip::compress(&vec![b' '; MAX_FRAME_BYTES + 10], dl_flate::Level::Fast);
    assert_eq!(decode_cloud_frame(&bomb, true), Err(WireError::TooLarge));
    assert_eq!(
        decode_cloud_frame(&vec![0; MAX_FRAME_BYTES + 1], false),
        Err(WireError::TooLarge)
    );
    assert!(decode_cloud_frame(b"not gzip", true).is_err());
    assert!(encode(&Frame::Ping(Ping { ts: 1 })).is_err());
}

#[test]
fn https_urls() {
    assert!(https_url_ok("https://cdn.example.com/a.exe"));
    assert!(https_url_ok("https://u:p@[::1]:443/x"));
    assert!(!https_url_ok("http://cdn.example.com/a.exe"));
    assert!(!https_url_ok("https:///a.exe"));
    assert!(!https_url_ok("https://a b/"));
    assert!(!https_url_ok("https://h:x/"));
}
