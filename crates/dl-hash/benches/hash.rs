//! `cargo bench -p dl-hash`
use dl_bench::Bench;
use dl_hash::{Digest, Hmac, Md5, Sha1, Sha256, Sha512, crc32};

fn main() {
    let mut b = Bench::from_args();
    let data = vec![0x5au8; 64 * 1024];
    let n = data.len() as u64;
    b.throughput("sha1 64 KiB", n, || Sha1::digest(&data));
    b.throughput("sha256 64 KiB", n, || Sha256::digest(&data));
    b.throughput("sha512 64 KiB", n, || Sha512::digest(&data));
    b.throughput("md5 64 KiB", n, || Md5::digest(&data));
    b.throughput("crc32 64 KiB", n, || crc32(&data));
    b.run("hmac-sha256 64 B", || {
        Hmac::<Sha256>::mac(b"secret-key", &data[..64])
    });
}
