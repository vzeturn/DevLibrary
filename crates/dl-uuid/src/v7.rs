//! Monotonic version 7 generation (RFC 9562 §6.2, Method 1).

#[cfg(feature = "std")]
use crate::Error;
use crate::Uuid;
use crate::generate::{UNIX_TS_MAX, with_version};

/// Width of the per-millisecond counter: the 12 bits of `rand_a` plus the 30 most
/// significant bits of `rand_b`.
const COUNTER_BITS: u32 = 42;
const COUNTER_MAX: u64 = (1 << COUNTER_BITS) - 1;

/// A generator of version 7 UUIDs that are strictly increasing, even when many are
/// created in the same millisecond or the clock goes backwards.
///
/// # Algorithm
///
/// RFC 9562 §6.2 **Method 1** (fixed-bit-length dedicated counter), laid out as:
///
/// | Bits | Field | Content |
/// |---|---|---|
/// | 48 | `unix_ts_ms` | Unix time in milliseconds |
/// | 4 | `ver` | `0111` |
/// | 12 | `rand_a` | counter, bits 41–30 |
/// | 2 | `var` | `10` |
/// | 30 | `rand_b` (high) | counter, bits 29–0 |
/// | 32 | `rand_b` (low) | random, new for every UUID |
///
/// - When the timestamp is greater than the previous one, the counter is reseeded with
///   41 random bits; its top bit starts at 0 (the "counter rollover guard" of §6.2), so
///   at least 2^41 increments fit in one millisecond.
/// - When the timestamp equals the previous one, or is smaller (clock moved backwards),
///   the previous timestamp is kept and the counter is incremented by one.
/// - If the counter overflows, the timestamp is advanced by 1 ms and the counter is
///   reseeded (§6.2 allows running ahead of the clock).
///
/// So each UUID is greater than the previous one from the same generator, as long as the
/// 48-bit timestamp does not overflow (year 10889). After a clock rollback, UUIDs carry
/// the last timestamp seen until the clock catches up.
///
/// The low 32 bits are random in every UUID, but consecutive UUIDs in the same
/// millisecond have consecutive counters, so a v7 UUID is partly predictable and
/// reveals its creation time. Use v4 for secrets and tokens.
///
/// # Usage
///
/// `Uuid::now_v7` (`std` feature) uses one process-wide generator. Create your own to choose the
/// clock or the random source (e.g. on `wasm32-unknown-unknown` or in `no_std`), or to
/// avoid sharing a lock between threads. A `V7Generator` is plain data: wrap it in a
/// `Mutex` to share it.
///
/// ```
/// use dl_uuid::V7Generator;
///
/// let mut generator = V7Generator::new();
/// // Timestamp and random bytes from the caller: works in no_std and in the browser.
/// let a = generator.generate_from(1_700_000_000_000, [0x11; 10]);
/// let b = generator.generate_from(1_700_000_000_000, [0x22; 10]); // same millisecond
/// let c = generator.generate_from(1_699_999_999_000, [0x33; 10]); // clock went back
/// assert!(a < b && b < c);
/// assert_eq!(c.v7_timestamp_ms(), Some(1_700_000_000_000));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V7Generator {
    /// Timestamp of the last UUID (masked to 48 bits; may run ahead of the clock).
    last_ms: u64,
    /// Counter of the last UUID, at most `COUNTER_MAX`.
    counter: u64,
    /// `false` until the first UUID.
    started: bool,
}

impl Default for V7Generator {
    fn default() -> Self {
        Self::new()
    }
}

impl V7Generator {
    /// A generator that has not issued any UUID yet.
    pub const fn new() -> Self {
        Self {
            last_ms: 0,
            counter: 0,
            started: false,
        }
    }

    /// The next UUID, from a Unix timestamp in milliseconds and 10 random bytes supplied
    /// by the caller. Pure computation: works in `no_std` and on every target.
    ///
    /// `random[0..6]` seeds the counter when a new millisecond starts (the low 41 bits
    /// of those 48 bits are used), and `random[6..10]` fills the 32 random low bits. For
    /// unguessable UUIDs the bytes must come from a CSPRNG. Timestamps wider than 48
    /// bits keep their low 48 bits.
    pub fn generate_from(&mut self, unix_ms: u64, random: [u8; 10]) -> Uuid {
        let unix_ms = unix_ms & UNIX_TS_MAX;
        if !self.started || unix_ms > self.last_ms {
            self.last_ms = unix_ms;
            self.counter = seed(&random);
        } else if self.counter < COUNTER_MAX {
            // Same millisecond, or the clock went backwards: keep the last timestamp.
            self.counter += 1;
        } else {
            // Counter exhausted: borrow the next millisecond.
            self.last_ms += 1;
            self.counter = seed(&random);
        }
        self.started = true;
        let tail = [random[6], random[7], random[8], random[9]];
        layout(self.last_ms, self.counter, tail)
    }

