use std::fmt::Write as _;
use std::io::Cursor;
use std::path::PathBuf;

use super::*;
use dl_hash::{Digest, Sha256};

fn open(bytes: &[u8]) -> ZipArchive<Cursor<&[u8]>> {
    ZipArchive::new(Cursor::new(bytes)).unwrap()
}

fn xorshift(mut x: u64) -> impl FnMut() -> u64 {
    move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }
}

fn sha_hex(data: &[u8]) -> String {
    let mut s = String::new();
    for b in Sha256::digest(data) {
        write!(s, "{b:02x}").unwrap();
    }
    s
}

fn fmt_time(t: DosDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        t.year(),
        t.month(),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

fn find(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len())
        .position(|w| w == needle)
        .expect("needle")
}

fn tmp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dl-zip-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// 5 MiB: half random bytes, half repetitive text.
fn big_data() -> Vec<u8> {
    let mut next = xorshift(0x9E37_79B9_7F4A_7C15);
    let mut v: Vec<u8> = (0..5 << 19).map(|_| next() as u8).collect();
    let mut i = 0u64;
    while v.len() < 5 << 20 {
        let line = format!("{i:08} INFO agent heartbeat ok, queue={}\n", next() % 16);
        v.extend_from_slice(line.as_bytes());
        i += 1;
    }
    v.truncate(5 << 20);
    v
}

const STAMP: (u16, u8, u8, u8, u8, u8) = (2024, 5, 6, 7, 8, 10);

fn stamp() -> DosDateTime {
    let (y, mo, d, h, mi, s) = STAMP;
    DosDateTime::new(y, mo, d, h, mi, s).unwrap()
}

fn opts(method: Method) -> Options {
    Options {
        method,
        modified: Modified::Dos(stamp()),
    }
}

/// (name, data, options); `None` options = directory.
type Spec = (String, Vec<u8>, Option<Options>);

fn mixed_specs(big: bool) -> Vec<Spec> {
    let deflate = opts(Method::Deflate(Level::Default));
    let stored = opts(Method::Stored);
    let mut csv = String::new();
    for i in 0..3000 {
        writeln!(csv, "{i},{},Phòng {}", i * i, 100 + i % 50).unwrap();
    }
    let mut v: Vec<Spec> = vec![
        ("empty.txt".into(), Vec::new(), Some(deflate)),
        ("logs/".into(), Vec::new(), None),
        ("logs/empty_dir/".into(), Vec::new(), None),
        (
            "logs/numbers.csv".into(),
            csv.clone().into_bytes(),
            Some(deflate),
        ),
        (
            "logs/numbers-stored.csv".into(),
            csv.into_bytes(),
            Some(stored),
        ),
        (
            "Tài liệu/hóa đơn tháng 5.json".into(),
            r#"{"khách":"Nguyễn Văn A","tổng":"1.250.000"}"#.into(),
            Some(stored),
        ),
        (
            "Tài liệu/Đà Nẵng ở đâu.txt".into(),
            "Đà Nẵng ở miền Trung Việt Nam. ".repeat(50).into_bytes(),
            Some(opts(Method::Deflate(Level::Best))),
        ),
        (
            "unix-time.txt".into(),
            b"modified 2024-05-06T07:08:11Z".to_vec(),
            Some(Options {
                method: Method::Deflate(Level::Fast),
                modified: Modified::Unix(1_714_979_291),
            }),
        ),
    ];
    if big {
        let data = big_data();
        v.push((
            "big-deflate.bin".into(),
            data.clone(),
            Some(opts(Method::Deflate(Level::Fast))),
        ));
        v.push(("big-stored.bin".into(), data, Some(stored)));
    }
    v
}

/// A small archive for the truncation and mutation tests.
fn small_specs() -> Vec<Spec> {
    vec![
        ("empty.txt".into(), Vec::new(), Some(opts(Method::Stored))),
        ("d/".into(), Vec::new(), None),
        (
            "d/text.txt".into(),
            b"zip zap zip zap zip zap zip zap ".repeat(12),
            Some(opts(Method::Deflate(Level::Default))),
        ),
        (
            "Đà Nẵng.txt".into(),
            "Việt Nam".into(),
            Some(opts(Method::Stored)),
        ),
        (
            "t.txt".into(),
            b"unix".to_vec(),
            Some(Options {
                method: Method::Stored,
                modified: Modified::Unix(1_714_979_291),
            }),
        ),
    ]
}

fn write_specs(specs: &[Spec], force_zip64: bool) -> Vec<u8> {
    let mut w = ZipWriter::new(Vec::new());
    w.force_zip64 = force_zip64;
    for (name, data, o) in specs {
        match o {
            Some(o) => w.add_file(name, data, *o).unwrap(),
            None => w.add_directory(name).unwrap(),
        }
    }
    w.finish().unwrap()
}

