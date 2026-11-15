//! `cargo bench -p dl-rand`
use dl_bench::Bench;
use dl_rand::Rng;

fn main() {
    let mut b = Bench::from_args();
    let mut rng = Rng::from_seed(1);
    b.run("xoshiro next_u64", || rng.next_u64());
    b.run("xoshiro below(1000)", || rng.below(1000));
    let mut buf = vec![0u8; 64 * 1024];
    b.throughput("xoshiro fill 64 KiB", buf.len() as u64, || {
        rng.fill_bytes(&mut buf)
    });
    let mut small = [0u8; 32];
    b.run("os fill 32 B", || dl_rand::fill(&mut small).unwrap());
    b.throughput("os fill 64 KiB", buf.len() as u64, || {
        dl_rand::fill(&mut buf).unwrap()
    });
}