    /// The next UUID for the given Unix timestamp in milliseconds, with random bits from
    /// the OS CSPRNG.
    ///
    /// This is the method to use on `wasm32-unknown-unknown`, where the system clock is
    /// not available: pass `Date.now()` from JavaScript.
    ///
    /// # Errors
    /// [`Error::RandomSource`] if the OS random number generator fails.
    #[cfg(feature = "std")]
    pub fn try_generate_at(&mut self, unix_ms: u64) -> Result<Uuid, Error> {
        let random = dl_rand::secure_bytes::<10>().map_err(Error::from_rand)?;
        Ok(self.generate_from(unix_ms, random))
    }

    /// The next UUID for the current system time, with random bits from the OS CSPRNG.
    ///
    /// Not available on `wasm32-unknown-unknown`; see [`try_generate_at`](Self::try_generate_at).
    ///
    /// # Errors
    /// [`Error::RandomSource`] if the OS random number generator fails.
    #[cfg(all(
        feature = "std",
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    pub fn try_generate(&mut self) -> Result<Uuid, Error> {
        self.try_generate_at(crate::generate::unix_ms_now())
    }

    /// Infallible form of [`try_generate`](Self::try_generate).
    ///
    /// ```
    /// use dl_uuid::V7Generator;
    /// let mut generator = V7Generator::new();
    /// let ids: Vec<_> = (0..1000).map(|_| generator.generate()).collect();
    /// assert!(ids.windows(2).all(|w| w[0] < w[1]));
    /// ```
    ///
    /// # Panics
    /// Only if the OS random number generator fails.
    #[cfg(all(
        feature = "std",
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    pub fn generate(&mut self) -> Uuid {
        match self.try_generate() {
            Ok(id) => id,
            Err(e) => panic!("dl-uuid: cannot generate a v7 UUID: {e}"),
        }
    }
}

/// A fresh counter: 41 random bits, top counter bit clear.
const fn seed(random: &[u8; 10]) -> u64 {
    let r = u64::from_be_bytes([
        0, 0, random[0], random[1], random[2], random[3], random[4], random[5],
    ]);
    r & (COUNTER_MAX >> 1)
}

/// Builds the UUID from the timestamp, the 42-bit counter and the 32-bit tail.
const fn layout(unix_ms: u64, counter: u64, tail: [u8; 4]) -> Uuid {
    let t = (unix_ms & UNIX_TS_MAX).to_be_bytes();
    // Counter bits 41..30 go to rand_a (byte 6 low nibble, byte 7), bits 29..0 to the
    // top of rand_b (byte 8 low 6 bits, bytes 9..11). `with_version` sets the rest.
    let c = counter.to_be_bytes();
    let rand_a = (counter >> 30) as u16;
    let [a0, a1] = rand_a.to_be_bytes();
    with_version(
        [
            t[2],
            t[3],
            t[4],
            t[5],
            t[6],
            t[7],
            a0,
            a1,
            c[4] & 0x3F,
            c[5],
            c[6],
            c[7],
            tail[0],
            tail[1],
            tail[2],
            tail[3],
        ],
        7,
    )
}

#[cfg(all(
    feature = "std",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
static GLOBAL: std::sync::Mutex<V7Generator> = std::sync::Mutex::new(V7Generator::new());

/// `Uuid::try_now_v7`: the process-wide generator.
#[cfg(all(
    feature = "std",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
