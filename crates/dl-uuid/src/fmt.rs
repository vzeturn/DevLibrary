//! Text output: `Display` and the four format adapters.

use crate::Uuid;
use alloc::string::String;
use core::fmt;

const LOWER: &[u8; 16] = b"0123456789abcdef";
const UPPER: &[u8; 16] = b"0123456789ABCDEF";

/// Offsets of the 16 hex digit pairs in the hyphenated form.
const HYPHENATED_PAIRS: [usize; 16] = [0, 2, 4, 6, 9, 11, 14, 16, 19, 21, 24, 26, 28, 30, 32, 34];

/// The 36-byte hyphenated form.
#[inline]
fn hyphenated_bytes(bytes: &[u8; 16], upper: bool) -> [u8; 36] {
    let digits = if upper { UPPER } else { LOWER };
    let mut out = [b'-'; 36];
    for (&b, &p) in bytes.iter().zip(&HYPHENATED_PAIRS) {
        out[p] = digits[usize::from(b >> 4)];
        out[p + 1] = digits[usize::from(b & 0x0F)];
    }
    out
}

/// The 32-byte simple form.
#[inline]
fn simple_bytes(bytes: &[u8; 16], upper: bool) -> [u8; 32] {
    let digits = if upper { UPPER } else { LOWER };
    let mut out = [0u8; 32];
    for (&b, pair) in bytes.iter().zip(out.as_chunks_mut::<2>().0) {
        *pair = [digits[usize::from(b >> 4)], digits[usize::from(b & 0x0F)]];
    }
    out
}

/// Views an encoded buffer as `&str`. The buffer only ever holds ASCII written by this
/// module, so the conversion cannot fail; the empty-string fallback is unreachable and
/// avoids a panic path.
fn ascii_str(buf: &[u8]) -> &str {
    core::str::from_utf8(buf).unwrap_or_default()
}

/// Writes `prefix`, the hex form and `suffix` into `buf`, whose length is exactly the
/// sum (each adapter passes a fixed-size array of its `LENGTH`).
#[inline]
fn encode<'b>(
    id: &Uuid,
    buf: &'b mut [u8],
    prefix: &[u8],
    hyphens: bool,
    suffix: &[u8],
    upper: bool,
) -> &'b str {
    let (pre, rest) = buf.split_at_mut(prefix.len());
    pre.copy_from_slice(prefix);
    let post = if hyphens {
        let (hex, post) = rest.split_at_mut(36);
        hex.copy_from_slice(&hyphenated_bytes(id.as_bytes(), upper));
        post
    } else {
        let (hex, post) = rest.split_at_mut(32);
        hex.copy_from_slice(&simple_bytes(id.as_bytes(), upper));
        post
    };
    post.copy_from_slice(suffix);
    ascii_str(buf)
}

macro_rules! adapter {
    (
        $(#[$meta:meta])*
        $name:ident, $method:ident, $len:literal, $prefix:literal, $hyphens:literal, $suffix:literal,
        $lower:literal, $upper:literal
    ) => {
        $(#[$meta])*
        ///
        /// `Display` and `{:x}` print lowercase hex digits, `{:X}` uppercase; width,
        /// fill and alignment flags are honoured.
        ///
        /// ```
        #[doc = concat!("use dl_uuid::{", stringify!($name), ", Uuid};")]
        /// let id = Uuid::from_u128(0x67e55044_10b1_426f_9247_bb680e5fe0c8);
        #[doc = concat!("assert_eq!(id.", stringify!($method), "().to_string(), \"", $lower, "\");")]
        #[doc = concat!("assert_eq!(format!(\"{:X}\", id.", stringify!($method), "()), \"", $upper, "\");")]
        #[doc = concat!("assert_eq!(id.", stringify!($method), "().to_string_upper(), \"", $upper, "\");")]
        ///
        #[doc = concat!("let mut buf = [0u8; ", stringify!($name), "::LENGTH];")]
        #[doc = concat!("assert_eq!(id.", stringify!($method), "().encode_lower(&mut buf), \"", $lower, "\");")]
        /// ```
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            /// Length of the encoded text in bytes.
            pub const LENGTH: usize = $len;

            /// Wraps a UUID.
            pub const fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            /// The wrapped UUID.
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// Unwraps the UUID.
            pub const fn into_uuid(self) -> Uuid {
                self.0
            }

            /// Writes the lowercase form into `buf` without allocating and returns it.
            pub fn encode_lower<'b>(&self, buf: &'b mut [u8; $len]) -> &'b str {
                encode(&self.0, buf, $prefix, $hyphens, $suffix, false)
            }

            /// Writes the form with uppercase hex digits into `buf` and returns it.
            pub fn encode_upper<'b>(&self, buf: &'b mut [u8; $len]) -> &'b str {
                encode(&self.0, buf, $prefix, $hyphens, $suffix, true)
            }

            /// The form with uppercase hex digits as a `String`.
            pub fn to_string_upper(&self) -> String {
                let mut buf = [0u8; $len];
                String::from(self.encode_upper(&mut buf))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::LowerHex::fmt(self, f)
            }
        }

        impl fmt::LowerHex for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let mut buf = [0u8; $len];
                f.pad(self.encode_lower(&mut buf))
            }
        }

        impl fmt::UpperHex for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let mut buf = [0u8; $len];
                f.pad(self.encode_upper(&mut buf))
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        impl From<$name> for Uuid {
            fn from(a: $name) -> Uuid {
                a.0
            }
        }
    };
}

