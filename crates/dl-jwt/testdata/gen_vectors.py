"""Generates the interop test vectors used in dl-jwt's unit tests (src/tests.rs).

Run:  python gen_vectors.py

Tokens are built with nothing but the Python standard library (json, hmac, hashlib,
base64), so they are independent of the Rust code under test. JSON is serialized
compactly with `ensure_ascii=False` and members in the order dl-jwt writes them
(header: alg, typ, kid, cty, extras; claims: iss, sub, aud, exp, nbf, iat, jti,
extras), so `encode` must reproduce these tokens byte for byte.

The script also recomputes the RFC 7515 Appendix A.1 signature from the JWK key and
asserts it matches the RFC, as a check of the method itself.

Tested with Python 3.13. The output is deterministic.
"""

import base64
import hashlib
import hmac
import json

HASHES = {"HS256": hashlib.sha256, "HS384": hashlib.sha384, "HS512": hashlib.sha512}


def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def b64url_decode(text: str) -> bytes:
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))


def compact(obj) -> bytes:
    return json.dumps(obj, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def sign(alg: str, key: bytes, signing_input: bytes) -> str:
    return b64url(hmac.new(key, signing_input, HASHES[alg]).digest())


def token(header: dict, claims: dict, key: bytes) -> str:
    signing_input = b64url(compact(header)) + "." + b64url(compact(claims))
    return signing_input + "." + sign(header["alg"], key, signing_input.encode("ascii"))


def rfc7515_a1() -> None:
    jwk_k = "AyM1SysPpbyDfgZld3umj1qzKObwVMkoqQ-EstJQLr_T-1qS0gZH75aKtMN3Yj0iPS4hcgUuTwjAzZr1Z9CAow"
    header = b'{"typ":"JWT",\r\n "alg":"HS256"}'
    payload = b'{"iss":"joe",\r\n "exp":1300819380,\r\n "http://example.com/is_root":true}'
    signing_input = (b64url(header) + "." + b64url(payload)).encode("ascii")
    sig = sign("HS256", b64url_decode(jwk_k), signing_input)
    assert sig == "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", sig
    assert len(b64url_decode(jwk_k)) == 64
    print("// RFC 7515 A.1 signature reproduced:", sig)


K256 = b"dl-jwt/HS256/test-key/0123456789"
K384 = b"dl-jwt HS384 test key, forty-eight bytes long!!!"
K512 = b"dl-jwt HS512 test key; it is exactly sixty-four bytes long, see?"
KDOTNET = b"ThisIsMyVerySecretKeyForJwtSigning!"
assert (len(K256), len(K384), len(K512)) == (32, 48, 64)
assert len(KDOTNET) >= 32

VECTORS = [
    (
        "HS256_FULL",
        {"alg": "HS256", "typ": "JWT"},
        {
            "iss": "https://auth.example.com",
            "sub": "user-42",
            "aud": "orders-api",
            "exp": 1700000900,
            "nbf": 1700000000,
            "iat": 1700000000,
            "jti": "a1b2c3",
            "name": "Nguyễn Văn An",
            "admin": True,
        },
        K256,
    ),
    (
        "HS384_KID",
        {"alg": "HS384", "typ": "JWT", "kid": "2024-06"},
        {"sub": "svc", "aud": ["billing", "orders-api"], "exp": 1700003600, "iat": 1700000000},
        K384,
    ),
    (
        "HS512",
        {"alg": "HS512", "typ": "JWT"},
        {"iss": "x", "exp": 1700000060, "scope": "read write"},
        K512,
    ),
    (
        # Claim set in the shape ASP.NET Core's JwtSecurityTokenHandler writes for
        # ClaimTypes.Name / NameIdentifier / Role with the default outbound claim map.
        "DOTNET",
        {"alg": "HS256", "typ": "JWT"},
        {
            "unique_name": "alice",
            "nameid": "42",
            "role": ["Admin", "User"],
            "nbf": 1700000000,
            "exp": 1700003600,
            "iat": 1700000000,
            "iss": "https://localhost:5001",
            "aud": "https://localhost:5001",
        },
        KDOTNET,
    ),
    (
        "FRACTIONAL_EXP",
        {"alg": "HS256", "typ": "JWT"},
        {"sub": "f", "exp": 1700000000.75, "iat": 1699999999.5},
        K256,
    ),
    (
        # The shape npm jsonwebtoken produces for jwt.sign({ userId: 7 }, key, { expiresIn: "15m" }).
        "NODE",
        {"alg": "HS256", "typ": "JWT"},
        {"userId": 7, "iat": 1700000000, "exp": 1700000900},
        K256,
    ),
]


def main() -> None:
    rfc7515_a1()
    for name, header, claims, key in VECTORS:
        print(f'const {name}: &str = "{token(header, claims, key)}";')


if __name__ == "__main__":
    main()
