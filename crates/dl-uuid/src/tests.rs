//! Cross-checks against RFC 9562 Appendix A and Python's `uuid` module.
//! Python values are printed by `testdata/gen_vectors.py` (Python 3.13).

use crate::{Uuid, Variant, Version};
use alloc::string::ToString;
use core::cmp::Ordering;

fn u(s: &str) -> Uuid {
    Uuid::parse(s).unwrap()
}

/// RFC 9562 Appendix A test vectors.
#[test]
fn rfc9562_appendix_a() {
    // A.1 v1: timestamp 0x1EC9414C232AB00, clock_seq 0x33C8, node 0x9F6BDECED846.
    let v1 = u("C232AB00-9414-11EC-B3C8-9F6BDECED846");
    assert_eq!(v1.version(), Some(Version::V1));
    assert_eq!(v1.gregorian_timestamp(), Some(0x1EC9414C232AB00));
    assert_eq!(v1.gregorian_timestamp(), Some(138_648_505_420_000_000));
    assert_eq!(v1.unix_timestamp_ms(), Some(1_645_557_742_000));
    assert_eq!(v1.v7_timestamp_ms(), None);
    let (_, _, _, d4) = v1.to_fields();
    assert_eq!(u16::from_be_bytes([d4[0], d4[1]]) & 0x3FFF, 0x33C8);
    assert_eq!(&d4[2..], &[0x9F, 0x6B, 0xDE, 0xCE, 0xD8, 0x46]);

    // A.2 v3 and A.4 v5: DNS namespace, name "www.example.com".
    let v3 = Uuid::new_v3(&Uuid::NAMESPACE_DNS, "www.example.com");
    assert_eq!(v3, u("5df41881-3aed-3515-88a7-2f4a814cf09e"));
    assert_eq!(v3.version(), Some(Version::V3));
    let v5 = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"www.example.com");
    assert_eq!(v5, u("2ed6657d-e927-568b-95e1-2665a8aea6a2"));
    assert_eq!(v5.version(), Some(Version::V5));

    // A.3 v4: random 919108F752D133205BACF847DB4148A8.
    let v4 = Uuid::v4_from(0x919108F752D133205BACF847DB4148A8u128.to_be_bytes());
    assert_eq!(v4, u("919108f7-52d1-4320-9bac-f847db4148a8"));

    // A.5 v6: the same instant, clock_seq and node as A.1.
    let v6 = u("1EC9414C-232A-6B00-B3C8-9F6BDECED846");
    assert_eq!(v6.version(), Some(Version::V6));
    assert_eq!(v6.gregorian_timestamp(), v1.gregorian_timestamp());
    assert_eq!(v6.unix_timestamp_ms(), Some(1_645_557_742_000));

    // A.6 v7: unix_ts_ms 0x017F22E279B0, rand_a 0xCC3, rand_b 0b01 + 0x8C4DC0C0C07398F.
    let rand_a: u16 = 0xCC3;
    let rand_b: u64 = 0b01 << 60 | 0x8C4DC0C0C07398F;
    let mut random = [0u8; 10];
    random[..2].copy_from_slice(&rand_a.to_be_bytes());
    random[2..].copy_from_slice(&rand_b.to_be_bytes());
    let v7 = Uuid::v7_from(0x017F22E279B0, random);
    assert_eq!(v7.to_string_upper(), "017F22E2-79B0-7CC3-98C4-DC0C0C07398F");
    assert_eq!(v7.v7_timestamp_ms(), Some(0x017F22E279B0));
    assert_eq!(v7.unix_timestamp_ms(), Some(1_645_557_742_000));
    assert_eq!(v7.gregorian_timestamp(), None);

    // B.1 v8 (time-based): custom_a 0x2489E9AD2EE2, custom_b 0xE00, custom_c 0xEC932D5F69181C0.
    let v8 = Uuid::v8_from(u("2489E9AD-2EE2-0E00-0EC9-32D5F69181C0").into_bytes());
    assert_eq!(v8, u("2489E9AD-2EE2-8E00-8EC9-32D5F69181C0"));
    // B.2 v8 (name-based): SHA-256 of the DNS namespace and "www.example.com".
    use dl_hash::{Digest, Sha256};
    let sha = Sha256::default()
        .chain(Uuid::NAMESPACE_DNS.as_bytes())
        .chain("www.example.com")
        .finalize();
    let mut custom = [0u8; 16];
    custom.copy_from_slice(&sha[..16]);
    let v8 = Uuid::v8_from(custom);
    assert_eq!(v8, u("5c146b14-3c52-8afd-938a-375d0df1fbf6"));
    assert_eq!(v8.version(), Some(Version::V8));

    // §5.9, §5.10.
    assert_eq!(
        Uuid::NIL.to_string(),
        "00000000-0000-0000-0000-000000000000"
    );
    assert_eq!(
        Uuid::MAX.to_string(),
        "ffffffff-ffff-ffff-ffff-ffffffffffff"
    );
}

