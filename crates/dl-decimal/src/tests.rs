//! Crate-level tests: Python-generated reference vectors, parsing and formatting tables,
//! boundaries, and randomized invariants.

use crate::{Decimal, Error, RoundingMode};
use dl_rand::Rng;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::format;
use std::hash::{Hash, Hasher};
use std::string::{String, ToString};
use std::vec::Vec;

use RoundingMode::{Ceiling, Down, Floor, HalfDown, HalfEven, HalfUp, Up};

fn d(s: &str) -> Decimal {
    s.parse().unwrap_or_else(|e| panic!("parse {s:?}: {e}"))
}

fn hash_of(x: Decimal) -> u64 {
    let mut h = DefaultHasher::new();
    x.hash(&mut h);
    h.finish()
}

// ---------------------------------------------------------------------------------
// Reference vectors (testdata/gen_vectors.py, Python `decimal` + Fraction cross-check)
// ---------------------------------------------------------------------------------

const VECTORS: &str = include_str!("../testdata/vectors.txt");

fn mode(s: &str) -> RoundingMode {
    match s {
        "he" => HalfEven,
        "hu" => HalfUp,
        "hd" => HalfDown,
        "up" => Up,
        "dn" => Down,
        "ce" => Ceiling,
        "fl" => Floor,
        _ => panic!("unknown mode {s}"),
    }
}

fn show_opt(r: Option<Decimal>) -> String {
    r.map_or_else(|| "none".into(), |v| v.to_string())
}

fn show_res(r: Result<Decimal, Error>) -> String {
    match r {
        Ok(v) => v.to_string(),
        Err(Error::Overflow) => "overflow".into(),
        Err(Error::DivisionByZero) => "divzero".into(),
        Err(Error::ScaleTooLarge) => "scale".into(),
        Err(e) => format!("{e:?}"),
    }
}

/// Maps the vector file's failure words onto `checked_*` results.
fn expect_opt(exp: &str) -> &str {
    if exp == "overflow" || exp == "divzero" {
        "none"
    } else {
        exp
    }
}

