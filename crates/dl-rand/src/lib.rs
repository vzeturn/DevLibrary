#![doc = include_str!("../README.md")]

mod os;
mod xoshiro;

use std::cell::RefCell;
use std::fmt;

pub use xoshiro::{Rng, Uniform};

/// Failure to obtain entropy from the operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The OS call failed with this error code (`errno` on Unix).
    Os(i32),
    /// This target has no known entropy source.
    Unsupported,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Os(code) => write!(f, "OS random source failed (code {code})"),
            Error::Unsupported => f.write_str("no OS random source on this target"),
        }
    }
}

impl std::error::Error for Error {}

/// Fills `buf` with cryptographically secure random bytes from the OS.
pub fn fill(buf: &mut [u8]) -> Result<(), Error> {
    if buf.is_empty() {
        return Ok(());
    }
    os::fill(buf)
}

/// Returns `N` cryptographically secure random bytes.
pub fn secure_bytes<const N: usize>() -> Result<[u8; N], Error> {
    let mut b = [0u8; N];
    fill(&mut b)?;
    Ok(b)
}

/// Returns a cryptographically secure random `u64`.
pub fn secure_u64() -> Result<u64, Error> {
    secure_bytes::<8>().map(u64::from_le_bytes)
}

/// Returns a cryptographically secure integer uniformly distributed in `0..n`.
///
/// # Panics
/// Panics if `n == 0`.
pub fn secure_below(n: u64) -> Result<u64, Error> {
    assert!(n > 0, "secure_below(0) has no valid output");
    // Rejection sampling: discard values from the incomplete last bucket.
    let zone = u64::MAX - (u64::MAX % n + 1) % n;
    loop {
        let v = secure_u64()?;
        if v <= zone {
            return Ok(v % n);
        }
    }
}

thread_local! {
    static THREAD_RNG: RefCell<Rng> = RefCell::new(Rng::from_os().unwrap_or_else(|_| Rng::from_seed(fallback_seed())));
}

/// Seed used only if the OS source fails. Mixes the clock and a stack address, which is
/// enough to make threads diverge but is not secret.
fn fallback_seed() -> u64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let local = 0u8;
    t ^ (&local as *const u8 as u64).rotate_left(32)
}

/// Runs `f` with this thread's fast generator.
pub fn with_thread_rng<R>(f: impl FnOnce(&mut Rng) -> R) -> R {
    THREAD_RNG.with(|r| f(&mut r.borrow_mut()))
}

/// Fast random `u64` from the thread-local generator.
pub fn u64() -> u64 {
    with_thread_rng(Rng::next_u64)
}

/// Fast random `f64` in `[0, 1)` from the thread-local generator.
pub fn f64() -> f64 {
    with_thread_rng(Rng::next_f64)
}

/// Fast random value in `range` (e.g. `range(1..=6)`) from the thread-local generator.
///
/// # Panics
/// Panics if the range is empty.
pub fn range<T: Uniform>(range: impl std::ops::RangeBounds<T>) -> T {
    with_thread_rng(|r| r.range(range))
}

/// Shuffles a slice in place with the thread-local generator.
pub fn shuffle<T>(items: &mut [T]) {
    with_thread_rng(|r| r.shuffle(items));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_bytes_are_not_constant() {
        let a = secure_bytes::<32>().unwrap();
        let b = secure_bytes::<32>().unwrap();
        assert_ne!(a, b);
        assert_ne!(a, [0; 32]);
        let mut big = vec![0u8; 200_000];
        fill(&mut big).unwrap();
        // 200 000 zero bytes in a row would be astronomically unlikely.
        assert!(big.iter().any(|&b| b != 0));
        assert!(big[150_000..].iter().any(|&b| b != 0));
    }

    #[test]
    fn secure_below_in_range() {
        for n in [1, 2, 3, 7, 10, 1_000_000, u64::MAX] {
            for _ in 0..50 {
                assert!(secure_below(n).unwrap() < n);
            }
        }
    }

    #[test]
    fn thread_helpers() {
        for _ in 0..1000 {
            let v = range(10..20);
            assert!((10..20).contains(&v));
            let f = f64();
            assert!((0.0..1.0).contains(&f));
        }
        let mut v: Vec<u32> = (0..100).collect();
        shuffle(&mut v);
        v.sort_unstable();
        assert_eq!(v, (0..100).collect::<Vec<_>>());
        assert_ne!(u64(), u64());
    }
}