#[test]
fn namespace_constants_match_python() {
    let cases = [
        (Uuid::NAMESPACE_DNS, "6ba7b810-9dad-11d1-80b4-00c04fd430c8"),
        (Uuid::NAMESPACE_URL, "6ba7b811-9dad-11d1-80b4-00c04fd430c8"),
        (Uuid::NAMESPACE_OID, "6ba7b812-9dad-11d1-80b4-00c04fd430c8"),
        (Uuid::NAMESPACE_X500, "6ba7b814-9dad-11d1-80b4-00c04fd430c8"),
    ];
    for (ns, s) in cases {
        assert_eq!(ns.to_string(), s);
        assert_eq!(ns.version(), Some(Version::V1));
    }
}

/// `uuid.uuid3(ns, name)` and `uuid.uuid5(ns, name)` from Python 3.13.
#[test]
fn v3_v5_match_python() {
    #[rustfmt::skip]
    let cases = [
        (Uuid::NAMESPACE_DNS, "www.example.com", "5df41881-3aed-3515-88a7-2f4a814cf09e", "2ed6657d-e927-568b-95e1-2665a8aea6a2"),
        (Uuid::NAMESPACE_DNS, "python.org", "6fa459ea-ee8a-3ca4-894e-db77e160355e", "886313e1-3b8a-5372-9b90-0c9aee199e5d"),
        (Uuid::NAMESPACE_DNS, "", "c87ee674-4ddc-3efe-a74e-dfe25da5d7b3", "4ebd0208-8328-5d69-8c44-ec50939c0967"),
        (Uuid::NAMESPACE_DNS, "https://example.com/path?q=1", "480d8e93-3f17-36ba-828a-89d4176f60c7", "1c5759b6-025c-5df2-866b-f454b718754c"),
        (Uuid::NAMESPACE_DNS, "1.3.6.1", "867db8e5-41e6-36a2-ad33-866053e3b1c1", "4d757760-8766-51b5-ba59-e9193a084966"),
        (Uuid::NAMESPACE_DNS, "cn=John Doe,o=Example", "c22317da-2373-31bf-88a2-4e530602ed76", "e4a32027-181a-5509-b1ea-e2d25a2fde00"),
        (Uuid::NAMESPACE_DNS, "Tiếng Việt", "6d3f0869-4b4e-39dd-94dd-c28546686b9f", "7ed5cb8e-4db0-55e1-ba2a-d67814ac97ec"),
        (Uuid::NAMESPACE_URL, "www.example.com", "a777199a-c522-31c4-8f4b-335feec7215b", "b63cdfa4-3df9-568e-97ae-006c5b8fd652"),
        (Uuid::NAMESPACE_URL, "python.org", "22fe6191-c161-3d86-a432-a81f343eda08", "7af94e2b-4dd9-50f0-9c9a-8a48519bdef0"),
        (Uuid::NAMESPACE_URL, "", "14cdb9b4-de01-3faa-aff5-65bc2f771745", "1b4db7eb-4057-5ddf-91e0-36dec72071f5"),
        (Uuid::NAMESPACE_URL, "https://example.com/path?q=1", "546e596b-05ff-3a1c-925b-641a72883f16", "4d939a02-ffec-5054-8c09-9a5221c624b7"),
        (Uuid::NAMESPACE_URL, "1.3.6.1", "6471c3b3-9de2-3810-bcbd-31ac16291e2c", "adb29e44-7874-5cbe-a1f7-b6b25330287d"),
        (Uuid::NAMESPACE_URL, "cn=John Doe,o=Example", "5c628041-2e04-34f4-a5aa-7a0a5bb33278", "702fbcc6-f1cd-5834-9276-0b76378b8185"),
        (Uuid::NAMESPACE_URL, "Tiếng Việt", "c5145ef7-51a6-3330-b5bd-a90a47a4451c", "323c5a98-037f-5e97-981a-32dcee7c81cd"),
        (Uuid::NAMESPACE_OID, "www.example.com", "c2c1e4de-6589-389e-9375-32a90c6a83a8", "a5e87d3b-479e-52da-b98a-db251a851854"),
        (Uuid::NAMESPACE_OID, "python.org", "edc9f521-7078-3737-aeed-76056ffc7959", "cd5d0bff-2444-5d26-ab53-4f7db1cb733d"),
        (Uuid::NAMESPACE_OID, "", "596b79dc-00dd-3991-a72f-d3696c38c64f", "0a68eb57-c88a-5f34-9e9d-27f85e68af4f"),
        (Uuid::NAMESPACE_OID, "https://example.com/path?q=1", "9f454d14-4637-365a-8fb1-a79e6cbe3016", "1d0a223b-a311-54bf-99be-14ca9b7d97a2"),
        (Uuid::NAMESPACE_OID, "1.3.6.1", "dd1a1cef-13d5-368a-ad82-eca71acd4cd1", "1447fa61-5277-5fef-a9b3-fbc6e44f4af3"),
        (Uuid::NAMESPACE_OID, "cn=John Doe,o=Example", "e59becd9-9221-3874-943e-92b1050fd038", "aebbb107-ce53-5d82-9bb2-a06e9194eafa"),
        (Uuid::NAMESPACE_OID, "Tiếng Việt", "117540ba-1735-3a58-bfe2-2655f5f3789e", "56e1c881-0f32-54fb-b4d0-e1b70411ad9e"),
        (Uuid::NAMESPACE_X500, "www.example.com", "f4ea5e25-91d4-38b0-b74f-af6c57210cae", "a1d3adb1-15b7-5395-a05f-9051a08769a2"),
        (Uuid::NAMESPACE_X500, "python.org", "94c1b751-792a-3c8b-8eb7-95ea2c6645bb", "e9246a06-296f-5b50-be57-0519806c97e8"),
        (Uuid::NAMESPACE_X500, "", "7aaf118c-f174-3eba-9ec5-680cd791a020", "b4bdf874-8c03-5bd8-8fd7-5e409dfd82c0"),
        (Uuid::NAMESPACE_X500, "https://example.com/path?q=1", "feab14aa-3a3b-3b13-86f1-1d3e89dff318", "bd9c3bd4-2ce2-51eb-884f-3629d1d2c6a3"),
        (Uuid::NAMESPACE_X500, "1.3.6.1", "3f63c9d2-c009-37fc-9ac5-2e85e0d3656f", "be68e4ca-e25f-53bc-9dc2-7d11588b720d"),
        (Uuid::NAMESPACE_X500, "cn=John Doe,o=Example", "1e2e71c2-4075-3b4d-82cf-6e0af31b0154", "af514fe8-6655-5388-a197-79d1296fbf5a"),
        (Uuid::NAMESPACE_X500, "Tiếng Việt", "265a3dc2-cad8-3722-9432-046d4e89fdab", "6fe9d837-56bc-5654-9dd0-23b033389a3f"),
    ];
    for (ns, name, v3, v5) in cases {
        assert_eq!(Uuid::new_v3(&ns, name).to_string(), v3, "v3 {ns} {name}");
        assert_eq!(Uuid::new_v5(&ns, name).to_string(), v5, "v5 {ns} {name}");
        assert_eq!(Uuid::new_v5(&ns, name.as_bytes()), Uuid::new_v5(&ns, name));
        assert_eq!(Uuid::new_v5(&ns, name).variant(), Variant::Rfc9562);
    }
}