fn manifest(specs: &[Spec]) -> String {
    let mut s = String::new();
    for (name, data, o) in specs {
        match o {
            None => writeln!(s, "{name}\tdir").unwrap(),
            Some(o) => {
                let t = match o.modified {
                    Modified::Dos(t) => t,
                    Modified::Unix(u) => DosDateTime::from_unix(u),
                };
                writeln!(
                    s,
                    "{name}\t{}\t{}\t{}",
                    data.len(),
                    sha_hex(data),
                    fmt_time(t)
                )
                .unwrap();
            }
        }
    }
    s
}

/// Checks an archive against manifest lines: `name TAB dir` or
/// `name TAB size TAB sha256 TAB time`.
fn check_manifest(bytes: &[u8], manifest: &str, what: &str) {
    let lines: Vec<Vec<&str>> = manifest
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.split('\t').collect())
        .collect();
    let mut z = open(bytes);
    assert_eq!(z.entries().len(), lines.len(), "{what}");
    for (i, f) in lines.iter().enumerate() {
        let e = z.entries()[i].clone();
        assert_eq!(e.name(), f[0], "{what}");
        if f[1] == "dir" {
            assert!(e.is_dir(), "{what} {}", f[0]);
            assert_eq!(e.size(), 0);
            continue;
        }
        assert!(!e.is_dir(), "{what} {}", f[0]);
        let data = z
            .read(i, 64 << 20)
            .unwrap_or_else(|err| panic!("{what} {}: {err}", f[0]));
        assert_eq!(data.len().to_string(), f[1], "{what} {}", f[0]);
        assert_eq!(sha_hex(&data), f[2], "{what} {}", f[0]);
        assert_eq!(fmt_time(e.modified()), f[3], "{what} {}", f[0]);
        assert_eq!(e.flags() & 0x800 != 0, !f[0].is_ascii(), "{what} {}", f[0]);
    }
}

#[test]
fn empty_archive() {
    let b = ZipWriter::new(Vec::new()).finish().unwrap();
    assert_eq!(b, [&b"PK\x05\x06"[..], &[0; 18]].concat());
    let z = open(&b);
    assert!(z.entries().is_empty());
    assert!(z.comment().is_empty());
}

#[test]
fn round_trip_mixed_entries() {
    let specs = mixed_specs(false);
    let b = write_specs(&specs, false);
    check_manifest(&b, &manifest(&specs), "mixed");
    let mut z = open(&b);
    let e = &z.entries()[0];
    assert_eq!((e.size(), e.method(), e.crc32()), (0, 0, 0));
    assert_eq!(e.unix_mode(), Some(0o100_644));
    let d = &z.entries()[1];
    assert!(d.is_dir() && d.name() == "logs/" && d.unix_mode() == Some(0o040_755));
    assert_eq!(d.external_attributes() & 0x10, 0x10);
    assert_eq!(z.entries()[3].method(), 8);
    assert!(z.entries()[3].compressed_size() * 3 < z.entries()[3].size());
    assert_eq!(z.entries()[4].method(), 0);
    assert_eq!(z.entries()[5].flags(), 0x800);
    assert_eq!(
        z.entries()[5].raw_name(),
        "Tài liệu/hóa đơn tháng 5.json".as_bytes()
    );
    let u = &z.entries()[7];
    assert_eq!(u.modified_unix(), Some(1_714_979_291));
    assert_eq!(u.modified(), DosDateTime::from_unix(1_714_979_291));
    assert_eq!(z.entries()[3].modified_unix(), None);
    assert_eq!(
        z.read_by_name("Tài liệu/hóa đơn tháng 5.json", 100)
            .unwrap(),
        r#"{"khách":"Nguyễn Văn A","tổng":"1.250.000"}"#.as_bytes()
    );
    assert!(matches!(z.read_by_name("missing", 100), Err(Error::NotFound(n)) if n == "missing"));
    assert!(matches!(
        z.read(99, 100),
        Err(Error::IndexOutOfRange { index: 99, len: 8 })
    ));
    assert_eq!(z.index_of("logs/numbers.csv"), Some(3));
}

#[test]
fn round_trip_5_mib_both_methods() {
    let data = big_data();
    for method in [Method::Stored, Method::Deflate(Level::Fast)] {
        let mut w = ZipWriter::new(Vec::new());
        w.add_file("big.bin", &data, opts(method)).unwrap();
        w.add_file("after.txt", b"after", opts(method)).unwrap();
        let b = w.finish().unwrap();
        let mut z = open(&b);
        let e = z.entries()[0].clone();
        assert_eq!(e.size(), data.len() as u64);
        if method == Method::Stored {
            assert_eq!(e.method(), 0);
            assert_eq!(e.compressed_size(), e.size());
        } else {
            assert_eq!(e.method(), 8);
            assert!(
                e.compressed_size() < e.size() * 3 / 4,
                "{}",
                e.compressed_size()
            );
        }
        assert!(z.read(0, data.len()).unwrap() == data, "{method:?}");
        assert_eq!(z.read(1, 5).unwrap(), b"after");
        assert!(matches!(
            z.read(0, data.len() - 1),
            Err(Error::TooLarge { index: 0, .. })
        ));
    }
}

