use alloc::vec;
use alloc::vec::Vec;
use std::string::{String, ToString};

use super::*;
use dl_hash::{Digest, Sha256};

fn samples() -> Vec<(&'static str, Vec<u8>)> {
    let mut rng = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let random: Vec<u8> = (0..70_000).map(|_| next() as u8).collect();
    let mut json = String::from("[");
    for i in 0..3000 {
        json.push_str(&std::format!(
            r#"{{"id":{i},"room":"{}","guest":"Trần Thị {}","amount":"{}.50"}},"#,
            100 + next() % 900,
            (b'A' + (next() % 26) as u8) as char,
            next() % 1_000_000
        ));
    }
    json.push(']');
    let skewed: Vec<u8> = (0..80_000)
        .map(|_| (next().trailing_zeros().min(199)) as u8)
        .collect();
    let mut periodic = Vec::new();
    for i in 0..100_000u32 {
        periodic.push((i % 251) as u8);
    }
    vec![
        ("empty", Vec::new()),
        ("one", b"a".to_vec()),
        ("run", vec![b'z'; 100_000]),
        ("random", random),
        ("json", json.into_bytes()),
        ("skewed", skewed),
        ("periodic", periodic),
        ("tail", b"abcabcabcabcabcab".to_vec()),
    ]
}

const LEVELS: [Level; 4] = [Level::Store, Level::Fast, Level::Default, Level::Best];

#[test]
fn round_trips_every_level_and_format() {
    for (name, data) in samples() {
        for level in LEVELS {
            let raw = deflate_raw::compress(&data, level);
            assert_eq!(
                deflate_raw::decompress(&raw, usize::MAX).unwrap(),
                data,
                "{name} {level:?}"
            );
            let gz = gzip::compress(&data, level);
            assert_eq!(
                gzip::decompress(&gz, usize::MAX).unwrap(),
                data,
                "{name} {level:?}"
            );
            let zz = zlib::compress(&data, level);
            assert_eq!(
                zlib::decompress(&zz, usize::MAX).unwrap(),
                data,
                "{name} {level:?}"
            );
            export(name, level, &data, &gz, &zz, &raw);
        }
    }
}

/// With DL_FLATE_EXPORT set, writes inputs and compressed streams for the Go check in
/// testdata/gen_go_streams.go (-verify).
fn export(name: &str, level: Level, data: &[u8], gz: &[u8], zz: &[u8], raw: &[u8]) {
    let Ok(dir) = std::env::var("DL_FLATE_EXPORT") else {
        return;
    };
    let base = std::format!("{dir}/{name}-{level:?}");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(std::format!("{base}.in"), data).unwrap();
    std::fs::write(std::format!("{base}.gz"), gz).unwrap();
    std::fs::write(std::format!("{base}.zz"), zz).unwrap();
    std::fs::write(std::format!("{base}.deflate"), raw).unwrap();
}

#[test]
fn compression_is_useful() {
    for (name, data) in samples() {
        let gz = gzip::compress(&data, Level::Default);
        // Never much larger than stored (header and trailer, 5 bytes per stored block of
        // at most 16 384 symbols).
        assert!(
            gz.len() <= data.len() + 18 + 5 * (data.len() / 16_384 + 1),
            "{name}: {} -> {}",
            data.len(),
            gz.len()
        );
        match name {
            "json" => assert!(
                gz.len() * 4 < data.len(),
                "{name}: {} -> {}",
                data.len(),
                gz.len()
            ),
            "run" | "periodic" => assert!(gz.len() < 1000, "{name}: {}", gz.len()),
            _ => {}
        }
        let best = gzip::compress(&data, Level::Best);
        assert!(best.len() <= gz.len() + 64, "{name}");
    }
}

/// Streams written by Go's compress/* (testdata/go_streams.txt): all levels, Huffman-only,
/// stored blocks, concatenated gzip members.
#[test]
fn decompresses_go_streams() {
    let data = include_str!("../testdata/go_streams.txt");
    let mut n = 0;
    for line in data
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let f: Vec<&str> = line.split(' ').collect();
        let comp = dl_encoding::base64::STANDARD.decode(f[5]).unwrap();
        let out = match f[0] {
            "gzip" => gzip::decompress(&comp, usize::MAX),
            "zlib" => zlib::decompress(&comp, usize::MAX),
            _ => deflate_raw::decompress(&comp, usize::MAX),
        }
        .unwrap_or_else(|e| panic!("{} {} {}: {e}", f[0], f[1], f[2]));
        assert_eq!(out.len().to_string(), f[3], "{line:.40}");
        let sum = dl_encoding::hex::encode(Sha256::digest(&out));
        assert_eq!(sum, f[4], "{} {} {}", f[0], f[1], f[2]);
        n += 1;
    }
    assert!(n >= 40, "{n}");
}