/// Python `UUID.bytes_le`, the layout of .NET `Guid.ToByteArray()`. The second row is
/// the example printed in the .NET `Guid.ToByteArray` documentation:
/// `35918bc9-196d-40ea-9779-889d79b753f0` -> `C9 8B 91 35 6D 19 EA 40 97 79 88 9D 79 B7 53 F0`.
#[test]
fn bytes_le_matches_dotnet_and_python() {
    let cases = [
        (
            "00112233-4455-6677-8899-aabbccddeeff",
            0x33221100554477668899aabbccddeeffu128,
        ),
        (
            "35918bc9-196d-40ea-9779-889d79b753f0",
            0xc98b91356d19ea409779889d79b753f0,
        ),
        (
            "017f22e2-79b0-7cc3-98c4-dc0c0c07398f",
            0xe2227f01b079c37c98c4dc0c0c07398f,
        ),
        ("00000000-0000-0000-0000-000000000000", 0),
        ("ffffffff-ffff-ffff-ffff-ffffffffffff", u128::MAX),
    ];
    for (s, le) in cases {
        let id = u(s);
        assert_eq!(id.to_bytes_le(), le.to_be_bytes(), "{s}");
        assert_eq!(Uuid::from_bytes_le(le.to_be_bytes()), id, "{s}");
    }
}

