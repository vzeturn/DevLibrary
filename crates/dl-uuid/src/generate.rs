//! UUID construction for each version.

#[cfg(feature = "std")]
use crate::Error;
use crate::Uuid;
use dl_hash::{Digest, Md5, Sha1};

/// Sets the 4-bit version field and the `10` variant bits (RFC 9562 §4.1, §4.2).
pub(crate) const fn with_version(mut b: [u8; 16], version: u8) -> Uuid {
    b[6] = (b[6] & 0x0F) | (version << 4);
    b[8] = (b[8] & 0x3F) | 0x80;
    Uuid::from_bytes(b)
}

/// Largest value of the 48-bit `unix_ts_ms` field.
pub(crate) const UNIX_TS_MAX: u64 = (1 << 48) - 1;

/// Unix time in milliseconds from the system clock (0 if the clock is before 1970).
#[cfg(all(
    feature = "std",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
pub(crate) fn unix_ms_now() -> u64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

impl Uuid {
    /// Version 3: MD5 of `namespace` followed by `name` (RFC 9562 §5.3).
    ///
    /// Deterministic: the same namespace and name always give the same UUID. Prefer
    /// [`new_v5`](Self::new_v5) unless another system expects v3.
    ///
    /// ```
    /// use dl_uuid::Uuid;
    /// // RFC 9562 Appendix A.2; Python: uuid.uuid3(uuid.NAMESPACE_DNS, "www.example.com")
    /// let id = Uuid::new_v3(&Uuid::NAMESPACE_DNS, "www.example.com");
    /// assert_eq!(id.to_string(), "5df41881-3aed-3515-88a7-2f4a814cf09e");
    /// ```
    pub fn new_v3(namespace: &Uuid, name: impl AsRef<[u8]>) -> Uuid {
        let digest = Md5::default()
            .chain(namespace.as_bytes())
            .chain(name)
            .finalize();
        with_version(digest, 3)
    }

    /// Version 5: the first 16 bytes of SHA-1 of `namespace` followed by `name`
    /// (RFC 9562 §5.5). Strings are hashed as their UTF-8 bytes, as in Python, .NET
    /// and Go.
    ///
    /// Deterministic: the same namespace and name always give the same UUID.
    ///
    /// ```
    /// use dl_uuid::Uuid;
    /// // RFC 9562 Appendix A.4; Python: uuid.uuid5(uuid.NAMESPACE_DNS, "www.example.com")
    /// let id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, "www.example.com");
    /// assert_eq!(id.to_string(), "2ed6657d-e927-568b-95e1-2665a8aea6a2");
    /// ```
    pub fn new_v5(namespace: &Uuid, name: impl AsRef<[u8]>) -> Uuid {
        let digest = Sha1::default()
            .chain(namespace.as_bytes())
            .chain(name)
            .finalize();
        let mut b = [0u8; 16];
        b.copy_from_slice(&digest[..16]);
        with_version(b, 5)
    }

    /// Version 4 from 16 caller-supplied random bytes: sets the version and variant
    /// bits and keeps the other 122 bits (RFC 9562 §5.4).
    ///
    /// Use this with your own random source (for example in `no_std`). The bytes must
    /// come from a cryptographically secure generator if the UUID must be unguessable.
    ///
    /// ```
    /// use dl_uuid::{Uuid, Version};
    /// // RFC 9562 Appendix A.3: random 919108F752D133205BACF847DB4148A8.
    /// let id = Uuid::v4_from(0x919108F752D133205BACF847DB4148A8u128.to_be_bytes());
    /// assert_eq!(id.to_string(), "919108f7-52d1-4320-9bac-f847db4148a8");
    /// assert_eq!(id.version(), Some(Version::V4));
    /// ```
    pub const fn v4_from(random: [u8; 16]) -> Uuid {
        with_version(random, 4)
    }

