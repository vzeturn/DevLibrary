#![cfg_attr(feature = "std", doc = include_str!("../README.md"))]
#![cfg_attr(
    not(feature = "std"),
    doc = "UUIDs (RFC 9562): parse, format, v3/v5/v7/v8 from caller-supplied data. \
           Enable the default `std` feature for OS-random v4/v7 generation; the full \
           guide is in README.md."
)]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod error;
mod fmt;
mod generate;
mod parse;
mod v7;

#[cfg(test)]
mod tests;

pub use error::Error;
pub use fmt::{Braced, Hyphenated, Simple, Urn};
pub use v7::V7Generator;

use core::cmp::Ordering;

/// A 128-bit universally unique identifier (RFC 9562), stored as 16 bytes in the
/// standard (big-endian, "network") order: the order of the hex digits in the text form.
///
/// `Ord` compares those bytes left to right, which is also the numeric order of
/// [`as_u128`](Self::as_u128). Version 7 UUIDs therefore sort by creation time.
/// SQL Server sorts `uniqueidentifier` differently; see [`cmp_sqlserver`](Self::cmp_sqlserver).
///
/// `Debug` and `Display` print the lowercase hyphenated form.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
pub struct Uuid([u8; 16]);

/// The version field of a UUID (RFC 9562 §4.2), which says how it was generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Version {
    /// The Nil UUID, all bits zero (§5.9).
    Nil,
    /// Version 1: Gregorian time and node ID (§5.1).
    V1,
    /// Version 2: DCE Security (§5.2).
    V2,
    /// Version 3: MD5 of a namespace and a name (§5.3).
    V3,
    /// Version 4: random (§5.4).
    V4,
    /// Version 5: SHA-1 of a namespace and a name (§5.5).
    V5,
    /// Version 6: reordered Gregorian time, sortable (§5.6).
    V6,
    /// Version 7: Unix time in milliseconds and random bits, sortable (§5.7).
    V7,
    /// Version 8: custom, application-defined layout (§5.8).
    V8,
    /// The Max UUID, all bits one (§5.10).
    Max,
}

impl Version {
    /// The value of the 4-bit version field: 1 to 8, 0 for [`Nil`](Self::Nil) and 15
    /// for [`Max`](Self::Max).
    pub const fn to_u8(self) -> u8 {
        match self {
            Version::Nil => 0,
            Version::V1 => 1,
            Version::V2 => 2,
            Version::V3 => 3,
            Version::V4 => 4,
            Version::V5 => 5,
            Version::V6 => 6,
            Version::V7 => 7,
            Version::V8 => 8,
            Version::Max => 15,
        }
    }
}

/// The variant field of a UUID (RFC 9562 §4.1), the top bits of byte 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Variant {
    /// `0xxx`: reserved, NCS backward compatibility. The Nil UUID has this variant.
    Ncs,
    /// `10xx`: the layout defined by RFC 9562 (and RFC 4122). Every UUID this crate
    /// generates has this variant.
    Rfc9562,
    /// `110x`: reserved, Microsoft backward compatibility (old COM GUIDs).
    Microsoft,
    /// `111x`: reserved for future definition. The Max UUID has this variant.
    Future,
}

/// 100-ns intervals between the Gregorian epoch (1582-10-15) and the Unix epoch
/// (RFC 9562 Appendix A, `Greg_Unix_offset`).
const GREGORIAN_UNIX_OFFSET: u64 = 0x01B2_1DD2_1381_4000;

impl Uuid {
    /// The Nil UUID `00000000-0000-0000-0000-000000000000` (RFC 9562 §5.9).
    pub const NIL: Uuid = Uuid([0; 16]);
    /// The Max UUID `ffffffff-ffff-ffff-ffff-ffffffffffff` (RFC 9562 §5.10).
    pub const MAX: Uuid = Uuid([0xFF; 16]);
    /// Namespace for fully qualified domain names, `6ba7b810-9dad-11d1-80b4-00c04fd430c8`
    /// (RFC 9562 §6.6).
    pub const NAMESPACE_DNS: Uuid = Uuid::from_u128(0x6ba7b810_9dad_11d1_80b4_00c04fd430c8);
    /// Namespace for URLs, `6ba7b811-9dad-11d1-80b4-00c04fd430c8` (RFC 9562 §6.6).
    pub const NAMESPACE_URL: Uuid = Uuid::from_u128(0x6ba7b811_9dad_11d1_80b4_00c04fd430c8);
    /// Namespace for ISO OIDs, `6ba7b812-9dad-11d1-80b4-00c04fd430c8` (RFC 9562 §6.6).
    pub const NAMESPACE_OID: Uuid = Uuid::from_u128(0x6ba7b812_9dad_11d1_80b4_00c04fd430c8);
    /// Namespace for X.500 distinguished names, `6ba7b814-9dad-11d1-80b4-00c04fd430c8`
    /// (RFC 9562 §6.6).
    pub const NAMESPACE_X500: Uuid = Uuid::from_u128(0x6ba7b814_9dad_11d1_80b4_00c04fd430c8);

