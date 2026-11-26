//! Golden vectors from `testdata/frames.txt`, generated independently of this crate by
//! `testdata/gen_frames.py` from the byte layout in `docs/protocol/dl-rt-1.md`.
//! Each valid vector is checked both ways: decode(hex) == fields and encode(fields) == hex.

use dl_agent_rt::{Channel, CloseCode, Ext, Frame, Kind, Limits};

const VECTORS: &str = include_str!("../testdata/frames.txt");

struct Vector {
    name: String,
    hex: Vec<u8>,
    max_frame: Option<usize>,
    decode_only: bool,
    fields: Option<String>,
    error: Option<String>,
}

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex length: {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex digit"))
        .collect()
}

fn parse() -> Vec<Vector> {
    let mut out = Vec::new();
    for block in VECTORS.split("\n\n") {
        let mut v = Vector {
            name: String::new(),
            hex: Vec::new(),
            max_frame: None,
            decode_only: false,
            fields: None,
            error: None,
        };
        for line in block
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
        {
            let (key, value) = line.split_once(':').expect("key: value line");
            let value = value.trim();
            match key {
                "name" => v.name = value.to_string(),
                "hex" => v.hex = unhex(value),
                "max_frame" => v.max_frame = Some(value.parse().expect("max_frame")),
                "mode" => v.decode_only = value == "decode",
                "fields" => v.fields = Some(value.to_string()),
                "error" => v.error = Some(value.to_string()),
                other => panic!("unknown key {other}"),
            }
        }
        if !v.name.is_empty() {
            out.push(v);
        }
    }
    out
}

/// Builds the expected frame from the vector's fields, without the decoder.
/// Returns the frame and the kind-specific extras (credit, close code, close reason).
fn frame_of(fields: &str) -> (Frame, Vec<(String, String)>) {
    let mut f = Frame::data(Channel::Control, 1, 0, Ext::NONE, Vec::new());
    let mut extras = Vec::new();
    for kv in fields.split(' ') {
        let (k, v) = kv.split_once('=').expect("key=value");
        match k {
            "version" => assert_eq!(v, "1"),
            "chan" => {
                f.chan = *Channel::ALL
                    .iter()
                    .find(|c| c.name() == v)
                    .expect("channel")
            }
            "kind" => {
                f.kind = (1..=8)
                    .filter_map(Kind::from_u8)
                    .find(|k| k.name() == v)
                    .expect("kind")
            }
            "seq" => f.seq = v.parse().expect("seq"),
            "sent_us" => f.sent_us = v.parse().expect("sent_us"),
            "payload" => f.payload = unhex(v),
            "t_detected_us" => f.ext.t_detected_us = Some(v.parse().expect("t_detected_us")),
            "t_read_us" => f.ext.t_read_us = Some(v.parse().expect("t_read_us")),
            _ => extras.push((k.to_string(), v.to_string())),
        }
    }
    (f, extras)
}

#[test]
fn golden_vectors_both_ways() {
    let vectors = parse();
    assert!(vectors.len() >= 50, "only {} vectors", vectors.len());
    for v in &vectors {
        let limits = Limits::new(v.max_frame.unwrap_or(1 << 20));
        let decoded = Frame::decode(&v.hex, &limits);
        match (&v.fields, &v.error) {
            (Some(fields), None) => {
                let (want, extras) = frame_of(fields);
                let got = decoded.unwrap_or_else(|e| panic!("{}: {e}", v.name));
                assert_eq!(got, want, "{}: decode", v.name);
                for (k, val) in extras {
                    match k.as_str() {
                        "credit" => {
                            assert_eq!(got.credit_bytes(), Some(val.parse().unwrap()), "{}", v.name)
                        }
                        "close_code" => assert_eq!(
                            got.close_code(),
                            Some(CloseCode(val.parse().unwrap())),
                            "{}",
                            v.name
                        ),
                        "close_reason" => assert_eq!(
                            got.close_reason().map(|s| s.as_bytes().to_vec()),
                            Some(unhex(&val)),
                            "{}",
                            v.name
                        ),
                        other => panic!("{}: unknown field {other}", v.name),
                    }
                }
                if !v.decode_only {
                    let bytes = want
                        .encode(&limits)
                        .unwrap_or_else(|e| panic!("{}: {e}", v.name));
                    assert_eq!(bytes, v.hex, "{}: encode", v.name);
                    assert_eq!(want.encoded_len(), v.hex.len(), "{}: encoded_len", v.name);
                }
            }
            (None, Some(error)) => {
                let e = decoded.expect_err(&v.name);
                assert_eq!(e.name(), error, "{}: {e}", v.name);
            }
            _ => panic!("{}: needs exactly one of fields / error", v.name),
        }
    }
}

#[test]
fn every_error_vector_kind_is_covered() {
    let names: Vec<String> = parse().into_iter().filter_map(|v| v.error).collect();
    for want in [
        "truncated",
        "version",
        "oversize",
        "channel",
        "kind",
        "flags",
        "ext",
        "trailing",
        "field",
    ] {
        assert!(names.iter().any(|n| n == want), "no vector for {want}");
    }
}
