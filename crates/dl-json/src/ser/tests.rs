use super::*;
use crate::{json, parse};
use alloc::string::ToString;
use alloc::vec::Vec;

/// `(f64 bits, JSON.stringify(x))` printed by `testdata/float_vectors.js` under Node 26.
const JS_FLOATS: &[(u64, &str)] = &[
    (0x3fb999999999999a, "0.1"),
    (0x3fe0000000000000, "0.5"),
    (0x3ff8000000000000, "1.5"),
    (0xbff8000000000000, "-1.5"),
    (0x4059000000000000, "100"),
    (0x405edd2f1a9fbe77, "123.456"),
    (0xc05edd2f1a9fbe77, "-123.456"),
    (0x3fd3333333333334, "0.30000000000000004"),
    (0x3fd5555555555555, "0.3333333333333333"),
    (0x3fe5555555555555, "0.6666666666666666"),
    (0x4011666666666666, "4.35"),
    (0x4415af1d78b58c40, "100000000000000000000"),
    (0x444b1ae4d6e2ef50, "1e+21"),
    (0x4454542ba12a337c, "1.5e+21"),
    (0x441ac53a7e04bcda, "123456789012345680000"),
    (0x4340000000000000, "9007199254740992"),
    (0x43f0000000000000, "18446744073709552000"),
    (0x3eb0c6f7a0b5ed8d, "0.000001"),
    (0x3e7ad7f29abcaf48, "1e-7"),
    (0x3e8421f5f40d8376, "1.5e-7"),
    (0x3eb4b3fd5942cd96, "0.000001234"),
    (0x3e501b2b29a4692b, "1.5e-8"),
    (0x3ee4f8b588e368f1, "0.00001"),
    (0x0000000000000001, "5e-324"),
    (0x0010000000000000, "2.2250738585072014e-308"),
    (0x7fefffffffffffff, "1.7976931348623157e+308"),
    (0x54b249ad2594c37d, "1e+100"),
    (0xab2bff2ee48e0530, "-1e-100"),
    (0x7fe1ccf385ebc8a0, "1e+308"),
    (0x3c36b082c2148b8e, "1.23e-18"),
    (0x3f201f31f46ed246, "0.000123"),
    (0x4132d68720000000, "1234567.125"),
    (0x41b1de784a000000, "299792458"),
    (0x44dfe185ca57c517, "6.02214076e+23"),
    (0x3c07a4da290c1653, "1.602176634e-19"),
    (0x400921fb54442d18, "3.141592653589793"),
    (0x4005bf0a8b145769, "2.718281828459045"),
    (0x4341c37937e08000, "10000000000000000"),
    (0x43e56a95319d63e1, "12345678901234567000"),
];

fn fmt_f64(f: f64) -> String {
    let mut s = String::new();
    write_f64(&mut s, f).unwrap();
    s
}

#[test]
fn floats_match_json_stringify() {
    for &(bits, js) in JS_FLOATS {
        let f = f64::from_bits(bits);
        // Documented differences: no `+` in positive exponents, and integral floats whose
        // zero-padded digits would read back as a different u64 use exponent form.
        let expected = match js {
            "12345678901234567000" => "1.2345678901234567e19".to_string(),
            _ => js.replace("e+", "e"),
        };
        assert_eq!(fmt_f64(f), expected, "{f:e}");
        // The output parses back to the same float (or an equal integer).
        assert_eq!(parse(&expected).unwrap(), f, "{expected}");
        assert_eq!(expected.parse::<f64>().unwrap().to_bits(), bits);
    }
    assert_eq!(fmt_f64(-0.0), "-0");
    assert_eq!(fmt_f64(0.0), "0");
}

#[test]
fn floats_round_trip_shortest() {
    // Deterministic sample over the whole bit space (xorshift), all finite values.
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..20_000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let f = f64::from_bits(x);
        if !f.is_finite() {
            continue;
        }
        let s = fmt_f64(f);
        assert_eq!(s.parse::<f64>().unwrap().to_bits(), f.to_bits(), "{s}");
        let v = parse(&s).unwrap();
        assert_eq!(v, f, "{s}");
    }
}