/// Python `UUID.fields` (time_low, time_mid, time_hi_version, clock_seq_hi_variant,
/// clock_seq_low, node) mapped onto the four GUID fields.
#[test]
fn fields_match_python() {
    let cases = [
        (
            "c232ab00-9414-11ec-b3c8-9f6bdeced846",
            0xc232ab00,
            0x9414,
            0x11ec,
            0xb3,
            0xc8,
            0x9f6bdeced846u64,
        ),
        (
            "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
            0x6ba7b810,
            0x9dad,
            0x11d1,
            0x80,
            0xb4,
            0x00c04fd430c8,
        ),
        (
            "00112233-4455-6677-8899-aabbccddeeff",
            0x00112233,
            0x4455,
            0x6677,
            0x88,
            0x99,
            0xaabbccddeeff,
        ),
    ];
    for (s, d1, d2, d3, hi, lo, node) in cases {
        let n = node.to_be_bytes();
        let d4 = [hi, lo, n[2], n[3], n[4], n[5], n[6], n[7]];
        assert_eq!(Uuid::from_fields(d1, d2, d3, d4), u(s));
        assert_eq!(u(s).to_fields(), (d1, d2, d3, d4));
    }
}

/// Python `UUID.version` (None if the variant is not RFC 4122/9562) and `UUID.variant`.
/// Python reports versions 0 and 15 for the RFC variant; this crate returns `None` for
/// version numbers that RFC 9562 does not define.
#[test]
fn version_and_variant_match_python() {
    let cases = [
        (
            "00000000-0000-0000-0000-000000000000",
            Some(Version::Nil),
            Variant::Ncs,
        ),
        (
            "ffffffff-ffff-ffff-ffff-ffffffffffff",
            Some(Version::Max),
            Variant::Future,
        ),
        (
            "c232ab00-9414-11ec-b3c8-9f6bdeced846",
            Some(Version::V1),
            Variant::Rfc9562,
        ),
        (
            "5df41881-3aed-3515-88a7-2f4a814cf09e",
            Some(Version::V3),
            Variant::Rfc9562,
        ),
        (
            "919108f7-52d1-4320-9bac-f847db4148a8",
            Some(Version::V4),
            Variant::Rfc9562,
        ),
        (
            "2ed6657d-e927-568b-95e1-2665a8aea6a2",
            Some(Version::V5),
            Variant::Rfc9562,
        ),
        (
            "1ec9414c-232a-6b00-b3c8-9f6bdeced846",
            Some(Version::V6),
            Variant::Rfc9562,
        ),
        (
            "017f22e2-79b0-7cc3-98c4-dc0c0c07398f",
            Some(Version::V7),
            Variant::Rfc9562,
        ),
        (
            "2489e9ad-2ee2-8e00-8ec9-32d5f69181c0",
            Some(Version::V8),
            Variant::Rfc9562,
        ),
        ("00000000-0000-4000-0000-000000000000", None, Variant::Ncs),
        (
            "00000000-0000-4000-c000-000000000000",
            None,
            Variant::Microsoft,
        ),
        (
            "00000000-0000-4000-e000-000000000000",
            None,
            Variant::Future,
        ),
        (
            "00000000-0000-f000-8000-000000000000",
            None,
            Variant::Rfc9562,
        ),
        (
            "00000000-0000-0000-8000-000000000000",
            None,
            Variant::Rfc9562,
        ),
        (
            "00000000-0000-2000-8000-000000000000",
            Some(Version::V2),
            Variant::Rfc9562,
        ),
    ];
    for (s, version, variant) in cases {
        assert_eq!(u(s).version(), version, "{s}");
        assert_eq!(u(s).variant(), variant, "{s}");
    }
    let numbers = [
        Version::Nil,
        Version::V1,
        Version::V2,
        Version::V3,
        Version::V4,
        Version::V5,
        Version::V6,
        Version::V7,
        Version::V8,
        Version::Max,
    ]
    .map(Version::to_u8);
    assert_eq!(numbers, [0, 1, 2, 3, 4, 5, 6, 7, 8, 15]);
}