#[test]
fn writer_names() {
    let mut w = ZipWriter::new(Vec::new());
    w.add_file("win\\sub\\a.txt", b"x", Options::default())
        .unwrap();
    w.add_directory("d\\e").unwrap();
    for bad in [
        "",
        "/abs",
        "C:/x",
        "c:x",
        "../x",
        "a/../../x",
        "a\0b",
        "./",
        "x/",
    ] {
        assert!(
            matches!(
                w.add_file(bad, b"", Options::default()),
                Err(Error::InvalidName { .. })
            ),
            "{bad:?}"
        );
    }
    assert!(matches!(
        w.add_directory(".."),
        Err(Error::InvalidName { .. })
    ));
    let long = "a".repeat(65_536);
    assert!(matches!(
        w.add_file(&long, b"", Options::default()),
        Err(Error::InvalidName { .. })
    ));
    let b = w.finish().unwrap();
    let z = open(&b);
    let names: Vec<&str> = z.entries().iter().map(Entry::name).collect();
    assert_eq!(names, ["win/sub/a.txt", "d/e/"]);
}

#[test]
fn crc_and_size_mismatch_are_detected() {
    let mut w = ZipWriter::new(Vec::new());
    w.add_file("a.txt", b"hello world", opts(Method::Stored))
        .unwrap();
    w.add_file(
        "b.txt",
        &b"abcabcabc".repeat(100),
        opts(Method::Deflate(Level::Default)),
    )
    .unwrap();
    let good = w.finish().unwrap();

    // A changed byte of stored data.
    let mut b = good.clone();
    b[30 + 5] ^= 1;
    assert!(matches!(
        open(&b).read(0, 100),
        Err(Error::ChecksumMismatch { index: 0, .. })
    ));
    assert_eq!(open(&b).read(1, 1000).unwrap(), b"abcabcabc".repeat(100));

    // A changed CRC in the second central directory header.
    let cd = find(&good, b"PK\x01\x02");
    let cd2 = cd + find(&good[cd + 4..], b"PK\x01\x02") + 4;
    let mut b = good.clone();
    b[cd2 + 16] ^= 0x80;
    let err = open(&b).read(1, 1000).unwrap_err();
    assert!(
        matches!(err, Error::ChecksumMismatch { index: 1, .. }),
        "{err}"
    );

    // Declared sizes smaller and larger than the data.
    for declared in [899u32, 901, 0] {
        let mut b = good.clone();
        b[cd2 + 24..cd2 + 28].copy_from_slice(&declared.to_le_bytes());
        let err = open(&b).read(1, 1000).unwrap_err();
        assert!(
            matches!(err, Error::SizeMismatch { index: 1, declared: d } if d == u64::from(declared)),
            "{declared}: {err}"
        );
    }
    // Stored entry whose sizes disagree.
    let mut b = good.clone();
    b[cd + 24] = 10;
    assert!(matches!(
        open(&b).read(0, 100),
        Err(Error::Malformed { .. })
    ));
    // A broken deflate stream.
    let mut b = good.clone();
    let data2 = find(&good, b"PK\x03\x04\x14") + 30 + 5;
    b[data2] = 0xFF;
    let err = open(&b).read(1, 1000).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Deflate { index: 1, .. }
                | Error::ChecksumMismatch { .. }
                | Error::SizeMismatch { .. }
        ),
        "{err}"
    );
}

