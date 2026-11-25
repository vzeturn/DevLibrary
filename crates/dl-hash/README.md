# dl-hash

SHA-1, SHA-2 (224/256/384/512), MD5, MD4 (for NTLM only), HMAC, PBKDF2, CRC-32 and constant-time comparison.
`#![no_std]`, no dependencies, no `unsafe`, builds for `wasm32-unknown-unknown`.

Part of [DevLibrary](https://github.com/vzeturn/devlibrary). Used by `dl-otp`, `dl-jwt`
and `dl-uuid`.

## Replaces

| Ecosystem | Package |
|---|---|
| C# / .NET | `SHA1`, `SHA256`, `SHA512`, `MD5`, `HMACSHA256`, `Rfc2898DeriveBytes.Pbkdf2`, `CryptographicOperations.FixedTimeEquals` |
| TypeScript / JS | `crypto.createHash`, `crypto.createHmac`, `crypto.pbkdf2Sync`, `crypto.timingSafeEqual`, `crc-32` |
| Go | `crypto/sha1`, `crypto/sha256`, `crypto/sha512`, `crypto/md5`, `crypto/hmac`, `x/crypto/pbkdf2`, `hash/crc32`, `crypto/subtle` |
| Python | `hashlib`, `hmac`, `hashlib.pbkdf2_hmac`, `zlib.crc32` |
| Rust | `sha1`, `sha2`, `md-5`, `hmac`, `pbkdf2`, `crc32fast`, `subtle` |

## Features

- One `Digest` trait for every hash: `digest(data)` in one call, or streaming with
  `update` / `chain` / `finalize`.
- `Hmac<D>` over any digest, with constant-time `verify`.
- `pbkdf2::<D>(password, salt, iterations, &mut out)` (RFC 8018), matching .NET
  `Rfc2898DeriveBytes.Pbkdf2` and Go `pbkdf2.Key`.
- `crc32` (IEEE, slicing-by-8) for zip, gzip, PNG and file checksums.
- `ct_eq(a, b)`: comparison whose time depends only on the lengths.
- Checked against the NIST, RFC 1321, RFC 2202, RFC 4231 and RFC 6070 test vectors.

## Installation

```toml
[dependencies]
dl-hash = { git = "https://github.com/vzeturn/devlibrary" }
```

From a local checkout: `dl-hash = { path = "../DevLibrary/crates/dl-hash" }`. Inside the
workspace: `dl-hash.workspace = true`. Minimum Rust version: 1.88.

## Quick start

```rust
use dl_hash::{ct_eq, crc32, pbkdf2, Digest, Hmac, Sha256, Sha512};

// One-shot and streaming hashes give the same digest.
let a = Sha256::digest(b"hello world");
let b = Sha256::default().chain(b"hello ").chain(b"world").finalize();
assert_eq!(a, b);

// HMAC: sign a webhook payload and verify it in constant time.
let secret = b"webhook-secret";
let tag = Hmac::<Sha256>::mac(secret, br#"{"event":"paid"}"#);
assert!(Hmac::<Sha256>::new(secret).chain(br#"{"event":"paid"}"#).verify(&tag));

// PBKDF2 key derivation (use >= 600 000 iterations for passwords, per OWASP).
let mut key = [0u8; 32];
pbkdf2::<Sha256>(b"password", b"salt", 1, &mut key);
assert_eq!(key[0], 0x12);

// Constant-time comparison of tokens.
assert!(ct_eq(b"token", b"token"));
assert!(!ct_eq(b"token", b"tokem"));

// CRC-32 check value from the standard.
assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
let _ = Sha512::digest(b"");
```

### Streaming a file

```rust,no_run
use dl_hash::{Digest, Sha256};
use std::io::Read;

let mut file = std::fs::File::open("invoice.pdf")?;
let mut hasher = Sha256::default();
let mut buf = [0u8; 64 * 1024];
loop {
    let n = file.read(&mut buf)?;
    if n == 0 { break; }
    hasher.update(&buf[..n]);
}
let digest = hasher.finalize();
println!("{}", digest.iter().map(|b| format!("{b:02x}")).collect::<String>());
# Ok::<(), std::io::Error>(())
```

## API overview

| Item | Purpose |
|---|---|
| `Digest` trait | `OUTPUT_LEN`, `BLOCK_LEN`, `update`, `chain`, `finalize`, `digest` |
| `Sha1`, `Sha224`, `Sha256`, `Sha384`, `Sha512`, `Md5`, `Md4` | Hash functions (`Md4` only for NTLM's `NTOWFv2`) |
| `Hmac<D>` | `new(key)`, `update`, `chain`, `finalize`, `verify(tag)`, `mac(key, data)` |
| `pbkdf2::<D>(password, salt, iterations, out)` | Key derivation (panics if `iterations == 0`) |
| `Crc32`, `crc32(data)` | Streaming and one-shot CRC-32 |
| `ct_eq(a, b)` | Constant-time equality |

## Security notes

- **SHA-1 and MD5 are broken for collision resistance.** They are here because protocols
  require them (HMAC-SHA1 in TOTP, legacy vendor APIs, checksums). HMAC-SHA1 is still
  considered secure as a MAC.
- **MD4 is completely broken.** It is here only because NTLM (MS-NLMP `NTOWFv2`, used by
  `dl-tds` for `DOMAIN\user` logins) hashes the password with it.
- Do not hash passwords with a bare SHA-256; use `pbkdf2` with a random salt and a high
  iteration count.
- The code is constant-time with respect to the data being hashed, but it has not been
  audited and is not hardened against physical side channels.

## Performance

`cargo bench -p dl-hash`, Intel Core i7-9850H, Windows, rustc 1.100 (shared machine,
expect ±30 %):

| Case | Throughput / time |
|---|---:|
| SHA-1, 64 KiB | 185 MiB/s |
| SHA-256, 64 KiB | 79 MiB/s |
| SHA-512, 64 KiB | 110 MiB/s |
| MD5, 64 KiB | 167 MiB/s |
| CRC-32, 64 KiB | 890 MiB/s |
| HMAC-SHA256, 64-byte message | 2.7 µs |

These are portable implementations without SHA-NI or SIMD, so bulk hashing is several
times slower than OpenSSL. For tokens, HMACs and OTP codes the cost is microseconds.
Hardware acceleration (SHA-NI, ARMv8 crypto extensions) is on the roadmap.

## Standards

- [FIPS 180-4](https://csrc.nist.gov/pubs/fips/180-4/upd1/final) (SHA-1, SHA-2)
- [RFC 1321](https://www.rfc-editor.org/rfc/rfc1321) (MD5), [RFC 1320](https://www.rfc-editor.org/rfc/rfc1320) (MD4)
- [RFC 2104](https://www.rfc-editor.org/rfc/rfc2104), [RFC 2202](https://www.rfc-editor.org/rfc/rfc2202), [RFC 4231](https://www.rfc-editor.org/rfc/rfc4231) (HMAC and its test vectors)
- [RFC 8018](https://www.rfc-editor.org/rfc/rfc8018), [RFC 6070](https://www.rfc-editor.org/rfc/rfc6070) (PBKDF2 and its test vectors)

## Limitations

- No SHA-3, BLAKE2/3, Argon2, bcrypt or scrypt yet (roadmap: Argon2id for passwords).
- No hardware acceleration.
- Not independently audited.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0) at your option.