#[test]
fn timestamps_only_for_time_based_versions() {
    for s in [
        "5df41881-3aed-3515-88a7-2f4a814cf09e",
        "919108f7-52d1-4320-9bac-f847db4148a8",
        "00000000-0000-0000-0000-000000000000",
        "ffffffff-ffff-ffff-ffff-ffffffffffff",
        // Version nibble 7 but Microsoft variant: not a v7 UUID.
        "017f22e2-79b0-7cc3-c8c4-dc0c0c07398f",
    ] {
        let id = u(s);
        assert_eq!(id.v7_timestamp_ms(), None, "{s}");
        assert_eq!(id.gregorian_timestamp(), None, "{s}");
        assert_eq!(id.unix_timestamp_ms(), None, "{s}");
    }
    // A v1 UUID from before 1970 has a Gregorian timestamp but no Unix one.
    let old = u("00000000-0000-1000-8000-000000000000");
    assert_eq!(old.gregorian_timestamp(), Some(0));
    assert_eq!(old.unix_timestamp_ms(), None);
    // The NAMESPACE_DNS UUID is a v1 UUID from 1998-02-04 (Python: UUID.time).
    assert_eq!(
        Uuid::NAMESPACE_DNS.gregorian_timestamp(),
        Some(131_059_232_331_511_824)
    );
    assert_eq!(
        Uuid::NAMESPACE_DNS.unix_timestamp_ms(),
        Some(886_630_433_151)
    );
}

#[test]
fn byte_and_integer_conversions() {
    let id = u("67e55044-10b1-426f-9247-bb680e5fe0c8");
    let bytes = [
        0x67, 0xe5, 0x50, 0x44, 0x10, 0xb1, 0x42, 0x6f, 0x92, 0x47, 0xbb, 0x68, 0x0e, 0x5f, 0xe0,
        0xc8,
    ];
    assert_eq!(id.as_bytes(), &bytes);
    assert_eq!(id.into_bytes(), bytes);
    assert_eq!(Uuid::from_bytes(bytes), id);
    assert_eq!(Uuid::from(bytes), id);
    assert_eq!(<[u8; 16]>::from(id), bytes);
    assert_eq!(id.as_ref(), &bytes[..]);
    assert_eq!(id.as_u128(), 0x67e55044_10b1_426f_9247_bb680e5fe0c8);
    assert_eq!(u128::from(id), id.as_u128());
    assert_eq!(Uuid::from(id.as_u128()), id);
    assert_eq!(Uuid::default(), Uuid::NIL);
    assert!(Uuid::NIL.is_nil() && !Uuid::NIL.is_max());
    assert!(Uuid::MAX.is_max() && !Uuid::MAX.is_nil());
    assert!(!id.is_nil());
    // Ord is byte order, which is u128 order.
    assert!(Uuid::from_u128(1) < Uuid::from_u128(2));
    assert!(Uuid::from_u128(1 << 64) > Uuid::from_u128(u64::MAX as u128));
}

