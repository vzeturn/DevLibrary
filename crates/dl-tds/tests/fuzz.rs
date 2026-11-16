//! Robustness: random and mutated server byte streams must produce errors, never panics.

use dl_rand::Rng;
use dl_tds::__fuzz::{parse_prelogin, parse_tokens, reassemble};

/// A valid response exercising most tokens and types, used as a mutation seed.
fn seed_stream() -> Vec<u8> {
    let mut b: Vec<u8> = Vec::new();
    let u16le = |b: &mut Vec<u8>, v: u16| b.extend_from_slice(&v.to_le_bytes());
    let bvar = |b: &mut Vec<u8>, s: &str| {
        b.push(s.len() as u8);
        for c in s.encode_utf16() {
            b.extend_from_slice(&c.to_le_bytes());
        }
    };
    // ENVCHANGE database
    let mut env = vec![1u8];
    bvar(&mut env, "db");
    bvar(&mut env, "master");
    b.push(0xE3);
    u16le(&mut b, env.len() as u16);
    b.extend_from_slice(&env);
    // COLMETADATA: int, nvarchar(20), decimal(18,4), datetime2(7), varchar(max), sql_variant, ntext
    b.push(0x81);
    u16le(&mut b, 7);
    let cols: Vec<(&str, Vec<u8>)> = vec![
        ("a", vec![0x26, 4]),
        ("b", vec![0xE7, 40, 0, 9, 4, 0xD0, 0, 52]),
        ("c", vec![0x6A, 9, 18, 4]),
        ("d", vec![0x2A, 7]),
        ("e", vec![0xA7, 0xFF, 0xFF, 0x2A, 4, 0xD0, 0, 0]),
        ("f", vec![0x62, 0x10, 0x1F, 0, 0]),
        (
            "g",
            vec![
                0x63, 0xFF, 0xFF, 0xFF, 0x7F, 9, 4, 0xD0, 0, 52, 1, 1, 0, b't', 0,
            ],
        ),
    ];
    for (name, ti) in &cols {
        b.extend_from_slice(&[0, 0, 0, 0, 1, 0]);
        b.extend_from_slice(ti);
        bvar(&mut b, name);
    }
    for i in 0..3u8 {
        b.push(0xD1);
        b.extend_from_slice(&[4, i, 0, 0, 0]);
        b.extend_from_slice(&[4, 0, b'h', 0, b'i', 0]);
        b.extend_from_slice(&[9, 1, 0x40, 0xE2, 1, 0, 0, 0, 0, 0]);
        b.extend_from_slice(&[8, 0, 0, 0, 0, 0, 0, 0, 0]);
        b.extend_from_slice(&2u64.to_le_bytes());
        b.extend_from_slice(&[2, 0, 0, 0, 0xC3, 0xF5, 0, 0, 0, 0]);
        b.extend_from_slice(&[6, 0, 0, 0, 0x38, 0, 7, 0, 0, 0]);
        b.push(16);
        b.extend_from_slice(&[0xAA; 16]);
        b.extend_from_slice(&[0; 8]);
        b.extend_from_slice(&2i32.to_le_bytes());
        b.extend_from_slice(&[b'z', 0]);
    }
    // NBCROW with everything NULL
    b.push(0xD2);
    b.push(0x7F);
    // INFO, ERROR
    for tok in [0xABu8, 0xAA] {
        let mut m = Vec::new();
        m.extend_from_slice(&50000i32.to_le_bytes());
        m.extend_from_slice(&[1, 16]);
        u16le(&mut m, 2);
        m.extend_from_slice(&[b'h', 0, b'i', 0]);
        bvar(&mut m, "s");
        bvar(&mut m, "p");
        m.extend_from_slice(&3i32.to_le_bytes());
        b.push(tok);
        u16le(&mut b, m.len() as u16);
        b.extend_from_slice(&m);
    }
    // RETURNSTATUS, RETURNVALUE, DONEPROC, DONE
    b.extend_from_slice(&[0x79, 1, 0, 0, 0]);
    b.extend_from_slice(&[
        0xAC, 1, 0, 2, b'@', 0, b'o', 0, 1, 0, 0, 0, 0, 0, 0, 0x26, 4, 4, 9, 0, 0, 0,
    ]);
    b.extend_from_slice(&[0xFE, 0x11, 0, 0xC1, 0, 3, 0, 0, 0, 0, 0, 0, 0]);
    b.extend_from_slice(&[0xFD, 0x02, 0, 0xC1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    b
}

#[test]
fn seed_stream_is_valid() {
    // The seed has an ERROR token, so the parser succeeds structurally.
    assert_eq!(parse_tokens(&seed_stream()).unwrap(), 1);
}

#[test]
fn truncations_never_panic() {
    let s = seed_stream();
    for n in 0..s.len() {
        let _ = parse_tokens(&s[..n]);
    }
}

#[test]
fn random_mutations_never_panic() {
    let s = seed_stream();
    let mut rng = Rng::from_seed(0x7D5_2026);
    for _ in 0..200_000 {
        let mut m = s.clone();
        let edits = rng.range(1..=4usize);
        for _ in 0..edits {
            match rng.below(4) {
                0 => {
                    let i = rng.below(m.len() as u64) as usize;
                    m[i] = rng.next_u32() as u8;
                }
                1 => {
                    let i = rng.below(m.len() as u64) as usize;
                    m[i] ^= 1 << rng.below(8);
                }
                2 => {
                    let i = rng.below(m.len() as u64) as usize;
                    m.truncate(i.max(1));
                }
                _ => {
                    let i = rng.below(m.len() as u64) as usize;
                    m.insert(i, *rng.choose(&[0x00, 0xFF, 0x7F, 0x80, 0xFE]).unwrap());
                }
            }
        }
        let _ = parse_tokens(&m);
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng::from_seed(42);
    let tokens = [
        0x79u8, 0x81, 0xA4, 0xA5, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xD1, 0xD2, 0xE3, 0xE4, 0xED,
        0xEE, 0xFD, 0xFE, 0xFF,
    ];
    for _ in 0..100_000 {
        let len = rng.range(0..300usize);
        let mut buf = vec![0u8; len];
        rng.fill_bytes(&mut buf);
        // Start with a real token most of the time so parsing goes deeper.
        if !buf.is_empty() && rng.below(4) != 0 {
            buf[0] = *rng.choose(&tokens).unwrap();
        }
        let _ = parse_tokens(&buf);
        let _ = parse_prelogin(&buf);
        let _ = reassemble(&buf);
    }
}

#[test]
fn huge_declared_lengths_are_rejected_cheaply() {
    // PLP announcing 2^63 bytes, chunk announcing 4 GiB.
    let mut b = vec![
        0x81, 1, 0, 0, 0, 0, 0, 1, 0, 0xA5, 0xFF, 0xFF, 1, b'x', 0, 0xD1,
    ];
    b.extend_from_slice(&(1u64 << 63).to_le_bytes());
    b.extend_from_slice(&u32::MAX.to_le_bytes());
    assert!(parse_tokens(&b).is_err());
    // COLMETADATA with 65534 columns.
    assert!(parse_tokens(&[0x81, 0xFE, 0xFF]).is_err());
}