    /// Version 7 from a Unix timestamp in milliseconds and 10 caller-supplied random
    /// bytes (RFC 9562 §5.7).
    ///
    /// Bytes 0–5 are the low 48 bits of `unix_ms` (big-endian); bytes 6–15 are
    /// `random`, then the version and variant bits overwrite 6 of those bits. No
    /// ordering is guaranteed between two UUIDs from the same millisecond; use a
    /// [`V7Generator`](crate::V7Generator) (or `Uuid::now_v7` with the `std` feature)
    /// for that.
    ///
    /// Works everywhere, including `no_std` and `wasm32-unknown-unknown`, where the
    /// caller supplies the time (e.g. `Date.now()`).
    ///
    /// ```
    /// use dl_uuid::Uuid;
    /// // RFC 9562 Appendix A.6: unix_ts_ms 0x017F22E279B0, rand_a 0xCC3, rand_b 0x18C4DC0C0C07398F.
    /// let random = [0x0C, 0xC3, 0x18, 0xC4, 0xDC, 0x0C, 0x0C, 0x07, 0x39, 0x8F];
    /// let id = Uuid::v7_from(0x017F22E279B0, random);
    /// assert_eq!(id.to_string_upper(), "017F22E2-79B0-7CC3-98C4-DC0C0C07398F");
    /// assert_eq!(id.v7_timestamp_ms(), Some(0x017F22E279B0));
    /// ```
    pub const fn v7_from(unix_ms: u64, random: [u8; 10]) -> Uuid {
        let t = (unix_ms & UNIX_TS_MAX).to_be_bytes();
        let r = random;
        with_version(
            [
                t[2], t[3], t[4], t[5], t[6], t[7], r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7],
                r[8], r[9],
            ],
            7,
        )
    }

    /// Version 8 (custom, RFC 9562 §5.8): keeps 122 application-defined bits and sets
    /// the version and variant bits.
    ///
    /// ```
    /// use dl_uuid::{Uuid, Version};
    /// // RFC 9562 Appendix B.1 (time-based example).
    /// let id = Uuid::v8_from(0x2489E9AD2EE20E000EC932D5F69181C0u128.to_be_bytes());
    /// assert_eq!(id.to_string(), "2489e9ad-2ee2-8e00-8ec9-32d5f69181c0");
    /// assert_eq!(id.version(), Some(Version::V8));
    /// ```
    pub const fn v8_from(custom: [u8; 16]) -> Uuid {
        with_version(custom, 8)
    }

    /// A random version 4 UUID from the operating system's CSPRNG.
    ///
    /// On `wasm32-unknown-unknown` the host must provide the `dl_rand.fill` import (see
    /// the `dl-rand` crate docs).
    ///
    /// ```
    /// use dl_uuid::{Uuid, Version};
    /// let id = Uuid::new_v4();
    /// assert_eq!(id.version(), Some(Version::V4));
    /// assert_ne!(id, Uuid::new_v4());
    /// ```
    ///
    /// # Panics
    /// Only if the OS random number generator fails, which does not happen on supported
    /// platforms in practice. Use [`try_new_v4`](Self::try_new_v4) to handle that case.
    #[cfg(feature = "std")]
    pub fn new_v4() -> Uuid {
        match Uuid::try_new_v4() {
            Ok(id) => id,
            Err(e) => panic!("dl-uuid: cannot generate a v4 UUID: {e}"),
        }
    }

    /// A random version 4 UUID from the operating system's CSPRNG.
    ///
    /// # Errors
    /// [`Error::RandomSource`] if the OS random number generator fails.
    #[cfg(feature = "std")]
    pub fn try_new_v4() -> Result<Uuid, Error> {
        dl_rand::secure_bytes::<16>()
            .map(Uuid::v4_from)
            .map_err(Error::from_rand)
    }

    /// A version 7 UUID for the current time, strictly greater than every UUID previously
    /// returned by `now_v7` in this process, from any thread.
    ///
    /// Uses one process-wide [`V7Generator`](crate::V7Generator) behind a mutex: the
    /// system clock in milliseconds, a 42-bit counter for IDs within the same
    /// millisecond, and 32 bits from the OS CSPRNG in every UUID. See the
    /// `V7Generator` docs for the exact layout and the clock-rollback behaviour.
    ///
    /// Not available on `wasm32-unknown-unknown`, where `std::time::SystemTime::now()`
    /// panics; use [`V7Generator::try_generate_at`](crate::V7Generator::try_generate_at)
    /// with a timestamp from JavaScript instead.
    ///
    /// ```
    /// use dl_uuid::Uuid;
    /// let a = Uuid::now_v7();
    /// let b = Uuid::now_v7();
    /// assert!(a < b);
    /// assert!(a.v7_timestamp_ms().unwrap() > 1_700_000_000_000);
    /// ```
    ///
    /// # Panics
    /// Only if the OS random number generator fails; see [`try_now_v7`](Self::try_now_v7).
    #[cfg(all(
        feature = "std",
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    pub fn now_v7() -> Uuid {
        match Uuid::try_now_v7() {
            Ok(id) => id,
            Err(e) => panic!("dl-uuid: cannot generate a v7 UUID: {e}"),
        }
    }

    /// Fallible form of [`now_v7`](Self::now_v7).
    ///
    /// # Errors
    /// [`Error::RandomSource`] if the OS random number generator fails.
    #[cfg(all(
        feature = "std",
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    pub fn try_now_v7() -> Result<Uuid, Error> {
        crate::v7::global_now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Variant, Version};
    use alloc::string::ToString;

    #[test]
    fn version_and_variant_bits_overwrite_only_six_bits() {
        for (fill, want6, want8) in [(0x00u8, 0x00u8, 0x80u8), (0xFF, 0x0F, 0xBF)] {
            for (v, id) in [
                (4, Uuid::v4_from([fill; 16])),
                (8, Uuid::v8_from([fill; 16])),
                (7, Uuid::v7_from(0, [fill; 10])),
            ] {
                let b = id.as_bytes();
                assert_eq!(b[6], want6 | (v << 4), "v{v}");
                assert_eq!(b[8], want8, "v{v}");
                assert_eq!(id.variant(), Variant::Rfc9562);
                for i in [7, 9, 10, 11, 12, 13, 14, 15] {
                    assert_eq!(b[i], fill);
                }
            }
        }
        assert_eq!(Uuid::v4_from([0; 16]).version(), Some(Version::V4));
    }

    #[test]
    fn v7_from_layout() {
        let id = Uuid::v7_from(0x0102_0304_0506, [0xAA; 10]);
        assert_eq!(id.to_string(), "01020304-0506-7aaa-aaaa-aaaaaaaaaaaa");
        // Timestamps wider than 48 bits keep their low 48 bits.
        let wrapped = Uuid::v7_from(u64::MAX, [0; 10]);
        assert_eq!(wrapped.v7_timestamp_ms(), Some(UNIX_TS_MAX));
        assert_eq!(Uuid::v7_from(1 << 48, [0; 10]).v7_timestamp_ms(), Some(0));
    }

    #[cfg(feature = "std")]
    #[test]
    fn new_v4_is_random_with_correct_bits() {
        let mut seen = std::collections::HashSet::new();
        let mut ones = [0u32; 128];
        const N: u32 = 2000;
        for _ in 0..N {
            let id = Uuid::new_v4();
            assert_eq!(id.version(), Some(Version::V4));
            assert_eq!(id.variant(), Variant::Rfc9562);
            assert!(seen.insert(id));
            let v = id.as_u128();
            for (bit, count) in ones.iter_mut().enumerate() {
                *count += ((v >> (127 - bit)) & 1) as u32;
            }
        }
        // Fixed bits: version 0100 at bits 48..52, variant 10 at bits 64..66.
        let fixed = [(48, 0), (49, 1), (50, 0), (51, 0), (64, 1), (65, 0)];
        for (bit, value) in fixed {
            assert_eq!(ones[bit], value * N, "bit {bit}");
        }
        // Every other bit is set about half the time (2000 draws: mean 1000, sd ~22).
        for (bit, &count) in ones.iter().enumerate() {
            if !fixed.iter().any(|&(b, _)| b == bit) {
                assert!((800..1200).contains(&count), "bit {bit} set {count} times");
            }
        }
        assert!(Uuid::try_new_v4().is_ok());
    }
}
