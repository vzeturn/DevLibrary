"""Independent HOTP/TOTP reference for the cross-check vectors in the unit tests of
src/hotp.rs, src/totp.rs and src/recovery.rs.

Only the Python standard library (hmac, hashlib, base64) is used, so the expected values do
not come from the Rust implementation under test. Run with Python 3.8+:

    python crates/dl-otp/testdata/cross_check.py
"""

import base64
import hashlib
import hmac
import struct

ALGS = {"SHA1": hashlib.sha1, "SHA256": hashlib.sha256, "SHA512": hashlib.sha512}


def hotp(key: bytes, counter: int, digits: int = 6, alg: str = "SHA1") -> str:
    mac = hmac.new(key, struct.pack(">Q", counter), ALGS[alg]).digest()
    off = mac[-1] & 0x0F
    value = int.from_bytes(mac[off : off + 4], "big") & 0x7FFFFFFF
    return str(value % 10**digits).zfill(digits)


def totp(key: bytes, t: int, digits=6, alg="SHA1", period=30, t0=0) -> str:
    return hotp(key, (t - t0) // period, digits, alg)


RFC_SEED = {
    "SHA1": b"12345678901234567890",
    "SHA256": b"12345678901234567890123456789012",
    "SHA512": b"1234567890123456789012345678901234567890123456789012345678901234",
}


def main() -> None:
    k = RFC_SEED["SHA1"]
    print("# RFC 4226 Appendix D, 6 digits")
    print([hotp(k, c) for c in range(10)])
    for d in (7, 8, 9, 10):
        print(f"# RFC 4226 seed, {d} digits, counters 0..9")
        print([hotp(k, c, d) for c in range(10)])
    print("# RFC 6238 Appendix B, 8 digits")
    for t in (59, 1111111109, 1111111111, 1234567890, 2000000000, 20000000000):
        print(t, [totp(RFC_SEED[a], t, 8, a) for a in ("SHA1", "SHA256", "SHA512")])
    print("# HOTP SHA256 / SHA512 with RFC 6238 seeds, 6 digits, counters 0..4")
    for a in ("SHA256", "SHA512"):
        print(a, [hotp(RFC_SEED[a], c, 6, a) for c in range(5)])
    print("# TOTP period 60, SHA1 seed, 6 digits")
    for t in (0, 59, 60, 1111111109, 1700000000):
        print(t, totp(k, t, 6, "SHA1", 60))
    print("# TOTP t0 = 1000, period 30, SHA1 seed, 6 digits")
    for t in (1000, 1029, 1030, 1111111109):
        print(t, totp(k, t, 6, "SHA1", 30, 1000))
    secret = base64.b32decode("JBSWY3DPEHPK3PXP")
    print("# JBSWY3DPEHPK3PXP (b'Hello!\\xde\\xad\\xbe\\xef'), TOTP 6 digits period 30")
    for t in (0, 1700000000, 1700000029, 1700000030):
        print(t, totp(secret, t))
    print("# JBSWY3DPEHPK3PXP, TOTP SHA256 8 digits period 60")
    for t in (1700000000,):
        print(t, totp(secret, t, 8, "SHA256", 60))
    print("# JBSWY3DPEHPK3PXP, TOTP SHA512 10 digits period 30")
    for t in (1700000000,):
        print(t, totp(secret, t, 10, "SHA512", 30))
    print("# SHA-256 of normalised recovery code 'k7m29xq4'")
    print(hashlib.sha256(b"k7m29xq4").hexdigest())


if __name__ == "__main__":
    main()