#[test]
fn zip_bomb_limits() {
    let zeros = vec![0u8; 4 << 20];
    let mut w = ZipWriter::new(Vec::new());
    w.add_file("zeros.bin", &zeros, opts(Method::Deflate(Level::Fast)))
        .unwrap();
    let good = w.finish().unwrap();
    assert!(good.len() < 64 << 10, "{}", good.len());
    let mut z = open(&good);
    assert!(matches!(
        z.read(0, 1 << 20),
        Err(Error::TooLarge { index: 0, size, limit }) if size == 4 << 20 && limit == 1 << 20
    ));
    assert_eq!(z.read(0, 4 << 20).unwrap(), zeros);
    // A lying declared size: decompression stops right after the declared size.
    let cd = find(&good, b"PK\x01\x02");
    let mut b = good.clone();
    b[cd + 24..cd + 28].copy_from_slice(&1000u32.to_le_bytes());
    assert!(matches!(
        open(&b).read(0, 1 << 20),
        Err(Error::SizeMismatch { declared: 1000, .. })
    ));

    let dir = tmp_dir("limits");
    let limits = ExtractLimits {
        max_entry_size: 1 << 20,
        ..ExtractLimits::default()
    };
    assert!(matches!(
        z.extract_to(&dir, &limits),
        Err(Error::TooLarge { .. })
    ));
    let limits = ExtractLimits {
        max_total_size: 1 << 20,
        ..ExtractLimits::default()
    };
    assert!(matches!(
        z.extract_to(&dir, &limits),
        Err(Error::TotalTooLarge { limit: 1_048_576 })
    ));
    let limits = ExtractLimits {
        max_entries: 0,
        ..ExtractLimits::default()
    };
    assert!(matches!(
        z.extract_to(&dir, &limits),
        Err(Error::TooManyEntries { count: 1, limit: 0 })
    ));
    assert!(!dir.exists());
}

/// An archive with one file whose name bytes are `name` (written with a placeholder of
/// the same length, then patched in the local and central headers).
fn with_raw_name(name: &[u8], data: &[u8]) -> Vec<u8> {
    let placeholder = "#".repeat(name.len());
    let mut w = ZipWriter::new(Vec::new());
    w.add_file("ok.txt", b"fine", opts(Method::Stored)).unwrap();
    w.add_file(&placeholder, data, opts(Method::Stored))
        .unwrap();
    let mut b = w.finish().unwrap();
    for _ in 0..2 {
        let at = find(&b, placeholder.as_bytes());
        b[at..at + name.len()].copy_from_slice(name);
    }
    b
}

#[test]
fn zip_slip_names_are_refused() {
    let bad: [&[u8]; 11] = [
        b"../evil.txt",
        b"a/../../evil.txt",
        b"..\\evil.txt",
        b"a\\..\\..\\evil.txt",
        b"/tmp/evil.txt",
        b"\\evil.txt",
        b"\\\\host\\share\\evil.txt",
        b"C:/evil.txt",
        b"C:evil.txt",
        b"a\0b.txt",
        b"a/..",
    ];
    for name in bad {
        let b = with_raw_name(name, b"evil");
        let mut z = open(&b);
        assert_eq!(z.entries()[1].raw_name(), name);
        assert_eq!(z.entries()[1].enclosed_name(), None, "{name:?}");
        let dir = tmp_dir("slip");
        let err = z.extract_to(&dir, &ExtractLimits::default()).unwrap_err();
        assert!(
            matches!(err, Error::UnsafeName { index: 1, .. }),
            "{name:?}: {err}"
        );
        // Checked before anything is written: not even the safe first entry.
        assert!(!dir.exists(), "{name:?}");
    }
    #[cfg(windows)]
    for name in [&b"a/b:ads"[..], b"CON", b"x/nul.txt", b"a/.. ", b"a/..."] {
        let b = with_raw_name(name, b"evil");
        let mut z = open(&b);
        assert_eq!(z.entries()[1].enclosed_name(), None, "{name:?}");
        let dir = tmp_dir("slip-win");
        assert!(matches!(
            z.extract_to(&dir, &ExtractLimits::default()),
            Err(Error::UnsafeName { .. })
        ));
        assert!(!dir.exists());
    }
}