    /// Creates a UUID from 16 bytes in standard (big-endian) order.
    ///
    /// For the byte order of .NET `Guid.ToByteArray()` and SQL Server, use
    /// [`from_bytes_le`](Self::from_bytes_le).
    #[inline]
    pub const fn from_bytes(bytes: [u8; 16]) -> Uuid {
        Uuid(bytes)
    }

    /// The 16 bytes in standard (big-endian) order.
    #[inline]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Consumes the UUID and returns its 16 bytes in standard (big-endian) order.
    #[inline]
    pub const fn into_bytes(self) -> [u8; 16] {
        self.0
    }

    /// Creates a UUID from a 128-bit integer; the most significant byte comes first.
    ///
    /// ```
    /// use dl_uuid::Uuid;
    /// let id = Uuid::from_u128(0x67e55044_10b1_426f_9247_bb680e5fe0c8);
    /// assert_eq!(id.to_string(), "67e55044-10b1-426f-9247-bb680e5fe0c8");
    /// ```
    #[inline]
    pub const fn from_u128(v: u128) -> Uuid {
        Uuid(v.to_be_bytes())
    }

    /// The UUID as a 128-bit integer (big-endian interpretation of the bytes).
    #[inline]
    pub const fn as_u128(&self) -> u128 {
        u128::from_be_bytes(self.0)
    }

    /// Creates a UUID from the four fields of the Windows `GUID` structure (`Data1` to
    /// `Data4`), the same arguments as the C# `Guid(int, short, short, byte[])`
    /// constructor. The fields are written big-endian, as in the text form.
    ///
    /// ```
    /// use dl_uuid::Uuid;
    /// let id = Uuid::from_fields(0x6ba7b810, 0x9dad, 0x11d1, [0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8]);
    /// assert_eq!(id, Uuid::NAMESPACE_DNS);
    /// ```
    pub const fn from_fields(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Uuid {
        let a = d1.to_be_bytes();
        let b = d2.to_be_bytes();
        let c = d3.to_be_bytes();
        Uuid([
            a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4],
            d4[5], d4[6], d4[7],
        ])
    }

