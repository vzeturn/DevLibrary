//! `cargo bench -p dl-uuid`
use dl_bench::Bench;
use dl_uuid::{Uuid, V7Generator};
use std::hint::black_box;

fn main() {
    let mut b = Bench::from_args();
    b.run("new_v4", Uuid::new_v4);
    b.run("now_v7", Uuid::now_v7);
    let mut generator = V7Generator::new();
    b.run("V7Generator::generate (own, no lock)", || {
        generator.generate()
    });
    let mut generator = V7Generator::new();
    let mut ms = 1_700_000_000_000u64;
    b.run("V7Generator::generate_from (no OS calls)", || {
        ms += 1;
        generator.generate_from(black_box(ms), [0x5A; 10])
    });
    b.run("new_v5 (DNS, www.example.com)", || {
        Uuid::new_v5(&Uuid::NAMESPACE_DNS, black_box("www.example.com"))
    });

    let hyphenated = "67e55044-10b1-426f-9247-bb680e5fe0c8";
    let simple = "67e5504410b1426f9247bb680e5fe0c8";
    let urn = "urn:uuid:67E55044-10B1-426F-9247-BB680E5FE0C8";
    b.run("parse hyphenated", || Uuid::parse(black_box(hyphenated)));
    b.run("parse simple", || Uuid::parse(black_box(simple)));
    b.run("parse urn (uppercase)", || Uuid::parse(black_box(urn)));

    let id = Uuid::parse(hyphenated).unwrap();
    b.run("to_string", || black_box(id).to_string());
    b.run("to_string_upper", || black_box(id).to_string_upper());
    let mut buf = [0u8; 36];
    b.run("encode_into (no alloc)", || {
        black_box(id).encode_into(&mut buf).len()
    });
}