#[test]
fn integers() {
    for n in [0u64, 7, 10, 99, 100, 101, 12345, u32::MAX as u64, u64::MAX] {
        assert_eq!(to_string(&Value::from(n)), n.to_string());
    }
    for n in [-1i64, -10, -99, -100, i64::MIN, i64::MIN + 1] {
        assert_eq!(to_string(&Value::from(n)), n.to_string());
    }
}

#[test]
fn string_escapes() {
    // Same as JSON.stringify("\0\x1f\b\f\n\r\t\"\\/\x7f"), plus U+2028/U+2029 escaped.
    let s = "\u{0}\u{1f}\u{8}\u{c}\n\r\t\"\\/\u{7f}";
    let js = r#""\u0000\u001f\b\f\n\r\t\"\\/"#.to_string() + "\u{7f}\"";
    assert_eq!(to_string(&Value::from(s)), js);
    assert_eq!(
        to_string(&Value::from("a\u{2028}b\u{2029}c")),
        r#""a\u2028b\u2029c""#
    );
    // Other characters with the same 0xE2 lead byte are kept as UTF-8.
    assert_eq!(
        to_string(&Value::from("“Đà Nẵng” – €")),
        "\"“Đà Nẵng” – €\""
    );
    assert_eq!(to_string(&Value::from("😀")), "\"😀\"");
    // Long runs exercise the 8-byte scanner, with specials at every offset.
    for pos in 0..20 {
        let mut s: String = "abcdefghijklmnopqrst".into();
        s.replace_range(pos..pos + 1, "\"");
        let out = to_string(&Value::from(s.as_str()));
        assert_eq!(out.matches("\\\"").count(), 1);
        assert_eq!(parse(&out).unwrap(), s.as_str());
    }
}

#[test]
fn compact_and_pretty() {
    let v = json!({
        "id": "INV-2024-001",
        "lines": [{"sku": "A", "qty": 2}, {"sku": "B", "qty": 1}],
        "empty_a": [],
        "empty_o": {},
        "paid": false,
        "note": null
    });
    assert_eq!(
        to_string(&v),
        r#"{"id":"INV-2024-001","lines":[{"sku":"A","qty":2},{"sku":"B","qty":1}],"empty_a":[],"empty_o":{},"paid":false,"note":null}"#
    );
    // Byte-for-byte what JSON.stringify(v, null, 2) prints.
    let pretty = r#"{
  "id": "INV-2024-001",
  "lines": [
    {
      "sku": "A",
      "qty": 2
    },
    {
      "sku": "B",
      "qty": 1
    }
  ],
  "empty_a": [],
  "empty_o": {},
  "paid": false,
  "note": null
}"#;
    assert_eq!(to_string_pretty(&v), pretty);
    assert_eq!(
        to_string_pretty_with(&json!([1, [2]]), "\t"),
        "[\n\t1,\n\t[\n\t\t2\n\t]\n]"
    );
    assert_eq!(to_string_pretty(&json!(5)), "5");
    let mut buf = String::from("x=");
    write_to(&json!([true]), &mut buf);
    write_pretty_to(&json!([]), &mut buf, "  ");
    assert_eq!(buf, "x=[true][]");
    assert_eq!(v.to_string(), to_string(&v));
}

#[test]
fn raw_numbers_verbatim() {
    let opts = crate::ParseOptions {
        keep_raw_numbers: true,
        ..Default::default()
    };
    let src = r#"[10.50,1E+2,-0.0,123456789012345678901234567890,1e400]"#;
    let v = crate::parse_with(src, opts).unwrap();
    assert_eq!(to_string(&v), src);
    let nums: Vec<_> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.to_string())
        .collect();
    assert_eq!(nums[0], "10.50");
}

#[test]
fn stack_buf_overflow_is_an_error() {
    let mut b = StackBuf::new();
    assert!(b.write_str(&"x".repeat(64)).is_ok());
    assert!(b.write_str("y").is_err());
    assert_eq!(b.as_str().len(), 64);
}
