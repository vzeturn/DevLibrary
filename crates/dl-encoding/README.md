# dl-encoding

Hex (Base16), Base64 and Base32 from [RFC 4648](https://www.rfc-editor.org/rfc/rfc4648),
with strict decoders. `#![no_std]` + `alloc`, no dependencies, no `unsafe`, builds for
`wasm32-unknown-unknown`.

Part of [DevLibrary](https://github.com/vzeturn/DevLibrary).

## Replaces

| Ecosystem | Package |
|---|---|
| C# / .NET | `Convert.ToBase64String`, `Convert.FromBase64String`, `Convert.ToHexString`, Base32 helpers in `Otp.NET` |
| TypeScript / JS | `btoa`/`atob`, `Buffer.from(x, "base64" \| "hex")`, `base64-js`, `hi-base32` |
| Go | `encoding/base64`, `encoding/hex`, `encoding/base32` |
| Python | `base64`, `binascii` |
| Rust | `base64`, `hex`, `data-encoding` |

## Features

- Base64 with four ready-made engines: `STANDARD`, `STANDARD_NO_PAD`, `URL_SAFE`,
  `URL_SAFE_NO_PAD` (JWT).
- Base32 (`A-Z2-7`) with and without padding, plus `decode_secret` for human-entered
  keys such as TOTP secrets (ignores case, spaces, dashes and padding).
- Hex in lower or upper case, decoding of either case, `decode_array::<N>` for fixed-size
  keys.
- **Strict decoding**: invalid characters, impossible lengths, misplaced padding and
  non-zero trailing bits are errors, so every accepted input has exactly one encoding
  (important for signatures and tokens).
- Errors carry the byte offset (`Error::InvalidByte { index, byte }`).
- Allocation-reusing variants: `encode_into`, `decode_into`.

## Installation

```toml
[dependencies]
dl-encoding = { git = "https://github.com/vzeturn/DevLibrary" }
```

From a local checkout: `dl-encoding = { path = "../DevLibrary/crates/dl-encoding" }`.
Inside the workspace: `dl-encoding.workspace = true`. Minimum Rust version: 1.88.

## Quick start

```rust
use dl_encoding::{base32, base64, hex, Error};

// Base64
assert_eq!(base64::STANDARD.encode(b"foobar"), "Zm9vYmFy");
assert_eq!(base64::URL_SAFE_NO_PAD.encode([0xfb, 0xff]), "-_8");
assert_eq!(base64::STANDARD.decode("Zm9vYg==").unwrap(), b"foob");

// Strictness: wrong alphabet, missing padding, extra bits are rejected.
assert!(base64::URL_SAFE_NO_PAD.decode("+/8").is_err());
assert_eq!(base64::STANDARD.decode("Zg"), Err(Error::InvalidLength { len: 2 }));
assert_eq!(base64::STANDARD_NO_PAD.decode("Zh"), Err(Error::TrailingBits { index: 1 }));

// Hex
assert_eq!(hex::encode([0xde, 0xad, 0xbe, 0xef]), "deadbeef");
assert_eq!(hex::encode_upper([0x01, 0xab]), "01AB");
let key: [u8; 4] = hex::decode_array("DEADbeef").unwrap();
assert_eq!(key, [0xde, 0xad, 0xbe, 0xef]);

// Base32, including secrets typed by people
assert_eq!(base32::STANDARD.encode(b"foo"), "MZXW6===");
let secret = base32::decode_secret("gezd gnbv-gy3t qojq").unwrap();
assert_eq!(secret, b"1234567890");
```

## API overview

| Item | Purpose |
|---|---|
| `base64::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD}` | `Engine` values |
| `base64::Engine::{encode, encode_into, decode, decode_into, encoded_len}` | Base64 |
| `base32::{STANDARD, STANDARD_NO_PAD}` + `Engine::{encode, decode, encoded_len}` | Base32 |
| `base32::decode_secret(&str)` | Lenient Base32 for human input |
| `hex::{encode, encode_upper, decode, decode_into, decode_array}` | Hex |
| `Error::{InvalidByte { index, byte }, InvalidLength { len }, TrailingBits { index }}` | Errors |

## Performance

`cargo bench -p dl-encoding`, 64 KiB input, Intel Core i7-9850H, Windows, rustc 1.100
(shared machine, expect ±30 %):

| Case | Throughput |
|---|---:|
| base64 encode / decode | 559 / 282 MiB/s |
| hex encode / decode | 188 / 141 MiB/s |
| base32 encode / decode | 110 / 145 MiB/s |

These are portable scalar loops. SIMD versions (as in `base64-simd`) are on the roadmap;
for typical payloads (tokens, keys, small attachments) the cost is negligible.

## Standards

- [RFC 4648](https://www.rfc-editor.org/rfc/rfc4648): every test vector of section 10 is a
  unit test.

## Limitations

- No MIME line wrapping (76 columns) and no whitespace skipping in Base64; strip line
  breaks before decoding.
- No Base32hex, Crockford Base32, Base58 or Ascii85.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0) at your option.
