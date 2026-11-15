//! `cargo bench -p dl-encoding`
use dl_bench::Bench;
use dl_encoding::{base32, base64, hex};

fn main() {
    let mut b = Bench::from_args();
    let data: Vec<u8> = (0..64 * 1024).map(|i| (i * 31 + 7) as u8).collect();
    let n = data.len() as u64;

    let b64 = base64::STANDARD.encode(&data);
    b.throughput("base64 encode 64 KiB", n, || base64::STANDARD.encode(&data));
    b.throughput("base64 decode 64 KiB", n, || {
        base64::STANDARD.decode(&b64).unwrap()
    });

    let h = hex::encode(&data);
    b.throughput("hex encode 64 KiB", n, || hex::encode(&data));
    b.throughput("hex decode 64 KiB", n, || hex::decode(&h).unwrap());

    let b32 = base32::STANDARD.encode(&data);
    b.throughput("base32 encode 64 KiB", n, || base32::STANDARD.encode(&data));
    b.throughput("base32 decode 64 KiB", n, || {
        base32::STANDARD.decode(&b32).unwrap()
    });
}