/// SQL Server `uniqueidentifier` order. Expected orders come from the cited sources:
/// the dbi services measurements on SQL Server 2022 and .NET `SqlGuid.CompareTo`, which
/// compares `Guid.ToByteArray()` bytes in the order 10..15, 8, 9, 6, 7, 4, 5, 0..3.
#[test]
fn sqlserver_ordering() {
    // dbi services example 1: ORDER BY returns the ffffffff... value first.
    let a = u("ffffffff-ffff-ffff-ffff-000000000000");
    let b = u("00000000-0000-0000-0000-000000000001");
    assert_eq!(a.cmp_sqlserver(&b), Ordering::Less);
    assert_eq!(a.cmp(&b), Ordering::Greater);
    // dbi services example 2: stored bytes 10 00 vs 00 20 in the third group decide.
    let a = u("ffffffff-ffff-0010-0000-aaaaaaaaaaaa");
    let b = u("ffffffff-ffff-2000-0000-aaaaaaaaaaaa");
    assert_eq!(b.cmp_sqlserver(&a), Ordering::Less);

    // One non-zero byte at each stored position, listed from least to most significant
    // for SQL Server (the SqlGuid order reversed).
    let order = [3usize, 2, 1, 0, 5, 4, 7, 6, 9, 8, 15, 14, 13, 12, 11, 10];
    let ids: alloc::vec::Vec<Uuid> = order
        .iter()
        .map(|&stored| {
            let mut le = [0u8; 16];
            le[stored] = 1;
            Uuid::from_bytes_le(le)
        })
        .collect();
    for w in ids.windows(2) {
        assert_eq!(
            w[0].cmp_sqlserver(&w[1]),
            Ordering::Less,
            "{} vs {}",
            w[0],
            w[1]
        );
    }
    // The first group is the least significant: 01000000-... < 00000001-... in SQL Server.
    assert_eq!(
        u("01000000-0000-0000-0000-000000000000")
            .cmp_sqlserver(&u("00000001-0000-0000-0000-000000000000")),
        Ordering::Less
    );

    // Equivalence with SqlGuid.CompareTo on pseudo-random values.
    const SQLGUID_ORDER: [usize; 16] = [10, 11, 12, 13, 14, 15, 8, 9, 6, 7, 4, 5, 0, 1, 2, 3];
    let sqlguid_cmp = |x: &Uuid, y: &Uuid| {
        let (bx, by) = (x.to_bytes_le(), y.to_bytes_le());
        SQLGUID_ORDER
            .iter()
            .map(|&i| bx[i].cmp(&by[i]))
            .find(|o| o.is_ne())
            .unwrap_or(Ordering::Equal)
    };
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut xorshift = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        u128::from(state)
    };
    let mut next = || {
        let v = xorshift() << 64 | xorshift();
        // Bytes of 0 or 1 make ties frequent, so every byte position gets compared.
        Uuid::from_u128(v & 0x0101_0101_0101_0101_0101_0101_0101_0101)
    };
    for _ in 0..10_000 {
        let (x, y) = (next(), next());
        assert_eq!(x.cmp_sqlserver(&y), sqlguid_cmp(&x, &y), "{x} vs {y}");
        assert_eq!(x.cmp_sqlserver(&x), Ordering::Equal);
    }
}