#[test]
fn reference_vectors() {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut failures = Vec::new();
    for (lineno, line) in VECTORS.lines().enumerate() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let t: Vec<&str> = line.split(' ').collect();
        let op = t[0];
        *counts.entry(op).or_default() += 1;
        let (got, want) = match op {
            "add" | "sub" | "mul" | "div" | "rem" => {
                let (a, b) = (d(t[1]), d(t[2]));
                let r = match op {
                    "add" => a.checked_add(b),
                    "sub" => a.checked_sub(b),
                    "mul" => a.checked_mul(b),
                    "div" => a.checked_div(b),
                    _ => a.checked_rem(b),
                };
                // Operators must agree with the checked forms.
                if let Some(v) = r {
                    let o = match op {
                        "add" => a + b,
                        "sub" => a - b,
                        "mul" => a * b,
                        "div" => a / b,
                        _ => a % b,
                    };
                    assert_eq!(o.to_string(), v.to_string(), "operator {line}");
                }
                if op == "add" || op == "mul" {
                    let swapped = if op == "add" {
                        b.checked_add(a)
                    } else {
                        b.checked_mul(a)
                    };
                    assert_eq!(show_opt(swapped), show_opt(r), "commutativity {line}");
                }
                (show_opt(r), expect_opt(t[3]).to_string())
            }
            "round" => {
                let r = d(t[1]).round_dp(t[2].parse().unwrap(), mode(t[3]));
                (r.to_string(), t[4].to_string())
            }
            "divr" => {
                let r = d(t[1]).div_round(d(t[2]), t[3].parse().unwrap(), mode(t[4]));
                (show_res(r), t[5].to_string())
            }
            "mult" => (
                show_res(d(t[1]).round_to_multiple(d(t[2]), mode(t[3]))),
                t[4].to_string(),
            ),
            "pct" => (show_res(d(t[1]).percent_of(d(t[2]))), t[3].to_string()),
            "pow" => (
                show_opt(d(t[1]).checked_pow(t[2].parse().unwrap())),
                expect_opt(t[3]).to_string(),
            ),
            "cmp" => {
                let (a, b) = (d(t[1]), d(t[2]));
                let c = a.cmp(&b) as i32;
                assert_eq!(a == b, c == 0, "{line}");
                assert_eq!(a.partial_cmp(&b), Some(a.cmp(&b)), "{line}");
                assert_eq!(b.cmp(&a) as i32, -c, "{line}");
                if a == b {
                    assert_eq!(hash_of(a), hash_of(b), "hash {line}");
                }
                (c.to_string(), t[3].to_string())
            }
            "ps" => (show_res(t[1].parse::<Decimal>()), t[2].to_string()),
            "pr" => (
                show_res(Decimal::parse_rounded(t[1], mode(t[2]))),
                t[3].to_string(),
            ),
            "f2d" => {
                let f = f64::from_bits(u64::from_str_radix(t[1], 16).unwrap());
                (show_res(Decimal::try_from(f)), t[2].to_string())
            }
            "d2f" => {
                let bits = d(t[1]).to_f64().to_bits();
                (format!("{bits:016x}"), t[2].to_string())
            }
            _ => panic!("unknown op in line {}: {line}", lineno + 1),
        };
        if got != want {
            failures.push(format!("line {}: {line}\n    got {got}", lineno + 1));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    for (op, min) in [
        ("add", 700),
        ("sub", 700),
        ("mul", 700),
        ("div", 700),
        ("rem", 700),
        ("round", 1000),
        ("divr", 500),
        ("mult", 400),
        ("pct", 250),
        ("pow", 200),
        ("cmp", 400),
        ("ps", 600),
        ("pr", 600),
        ("f2d", 400),
        ("d2f", 300),
    ] {
        assert!(
            counts.get(op).copied().unwrap_or(0) >= min,
            "too few {op} vectors: {counts:?}"
        );
    }
}

// ---------------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------------

#[test]
fn parse_valid() {
    // (input, Display of the result, mantissa, scale)
    let cases: &[(&str, &str, i128, u32)] = &[
        ("0", "0", 0, 0),
        ("-0", "0", 0, 0),
        ("+3", "3", 3, 0),
        ("1.50", "1.50", 150, 2),
        ("-0.5", "-0.5", -5, 1),
        (".5", "0.5", 5, 1),
        ("5.", "5", 5, 0),
        ("+.25", "0.25", 25, 2),
        ("007.10", "7.10", 710, 2),
        ("0.000", "0.000", 0, 3),
        ("1.5e3", "1500", 1500, 0),
        ("2E-2", "0.02", 2, 2),
        ("1.5E+1", "15", 15, 0),
        ("12.345e1", "123.45", 12345, 2),
        ("-5.e-3", "-0.005", -5, 3),
        ("0e999999999999999999999", "0", 0, 0),
        ("0e-28", "0.0000000000000000000000000000", 0, 28),
        ("0.0e-27", "0.0000000000000000000000000000", 0, 28),
        ("1e-28", "0.0000000000000000000000000001", 1, 28),
        ("10e-28", "0.0000000000000000000000000010", 10, 28),
        (
            "9e37",
            "90000000000000000000000000000000000000",
            90_000_000_000_000_000_000_000_000_000_000_000_000,
            0,
        ),
        ("1.50e1", "15.0", 150, 1),
        ("-000.000", "0.000", 0, 3),
        ("1E0", "1", 1, 0),
        ("1e-0", "1", 1, 0),
        ("1e+00000000000000000000000000000000000", "1", 1, 0),
        (
            "99999999999999999999999999999999999999",
            "99999999999999999999999999999999999999",
            99_999_999_999_999_999_999_999_999_999_999_999_999,
            0,
        ),
        (
            "-9999999999.9999999999999999999999999999",
            "-9999999999.9999999999999999999999999999",
            -99_999_999_999_999_999_999_999_999_999_999_999_999,
            28,
        ),
        ("0000000000000000000000000000000000000000000001", "1", 1, 0),
        (
            "0.0000000000000000000000000000",
            "0.0000000000000000000000000000",
            0,
            28,
        ),
        (
            "1.0000000000000000000000000000",
            "1.0000000000000000000000000000",
            10_000_000_000_000_000_000_000_000_000,
            28,
        ),
        (
            "1234567890.1234567890123456789012345678",
            "1234567890.1234567890123456789012345678",
            12_345_678_901_234_567_890_123_456_789_012_345_678,
            28,
        ),
        (
            "1e37",
            "10000000000000000000000000000000000000",
            10_000_000_000_000_000_000_000_000_000_000_000_000,
            0,
        ),
    ];
    for &(s, shown, m, sc) in cases {
        let x = d(s);
        assert_eq!(x.to_string(), shown, "{s}");
        assert_eq!((x.mantissa(), x.scale()), (m, sc), "{s}");
    }
}

#[test]
fn parse_invalid() {
    let cases: &[(&str, Error)] = &[
        ("", Error::InvalidFormat { offset: 0 }),
        ("-", Error::InvalidFormat { offset: 1 }),
        ("+", Error::InvalidFormat { offset: 1 }),
        (".", Error::InvalidFormat { offset: 1 }),
        ("-.", Error::InvalidFormat { offset: 2 }),
        ("abc", Error::InvalidFormat { offset: 0 }),
        (" 1", Error::InvalidFormat { offset: 0 }),
        ("1 ", Error::InvalidFormat { offset: 1 }),
        ("1,000", Error::InvalidFormat { offset: 1 }),
        ("1_000", Error::InvalidFormat { offset: 1 }),
        ("1.2.3", Error::InvalidFormat { offset: 3 }),
        ("--1", Error::InvalidFormat { offset: 1 }),
        ("+-1", Error::InvalidFormat { offset: 1 }),
        ("1e", Error::InvalidFormat { offset: 2 }),
        ("1e+", Error::InvalidFormat { offset: 3 }),
        ("1e5x", Error::InvalidFormat { offset: 3 }),
        ("1e1.5", Error::InvalidFormat { offset: 3 }),
        (".e1", Error::InvalidFormat { offset: 1 }),
        ("e1", Error::InvalidFormat { offset: 0 }),
        ("NaN", Error::InvalidFormat { offset: 0 }),
        ("inf", Error::InvalidFormat { offset: 0 }),
        ("١", Error::InvalidFormat { offset: 0 }),
        ("12\u{a0}", Error::InvalidFormat { offset: 2 }),
        ("0x10", Error::InvalidFormat { offset: 1 }),
        ("100000000000000000000000000000000000000", Error::Overflow),
        ("1e38", Error::Overflow),
        ("123456789012345678901234567890123456789", Error::Overflow),
        ("1234567890123456789012345678901234567.89", Error::Overflow),
        ("0.00000000000000000000000000001", Error::ScaleTooLarge),
        ("1e-29", Error::ScaleTooLarge),
        ("1.00000000000000000000000000001", Error::ScaleTooLarge),
        ("1e99999999999999999999999999", Error::Overflow),
        ("1e-99999999999999999999999999", Error::ScaleTooLarge),
        // The written scale is never changed, so zeros count too.
        ("1.00000000000000000000000000000", Error::ScaleTooLarge),
        ("0.00000000000000000000000000000", Error::ScaleTooLarge),
        ("0e-29", Error::ScaleTooLarge),
        ("10e-30", Error::ScaleTooLarge),
        ("99999999999999999999999999999999999999.0", Error::Overflow),
        ("12345678901.1234567890123456789012345678", Error::Overflow),
        (
            "1234567890123456789012345678901234567890e-10",
            Error::Overflow,
        ),
        ("10e37", Error::Overflow),
        ("10e-29", Error::ScaleTooLarge),
        ("-1e38", Error::Overflow),
        ("1.5e", Error::InvalidFormat { offset: 4 }),
        ("1.5e-", Error::InvalidFormat { offset: 5 }),
        ("1.5ee1", Error::InvalidFormat { offset: 4 }),
        ("1.5e1e1", Error::InvalidFormat { offset: 5 }),
        ("1e--1", Error::InvalidFormat { offset: 3 }),
        ("1.-5", Error::InvalidFormat { offset: 2 }),
        ("5..", Error::InvalidFormat { offset: 2 }),
        ("\u{2212}1", Error::InvalidFormat { offset: 0 }),
        ("1\0", Error::InvalidFormat { offset: 1 }),
        ("Infinity", Error::InvalidFormat { offset: 0 }),
        ("+inf", Error::InvalidFormat { offset: 1 }),
        ("1.5 ", Error::InvalidFormat { offset: 3 }),
        ("\t1", Error::InvalidFormat { offset: 0 }),
    ];
    for (s, e) in cases {
        assert_eq!(s.parse::<Decimal>(), Err(*e), "{s:?}");
    }
}

#[test]
fn parse_rounded_cases() {
    let r = |s: &str, m| Decimal::parse_rounded(s, m).map(|x| x.to_string());
    assert_eq!(
        r("0.00000000000000000000000000005", HalfEven).unwrap(),
        "0.0000000000000000000000000000"
    );
    assert_eq!(
        r("0.00000000000000000000000000015", HalfEven).unwrap(),
        "0.0000000000000000000000000002"
    );
    assert_eq!(
        r("0.00000000000000000000000000005", HalfUp).unwrap(),
        "0.0000000000000000000000000001"
    );
    assert_eq!(r("1e-99999", Up).unwrap(), "0.0000000000000000000000000001");
    assert_eq!(
        r("-1e-99999", Floor).unwrap(),
        "-0.0000000000000000000000000001"
    );
    assert_eq!(
        r("-1e-99999", Ceiling).unwrap(),
        "0.0000000000000000000000000000"
    );
    assert_eq!(
        r("123456789012345678901234567890123456789.4", HalfUp),
        Err(Error::Overflow)
    );
    assert_eq!(
        r("99999999999999999999999999999999999999.5", HalfUp),
        Err(Error::Overflow)
    );
    assert_eq!(
        r("9999999999999999999999999999999999999.95", HalfUp).unwrap(),
        "10000000000000000000000000000000000000"
    );
    assert_eq!(r("1.5", Down).unwrap(), "1.5");
    assert_eq!(r("x", Down), Err(Error::InvalidFormat { offset: 0 }));
}

// ---------------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------------

#[test]
fn display_and_flags() {
    assert_eq!(d("1.50").to_string(), "1.50");
    assert_eq!(format!("{:?}", d("-0.050")), "-0.050");
    assert_eq!(
        Decimal::new(5, 28).unwrap().to_string(),
        "0.0000000000000000000000000005"
    );
    assert_eq!(
        Decimal::MAX.to_string(),
        "99999999999999999999999999999999999999"
    );
    assert_eq!(
        Decimal::MIN.to_string(),
        "-99999999999999999999999999999999999999"
    );
    assert_eq!(
        Decimal::new(10_000_000_000_000_000_000, 0)
            .unwrap()
            .to_string(),
        "10000000000000000000"
    );
    assert_eq!(
        Decimal::new(10_000_000_000_000_000_001, 19)
            .unwrap()
            .to_string(),
        "1.0000000000000000001"
    );
    // Precision rounds half-up, or pads.
    assert_eq!(format!("{:.2}", d("2.345")), "2.35");
    assert_eq!(format!("{:.2}", d("-2.345")), "-2.35");
    assert_eq!(format!("{:.2}", d("2.344")), "2.34");
    assert_eq!(format!("{:.0}", d("2.5")), "3");
    assert_eq!(format!("{:.0}", d("-0.4")), "0");
    assert_eq!(format!("{:.2}", d("-0.004")), "0.00");
    assert_eq!(format!("{:.4}", d("1.5")), "1.5000");
    assert_eq!(format!("{:.2}", d("7")), "7.00");
    assert_eq!(
        format!("{:.30}", d("1")),
        "1.000000000000000000000000000000"
    );
    // Sign, width, fill, zero padding.
    assert_eq!(format!("{:+}", d("1.5")), "+1.5");
    assert_eq!(format!("{:+}", d("0")), "+0");
    assert_eq!(format!("{:+}", d("-1.5")), "-1.5");
    assert_eq!(format!("{:>8}", d("1.5")), "     1.5");
    assert_eq!(format!("{:<8}|", d("-1.5")), "-1.5    |");
    assert_eq!(format!("{:08.2}", d("-1.5")), "-0001.50");
    assert_eq!(format!("{:*^9}", d("1.25")), "**1.25***");
}

#[test]
fn grouped() {
    let x = d("1234567.505");
    assert_eq!(x.to_string_grouped(2, '.', ',', HalfUp), "1.234.567,51");
    assert_eq!(x.to_string_grouped(2, ',', '.', HalfUp), "1,234,567.51");
    assert_eq!(x.to_string_grouped(2, ',', '.', HalfEven), "1,234,567.50");
    assert_eq!(x.to_string_grouped(2, ',', '.', Down), "1,234,567.50");
    assert_eq!(x.to_string_grouped(5, ' ', ',', HalfUp), "1 234 567,50500");
    assert_eq!(x.to_string_grouped(0, '.', ',', HalfUp), "1.234.568");
    assert_eq!(
        d("-1234567.5").to_string_grouped(0, '.', ',', HalfUp),
        "-1.234.568"
    );
    assert_eq!(d("-0.004").to_string_grouped(2, ',', '.', HalfUp), "0.00");
    assert_eq!(
        d("999.995").to_string_grouped(2, ',', '.', HalfUp),
        "1,000.00"
    );
    assert_eq!(d("0").to_string_grouped(0, ',', '.', HalfUp), "0");
    assert_eq!(d("123").to_string_grouped(0, ',', '.', HalfUp), "123");
    assert_eq!(
        d("123456").to_string_grouped(1, ',', '.', HalfUp),
        "123,456.0"
    );
    assert_eq!(
        Decimal::MIN.to_string_grouped(0, ',', '.', HalfUp),
        "-99,999,999,999,999,999,999,999,999,999,999,999,999"
    );
    assert_eq!(
        d("1000000").to_string_grouped(0, '\u{202f}', ',', HalfUp),
        "1\u{202f}000\u{202f}000"
    );
}

// ---------------------------------------------------------------------------------
// Rounding
// ---------------------------------------------------------------------------------

#[test]
fn rounding_mode_table() {
    // Matches the table in the RoundingMode docs (Python decimal quantize, see vectors).
    let values = ["2.5", "1.5", "-2.5", "2.4", "-2.6"];
    let table: &[(RoundingMode, [&str; 5])] = &[
        (HalfEven, ["2", "2", "-2", "2", "-3"]),
        (HalfUp, ["3", "2", "-3", "2", "-3"]),
        (HalfDown, ["2", "1", "-2", "2", "-3"]),
        (Up, ["3", "2", "-3", "3", "-3"]),
        (Down, ["2", "1", "-2", "2", "-2"]),
        (Ceiling, ["3", "2", "-2", "3", "-2"]),
        (Floor, ["2", "1", "-3", "2", "-3"]),
    ];
    for (m, expected) in table {
        for (v, e) in values.iter().zip(expected) {
            assert_eq!(d(v).round_dp(0, *m).to_string(), *e, "{v} {m:?}");
        }
    }
}

#[test]
fn rounding_helpers() {
    assert_eq!(d("2.345").round(2).to_string(), "2.35");
    assert_eq!(d("-2.345").round(2).to_string(), "-2.35");
    assert_eq!(d("2.345").round(5).to_string(), "2.345");
    assert_eq!(d("2.999").trunc(2).to_string(), "2.99");
    assert_eq!(d("-2.999").trunc(0).to_string(), "-2");
    assert_eq!(d("-2.1").floor().to_string(), "-3");
    assert_eq!(d("-2.9").ceil().to_string(), "-2");
    assert_eq!(d("2.1").ceil().to_string(), "3");
    assert_eq!(d("2.0").floor().to_string(), "2");
    assert_eq!(d("-7.25").integer_part().to_string(), "-7");
    assert_eq!(d("-7.25").fract().to_string(), "-0.25");
    assert_eq!(d("7").fract().to_string(), "0");
    assert_eq!(d("1.5").with_scale(3, HalfUp).unwrap().to_string(), "1.500");
    assert_eq!(
        d("1.555").with_scale(2, HalfEven).unwrap().to_string(),
        "1.56"
    );
    assert_eq!(d("1.5").with_scale(29, HalfUp), Err(Error::ScaleTooLarge));
    assert_eq!(Decimal::MAX.with_scale(1, HalfUp), Err(Error::Overflow));
    assert_eq!(d("1.5").rescale(2, HalfUp).unwrap().to_string(), "1.50");
    assert_eq!(d("1.005").rescale(2, HalfUp).unwrap().to_string(), "1.01");
    assert_eq!(d("1.005").rescale(2, HalfEven).unwrap().to_string(), "1.00");
    assert_eq!(d("-1.005").rescale(2, Floor).unwrap().to_string(), "-1.01");
    assert_eq!(d("7").rescale(28, Down).unwrap().scale(), 28);
    assert_eq!(d("1.5").rescale(29, HalfUp), Err(Error::ScaleTooLarge));
    assert_eq!(
        Decimal::new(1, 28).unwrap().rescale(0, Up).unwrap(),
        Decimal::ONE
    );
    // Rounding at the largest magnitude never overflows.
    let m = Decimal::new(99_999_999_999_999_999_999_999_999_999_999_999_999, 1).unwrap();
    assert_eq!(
        m.round(0).to_string(),
        "10000000000000000000000000000000000000"
    );
    assert_eq!(Decimal::MIN.round_dp(0, Up), Decimal::MIN);
}

#[test]
fn round_to_multiple_cash() {
    let vnd = |s: &str, step: i64, m| {
        d(s).round_to_multiple(Decimal::from(step), m)
            .unwrap()
            .to_string()
    };
    assert_eq!(vnd("123250", 500, HalfUp), "123500");
    assert_eq!(vnd("123249", 500, HalfUp), "123000");
    assert_eq!(vnd("123250", 500, HalfEven), "123000");
    assert_eq!(vnd("123001", 1000, Up), "124000");
    assert_eq!(vnd("123999", 1000, Down), "123000");
    assert_eq!(vnd("-123250", 500, HalfUp), "-123500");
    assert_eq!(vnd("-123250", 500, Ceiling), "-123000");
    assert_eq!(vnd("-123250", -500, Floor), "-123500");
    assert_eq!(vnd("0", 500, Up), "0");
    let p = d("1.234").round_to_multiple(d("0.05"), HalfUp).unwrap();
    assert_eq!(p.to_string(), "1.25");
    assert_eq!(
        d("1").round_to_multiple(Decimal::ZERO, HalfUp),
        Err(Error::DivisionByZero)
    );
    assert_eq!(
        Decimal::MAX.round_to_multiple(d("0.5"), HalfUp).unwrap(),
        Decimal::MAX
    );
    // 10^38 - 1 is divisible by 3 but not by 7.
    assert_eq!(Decimal::MAX.round_to_multiple(d("3"), Up), Ok(Decimal::MAX));
    assert_eq!(
        Decimal::MAX.round_to_multiple(d("7"), Up),
        Err(Error::Overflow)
    );
}

// ---------------------------------------------------------------------------------
// Arithmetic boundaries and operators
// ---------------------------------------------------------------------------------

#[test]
fn boundaries() {
    let one = Decimal::ONE;
    let tiny = Decimal::new(1, 28).unwrap();
    assert_eq!(Decimal::MAX.checked_add(one), None);
    assert_eq!(Decimal::MIN.checked_sub(one), None);
    assert_eq!(Decimal::MAX.checked_add(Decimal::MIN), Some(Decimal::ZERO));
    assert_eq!(Decimal::MAX.checked_mul(one), Some(Decimal::MAX));
    assert_eq!(Decimal::MAX.checked_mul(Decimal::from(2)), None);
    assert_eq!(
        Decimal::MAX.checked_mul(d("0.1")).unwrap().to_string(),
        "9999999999999999999999999999999999999.9"
    );
    assert_eq!(Decimal::MAX.checked_div(d("0.1")), None);
    assert_eq!(Decimal::MAX.checked_div(Decimal::MAX), Some(one));
    assert_eq!(one.checked_div(Decimal::ZERO), None);
    assert_eq!(one.checked_rem(Decimal::ZERO), None);
    assert_eq!(
        tiny.checked_mul(tiny).unwrap().to_string(),
        "0.0000000000000000000000000000"
    );
    assert_eq!((tiny / Decimal::from(2)).to_string(), "0");
    assert_eq!(
        (Decimal::new(3, 28).unwrap() / Decimal::from(2)).to_string(),
        "0.0000000000000000000000000002"
    );
    // Sum carrying into a 39th digit reduces the scale.
    let x = Decimal::new(99_999_999_999_999_999_999_999_999_999_999_999_999, 28).unwrap();
    assert_eq!(
        (x + tiny).to_string(),
        "10000000000.000000000000000000000000000"
    );
    assert_eq!((-Decimal::MIN), Decimal::MAX);
    assert_eq!(Decimal::MIN.abs(), Decimal::MAX);
    // Division examples from the docs.
    assert_eq!(
        (one / Decimal::from(3)).to_string(),
        "0.3333333333333333333333333333"
    );
    assert_eq!(
        (Decimal::from(2) / Decimal::from(3)).to_string(),
        "0.6666666666666666666666666667"
    );
    assert_eq!((d("2.50") / one).to_string(), "2.5");
    assert_eq!((Decimal::TEN / d("0.5")).to_string(), "20");
    assert_eq!((Decimal::from(-7) % Decimal::from(3)).to_string(), "-1");
    assert_eq!((Decimal::from(7) % Decimal::from(-3)).to_string(), "1");
    assert_eq!((d("7.5") % Decimal::from(2)).to_string(), "1.5");
}

#[test]
#[allow(clippy::op_ref)] // the reference forms are under test
fn operators() {
    let mut x = d("10.5");
    x += d("0.25");
    assert_eq!(x.to_string(), "10.75");
    x -= Decimal::ONE;
    x *= 2;
    assert_eq!(x.to_string(), "19.50");
    x /= 4u32;
    assert_eq!(x.to_string(), "4.875");
    x %= Decimal::ONE;
    assert_eq!(x.to_string(), "0.875");
    let a = d("1.1");
    let b = d("2.2");
    assert_eq!(&a + &b, d("3.3"));
    assert_eq!(a + &b, d("3.3"));
    assert_eq!(&a * b, d("2.42"));
    assert_eq!(-&a, d("-1.1"));
    assert_eq!(a * 3i64 + 1u64 - 2usize, d("2.3"));
    assert_eq!(b / 2, a);
    let v = [d("0.1"), d("0.2"), d("0.3")];
    assert_eq!(v.iter().sum::<Decimal>(), d("0.6"));
    assert_eq!(v.into_iter().sum::<Decimal>().to_string(), "0.6");
    assert_eq!(v.iter().product::<Decimal>().to_string(), "0.006");
    assert_eq!(Decimal::checked_sum(v), Some(d("0.6")));
    assert_eq!(Decimal::checked_sum([Decimal::MAX, Decimal::ONE]), None);
    assert_eq!(Decimal::checked_sum([]), Some(Decimal::ZERO));
}

#[test]
#[should_panic(expected = "attempt to divide by zero")]
fn div_by_zero_panics() {
    let _ = Decimal::ONE / Decimal::ZERO;
}

#[test]
#[should_panic(expected = "attempt to add with overflow")]
fn add_overflow_panics() {
    let _ = Decimal::MAX + Decimal::ONE;
}

#[test]
#[should_panic(expected = "attempt to multiply with overflow")]
fn sum_product_overflow_panics() {
    let _: Decimal = [Decimal::MAX, Decimal::TEN].iter().product();
}

#[test]
#[should_panic(expected = "remainder with a divisor of zero")]
fn rem_by_zero_panics() {
    let _ = Decimal::ONE % Decimal::ZERO;
}

#[test]
fn div_round_examples() {
    let r = |a: &str, b: &str, dp, m| d(a).div_round(d(b), dp, m).map(|x| x.to_string());
    assert_eq!(r("100000", "3", 2, HalfUp).unwrap(), "33333.33");
    assert_eq!(r("100000", "6", 0, HalfUp).unwrap(), "16667");
    assert_eq!(r("10", "4", 0, HalfEven).unwrap(), "2");
    assert_eq!(r("10", "4", 0, HalfUp).unwrap(), "3");
    assert_eq!(r("-10", "4", 0, HalfUp).unwrap(), "-3");
    assert_eq!(r("1", "8", 4, Down).unwrap(), "0.1250");
    assert_eq!(r("0", "8", 2, Down).unwrap(), "0.00");
    assert_eq!(r("1", "0", 2, Down), Err(Error::DivisionByZero));
    assert_eq!(r("1", "3", 29, Down), Err(Error::ScaleTooLarge));
    assert_eq!(
        r("99999999999999999999999999999999999999", "0.1", 0, Down),
        Err(Error::Overflow)
    );
    assert_eq!(
        r("1", "0.0000000000000000000000000001", 28, Down),
        Err(Error::Overflow)
    );
    assert_eq!(
        r(
            "99999999999999999999999999999999999999",
            "0.0000000000000000000000000001",
            0,
            Down
        ),
        Err(Error::Overflow)
    );
}

#[test]
fn pow() {
    assert_eq!(d("1.1").checked_pow(2).unwrap().to_string(), "1.21");
    assert_eq!(d("-2").checked_pow(3).unwrap().to_string(), "-8");
    assert_eq!(d("0.5").checked_pow(0).unwrap().to_string(), "1");
    assert_eq!(
        Decimal::TEN.checked_pow(37).unwrap().to_string(),
        "10000000000000000000000000000000000000"
    );
    assert_eq!(Decimal::TEN.checked_pow(38), None);
    assert_eq!(
        d("0.1").checked_pow(28).unwrap().to_string(),
        "0.0000000000000000000000000001"
    );
    assert_eq!(
        d("0.1").checked_pow(29).unwrap().to_string(),
        "0.0000000000000000000000000000"
    );
}

// ---------------------------------------------------------------------------------
// Constructors, conversions, inspection
// ---------------------------------------------------------------------------------

#[test]
fn constructors() {
    const RATE: Decimal = Decimal::from_i128_scale(8, 2);
    assert_eq!(RATE.to_string(), "0.08");
    assert_eq!(Decimal::new(1250, 2).unwrap().to_string(), "12.50");
    assert_eq!(Decimal::new(1, 29), Err(Error::ScaleTooLarge));
    assert_eq!(
        Decimal::new(100_000_000_000_000_000_000_000_000_000_000_000_000, 0),
        Err(Error::Overflow)
    );
    assert_eq!(Decimal::from_parts(5, true, 1).unwrap().to_string(), "-0.5");
    assert_eq!(Decimal::from_parts(5, true, 29), Err(Error::ScaleTooLarge));
    assert_eq!(Decimal::try_from(i128::MAX), Err(Error::Overflow));
    assert_eq!(Decimal::try_from(u128::MAX), Err(Error::Overflow));
    assert_eq!(
        Decimal::try_from(-99_999_999_999_999_999_999_999_999_999_999_999_999i128),
        Ok(Decimal::MIN)
    );
    assert_eq!(
        Decimal::try_from(99_999_999_999_999_999_999_999_999_999_999_999_999u128),
        Ok(Decimal::MAX)
    );
    assert_eq!(Decimal::from(u64::MAX).to_string(), "18446744073709551615");
    assert_eq!(Decimal::from(i64::MIN).to_string(), "-9223372036854775808");
    assert_eq!(Decimal::from(-5i8), Decimal::from(-5i64));
    assert_eq!(Decimal::default(), Decimal::ZERO);
    assert_eq!(Decimal::ONE_HUNDRED, Decimal::from(100));
}

#[test]
#[should_panic(expected = "out of range")]
fn from_i128_scale_panics_at_runtime() {
    let scale = std::hint::black_box(29);
    let _ = Decimal::from_i128_scale(1, scale);
}

#[test]
fn inspection() {
    let x = d("-12.3400");
    assert_eq!((x.mantissa(), x.scale()), (-123400, 4));
    assert!(x.is_negative() && !x.is_positive() && !x.is_zero());
    assert_eq!(x.signum(), Decimal::from(-1));
    assert_eq!(d("0.00").signum(), Decimal::ZERO);
    assert_eq!(x.abs().to_string(), "12.3400");
    assert_eq!(x.normalize().to_string(), "-12.34");
    assert_eq!(d("100").normalize().to_string(), "100");
    assert_eq!(d("0.000").normalize().to_string(), "0");
    assert_eq!(x.digits(), 6);
    assert_eq!(Decimal::ZERO.digits(), 1);
    assert_eq!(Decimal::MAX.digits(), 38);
    assert_eq!(d("-12.99").to_i64(), Some(-12));
    assert_eq!(Decimal::MAX.to_i64(), None);
    assert_eq!(
        Decimal::MAX.to_i128(),
        Some(99_999_999_999_999_999_999_999_999_999_999_999_999)
    );
    assert_eq!(Decimal::MIN.to_i128(), Some(-Decimal::MAX.mantissa()));
    assert_eq!(d("-0.5").to_i128(), Some(0));
    assert_eq!(d("-9223372036854775808.9").to_i64(), Some(i64::MIN));
    assert_eq!(d("9223372036854775807.99").to_i64(), Some(i64::MAX));
    assert_eq!(d("9223372036854775808").to_i64(), None);
    assert_eq!(d("-9223372036854775809").to_i64(), None);
    assert_eq!(
        Decimal::new(-99_999_999_999_999_999_999_999_999_999_999_999_999, 28)
            .unwrap()
            .to_i64(),
        Some(-9_999_999_999)
    );
    assert_eq!(d("1.5").max(d("1.50")), d("1.5"));
    assert_eq!(d("3").clamp(d("0"), d("2.5")), d("2.5"));
    assert_eq!(d("-3").min(d("2")), d("-3"));
}

#[test]
fn floats() {
    let f = |v: f64| Decimal::try_from(v).map(|x| x.to_string());
    assert_eq!(f(0.1).unwrap(), "0.1");
    assert_eq!(f(0.1 + 0.2).unwrap(), "0.30000000000000004");
    assert_eq!(f(100.0).unwrap(), "100");
    assert_eq!(f(-0.0).unwrap(), "0");
    assert_eq!(f(1e20).unwrap(), "100000000000000000000");
    assert_eq!(f(19.99).unwrap(), "19.99");
    assert_eq!(f(1e-28).unwrap(), "0.0000000000000000000000000001");
    assert_eq!(f(1e-29), Err(Error::ScaleTooLarge));
    assert_eq!(f(1e38), Err(Error::Overflow));
    assert_eq!(f(f64::NAN), Err(Error::NotFinite));
    assert_eq!(f(f64::INFINITY), Err(Error::NotFinite));
    assert_eq!(f(f64::NEG_INFINITY), Err(Error::NotFinite));
    assert_eq!(Decimal::try_from(0.1f32).unwrap().to_string(), "0.1");
    assert_eq!(
        Decimal::try_from(16_777_217f32).unwrap().to_string(),
        "16777216"
    );
    assert_eq!(d("0.1").to_f64(), 0.1);
    assert_eq!(d("0.30000000000000004").to_f64(), 0.1 + 0.2);
    assert_eq!(Decimal::MAX.to_f64(), 1e38);
    assert_eq!(Decimal::MIN.to_f64(), -1e38);
    assert_eq!(d("-0.0000000000000000000000000001").to_f64(), -1e-28);
    // Halfway between two doubles (2^53 + 1) rounds to even.
    assert_eq!(d("9007199254740993").to_f64(), 9007199254740992.0);
    let mut rng = Rng::from_seed(77);
    for _ in 0..5_000 {
        let v = f64::from_bits(rng.next_u64());
        if let Ok(x) = Decimal::try_from(v) {
            // Exact shortest representation round-trips.
            assert_eq!(x.to_f64(), if v == 0.0 { 0.0 } else { v }, "{v:e}");
            assert_eq!(x.to_string().parse::<f64>().unwrap(), v);
        }
    }
}

#[test]
fn errors_display() {
    assert_eq!(
        Error::InvalidFormat { offset: 3 }.to_string(),
        "invalid decimal at offset 3"
    );
    assert_eq!(
        Error::Overflow.to_string(),
        "decimal overflow: more than 38 digits needed"
    );
    assert_eq!(Error::DivisionByZero.to_string(), "division by zero");
    assert_eq!(
        Error::ScaleTooLarge.to_string(),
        "more than 28 decimal places"
    );
    assert_eq!(Error::NotFinite.to_string(), "float is NaN or infinite");
    let e: &dyn core::error::Error = &Error::Overflow;
    assert!(e.source().is_none());
}

// ---------------------------------------------------------------------------------
// Money helpers
// ---------------------------------------------------------------------------------

#[test]
fn allocate_and_split() {
    let dv = |v: &[i64]| v.iter().map(|&x| Decimal::from(x)).collect::<Vec<_>>();
    assert_eq!(Decimal::from(100).split(3, 0).unwrap(), dv(&[34, 33, 33]));
    assert_eq!(
        d("100").split(3, 2).unwrap(),
        ["33.34", "33.33", "33.33"].map(d)
    );
    assert_eq!(d("-100").split(3, 0).unwrap(), dv(&[-34, -33, -33]));
    assert_eq!(
        d("0.05").split(3, 2).unwrap(),
        ["0.02", "0.02", "0.01"].map(d)
    );
    assert_eq!(d("1").split(1, 0).unwrap(), dv(&[1]));
    assert_eq!(d("1").split(0, 0), Err(Error::DivisionByZero));
    assert_eq!(d("1.005").split(2, 2), Err(Error::ScaleTooLarge));
    assert_eq!(d("1.000").split(2, 2).unwrap(), ["0.50", "0.50"].map(d));
    assert_eq!(Decimal::MAX.split(2, 1), Err(Error::Overflow));
    assert_eq!(d("1").split(2, 29), Err(Error::ScaleTooLarge));

    assert_eq!(
        Decimal::from(100).allocate(&[1, 1, 1], 0).unwrap(),
        dv(&[34, 33, 33])
    );
    assert_eq!(
        Decimal::from(1).allocate(&[30, 70], 0).unwrap(),
        dv(&[0, 1])
    );
    assert_eq!(
        Decimal::from(10).allocate(&[0, 1, 0], 0).unwrap(),
        dv(&[0, 10, 0])
    );
    assert_eq!(
        Decimal::from(5).allocate(&[1, 1, 1, 1], 0).unwrap(),
        dv(&[2, 1, 1, 1])
    );
    assert_eq!(
        d("1000000").allocate(&[1, 2, 3], 0).unwrap(),
        dv(&[166667, 333333, 500000])
    );
    assert_eq!(d("1").allocate::<u32>(&[], 0), Err(Error::DivisionByZero));
    assert_eq!(d("1").allocate(&[0, 0], 0), Err(Error::DivisionByZero));
    let parts = Decimal::MAX.allocate(&[u32::MAX, 1, 7], 0).unwrap();
    assert_eq!(Decimal::checked_sum(parts), Some(Decimal::MAX));
    let parts = Decimal::MIN
        .allocate(&[Decimal::MAX, Decimal::ONE], 0)
        .unwrap();
    assert_eq!(Decimal::checked_sum(parts), Some(Decimal::MIN));
    let parts = Decimal::MAX.allocate(&[d("1e37"); 7], 0).unwrap();
    assert_eq!(Decimal::checked_sum(parts), Some(Decimal::MAX));
    // Ratios whose sum does not fit in i128.
    assert_eq!(
        Decimal::MAX.allocate(&[Decimal::MAX; 7], 0),
        Err(Error::Overflow)
    );
    let parts = Decimal::MAX
        .allocate(&[d("0.0000000000000000000000000001"), d("1")], 0)
        .unwrap();
    assert_eq!(Decimal::checked_sum(parts), Some(Decimal::MAX));

    // Decimal weights: spreading an invoice discount over line amounts.
    let lines = ["120000", "45500", "9900"].map(d);
    assert_eq!(
        Decimal::from(50_000).allocate(&lines, 0).unwrap(),
        dv(&[34208, 12970, 2822])
    );
    assert_eq!(
        d("100").allocate(&[d("0.3"), d("0.70")], 2).unwrap(),
        ["30.00", "70.00"].map(d)
    );
    assert_eq!(
        d("10").allocate(&[d("1.5"), d("1.50"), d("0")], 0).unwrap(),
        dv(&[5, 5, 0])
    );
    // Negative amounts mirror positive ones.
    assert_eq!(
        d("-100").allocate(&[1, 1, 1], 0).unwrap(),
        dv(&[-34, -33, -33])
    );
    assert_eq!(
        d("-0.05").allocate(&[1, 1], 2).unwrap(),
        ["-0.03", "-0.02"].map(d)
    );
    // Negative ratios: shares 200 and -100 are exact; 10 × [2, -1] / 1 likewise.
    assert_eq!(d("100").allocate(&[2, -1], 0).unwrap(), dv(&[200, -100]));
    // 10 × [1, 1, -1] / 1 = [10, 10, -10].
    assert_eq!(
        d("10").allocate(&[1, 1, -1], 0).unwrap(),
        dv(&[10, 10, -10])
    );
    // 10 × [3, -1] / 2 = [15, -5]; 1 × [3, -1] / 2 = [1.5, -0.5] -> floors [1, -1],
    // one unit left, both fractions 1/2, the first gets it: [2, -1].
    assert_eq!(d("1").allocate(&[3, -1], 0).unwrap(), dv(&[2, -1]));
    assert_eq!(d("-1").allocate(&[3, -1], 0).unwrap(), dv(&[-2, 1]));
    assert_eq!(d("1").allocate(&[-1, -1], 0).unwrap(), dv(&[1, 0]));
    assert_eq!(d("1").allocate(&[1, -1], 0), Err(Error::DivisionByZero));
    assert_eq!(
        d("1").allocate(&[d("1e30"), d("0.000000001")], 0),
        Err(Error::Overflow)
    );
    assert_eq!(Decimal::MAX.allocate(&[2, -1], 0), Err(Error::Overflow));
    assert_eq!(d("1.5").allocate(&[1, 1], 0), Err(Error::ScaleTooLarge));
    assert_eq!(d("1.5").allocate(&[1, 1], 29), Err(Error::ScaleTooLarge));
    assert_eq!(
        d("0").allocate(&[1, 2], 2).unwrap(),
        ["0.00", "0.00"].map(d)
    );

    // Randomized: the parts always add up exactly and stay within one unit of the
    // exact proportional share.
    let mut rng = Rng::from_seed(2024);
    for _ in 0..2_000 {
        let dp = rng.range(0..=4u32);
        let units = i128::from(rng.next_u64() >> rng.range(0..64u32))
            * if rng.next_bool() { 1 } else { -1 };
        let total = Decimal::new(units, dp).unwrap();
        let n = rng.range(1..=12usize);
        let ratios: Vec<u32> = (0..n).map(|_| rng.range(0..=1000u32)).collect();
        let parts = total.split(n, dp).unwrap();
        assert_eq!(Decimal::checked_sum(parts.iter().copied()), Some(total));
        assert!(parts.iter().all(|p| p.scale() == dp));
        if ratios.iter().all(|&r| r == 0) {
            assert_eq!(total.allocate(&ratios, dp), Err(Error::DivisionByZero));
            continue;
        }
        let parts = total.allocate(&ratios, dp).unwrap();
        assert_eq!(
            Decimal::checked_sum(parts.iter().copied()),
            Some(total),
            "{total} {ratios:?}"
        );
        let rsum: u32 = ratios.iter().sum();
        let unit = Decimal::new(1, dp).unwrap();
        for (p, &r) in parts.iter().zip(&ratios) {
            let share = total
                .checked_mul(Decimal::from(r))
                .unwrap()
                .checked_div(Decimal::from(rsum))
                .unwrap();
            assert!((*p - share).abs() < unit, "{p} vs {share}");
            if r == 0 {
                assert!(p.is_zero());
            }
        }
        // The same ratios as decimals with another scale give the same parts.
        let scaled: Vec<Decimal> = ratios
            .iter()
            .map(|&r| Decimal::new(i128::from(r) * 25, 2).unwrap())
            .collect();
        assert_eq!(total.allocate(&scaled, dp).unwrap(), parts);
        // Mixed-sign decimal weights: exact sum, every part within one unit of its share.
        let weights: Vec<Decimal> = (0..n)
            .map(|_| {
                let m = i128::from(rng.range(0..=100_000u32)) - 30_000;
                Decimal::new(m, rng.range(0..=3u32)).unwrap()
            })
            .collect();
        let wsum: Decimal = weights.iter().sum();
        match total.allocate(&weights, dp) {
            Err(Error::DivisionByZero) => assert!(wsum.is_zero()),
            Err(Error::Overflow) => {}
            Err(e) => panic!("{e:?}"),
            Ok(parts) => {
                assert_eq!(Decimal::checked_sum(parts.iter().copied()), Some(total));
                for (p, w) in parts.iter().zip(&weights) {
                    // p within one unit of total × w / wsum  <=>  |p × wsum - total × w| < |wsum| × unit
                    let lhs = (*p * wsum - total * *w).abs();
                    assert!(lhs < wsum.abs() * unit, "{total} {weights:?} {parts:?}");
                    assert_eq!(p.scale(), dp);
                }
            }
        }
    }
}

#[test]
fn percent_of() {
    assert_eq!(
        d("1234567").percent_of(d("10")).unwrap().to_string(),
        "123456.70"
    );
    assert_eq!(
        d("1234567")
            .percent_of(d("8"))
            .unwrap()
            .round(0)
            .to_string(),
        "98765"
    );
    assert_eq!(
        d("59971.50").percent_of(d("10")).unwrap().to_string(),
        "5997.1500"
    );
    assert_eq!(
        d("-200").percent_of(d("12.5")).unwrap().to_string(),
        "-25.000"
    );
    assert_eq!(Decimal::MAX.percent_of(d("1000")), Err(Error::Overflow));
    assert_eq!(Decimal::MAX.percent_of(d("100")).unwrap(), Decimal::MAX);
}

// ---------------------------------------------------------------------------------
// Randomized invariants
// ---------------------------------------------------------------------------------

fn random_decimal(rng: &mut Rng) -> Decimal {
    let digits = rng.range(1..=38u32);
    let mut m: i128 = 0;
    for _ in 0..digits {
        m = m * 10 + i128::from(rng.below(10));
    }
    if rng.next_bool() {
        m = -m;
    }
    let scale = if rng.next_bool() {
        rng.range(0..=4u32)
    } else {
        rng.range(0..=28u32)
    };
    Decimal::new(m, scale).unwrap()
}

#[test]
fn random_invariants() {
    let mut rng = Rng::from_seed(0x5eed);
    for _ in 0..20_000 {
        let a = random_decimal(&mut rng);
        let b = random_decimal(&mut rng);
        // Display / parse round trip keeps the exact representation.
        let s = a.to_string();
        let p = d(&s);
        assert_eq!((p.mantissa(), p.scale()), (a.mantissa(), a.scale()), "{s}");
        assert_eq!(format!("{a:?}"), s);
        // Equality and hashing ignore trailing zeros.
        let n = a.normalize();
        assert_eq!(n, a);
        assert_eq!(hash_of(n), hash_of(a));
        if let Ok(w) = a.with_scale(28, Down) {
            assert_eq!(w, a);
            assert_eq!(hash_of(w), hash_of(a));
        }
        // Ordering agrees with the sign of the difference when it is exact.
        if let Some(diff) = a.checked_sub(b)
            && diff + b == a
        {
            assert_eq!(a.cmp(&b), diff.cmp(&Decimal::ZERO), "{a} {b}");
        }
        // Exact sums agree with i128 arithmetic at equal scale.
        let s0 = a.scale().max(b.scale());
        if let (Ok(x), Ok(y)) = (a.with_scale(s0, Down), b.with_scale(s0, Down))
            && let Some(sum) = x.mantissa().checked_add(y.mantissa())
            && let Ok(expected) = Decimal::new(sum, s0)
        {
            let got = a + b;
            assert_eq!(
                (got.mantissa(), got.scale()),
                (expected.mantissa(), expected.scale())
            );
        }
        // Remainder identity: a == trunc(a / b) * b + a % b, with |a % b| < |b|.
        if !b.is_zero()
            && let Ok(q) = a.div_round(b, 0, Down)
            && let Some(prod) = q.checked_mul(b)
        {
            let r = a % b;
            assert!(r.abs() < b.abs());
            assert!(r.is_zero() || r.is_negative() == a.is_negative());
            if let Some(back) = prod.checked_add(r)
                && prod.scale() <= 28
                && q.digits() + b.digits() <= 38
            {
                assert_eq!(back, a, "{a} % {b}");
            }
        }
        // Rounding is monotone and bounded by one unit.
        let dp = rng.range(0..=a.scale());
        let unit = Decimal::new(1, dp).unwrap();
        let fl = a.round_dp(dp, Floor);
        let ce = a.round_dp(dp, Ceiling);
        assert!(fl <= a && a <= ce && ce - fl <= unit);
        for m in [HalfEven, HalfUp, HalfDown, Up, Down] {
            let r = a.round_dp(dp, m);
            assert!(r == fl || r == ce, "{a} {dp} {m:?}");
            assert_eq!(r.scale(), dp.min(a.scale()));
        }
    }
}

#[test]
fn parse_never_panics_and_round_trips() {
    const ALPHABET: &[u8] = b"0123456789000000000.eE+-x _,\xc3\xa9";
    let mut rng = Rng::from_seed(0xfa22);
    let mut ok = 0;
    for i in 0..50_000 {
        let bytes: Vec<u8> = match i % 3 {
            // Well-formed numbers of every length: [sign] int [. frac] [e [sign] exp].
            0 => {
                let mut s = String::new();
                let digits = |rng: &mut Rng, s: &mut String, n: usize| {
                    for _ in 0..n {
                        s.push(char::from(b'0' + rng.below(10) as u8));
                    }
                };
                s.push_str(["", "-", "+"][rng.below(3) as usize]);
                let n_int = rng.range(0..=42usize);
                digits(&mut rng, &mut s, n_int);
                if n_int == 0 || rng.next_bool() {
                    s.push('.');
                    let n_frac = rng.range(usize::from(n_int == 0)..=32usize);
                    digits(&mut rng, &mut s, n_frac);
                }
                if rng.below(3) == 0 {
                    s.push_str(["e", "E-", "e+", "e-"][rng.below(4) as usize]);
                    let n_exp = rng.range(1..=3usize);
                    digits(&mut rng, &mut s, n_exp);
                }
                s.into_bytes()
            }
            // Random bytes, mostly invalid UTF-8 or junk.
            1 if i % 9 == 1 => (0..rng.range(0..=20usize))
                .map(|_| rng.below(256) as u8)
                .collect(),
            // Number-like junk.
            _ => (0..rng.range(0..=60usize))
                .map(|_| ALPHABET[rng.below(ALPHABET.len() as u64) as usize])
                .collect(),
        };
        let Ok(s) = core::str::from_utf8(&bytes) else {
            continue;
        };
        let strict = s.parse::<Decimal>();
        if let Err(Error::InvalidFormat { offset }) = strict {
            assert!(offset <= s.len(), "{s:?}");
        }
        for m in [HalfEven, HalfUp, HalfDown, Up, Down, Ceiling, Floor] {
            let rounded = Decimal::parse_rounded(s, m);
            match (strict, rounded) {
                // An exactly representable input is never changed by rounding.
                (Ok(a), Ok(b)) => assert_eq!((a.mantissa(), a.scale()), (b.mantissa(), b.scale())),
                (
                    Err(Error::InvalidFormat { offset: a }),
                    Err(Error::InvalidFormat { offset: b }),
                ) => {
                    assert_eq!(a, b)
                }
                (Err(Error::InvalidFormat { .. }), r)
                | (_, r @ Err(Error::InvalidFormat { .. })) => {
                    panic!("{s:?}: strict {strict:?} vs rounded {r:?}")
                }
                (Err(_), Ok(_)) => {}
                (Ok(_), Err(e)) => panic!("{s:?}: rounded failed with {e:?}"),
                (Err(_), Err(e)) => assert_eq!(e, Error::Overflow, "{s:?}"),
            }
        }
        if let Ok(x) = strict {
            ok += 1;
            let back: Decimal = x.to_string().parse().unwrap();
            assert_eq!((back.mantissa(), back.scale()), (x.mantissa(), x.scale()));
        }
    }
    assert!(ok > 3_000, "only {ok} valid inputs");
}

#[test]
fn precision_formatting_matches_round_half_up() {
    let mut rng = Rng::from_seed(0xf0f0);
    for _ in 0..5_000 {
        let a = random_decimal(&mut rng);
        let p = rng.range(0..=30usize);
        let s = format!("{a:.p$}");
        let expected = a.round_dp(p as u32, HalfUp);
        let parsed = Decimal::parse_rounded(&s, Down).unwrap();
        assert_eq!(parsed, expected, "{a} {p}");
        let frac = s.split_once('.').map_or(0, |(_, f)| f.len());
        assert_eq!(frac, p, "{a} {p} {s}");
    }
}
