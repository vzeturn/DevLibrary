//! Bit-exact comparison with segno, an independent encoder.
//!
//! `vectors.txt` is produced by `gen_vectors.py` (see its docstring for the format). Each
//! case fixes version, ECC level, segment modes and mask so both encoders must produce the
//! same matrix, module for module.
//!
//! Automatic masking: dl-qr scores each mask on the finished symbol, with format and
//! version information drawn (like Nayuki's library, ZXing, QRCoder and node `qrcode`).
//! segno scores before drawing them, so its own choice can differ. `best_mask` is segno's
//! scoring function applied to segno's finished symbols, which is what dl-qr must match.

use dl_qr::{Ecc, QrCode, Segment};

struct Case {
    name: String,
    ecc: Ecc,
    version: u8,
    mask: u8,
    segments: Vec<Segment>,
    /// Concatenated segment bytes, for the `text` check.
    payload: Vec<u8>,
    text: bool,
    best_mask: u8,
    segno_mask: u8,
    rows: Vec<Vec<bool>>,
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn parse() -> Vec<Case> {
    let mut cases = Vec::new();
    let mut lines = include_str!("vectors.txt")
        .lines()
        .filter(|l| !l.starts_with('#'));
    while let Some(line) = lines.next() {
        let name = line.strip_prefix("case ").expect("case line").to_string();
        let mut c = Case {
            name,
            ecc: Ecc::Low,
            version: 0,
            mask: 0,
            segments: Vec::new(),
            payload: Vec::new(),
            text: false,
            best_mask: 0,
            segno_mask: 0,
            rows: Vec::new(),
        };
        loop {
            let line = lines.next().expect("unterminated case");
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            match key {
                "ecc" => {
                    c.ecc = match value {
                        "L" => Ecc::Low,
                        "M" => Ecc::Medium,
                        "Q" => Ecc::Quartile,
                        _ => Ecc::High,
                    }
                }
                "version" => c.version = value.parse().unwrap(),
                "mask" => c.mask = value.parse().unwrap(),
                "seg" => {
                    let (mode, data) = value.split_once(' ').unwrap();
                    let seg = match mode {
                        "eci" => Segment::eci(data.parse().unwrap()).unwrap(),
                        _ => {
                            let bytes = unhex(data);
                            c.payload.extend_from_slice(&bytes);
                            let s = std::str::from_utf8(&bytes);
                            match mode {
                                "numeric" => Segment::numeric(s.unwrap()).unwrap(),
                                "alphanumeric" => Segment::alphanumeric(s.unwrap()).unwrap(),
                                _ => Segment::bytes(&bytes),
                            }
                        }
                    };
                    c.segments.push(seg);
                }
                "text" => c.text = true,
                "penalty" => {}
                "best_mask" => c.best_mask = value.parse().unwrap(),
                "segno_mask" => c.segno_mask = value.parse().unwrap(),
                "rows" => {
                    let n: usize = value.parse().unwrap();
                    for _ in 0..n {
                        let hex = lines.next().unwrap();
                        let row: Vec<bool> = hex
                            .chars()
                            .flat_map(|ch| {
                                let v = ch.to_digit(16).unwrap();
                                (0..4).rev().map(move |i| (v >> i) & 1 == 1)
                            })
                            .take(n)
                            .collect();
                        c.rows.push(row);
                    }
                }
                "end" => break,
                other => panic!("unknown key {other}"),
            }
        }
        cases.push(c);
    }
    cases
}

fn diff(name: &str, got: &QrCode, want: &[Vec<bool>]) -> Option<String> {
    let m = got.to_matrix();
    if m.len() != want.len() {
        return Some(format!("{name}: size {} != {}", m.len(), want.len()));
    }
    let bad: Vec<(usize, usize)> = (0..m.len())
        .flat_map(|y| (0..m.len()).map(move |x| (x, y)))
        .filter(|&(x, y)| m[y][x] != want[y][x])
        .collect();
    if bad.is_empty() {
        None
    } else {
        Some(format!(
            "{name}: {} modules differ, first at {:?}",
            bad.len(),
            &bad[..bad.len().min(5)]
        ))
    }
}

#[test]
fn matrices_match_segno() {
    let cases = parse();
    assert!(cases.len() >= 25);
    let mut failures = Vec::new();
    for c in &cases {
        let b = QrCode::builder()
            .ecc(c.ecc)
            .version(c.version)
            .mask(Some(c.mask));
        let qr = b.encode_segments(&c.segments).unwrap();
        assert_eq!(
            (qr.version(), qr.ecc(), qr.mask()),
            (c.version, c.ecc, c.mask),
            "{}",
            c.name
        );
        failures.extend(diff(&c.name, &qr, &c.rows));
        if c.text {
            let text = std::str::from_utf8(&c.payload).unwrap();
            let qr = b.encode_text(text).unwrap();
            failures.extend(diff(&format!("{} (encode_text)", c.name), &qr, &c.rows));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn automatic_mask_matches_segno_scoring() {
    let mut failures = Vec::new();
    let mut native_agree = 0;
    let cases = parse();
    for c in &cases {
        let qr = QrCode::builder()
            .ecc(c.ecc)
            .version(c.version)
            .encode_segments(&c.segments)
            .unwrap();
        if qr.mask() != c.best_mask {
            failures.push(format!("{}: mask {} != {}", c.name, qr.mask(), c.best_mask));
        }
        native_agree += usize::from(qr.mask() == c.segno_mask);
    }
    println!(
        "automatic mask equals segno's own choice in {native_agree}/{} cases",
        cases.len()
    );
    assert!(failures.is_empty(), "{failures:#?}");
}

/// `max-*` cases fill their version exactly (checked against segno by the generator):
/// one more character must not fit, and `encode_text` must pick that version on its own.
#[test]
fn max_capacity_cases_are_full() {
    let mut checked = 0;
    for c in parse().iter().filter(|c| c.name.starts_with("max-")) {
        assert!(c.text, "{}", c.name);
        let mut more = std::str::from_utf8(&c.payload).unwrap().to_string();
        let qr = QrCode::builder().ecc(c.ecc).encode_text(&more).unwrap();
        assert_eq!(qr.version(), c.version, "{}", c.name);
        more.push(match c.segments[0].mode() {
            dl_qr::Mode::Numeric => '0',
            dl_qr::Mode::Alphanumeric => 'A',
            _ => 'a',
        });
        let err = QrCode::builder()
            .ecc(c.ecc)
            .version(c.version)
            .encode_text(&more)
            .unwrap_err();
        assert!(
            matches!(err, dl_qr::Error::DataTooLong { bits, capacity } if bits > capacity),
            "{}: {err:?}",
            c.name
        );
        checked += 1;
    }
    assert!(checked >= 10);
}

#[test]
fn encode_bytes_matches_byte_segment() {
    for c in parse()
        .iter()
        .filter(|c| c.segments.len() == 1 && c.segments[0].mode() == dl_qr::Mode::Byte)
    {
        let qr = QrCode::builder()
            .ecc(c.ecc)
            .version(c.version)
            .mask(Some(c.mask))
            .encode_bytes(&c.payload)
            .unwrap();
        assert!(diff(&c.name, &qr, &c.rows).is_none(), "{}", c.name);
    }
}