pub(crate) fn global_now() -> Result<Uuid, Error> {
    // Random bytes and the clock are read outside the lock. A thread that read an older
    // timestamp but takes the lock later is handled like a clock rollback, so the
    // order of UUIDs still follows the order in which the lock is taken.
    let random = dl_rand::secure_bytes::<10>().map_err(Error::from_rand)?;
    let now = crate::generate::unix_ms_now();
    // The generator state is valid after any panic (no panic can happen while it is
    // half-updated), so a poisoned lock is safe to reuse.
    let mut generator = GLOBAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok(generator.generate_from(now, random))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Variant, Version};
    use alloc::string::ToString;

    /// Extracts the 42-bit counter from rand_a and the top 30 bits of rand_b.
    fn counter(id: &Uuid) -> u64 {
        let v = id.as_u128();
        let rand_a = ((v >> 64) & 0x0FFF) as u64;
        let rand_b_hi = ((v >> 32) & 0x3FFF_FFFF) as u64;
        rand_a << 30 | rand_b_hi
    }

    #[test]
    fn layout_bits() {
        let id = layout(0x017F22E279B0, COUNTER_MAX, [0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(id.to_string(), "017f22e2-79b0-7fff-bfff-ffffdeadbeef");
        assert_eq!(counter(&id), COUNTER_MAX);
        let id = layout(1, 0, [0; 4]);
        assert_eq!(id.to_string(), "00000000-0001-7000-8000-000000000000");
        let id = layout(1, 1 << 30, [0; 4]);
        assert_eq!(id.to_string(), "00000000-0001-7001-8000-000000000000");
        let id = layout(1, (1 << 30) - 1, [0; 4]);
        assert_eq!(id.to_string(), "00000000-0001-7000-bfff-ffff00000000");
    }

    #[test]
    fn seeding_and_increment() {
        let mut g = V7Generator::new();
        let r = [0xFF; 10];
        let a = g.generate_from(5, r);
        // Seed keeps 41 bits: the top counter bit is clear.
        assert_eq!(counter(&a), COUNTER_MAX >> 1);
        assert_eq!(a.v7_timestamp_ms(), Some(5));
        let b = g.generate_from(5, [0; 10]);
        assert_eq!(counter(&b), (COUNTER_MAX >> 1) + 1);
        assert_eq!(
            b.as_u128() & 0xFFFF_FFFF,
            0,
            "tail comes from the new random bytes"
        );
        // New millisecond: reseeded from random[0..6].
        let c = g.generate_from(6, [0, 0, 0, 0, 0, 7, 1, 2, 3, 4]);
        assert_eq!(counter(&c), 7);
        assert_eq!(c.as_u128() & 0xFFFF_FFFF, 0x0102_0304);
        assert!(a < b && b < c);
    }

    #[test]
    fn clock_backwards_keeps_order() {
        let mut g = V7Generator::new();
        let a = g.generate_from(1_000, [0; 10]);
        let b = g.generate_from(400, [0xFF; 10]);
        let c = g.generate_from(999, [0xFF; 10]);
        let d = g.generate_from(1_001, [0; 10]);
        assert!(a < b && b < c && c < d);
        assert_eq!(b.v7_timestamp_ms(), Some(1_000));
        assert_eq!(c.v7_timestamp_ms(), Some(1_000));
        assert_eq!(d.v7_timestamp_ms(), Some(1_001));
    }

    #[test]
    fn counter_overflow_borrows_next_millisecond() {
        let mut g = V7Generator {
            last_ms: 42,
            counter: COUNTER_MAX - 1,
            started: true,
        };
        let a = g.generate_from(42, [0xFF; 10]);
        assert_eq!(counter(&a), COUNTER_MAX);
        let b = g.generate_from(42, [0xFF; 10]);
        assert_eq!(b.v7_timestamp_ms(), Some(43));
        assert_eq!(counter(&b), COUNTER_MAX >> 1);
        assert!(a < b);
        // The real clock reaching 43 continues from the borrowed state.
        let c = g.generate_from(43, [0; 10]);
        assert!(b < c);
        assert_eq!(c.v7_timestamp_ms(), Some(43));
    }

    #[test]
    fn deterministic_monotonic_100k() {
        // 100,000 IDs over 25 simulated milliseconds, with a rollback in the middle.
        let mut g = V7Generator::new();
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut prev = Uuid::NIL;
        for i in 0..100_000u64 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let mut random = [0u8; 10];
            random[..8].copy_from_slice(&state.to_le_bytes());
            random[8..].copy_from_slice(&state.to_be_bytes()[..2]);
            let ms = if (50_000..60_000).contains(&i) {
                1_000
            } else {
                1_000 + i / 4_000
            };
            let id = g.generate_from(ms, random);
            assert!(id > prev, "#{i}: {id} <= {prev}");
            assert_eq!(id.version(), Some(Version::V7));
            assert_eq!(id.variant(), Variant::Rfc9562);
            prev = id;
        }
    }

    #[cfg(all(
        feature = "std",
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    #[test]
    fn now_v7_monotonic_100k() {
        let start = crate::generate::unix_ms_now();
        let mut prev = Uuid::now_v7();
        for _ in 0..100_000 {
            let id = Uuid::now_v7();
            assert!(id > prev, "{id} <= {prev}");
            assert_eq!(id.version(), Some(Version::V7));
            assert_eq!(id.variant(), Variant::Rfc9562);
            prev = id;
        }
        let ts = prev.v7_timestamp_ms().unwrap();
        assert!(ts >= start && ts <= crate::generate::unix_ms_now() + 1);
    }

    #[cfg(all(
        feature = "std",
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    #[test]
    fn now_v7_monotonic_across_threads() {
        // Each thread's IDs are strictly increasing, and no two threads get the same ID.
        let threads: std::vec::Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    (0..20_000)
                        .map(|_| Uuid::now_v7())
                        .collect::<std::vec::Vec<_>>()
                })
            })
            .collect();
        let mut all = std::collections::HashSet::new();
        for t in threads {
            let ids = t.join().unwrap();
            assert!(ids.windows(2).all(|w| w[0] < w[1]));
            all.extend(ids);
        }
        assert_eq!(all.len(), 80_000);
        let mut generator = V7Generator::default();
        let a = generator.generate();
        let b = generator.try_generate().unwrap();
        let c = generator.try_generate_at(0).unwrap();
        assert!(a < b && b < c);
    }
}