adapter!(
    /// The hyphenated form `67e55044-10b1-426f-9247-bb680e5fe0c8` (36 bytes), the
    /// canonical form of RFC 9562 §4 and the output of C# `Guid.ToString()` / `"D"`.
    Hyphenated,
    hyphenated,
    36,
    b"",
    true,
    b"",
    "67e55044-10b1-426f-9247-bb680e5fe0c8",
    "67E55044-10B1-426F-9247-BB680E5FE0C8"
);

adapter!(
    /// The simple form `67e5504410b1426f9247bb680e5fe0c8` (32 hex digits, no hyphens),
    /// like C# `Guid.ToString("N")` and Python `UUID.hex`.
    Simple,
    simple,
    32,
    b"",
    false,
    b"",
    "67e5504410b1426f9247bb680e5fe0c8",
    "67E5504410B1426F9247BB680E5FE0C8"
);

adapter!(
    /// The braced form `{67e55044-10b1-426f-9247-bb680e5fe0c8}` (38 bytes), like C#
    /// `Guid.ToString("B")` and the Windows registry.
    Braced,
    braced,
    38,
    b"{",
    true,
    b"}",
    "{67e55044-10b1-426f-9247-bb680e5fe0c8}",
    "{67E55044-10B1-426F-9247-BB680E5FE0C8}"
);

adapter!(
    /// The URN form `urn:uuid:67e55044-10b1-426f-9247-bb680e5fe0c8` (45 bytes, RFC 9562
    /// §4). The `urn:uuid:` prefix is always lowercase; only the hex digits change case.
    Urn,
    urn,
    45,
    b"urn:uuid:",
    true,
    b"",
    "urn:uuid:67e55044-10b1-426f-9247-bb680e5fe0c8",
    "urn:uuid:67E55044-10B1-426F-9247-BB680E5FE0C8"
);

impl Uuid {
    /// Formats as `67e55044-10b1-426f-9247-bb680e5fe0c8` (also what `Display` prints).
    pub const fn hyphenated(self) -> Hyphenated {
        Hyphenated(self)
    }

    /// Formats as `67e5504410b1426f9247bb680e5fe0c8`.
    pub const fn simple(self) -> Simple {
        Simple(self)
    }

    /// Formats as `{67e55044-10b1-426f-9247-bb680e5fe0c8}`.
    pub const fn braced(self) -> Braced {
        Braced(self)
    }

    /// Formats as `urn:uuid:67e55044-10b1-426f-9247-bb680e5fe0c8`.
    pub const fn urn(self) -> Urn {
        Urn(self)
    }

    /// Writes the lowercase hyphenated form into `buf` without allocating.
    ///
    /// ```
    /// use dl_uuid::Uuid;
    /// let mut buf = [0u8; 36];
    /// let s = Uuid::NAMESPACE_DNS.encode_into(&mut buf);
    /// assert_eq!(s, "6ba7b810-9dad-11d1-80b4-00c04fd430c8");
    /// ```
    pub fn encode_into<'b>(&self, buf: &'b mut [u8; 36]) -> &'b str {
        self.hyphenated().encode_lower(buf)
    }

    /// The hyphenated form with uppercase hex digits, like C# `ToString().ToUpper()`
    /// and SQL Server's `CAST(id AS varchar(36))`.
    ///
    /// ```
    /// use dl_uuid::Uuid;
    /// assert_eq!(Uuid::NAMESPACE_DNS.to_string_upper(), "6BA7B810-9DAD-11D1-80B4-00C04FD430C8");
    /// ```
    pub fn to_string_upper(&self) -> String {
        self.hyphenated().to_string_upper()
    }
}

