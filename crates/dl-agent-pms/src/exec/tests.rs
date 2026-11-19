use super::*;

fn p(json_text: &str) -> ops::Param {
    let m = Manifest::parse(
        format!(
            r#"{{"manifest_version":2,"allowlist_hash":"","procs":["P"],"excluded_proc_patterns":[],"ops":[{{"name":"a.b.c","contract_ver":1,"kind":"write","target":"FO","target_db":"SMILE_FO","timeout_ms":1,"idempotent":false,"calls":[{{"proc":"P","params":[{json_text}]}}]}}]}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    m.ops[0].calls[0].params[0].clone()
}

fn bind(param: &str, value: &str) -> Result<ParamValue, String> {
    bind_value(&p(param), Some(&json::parse(value.as_bytes()).unwrap()))
}

#[test]
fn binds_like_go() {
    let int = r#"{"name":"N","type":"int","arg":"n"}"#;
    assert_eq!(bind(int, "5"), Ok(ParamValue::I64(Some(5))));
    assert_eq!(bind(int, "\"5\"").unwrap_err(), "int expects an integer");
    assert_eq!(bind(int, "1.0").unwrap_err(), "int expects an integer");
    assert_eq!(bind(int, "2147483648").unwrap_err(), "int out of range");
    assert_eq!(
        bind(int, "99999999999999999999").unwrap_err(),
        "int out of range"
    );
    let tiny = r#"{"name":"N","type":"tinyint","arg":"n"}"#;
    assert_eq!(bind(tiny, "-1").unwrap_err(), "tinyint out of range");
    let dec = r#"{"name":"D","type":"decimal","arg":"d"}"#;
    assert_eq!(
        bind(dec, "12.50"),
        Ok(ParamValue::String(Some("12.50".into())))
    );
    assert_eq!(
        bind(dec, "\"-0.5\""),
        Ok(ParamValue::String(Some("-0.5".into())))
    );
    assert!(bind(dec, "1e3").is_err());
    assert!(bind(dec, "\".5\"").is_err());
    let s = r#"{"name":"S","type":"nvarchar","arg":"s","size":3}"#;
    assert_eq!(
        bind(s, "\"abc\""),
        Ok(ParamValue::String(Some("abc".into())))
    );
    assert_eq!(
        bind(s, "\"abcd\"").unwrap_err(),
        "nvarchar longer than 3 characters"
    );
    let max = r#"{"name":"S","type":"varchar","arg":"s","size":"max"}"#;
    assert_eq!(
        bind(max, &format!("\"{}\"", "x".repeat(9000))).map(|_| ()),
        Ok(())
    );
    let d = r#"{"name":"D","type":"date","arg":"d"}"#;
    assert!(bind(d, "\"2026-02-28\"").is_ok());
    assert!(bind(d, "\"2026-02-30\"").is_err());
    assert!(bind(d, "\"2026-2-28\"").is_err());
    let dt = r#"{"name":"T","type":"datetime2","arg":"t"}"#;
    match bind(dt, "\"2026-11-15T10:00:00+07:00\"").unwrap() {
        ParamValue::DateTime(Some(x)) => assert_eq!(x.hour(), 3),
        other => panic!("{other:?}"),
    }
    assert!(bind(dt, "\"2026-11-15T10:00:00\"").is_err());
    let b = r#"{"name":"B","type":"varbinary","arg":"b","size":2}"#;
    assert_eq!(bind(b, "\"AAE=\""), Ok(ParamValue::Bytes(Some(vec![0, 1]))));
    assert!(bind(b, "\"AAEC\"").is_err());
    let bit = r#"{"name":"B","type":"bit","arg":"b"}"#;
    assert!(bind(bit, "1").is_err());
    let nullable = p(r#"{"name":"N","type":"int","arg":"n","nullable":true}"#);
    assert_eq!(bind_value(&nullable, None), Ok(ParamValue::I64(None)));
    assert!(bind_value(&p(int), Some(&J::Null)).is_err());
}

#[test]
fn args_must_match_the_plan() {
    let m = Manifest::load().unwrap();
    let op = m.lookup("fo.hotelDate.get").unwrap();
    let args = |s: &str| json::parse(s.as_bytes()).unwrap();
    let calls = bind_args(op, &args("{}")).unwrap();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].params.is_empty());
    assert_eq!(
        bind_args(op, &args(r#"{"z":1,"a":2}"#)).err().unwrap(),
        "invalid_args: a: not an argument of this op"
    );
}

#[test]
fn hotel_date_needs_a_row_with_a_date() {
    let row = |v: J| {
        let mut r = Row::new();
        r.insert("HotelDate".into(), v);
        r
    };
    let ok = vec![vec![row(J::String("2026-11-19T00:00:00.000Z".into()))]];
    assert_eq!(missing_data("fo.hotelDate.get", &ok), None);
    for sets in [
        vec![],
        vec![vec![]],
        vec![vec![row(J::Null)]],
        vec![vec![Row::new()]],
    ] {
        assert_eq!(
            missing_data("fo.hotelDate.get", &sets),
            Some("GetHotelDate1 returned no HotelDate")
        );
    }
    assert_eq!(missing_data("a.b.c", &[]), None);
}

#[test]
fn values_encode_like_go() {
    let e = |t: &str, v: V| json::to_string(&encode_value(t, &v).unwrap());
    assert_eq!(
        e("BIGINT", V::I64(9007199254740993)),
        "\"9007199254740993\""
    );
    assert_eq!(e("INT", V::I32(-5)), "-5");
    assert_eq!(e("TINYINT", V::I8(200)), "200");
    assert_eq!(e("FLOAT", V::F64(0.1)), "0.1");
    assert_eq!(e("FLOAT", V::F64(1e21)), "1e+21");
    assert_eq!(e("FLOAT", V::F64(1.5e-7)), "1.5e-7");
    assert_eq!(e("FLOAT", V::F64(123456789.0)), "123456789");
    assert_eq!(e("REAL", V::F32(0.1)), "0.10000000149011612");
    assert_eq!(e("VARBINARY", V::Bytes(vec![1, 2])), "\"AQI=\"");
    assert_eq!(
        e("NVARCHAR", V::String("<a>".into())),
        "\"\\u003ca\\u003e\""
    );
    let date = Date::new(2026, 11, 15).unwrap();
    assert_eq!(e("DATE", V::Date(date)), "\"2026-11-15T00:00:00.000Z\"");
    let t = dl_time::Time::new(10, 5, 1, 123_456_700).unwrap();
    assert_eq!(e("TIME", V::Time(t)), "\"10:05:01.1234567\"");
    assert_eq!(
        e("DATETIME2", V::DateTime(date.at(t))),
        "\"2026-11-15T10:05:01.1234567Z\""
    );
    let ms = dl_time::Time::new(10, 5, 1, 120_000_000).unwrap();
    assert_eq!(
        e("DATETIME2", V::DateTime(date.at(ms))),
        "\"2026-11-15T10:05:01.120Z\""
    );
    let ticks = dl_time::Time::new(23, 59, 59, 999_666_667).unwrap();
    assert_eq!(
        e("DATETIME", V::DateTime(date.at(ticks))),
        "\"2026-11-16T00:00:00.000Z\""
    );
    let off = OffsetDateTime::parse_rfc3339("2026-11-15T10:00:00+07:00").unwrap();
    assert_eq!(
        e("DATETIMEOFFSET", V::DateTimeOffset(off)),
        "\"2026-11-15T10:00:00.000+07:00\""
    );
    let utc = OffsetDateTime::parse_rfc3339("2026-11-15T10:00:00+00:00").unwrap();
    assert_eq!(
        e("DATETIMEOFFSET", V::DateTimeOffset(utc)),
        "\"2026-11-15T10:00:00.000Z\""
    );
    assert!(encode_value("FLOAT", &V::F64(f64::NAN)).is_err());
    assert_eq!(go_str_len("a\"\u{1}é<"), 2 + 1 + 2 + 6 + 2 + 1);
}

#[test]
fn classification() {
    let ctx = Ctx::background();
    let server = |number, class| {
        ExecErr::Db(DbError::Tds(dl_tds::Error::Server {
            number,
            state: 1,
            class,
            message: "m".into(),
            server: String::new(),
            procedure: String::new(),
            line: 1,
        }))
    };
    let r = classify("i", &ctx, &server(1205, 13));
    assert_eq!(
        (r.status.as_str(), r.error.as_ref().unwrap().code.as_str()),
        ("failed", CODE_DEADLOCK)
    );
    assert_eq!(r.error.unwrap().sql_number, Some(1205));
    let r = classify("i", &ctx, &server(50001, 16));
    assert_eq!(
        (r.status.as_str(), r.error.unwrap().message.as_str()),
        ("rejected", "m")
    );
    let r = classify("i", &ctx, &server(4060, 20));
    assert_eq!(r.error.unwrap().code, CODE_DB_UNAVAILABLE);
    let r = classify("i", &ctx, &ExecErr::Db(DbError::Tds(dl_tds::Error::Broken)));
    assert_eq!(r.error.unwrap().message, "database_unavailable");
    let done = Ctx::background();
    done.cancel();
    let r = classify("i", &done, &server(50001, 16));
    assert_eq!(r.error.unwrap().code, wire::ERR_OP_TIMEOUT);
    assert!(valid_request_key("req-1:a.b_c"));
    assert!(!valid_request_key("-x"));
}