#[test]
fn extract_to_writes_files_and_directories() {
    let specs = mixed_specs(false);
    let base = write_specs(&specs, false);
    // Plus a Windows PowerShell 5.1 style name with backslashes.
    let b = {
        let mut z = open(&base);
        let mut w = ZipWriter::new(Vec::new());
        for i in 0..z.entries().len() {
            let e = z.entries()[i].clone();
            if e.is_dir() {
                w.add_directory(e.name()).unwrap();
            } else {
                let data = z.read(i, 1 << 20).unwrap();
                w.add_file(e.name(), &data, opts(Method::Deflate(Level::Fast)))
                    .unwrap();
            }
        }
        w.add_file("############", b"backslash", opts(Method::Stored))
            .unwrap();
        let mut out = w.finish().unwrap();
        for _ in 0..2 {
            let at = find(&out, b"############");
            out[at..at + 12].copy_from_slice(b"ps\\sub\\x.txt");
        }
        out
    };
    let dir = tmp_dir("extract");
    let mut z = open(&b);
    z.extract_to(&dir, &ExtractLimits::default()).unwrap();
    for (name, data, o) in &specs {
        let p = dir.join(name);
        if o.is_none() {
            assert!(p.is_dir(), "{name}");
        } else {
            assert_eq!(&std::fs::read(&p).unwrap(), data, "{name}");
        }
    }
    assert!(dir.join("logs/empty_dir").is_dir());
    assert_eq!(
        std::fs::read(dir.join("ps/sub/x.txt")).unwrap(),
        b"backslash"
    );
    // Extracting again overwrites.
    z.extract_to(&dir, &ExtractLimits::default()).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

fn check_zip64_end(b: &[u8]) {
    let n = b.len();
    assert_eq!(&b[n - 22..n - 18], b"PK\x05\x06");
    assert_eq!(&b[n - 12..n - 6], &[0xFF; 6], "EOCD fields are markers");
    assert_eq!(&b[n - 42..n - 38], b"PK\x06\x07", "ZIP64 locator");
    assert_eq!(&b[n - 98..n - 94], b"PK\x06\x06", "ZIP64 record");
}

#[test]
fn zip64_more_than_65535_entries() {
    const N: usize = 65_600;
    let mut w = ZipWriter::new(Vec::new());
    for i in 0..N {
        w.add_file(
            &format!("f/{i}"),
            i.to_string().as_bytes(),
            opts(Method::Stored),
        )
        .unwrap();
    }
    let b = w.finish().unwrap();
    check_zip64_end(&b);
    // Entries themselves are small: no ZIP64 extra fields.
    assert_eq!(&b[4..6], &10u16.to_le_bytes());
    let mut z = open(&b);
    assert_eq!(z.entries().len(), N);
    for i in [0, 1, 65_534, 65_535, 65_536, N - 1] {
        assert_eq!(z.entries()[i].name(), format!("f/{i}"));
        assert_eq!(z.read(i, 10).unwrap(), i.to_string().as_bytes());
    }
    assert_eq!(z.read_by_name("f/40000", 10).unwrap(), b"40000");
}

#[test]
fn zip64_forced_small_archive() {
    let specs = mixed_specs(false);
    let b = write_specs(&specs, true);
    check_zip64_end(&b);
    // Local header: version 45, sizes are markers, ZIP64 extra field with both sizes.
    assert_eq!(&b[4..6], &45u16.to_le_bytes());
    assert_eq!(&b[18..26], &[0xFF; 8]);
    assert_eq!(&b[30 + 9..30 + 13], &[1, 0, 16, 0]);
    // Central header: sizes and offset are markers.
    let cd = find(&b, b"PK\x01\x02");
    assert_eq!(&b[cd + 20..cd + 28], &[0xFF; 8]);
    assert_eq!(&b[cd + 42..cd + 46], &[0xFF; 4]);
    check_manifest(&b, &manifest(&specs), "forced zip64");
    assert_eq!(open(&b).entries()[7].modified_unix(), Some(1_714_979_291));
}

#[test]
fn data_in_front_of_the_archive() {
    for force in [false, true] {
        let specs = mixed_specs(false);
        let b = [vec![0xAA; 1000], write_specs(&specs, force)].concat();
        check_manifest(&b, &manifest(&specs), "prefixed");
    }
}

#[test]
fn archive_comment() {
    let mut b = write_specs(&mixed_specs(false), false);
    let n = b.len();
    b[n - 2..].copy_from_slice(&7u16.to_le_bytes());
    b.extend_from_slice(b"comment");
    let z = open(&b);
    assert_eq!(z.comment(), b"comment");
    assert_eq!(z.entries().len(), 8);
    // A comment length past the end: no valid record.
    b.pop();
    assert!(matches!(
        ZipArchive::new(Cursor::new(&b)),
        Err(Error::NotZip)
    ));
}

#[test]
fn cp437_names() {
    let b = with_raw_name(b"caf\x82_\xA5.txt", b"x");
    let z = open(&b);
    assert_eq!(z.entries()[1].flags() & 0x800, 0);
    assert_eq!(z.entries()[1].name(), "café_Ñ.txt");
    assert_eq!(
        z.entries()[1].enclosed_name(),
        Some(PathBuf::from("café_Ñ.txt"))
    );
}

struct Raw {
    name: Vec<u8>,
    flags: u16,
    method: u16,
    made_by: u16,
    external: u32,
    data: Vec<u8>,
    crc: u32,
    size: u32,
    central_extra: Vec<u8>,
}

impl Raw {
    fn stored(name: &[u8], data: &[u8]) -> Raw {
        Raw {
            name: name.to_vec(),
            flags: 0,
            method: 0,
            made_by: 20,
            external: 0,
            data: data.to_vec(),
            crc: dl_hash::crc32(data),
            size: data.len() as u32,
            central_extra: Vec::new(),
        }
    }

    fn deflated(name: &[u8], data: &[u8]) -> Raw {
        Raw {
            method: 8,
            data: dl_flate::deflate_raw::compress(data, Level::Default),
            ..Raw::stored(name, data)
        }
    }
}

/// Writes entries by hand; flag bit 3 entries get zeros in the local header and a data
/// descriptor (with signature) after the data.
fn raw_archive(entries: &[Raw]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd = Vec::new();
    let p16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let p32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    for e in entries {
        let off = out.len() as u32;
        let dd = e.flags & 8 != 0;
        let csize = e.data.len() as u32;
        p32(&mut out, SIG_LOCAL);
        p16(&mut out, 20);
        p16(&mut out, e.flags);
        p16(&mut out, e.method);
        p16(&mut out, 0);
        p16(&mut out, 0x21);
        for x in [e.crc, csize, e.size] {
            p32(&mut out, if dd { 0 } else { x });
        }
        p16(&mut out, e.name.len() as u16);
        p16(&mut out, 0);
        out.extend_from_slice(&e.name);
        out.extend_from_slice(&e.data);
        if dd {
            for x in [0x0807_4b50, e.crc, csize, e.size] {
                p32(&mut out, x);
            }
        }
        p32(&mut cd, SIG_CENTRAL);
        p16(&mut cd, e.made_by);
        p16(&mut cd, 20);
        p16(&mut cd, e.flags);
        p16(&mut cd, e.method);
        p16(&mut cd, 0);
        p16(&mut cd, 0x21);
        for x in [e.crc, csize, e.size] {
            p32(&mut cd, x);
        }
        p16(&mut cd, e.name.len() as u16);
        p16(&mut cd, e.central_extra.len() as u16);
        for _ in 0..3 {
            p16(&mut cd, 0);
        }
        p32(&mut cd, e.external);
        p32(&mut cd, off);
        cd.extend_from_slice(&e.name);
        cd.extend_from_slice(&e.central_extra);
    }
    let cd_start = out.len() as u32;
    out.extend_from_slice(&cd);
    p32(&mut out, SIG_EOCD);
    p32(&mut out, 0);
    p16(&mut out, entries.len() as u16);
    p16(&mut out, entries.len() as u16);
    p32(&mut out, cd.len() as u32);
    p32(&mut out, cd_start);
    p16(&mut out, 0);
    out
}

#[test]
fn data_descriptors() {
    let text = b"descriptor ".repeat(40);
    let mut a = Raw::deflated(b"a.txt", &text);
    a.flags = 8;
    let mut b = Raw::stored(b"b.txt", b"stored with descriptor");
    b.flags = 8;
    let c = Raw::stored(b"c.txt", b"plain");
    let bytes = raw_archive(&[a, b, c]);
    let mut z = open(&bytes);
    assert_eq!(z.entries()[0].flags() & 8, 8);
    assert_eq!(z.read(0, 1000).unwrap(), text);
    assert_eq!(z.read(1, 1000).unwrap(), b"stored with descriptor");
    assert_eq!(z.read(2, 1000).unwrap(), b"plain");
}

#[test]
fn encrypted_and_unsupported_methods_are_refused() {
    let mut enc = Raw::stored(b"secret.txt", b"xxxxxxxxxxxx");
    enc.flags = 1;
    let mut bzip2 = Raw::stored(b"b.bz2", b"BZh9");
    bzip2.method = 12;
    let mut lzma = Raw::stored(b"c.lzma", b"....");
    lzma.method = 14;
    let bytes = raw_archive(&[enc, bzip2, lzma]);
    let mut z = open(&bytes);
    assert!(z.entries()[0].is_encrypted());
    assert!(matches!(z.read(0, 100), Err(Error::Encrypted { index: 0 })));
    assert!(matches!(
        z.read(1, 100),
        Err(Error::UnsupportedMethod {
            index: 1,
            method: 12
        })
    ));
    assert!(matches!(
        z.read(2, 100),
        Err(Error::UnsupportedMethod {
            index: 2,
            method: 14
        })
    ));
    let dir = tmp_dir("encrypted");
    assert!(z.extract_to(&dir, &ExtractLimits::default()).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unicode_path_extra_and_attributes() {
    let raw_name = b"na\x8Bve.txt"; // CP437 "naïve.txt"
    let up = |name: &str, crc: u32| {
        let mut x = vec![0x75, 0x70];
        x.extend_from_slice(&(5 + name.len() as u16).to_le_bytes());
        x.push(1);
        x.extend_from_slice(&crc.to_le_bytes());
        x.extend_from_slice(name.as_bytes());
        x
    };
    let mut a = Raw::stored(raw_name, b"a");
    a.central_extra = up("naïve-ünïcode.txt", dl_hash::crc32(raw_name));
    let mut b = Raw::stored(raw_name, b"b");
    b.central_extra = up("stale.txt", 1234);
    let mut dos_dir = Raw::stored(b"dosdir", b"");
    dos_dir.external = 0x10;
    let mut unix_dir = Raw::stored(b"unixdir", b"");
    unix_dir.made_by = 0x0314;
    unix_dir.external = 0o040_755 << 16;
    let mut exe = Raw::stored(b"run.sh", b"#!/bin/sh\n");
    exe.made_by = 0x0314;
    exe.external = 0o100_755 << 16;
    let mut bad_utf8 = Raw::stored(b"bad\xFF\xFE.txt", b"");
    bad_utf8.flags = 0x800;
    let bytes = raw_archive(&[a, b, dos_dir, unix_dir, exe, bad_utf8]);
    let z = open(&bytes);
    let e = z.entries();
    assert_eq!(e[0].name(), "naïve-ünïcode.txt");
    assert_eq!(e[1].name(), "naïve.txt");
    assert!(e[2].is_dir() && e[2].unix_mode().is_none());
    assert!(e[3].is_dir());
    assert!(!e[4].is_dir());
    assert_eq!(e[4].unix_mode(), Some(0o100_755));
    assert_eq!(e[5].name(), "bad\u{FFFD}\u{FFFD}.txt");
}

#[test]
fn malformed_archives_are_refused() {
    assert!(matches!(
        ZipArchive::new(Cursor::new(&[][..])),
        Err(Error::NotZip)
    ));
    assert!(matches!(
        ZipArchive::new(Cursor::new(&[0u8; 100][..])),
        Err(Error::NotZip)
    ));
    let good = write_specs(&mixed_specs(false), false);
    let n = good.len();
    // Entry count larger than the directory.
    let mut b = good.clone();
    b[n - 12] = 9;
    b[n - 14] = 9;
    assert!(matches!(
        ZipArchive::new(Cursor::new(&b)),
        Err(Error::Malformed { .. })
    ));
    // Central directory offset past its end record.
    let mut b = good.clone();
    b[n - 3] = 0x7F;
    assert!(matches!(
        ZipArchive::new(Cursor::new(&b)),
        Err(Error::Malformed { .. })
    ));
    // Multi-disk.
    let mut b = good.clone();
    b[n - 18] = 1;
    assert!(matches!(
        ZipArchive::new(Cursor::new(&b)),
        Err(Error::MultiDisk)
    ));
    // ZIP64 markers without ZIP64 records.
    let mut b = good.clone();
    b[n - 6..n - 2].copy_from_slice(&[0xFF; 4]);
    assert!(matches!(
        ZipArchive::new(Cursor::new(&b)),
        Err(Error::Malformed { .. })
    ));
    // A local header offset pointing elsewhere.
    let mut b = good.clone();
    let cd = find(&good, b"PK\x01\x02");
    b[cd + 42] = 3;
    let mut z = ZipArchive::new(Cursor::new(&b)).unwrap();
    assert!(matches!(z.read(0, 10), Err(Error::Malformed { .. })));
}

/// Opens `b` and reads every entry; only checks that nothing panics.
fn exercise(b: &[u8]) -> usize {
    let Ok(mut z) = ZipArchive::new(Cursor::new(b)) else {
        return 0;
    };
    let mut ok = 0;
    for i in 0..z.entries().len() {
        let e = &z.entries()[i];
        let _ = (
            e.is_dir(),
            e.enclosed_name(),
            e.modified().to_unix(),
            e.unix_mode(),
        );
        if z.read(i, 1 << 20).is_ok() {
            ok += 1;
        }
    }
    let _ = z.read(usize::MAX, 1);
    ok
}

#[test]
fn truncated_archives_never_panic() {
    for force in [false, true] {
        let b = write_specs(&small_specs(), force);
        assert_eq!(exercise(&b), 5);
        for cut in 0..b.len() {
            exercise(&b[..cut]);
            if cut % 7 == 0 {
                exercise(&b[cut..]);
            }
        }
    }
}

#[test]
fn random_mutations_never_panic() {
    let mut next = xorshift(0x1234_5678_9ABC_DEF1);
    for force in [false, true] {
        let good = write_specs(&small_specs(), force);
        let tail = good.len().min(400);
        for round in 0..3000 {
            let mut b = good.clone();
            for _ in 0..1 + next() % 4 {
                // Half of the changes hit the central directory and end records.
                let at = if round % 2 == 0 {
                    next() as usize % b.len()
                } else {
                    b.len() - 1 - next() as usize % tail
                };
                b[at] = match next() % 4 {
                    0 => 0xFF,
                    1 => 0,
                    _ => next() as u8,
                };
            }
            exercise(&b);
        }
    }
    let raw = raw_archive(&[
        Raw::deflated(b"a", &b"abc".repeat(50)),
        Raw::stored(b"b", b"x"),
    ]);
    for _ in 0..3000 {
        let mut b = raw.clone();
        let at = next() as usize % b.len();
        b[at] = next() as u8;
        exercise(&b);
    }
}

#[test]
fn reads_dotnet_archives() {
    let sets: [(&str, &[u8], &str); 4] = [
        (
            "deflate",
            include_bytes!("../testdata/dotnet_deflate.zip"),
            include_str!("../testdata/dotnet_deflate.txt"),
        ),
        (
            "stored",
            include_bytes!("../testdata/dotnet_stored.zip"),
            include_str!("../testdata/dotnet_stored.txt"),
        ),
        (
            "unseekable",
            include_bytes!("../testdata/dotnet_unseekable.zip"),
            include_str!("../testdata/dotnet_unseekable.txt"),
        ),
        (
            "cp437",
            include_bytes!("../testdata/dotnet_cp437.zip"),
            include_str!("../testdata/dotnet_cp437.txt"),
        ),
    ];
    for (what, zip, manifest) in sets {
        if what == "cp437" {
            // Names are CP437 bytes without the UTF-8 flag.
            let z = open(zip);
            assert_eq!(z.entries()[0].raw_name(), b"caf\x82_\xA5.txt");
            assert_eq!(z.entries()[1].raw_name(), b"\x8E/\x94.txt");
            assert!(z.entries().iter().all(|e| e.flags() & 0x800 == 0));
            let mut z = open(zip);
            assert_eq!(z.read_by_name("café_Ñ.txt", 100).unwrap(), b"cp437\n");
            assert_eq!(z.read_by_name("Ä/ö.txt", 100).unwrap(), b"x");
            assert_eq!(fmt_time(z.entries()[0].modified()), "2024-05-06T07:08:10");
            continue;
        }
        check_manifest(zip, manifest, what);
        let z = open(zip);
        let mut files = z.entries().iter().filter(|e| !e.is_dir());
        match what {
            "stored" => assert!(z.entries().iter().all(|e| e.method() == 0)),
            "deflate" => assert_eq!(z.entries()[1].method(), 8),
            // .NET writes data descriptors for every non-empty file on unseekable output.
            _ => assert!(
                files.all(|e| e.size() == 0 || e.flags() & 8 != 0),
                "data descriptors"
            ),
        }
        let dir = tmp_dir(&format!("dotnet-{what}"));
        open(zip)
            .extract_to(&dir, &ExtractLimits::default())
            .unwrap();
        assert!(dir.join("empty_dir").is_dir());
        assert_eq!(std::fs::read(dir.join("data/empty.txt")).unwrap(), b"");
        assert!(dir.join("Tài liệu/hóa đơn tháng 5.json").is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn reads_dotnet_zip64_archive() {
    let gz = include_bytes!("../testdata/dotnet_zip64.zip.gz");
    let b = dl_flate::gzip::decompress(gz, 64 << 20).unwrap();
    let n = b.len();
    assert_eq!(&b[n - 42..n - 38], b"PK\x06\x07", "ZIP64 locator");
    let mut z = open(&b);
    assert_eq!(z.entries().len(), 65_600);
    for (i, e) in z.entries().iter().enumerate() {
        assert_eq!(e.name(), format!("f/{i}"));
    }
    for i in [0, 1, 4096, 65_535, 65_536, 65_599] {
        let want = if i % 4096 == 0 {
            i.to_string()
        } else {
            "x".into()
        };
        assert_eq!(z.read(i, 10).unwrap(), want.as_bytes(), "{i}");
    }
    assert_eq!(
        fmt_time(z.entries()[65_599].modified()),
        "2024-05-06T07:08:10"
    );
}

/// Writes archives and manifests for `dotnet run testdata/gen_dotnet_zips.cs -- verify
/// <dir>` into `$DL_ZIP_EXPORT` (default `<temp>/dl-zip-export`).
#[test]
#[ignore = "writes archives for the .NET check in testdata/gen_dotnet_zips.cs"]
fn export_for_dotnet() {
    let dir = std::env::var_os("DL_ZIP_EXPORT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("dl-zip-export"));
    std::fs::create_dir_all(&dir).unwrap();
    let mut sets: Vec<(&str, Vec<Spec>, bool)> = vec![
        ("dlzip_mixed", mixed_specs(true), false),
        ("dlzip_zip64_forced", mixed_specs(false), true),
    ];
    let many: Vec<Spec> = (0..70_000)
        .map(|i| {
            (
                format!("many/{i}"),
                i.to_string().into_bytes(),
                Some(opts(Method::Stored)),
            )
        })
        .collect();
    sets.push(("dlzip_many_entries", many, false));
    for (name, specs, force) in &sets {
        let b = write_specs(specs, *force);
        let m = manifest(specs);
        check_manifest(&b, &m, name);
        std::fs::write(dir.join(format!("{name}.zip")), &b).unwrap();
        std::fs::write(dir.join(format!("{name}.txt")), m).unwrap();
    }
    std::println!("wrote {} archives to {}", sets.len(), dir.display());
}