impl fmt::Display for Uuid {
    /// Lowercase hyphenated form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.hyphenated(), f)
    }
}

impl fmt::Debug for Uuid {
    /// Lowercase hyphenated form, without a wrapper.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.hyphenated(), f)
    }
}

impl fmt::LowerHex for Uuid {
    /// Lowercase hyphenated form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.hyphenated(), f)
    }
}

impl fmt::UpperHex for Uuid {
    /// Uppercase hyphenated form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.hyphenated(), f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::ToString;

    #[test]
    fn format_table() {
        let cases: &[(u128, &str, &str)] = &[
            (
                0,
                "00000000-0000-0000-0000-000000000000",
                "00000000000000000000000000000000",
            ),
            (
                u128::MAX,
                "ffffffff-ffff-ffff-ffff-ffffffffffff",
                "ffffffffffffffffffffffffffffffff",
            ),
            (
                0x0123456789abcdef_fedcba9876543210,
                "01234567-89ab-cdef-fedc-ba9876543210",
                "0123456789abcdeffedcba9876543210",
            ),
            (
                1,
                "00000000-0000-0000-0000-000000000001",
                "00000000000000000000000000000001",
            ),
            (
                1 << 127,
                "80000000-0000-0000-0000-000000000000",
                "80000000000000000000000000000000",
            ),
        ];
        for &(v, hyph, simple) in cases {
            let id = Uuid::from_u128(v);
            assert_eq!(id.to_string(), hyph);
            assert_eq!(format!("{id:?}"), hyph);
            assert_eq!(format!("{id:x}"), hyph);
            assert_eq!(format!("{id:X}"), hyph.to_ascii_uppercase());
            assert_eq!(id.to_string_upper(), hyph.to_ascii_uppercase());
            assert_eq!(id.hyphenated().to_string(), hyph);
            assert_eq!(id.simple().to_string(), simple);
            assert_eq!(id.simple().to_string_upper(), simple.to_ascii_uppercase());
            assert_eq!(id.braced().to_string(), format!("{{{hyph}}}"));
            assert_eq!(
                id.braced().to_string_upper(),
                format!("{{{}}}", hyph.to_ascii_uppercase())
            );
            assert_eq!(id.urn().to_string(), format!("urn:uuid:{hyph}"));
            assert_eq!(
                id.urn().to_string_upper(),
                format!("urn:uuid:{}", hyph.to_ascii_uppercase())
            );
            let mut buf = [0u8; 36];
            assert_eq!(id.encode_into(&mut buf), hyph);
            assert_eq!(
                id.hyphenated().encode_upper(&mut buf),
                hyph.to_ascii_uppercase()
            );
            let mut buf = [0u8; 32];
            assert_eq!(id.simple().encode_lower(&mut buf), simple);
            let mut buf = [0u8; 38];
            assert_eq!(id.braced().encode_lower(&mut buf).len(), Braced::LENGTH);
            let mut buf = [0u8; 45];
            assert_eq!(id.urn().encode_upper(&mut buf).len(), Urn::LENGTH);
        }
    }

    #[test]
    fn padding_flags() {
        let id = Uuid::NIL;
        assert_eq!(
            format!("[{id:>40}]"),
            "[    00000000-0000-0000-0000-000000000000]"
        );
        assert_eq!(
            format!("[{:*<34}]", id.simple()),
            "[00000000000000000000000000000000**]"
        );
        assert_eq!(
            format!("[{:^40X}]", Uuid::MAX.braced()),
            "[ {FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF} ]"
        );
        // A precision truncates, as for any string.
        assert_eq!(format!("{:.8}", id), "00000000");
    }

    #[test]
    fn adapter_conversions() {
        let id = Uuid::NAMESPACE_URL;
        assert_eq!(*id.simple().as_uuid(), id);
        assert_eq!(id.braced().into_uuid(), id);
        assert_eq!(Uuid::from(Urn::from(id)), id);
        assert_eq!(Hyphenated::from_uuid(id), id.hyphenated());
        assert_eq!(
            (
                Hyphenated::LENGTH,
                Simple::LENGTH,
                Braced::LENGTH,
                Urn::LENGTH
            ),
            (36, 32, 38, 45)
        );
    }
}