#[test]
fn known_streams() {
    // zlib.compress(b"abc") from Python's zlib (a fixed-Huffman block).
    let z = [
        0x78, 0x9c, 0x4b, 0x4c, 0x4a, 0x06, 0x00, 0x02, 0x4d, 0x01, 0x27,
    ];
    assert_eq!(zlib::decompress(&z, 100).unwrap(), b"abc");
    assert_eq!(zlib::adler32(b"abc"), 0x024d_0127);
    // A stored block "hi".
    let s = [0x01, 0x02, 0x00, 0xfd, 0xff, b'h', b'i'];
    assert_eq!(deflate_raw::decompress(&s, 100).unwrap(), b"hi");
    let (_, end) =
        deflate_raw::decompress_with_end(&[0x01, 0x00, 0x00, 0xff, 0xff, 9, 9], 10).unwrap();
    assert_eq!(end, 5);
}

#[test]
fn malformed_input_is_refused_without_panicking() {
    let kind = |r: Result<Vec<u8>, Error>| r.unwrap_err().kind();
    assert_eq!(
        kind(deflate_raw::decompress(&[0x07], 10)),
        ErrorKind::InvalidBlockType
    );
    assert_eq!(
        kind(deflate_raw::decompress(&[0x01, 0x02, 0x00, 0x00, 0x00], 10)),
        ErrorKind::StoredLength
    );
    assert_eq!(
        kind(deflate_raw::decompress(&[], 10)),
        ErrorKind::UnexpectedEof
    );
    // Fixed block whose first symbol is a match (length 3, distance 1) with no output yet.
    let mut bits: Vec<(u32, u32)> = vec![(1, 1), (1, 2)];
    bits.push((0b100_0000, 7)); // symbol 257 = 0000001, written MSB first
    bits.push((0, 5)); // distance symbol 0
    bits.push((0, 7)); // end of block
    let (mut buf, mut cnt, mut stream) = (0u64, 0u32, Vec::new());
    for (v, n) in bits {
        buf |= u64::from(v) << cnt;
        cnt += n;
    }
    while cnt > 0 {
        stream.push(buf as u8);
        buf >>= 8;
        cnt = cnt.saturating_sub(8);
    }
    assert_eq!(
        kind(deflate_raw::decompress(&stream, 10)),
        ErrorKind::DistanceTooFar
    );
    // Decompression bomb: 100 KB of zeros with a 1 KB limit.
    let bomb = gzip::compress(&vec![0u8; 100_000], Level::Best);
    assert!(matches!(
        kind(gzip::decompress(&bomb, 1000)),
        ErrorKind::TooLarge { limit: 1000 }
    ));
    // Corrupted CRC, length, header, truncation.
    let mut gz = gzip::compress(b"hello hello hello", Level::Default);
    let n = gz.len();
    gz[n - 8] ^= 1;
    assert_eq!(
        kind(gzip::decompress(&gz, 100)),
        ErrorKind::ChecksumMismatch
    );
    gz[n - 8] ^= 1;
    gz[n - 4] ^= 1;
    assert_eq!(kind(gzip::decompress(&gz, 100)), ErrorKind::LengthMismatch);
    gz[n - 4] ^= 1;
    assert_eq!(
        kind(gzip::decompress(&gz[..n - 3], 100)),
        ErrorKind::UnexpectedEof
    );
    gz[0] = 0;
    assert_eq!(kind(gzip::decompress(&gz, 100)), ErrorKind::InvalidHeader);
    let mut zz = zlib::compress(b"x", Level::Default);
    zz.push(0);
    assert_eq!(kind(zlib::decompress(&zz, 100)), ErrorKind::TrailingData);
    assert_eq!(
        kind(zlib::decompress(&[0x78, 0x9d], 100)),
        ErrorKind::InvalidHeader
    );
    // Random garbage never panics.
    let mut x = 88172645463325252u64;
    for len in 0..300 {
        let junk: Vec<u8> = (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let _ = deflate_raw::decompress(&junk, 1 << 16);
        let mut g = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
        g.extend_from_slice(&junk);
        let _ = gzip::decompress(&g, 1 << 16);
    }
    // Every truncation of a valid stream is an error, never a panic.
    let full = gzip::compress(&samples()[4].1[..5000], Level::Default);
    for cut in 0..full.len() {
        assert!(gzip::decompress(&full[..cut], 1 << 20).is_err(), "{cut}");
    }
}