    /// The four `GUID` fields `(Data1, Data2, Data3, Data4)`; the inverse of
    /// [`from_fields`](Self::from_fields).
    pub const fn to_fields(&self) -> (u32, u16, u16, [u8; 8]) {
        let b = &self.0;
        (
            u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
            u16::from_be_bytes([b[4], b[5]]),
            u16::from_be_bytes([b[6], b[7]]),
            [b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]],
        )
    }

    /// Creates a UUID from the mixed-endian layout of .NET `Guid.ToByteArray()`,
    /// `new Guid(byte[])`, Python `UUID.bytes_le` and SQL Server's on-disk
    /// `uniqueidentifier` (what `CAST(id AS binary(16))` returns).
    ///
    /// In that layout the first three groups (`Data1` as 4 bytes, `Data2` and `Data3`
    /// as 2 bytes each) are little-endian and the last 8 bytes are as written.
    ///
    /// ```
    /// use dl_uuid::Uuid;
    /// // Output of the Guid.ToByteArray() example in the .NET documentation:
    /// // Guid: 35918bc9-196d-40ea-9779-889d79b753f0
    /// // C9 8B 91 35 6D 19 EA 40 97 79 88 9D 79 B7 53 F0
    /// let from_csharp = [
    ///     0xC9, 0x8B, 0x91, 0x35, 0x6D, 0x19, 0xEA, 0x40,
    ///     0x97, 0x79, 0x88, 0x9D, 0x79, 0xB7, 0x53, 0xF0,
    /// ];
    /// let id = Uuid::from_bytes_le(from_csharp);
    /// assert_eq!(id.to_string(), "35918bc9-196d-40ea-9779-889d79b753f0");
    /// assert_eq!(id.to_bytes_le(), from_csharp);
    /// ```
    pub const fn from_bytes_le(b: [u8; 16]) -> Uuid {
        Uuid(swap_guid_order(b))
    }

    /// The bytes in the mixed-endian layout of .NET `Guid.ToByteArray()` (see
    /// [`from_bytes_le`](Self::from_bytes_le)).
    ///
    /// ```
    /// use dl_uuid::Uuid;
    /// // C#: new Guid("00112233-4455-6677-8899-aabbccddeeff").ToByteArray()
    /// let id: Uuid = "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap();
    /// assert_eq!(
    ///     id.to_bytes_le(),
    ///     [0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66,
    ///      0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
    /// );
    /// ```
    pub const fn to_bytes_le(&self) -> [u8; 16] {
        swap_guid_order(self.0)
    }

    /// `true` for the Nil UUID (all zeros).
    #[inline]
    pub const fn is_nil(&self) -> bool {
        self.as_u128() == 0
    }

    /// `true` for the Max UUID (all ones).
    #[inline]
    pub const fn is_max(&self) -> bool {
        self.as_u128() == u128::MAX
    }

    /// The variant field (RFC 9562 §4.1).
    pub const fn variant(&self) -> Variant {
        let b = self.0[8];
        if b & 0x80 == 0 {
            Variant::Ncs
        } else if b & 0x40 == 0 {
            Variant::Rfc9562
        } else if b & 0x20 == 0 {
            Variant::Microsoft
        } else {
            Variant::Future
        }
    }

    /// The version (RFC 9562 §4.2), or `None` if the UUID is not Nil, not Max, and
    /// either does not have the [`Variant::Rfc9562`] variant or has a version number
    /// outside 1 to 8.
    ///
    /// ```
    /// use dl_uuid::{Uuid, Version};
    /// let id: Uuid = "017f22e2-79b0-7cc3-98c4-dc0c0c07398f".parse().unwrap();
    /// assert_eq!(id.version(), Some(Version::V7));
    /// assert_eq!(Uuid::NIL.version(), Some(Version::Nil));
    /// ```
    pub const fn version(&self) -> Option<Version> {
        if self.is_nil() {
            return Some(Version::Nil);
        }
        if self.is_max() {
            return Some(Version::Max);
        }
        if !matches!(self.variant(), Variant::Rfc9562) {
            return None;
        }
        match self.0[6] >> 4 {
            1 => Some(Version::V1),
            2 => Some(Version::V2),
            3 => Some(Version::V3),
            4 => Some(Version::V4),
            5 => Some(Version::V5),
            6 => Some(Version::V6),
            7 => Some(Version::V7),
            8 => Some(Version::V8),
            _ => None,
        }
    }

    /// The 48-bit Unix timestamp in milliseconds of a version 7 UUID, or `None` for
    /// other versions.
    ///
    /// ```
    /// use dl_uuid::Uuid;
    /// let id: Uuid = "017f22e2-79b0-7cc3-98c4-dc0c0c07398f".parse().unwrap();
    /// assert_eq!(id.v7_timestamp_ms(), Some(1_645_557_742_000)); // 2022-02-22 19:22:22 UTC
    /// ```
    pub const fn v7_timestamp_ms(&self) -> Option<u64> {
        if !matches!(self.version(), Some(Version::V7)) {
            return None;
        }
        let b = &self.0;
        Some(u64::from_be_bytes([
            0, 0, b[0], b[1], b[2], b[3], b[4], b[5],
        ]))
    }

    /// The 60-bit timestamp of a version 1 or version 6 UUID: 100-ns intervals since
    /// 1582-10-15 00:00:00 UTC (the same value as Python's `UUID.time`). `None` for
    /// other versions.
    ///
    /// ```
    /// use dl_uuid::Uuid;
    /// // RFC 9562 Appendix A.1 and A.5 encode the same instant as v1 and v6.
    /// let v1: Uuid = "c232ab00-9414-11ec-b3c8-9f6bdeced846".parse().unwrap();
    /// let v6: Uuid = "1ec9414c-232a-6b00-b3c8-9f6bdeced846".parse().unwrap();
    /// assert_eq!(v1.gregorian_timestamp(), Some(0x1EC9414C232AB00));
    /// assert_eq!(v6.gregorian_timestamp(), Some(0x1EC9414C232AB00));
    /// ```
    pub const fn gregorian_timestamp(&self) -> Option<u64> {
        let b = &self.0;
        match self.version() {
            Some(Version::V1) => {
                let low = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64;
                let mid = u16::from_be_bytes([b[4], b[5]]) as u64;
                let high = (u16::from_be_bytes([b[6], b[7]]) & 0x0FFF) as u64;
                Some(high << 48 | mid << 32 | low)
            }
            Some(Version::V6) => {
                let high = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64;
                let mid = u16::from_be_bytes([b[4], b[5]]) as u64;
                let low = (u16::from_be_bytes([b[6], b[7]]) & 0x0FFF) as u64;
                Some(high << 28 | mid << 12 | low)
            }
            _ => None,
        }
    }

    /// The creation time in Unix milliseconds of a version 1, 6 or 7 UUID.
    ///
    /// For v1 and v6 the 100-ns timestamp is rounded down to whole milliseconds, and
    /// `None` is returned if it is before 1970. `None` for other versions.
    ///
    /// ```
    /// use dl_uuid::Uuid;
    /// let v1: Uuid = "c232ab00-9414-11ec-b3c8-9f6bdeced846".parse().unwrap();
    /// let v7: Uuid = "017f22e2-79b0-7cc3-98c4-dc0c0c07398f".parse().unwrap();
    /// assert_eq!(v1.unix_timestamp_ms(), Some(1_645_557_742_000));
    /// assert_eq!(v7.unix_timestamp_ms(), Some(1_645_557_742_000));
    /// ```
    pub const fn unix_timestamp_ms(&self) -> Option<u64> {
        if let Some(ms) = self.v7_timestamp_ms() {
            return Some(ms);
        }
        match self.gregorian_timestamp() {
            Some(t) if t >= GREGORIAN_UNIX_OFFSET => Some((t - GREGORIAN_UNIX_OFFSET) / 10_000),
            _ => None,
        }
    }

    /// Compares two UUIDs the way SQL Server orders `uniqueidentifier` values
    /// (`ORDER BY`, index order), which differs from [`Ord`].
    ///
    /// SQL Server compares the last 6 bytes (the final group of the text form) first,
    /// then bytes 8–9 (fourth group), then the third, second and first groups; within
    /// the first three groups the bytes are compared in reverse, because they are stored
    /// little-endian. In terms of the stored (mixed-endian) bytes this is the order
    /// 10–15, 8–9, 6–7, 4–5, 0–3, the same permutation .NET's `SqlGuid.CompareTo` uses.
    ///
    /// Sources: Microsoft, "Comparing GUID and uniqueidentifier values"
    /// (<https://learn.microsoft.com/en-us/sql/connect/ado-net/sql/compare-guid-uniqueidentifier-values>)
    /// and dbi services, "Why UUIDv7 does not reduce fragmentation in SQL Server"
    /// (<https://www.dbi-services.com/blog/why-uuidv7-does-not-reduce-fragmentation-in-sql-server/>),
    /// which verified this order on 10,000 `NEWID()` values in SQL Server 2022.
    ///
    /// ```
    /// use core::cmp::Ordering;
    /// use dl_uuid::Uuid;
    /// let a: Uuid = "ffffffff-ffff-ffff-ffff-000000000000".parse().unwrap();
    /// let b: Uuid = "00000000-0000-0000-0000-000000000001".parse().unwrap();
    /// assert_eq!(a.cmp(&b), Ordering::Greater); // byte order
    /// assert_eq!(a.cmp_sqlserver(&b), Ordering::Less); // SQL Server returns `a` first
    ///
    /// let mut ids = vec![b, a];
    /// ids.sort_by(Uuid::cmp_sqlserver);
    /// assert_eq!(ids, [a, b]);
    /// ```
    pub fn cmp_sqlserver(&self, other: &Uuid) -> Ordering {
        sqlserver_key(&self.0).cmp(&sqlserver_key(&other.0))
    }
}

/// Swaps between standard and .NET `GUID` byte order (the operation is its own inverse).
const fn swap_guid_order(b: [u8; 16]) -> [u8; 16] {
    [
        b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13],
        b[14], b[15],
    ]
}

/// Standard-order bytes rearranged so that a plain byte comparison gives SQL Server's
/// `uniqueidentifier` order.
const fn sqlserver_key(b: &[u8; 16]) -> [u8; 16] {
    [
        b[10], b[11], b[12], b[13], b[14], b[15], b[8], b[9], b[7], b[6], b[5], b[4], b[3], b[2],
        b[1], b[0],
    ]
}

impl From<[u8; 16]> for Uuid {
    fn from(bytes: [u8; 16]) -> Uuid {
        Uuid(bytes)
    }
}

impl From<Uuid> for [u8; 16] {
    fn from(id: Uuid) -> [u8; 16] {
        id.0
    }
}

impl From<u128> for Uuid {
    fn from(v: u128) -> Uuid {
        Uuid::from_u128(v)
    }
}

impl From<Uuid> for u128 {
    fn from(id: Uuid) -> u128 {
        id.as_u128()
    }
}

impl AsRef<[u8]> for Uuid {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
