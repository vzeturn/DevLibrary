"""Prints the cross-check vectors used by `src/tests.rs`, computed with Python's `uuid`.

Run: python crates/dl-uuid/testdata/gen_vectors.py

The output is Rust source for the constant tables in `src/tests.rs`. Every expected value
in those tables comes from this script (Python 3.13) or from RFC 9562 Appendix A.
"""

import uuid

NAMESPACES = [
    ("NAMESPACE_DNS", uuid.NAMESPACE_DNS),
    ("NAMESPACE_URL", uuid.NAMESPACE_URL),
    ("NAMESPACE_OID", uuid.NAMESPACE_OID),
    ("NAMESPACE_X500", uuid.NAMESPACE_X500),
]

NAMES = [
    "www.example.com",
    "python.org",
    "",
    "https://example.com/path?q=1",
    "1.3.6.1",
    "cn=John Doe,o=Example",
    "Tiếng Việt",
]


def rust_str(s: str) -> str:
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


print("// Namespace constants")
for const, ns in NAMESPACES:
    print(f'(Uuid::{const}, "{ns}"),')

print()
print("// (namespace, name, v3, v5)")
for const, ns in NAMESPACES:
    for name in NAMES:
        v3 = uuid.uuid3(ns, name)
        v5 = uuid.uuid5(ns, name)
        print(f'(Uuid::{const}, {rust_str(name)}, "{v3}", "{v5}"),')

print()
print("// (uuid, bytes_le) -- Python bytes_le is the layout of .NET Guid.ToByteArray()")
for s in [
    "00112233-4455-6677-8899-aabbccddeeff",
    "35918bc9-196d-40ea-9779-889d79b753f0",
    "017f22e2-79b0-7cc3-98c4-dc0c0c07398f",
    "00000000-0000-0000-0000-000000000000",
    "ffffffff-ffff-ffff-ffff-ffffffffffff",
]:
    u = uuid.UUID(s)
    print(f'("{u}", "{u.bytes_le.hex()}"),')

print()
print("// (uuid, time_low, time_mid, time_hi_version, clock_seq_hi, clock_seq_low, node)")
for s in [
    "c232ab00-9414-11ec-b3c8-9f6bdeced846",
    "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
    "00112233-4455-6677-8899-aabbccddeeff",
]:
    u = uuid.UUID(s)
    print(
        f'("{u}", 0x{u.time_low:08x}, 0x{u.time_mid:04x}, 0x{u.time_hi_version:04x}, '
        f"0x{u.clock_seq_hi_variant:02x}, 0x{u.clock_seq_low:02x}, 0x{u.node:012x}),"
    )

print()
print("// (uuid, version or None, variant)")
for s in [
    "00000000-0000-0000-0000-000000000000",
    "ffffffff-ffff-ffff-ffff-ffffffffffff",
    "c232ab00-9414-11ec-b3c8-9f6bdeced846",
    "5df41881-3aed-3515-88a7-2f4a814cf09e",
    "919108f7-52d1-4320-9bac-f847db4148a8",
    "2ed6657d-e927-568b-95e1-2665a8aea6a2",
    "1ec9414c-232a-6b00-b3c8-9f6bdeced846",
    "017f22e2-79b0-7cc3-98c4-dc0c0c07398f",
    "2489e9ad-2ee2-8e00-8ec9-32d5f69181c0",
    "00000000-0000-4000-0000-000000000000",
    "00000000-0000-4000-c000-000000000000",
    "00000000-0000-4000-e000-000000000000",
    "00000000-0000-f000-8000-000000000000",
    "00000000-0000-0000-8000-000000000000",
]:
    u = uuid.UUID(s)
    print(f'("{u}", {u.version}, {u.variant!r}),')

print()
print("// v1 timestamp (60-bit, 100 ns since 1582-10-15) per Python UUID.time")
u = uuid.UUID("c232ab00-9414-11ec-b3c8-9f6bdeced846")
print(f"0x{u.time:x} = {u.time}, unix ms = {(u.time - 0x01B21DD213814000) // 10_000}")
t = uuid.NAMESPACE_DNS.time
print(f"NAMESPACE_DNS: {t}, unix ms = {(t - 0x01B21DD213814000) // 10_000}")

print()
print("// Parse: input -> canonical (Python accepts each of these)")
for s in [
    "67e55044-10b1-426f-9247-bb680e5fe0c8",
    "67E55044-10B1-426F-9247-BB680E5FE0C8",
    "67e5504410b1426f9247bb680e5fe0c8",
    "{67e55044-10b1-426f-9247-bb680e5fe0c8}",
    "urn:uuid:67e55044-10b1-426f-9247-bb680e5fe0c8",
    "urn:uuid:67E55044-10B1-426F-9247-BB680E5FE0C8",
    "{67E55044-10b1-426F-9247-bb680E5fe0C8}",
]:
    print(f'("{s}", "{uuid.UUID(s)}"),')

# Python only strips a lowercase "urn:uuid:" prefix. dl-uuid also accepts "URN:UUID:"
# because RFC 8141 makes the "urn" scheme and the namespace ID case-insensitive.
