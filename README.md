# DevLibrary

A family of small, fast Rust libraries for business software, written from scratch with
**zero third-party dependencies**. Each crate replaces a package you would otherwise pull
from NuGet, npm, Go modules or PyPI: JSON, money decimals, dates, logging, UUIDs, JWT,
TOTP/MFA, QR codes, CLI parsing, hashing, encodings, randomness, and Vietnam-specific
helpers (amount in words, tax codes, VietQR).

- **No supply chain.** The only code in your build is this repository and the Rust
  standard library. Nothing to audit upstream, no transitive breakage.
- **Correct first.** Every crate is tested against the official vectors of its standard
  (RFCs, NIST, ISO) or against an independent reference implementation, and documents
  where it deliberately differs.
- **Runs everywhere Rust runs.** Linux, macOS and Windows, and `wasm32-unknown-unknown`
  for the browser. Most crates are `#![no_std]` + `alloc`.
- **Safe by default.** `unsafe` is denied workspace-wide (allowed only for the OS calls
  in `dl-rand` and the C ABI in `dl-ffi`). Parsers return typed errors with positions and
  never panic on input.
- **Documented.** Every crate has a full guide in its README, and every example in it is
  compiled and run as a test.

[Tiếng Việt](#tiếng-việt) · [Crates](#crates) · [Getting started](#getting-started) ·
[Platform support](#platform-support) · [Contributing](CONTRIBUTING.md) ·
[Security](SECURITY.md)

## Crates

| Crate | What it does | Replaces | `no_std` | Docs |
|---|---|---|:---:|---|
| [`dl-json`](crates/dl-json) | Strict RFC 8259 parser/serializer, ordered `Map`, exact raw numbers, JSON Pointer, `json!`, `ToJson`/`FromJson` | Newtonsoft.Json, System.Text.Json, serde_json, gjson | ✅ | [README](crates/dl-json/README.md) |
| [`dl-decimal`](crates/dl-decimal) | 38-digit exact decimal for money, 7 rounding modes, VAT, bill splitting, grouped formatting | C# `decimal`, shopspring/decimal, decimal.js | ✅ | [README](crates/dl-decimal/README.md) |
| [`dl-time`](crates/dl-time) | Dates, times, offsets, durations; RFC 3339, ISO 8601, HTTP dates; .NET-style format patterns in English and Vietnamese | date-fns, dayjs, .NET `DateTime` formatting | ✅ | [README](crates/dl-time/README.md) |
| [`dl-log`](crates/dl-log) | Structured logging: macros with fields, filters (`RUST_LOG` syntax), text/JSON/CLEF/logfmt, rolling files, async sink | Serilog, winston, pino, logrus, tracing-subscriber | – | [README](crates/dl-log/README.md) |
| [`dl-uuid`](crates/dl-uuid) | UUID v4/v7/v3/v5 (RFC 9562), every text form, .NET `Guid` byte layout, SQL Server ordering | System.Guid helpers, npm uuid, google/uuid | ✅ | [README](crates/dl-uuid/README.md) |
| [`dl-jwt`](crates/dl-jwt) | JWT with HS256/384/512, strict validation, key rotation by `kid` | System.IdentityModel.Tokens.Jwt, jsonwebtoken, PyJWT | ✅ | [README](crates/dl-jwt/README.md) |
| [`dl-otp`](crates/dl-otp) | HOTP/TOTP for MFA, `otpauth://` URIs, replay protection, recovery codes | Otp.NET, otplib, pyotp | ✅ | [README](crates/dl-otp/README.md) |
| [`dl-qr`](crates/dl-qr) | QR Code encoder, versions 1–40, optimal segmentation, SVG and terminal output; bit-exact with segno | QRCoder, qrcode, qrcode.react | ✅ | [README](crates/dl-qr/README.md) |
| [`dl-cli`](crates/dl-cli) | Command-line parser: subcommands, env fallbacks, typed values, suggestions, generated help | clap, commander, System.CommandLine, cobra | – | [README](crates/dl-cli/README.md) |
| [`dl-vn`](crates/dl-vn) | Vietnam: amount in words for e-invoices, diacritics, collation, MST, phone, CCCD, VietQR | ad-hoc helpers in every VN project | ✅ | [README](crates/dl-vn/README.md) |
| [`dl-hash`](crates/dl-hash) | SHA-1/2, MD5, HMAC, PBKDF2, CRC-32, constant-time compare | System.Security.Cryptography hashes, Node `crypto`, hashlib | ✅ | [README](crates/dl-hash/README.md) |
| [`dl-encoding`](crates/dl-encoding) | Strict Base64 / Base32 / hex (RFC 4648) | `Convert.ToBase64String`, Buffer, base64 crate | ✅ | [README](crates/dl-encoding/README.md) |
| [`dl-rand`](crates/dl-rand) | OS CSPRNG and fast xoshiro256** | RandomNumberGenerator, crypto.getRandomValues, rand | – | [README](crates/dl-rand/README.md) |
| [`dl-bench`](crates/dl-bench) | Tiny benchmark harness used by the other crates | criterion | – | [README](crates/dl-bench/README.md) |
| [`dl-ffi`](crates/dl-ffi) | C ABI for .NET, Go (cgo), Flutter and WebAssembly hosts: the rules stay in Rust | hand-ported rules in every language | – | [README](crates/dl-ffi/README.md) |
| [`dl-tls`](crates/dl-tls) | TLS 1.2/1.3 client over the OS stack: SChannel on Windows, system OpenSSL (loaded at run time) on Linux | rustls, native-tls, Go `crypto/tls` client | – | [README](crates/dl-tls/README.md) |
| [`dl-http`](crates/dl-http) | HTTP/1.1 client: URLs, keep-alive pool, deadlines, chunked bodies, `HTTP(S)_PROXY` and CONNECT; sans-I/O message layer | Go `net/http` client, `HttpClient`, axios, ureq | – | [README](crates/dl-http/README.md) |
| [`dl-ws`](crates/dl-ws) | WebSocket (RFC 6455): `ws://`/`wss://` client over dl-http (TLS, proxies, deadlines), frame codec, server-side framing | nhooyr.io/websocket, coder/websocket, `ClientWebSocket`, ws, tungstenite | – | [README](crates/dl-ws/README.md) |
| [`dl-tds`](crates/dl-tds) | SQL Server client (TDS 7.4/8): SQL login, `sp_executesql` parameters, every common type, collation code pages, login-only/required/strict TLS; conversions match go-mssqldb | go-mssqldb, Microsoft.Data.SqlClient, tedious, tiberius | – | [README](crates/dl-tds/README.md) |
| [`dl-winsvc`](crates/dl-winsvc) | Windows services (dispatcher, SCM install/start/stop/delete) and DPAPI; stubs elsewhere | `x/sys/windows/svc` + `mgr`, `ProtectedData`, windows-service | – | [README](crates/dl-winsvc/README.md) |

### Using the crates from other languages

| Host | How | Package |
|---|---|---|
| .NET 8 / 10 | [`bindings/dotnet`](bindings/dotnet): `LibraryImport` binding, AOT and trimming compatible, no `PackageReference` | `DevLibrary` NuGet with `runtimes/{win-x64,win-arm64,linux-x64,linux-arm64,osx-x64,osx-arm64}/native` (release assets) |
| C / C++ / cgo | [`crates/dl-ffi/include/dl_ffi.h`](crates/dl-ffi/include/dl_ffi.h) | `dl-ffi-<rid>.zip` (release assets) |
| Browser / Node | `dl_ffi.wasm` (≈45 KB) | release asset |

How the crates depend on each other (all arrows stay inside this repository):

```text
dl-jwt ──► dl-json, dl-hash, dl-encoding
dl-otp ──► dl-hash, dl-encoding, dl-rand
dl-uuid ─► dl-hash, dl-rand
dl-log ──► dl-time, dl-json
dl-http ─► dl-tls, dl-encoding
dl-ws ───► dl-http, dl-hash, dl-encoding, dl-rand
dl-tds ──► dl-tls, dl-decimal, dl-time, dl-uuid
dl-tls, dl-winsvc: OS libraries only (secur32/crypt32/advapi32 on Windows, libssl at run time on Linux)
dl-json, dl-decimal, dl-time, dl-qr, dl-vn, dl-cli, dl-hash, dl-encoding, dl-rand: no dependencies
```

## Getting started

The crates are not on crates.io yet. Add them as git dependencies; cargo finds each
package by name inside the repository:

```toml
[dependencies]
dl-json    = { git = "https://github.com/vzeturn/DevLibrary" }
dl-decimal = { git = "https://github.com/vzeturn/DevLibrary" }
dl-time    = { git = "https://github.com/vzeturn/DevLibrary" }
```

For reproducible builds, pin a tag or commit and update deliberately:

```toml
dl-json = { git = "https://github.com/vzeturn/DevLibrary", tag = "v0.1.0" }
# or: rev = "<commit sha>"
```

With a local checkout next to your project, use path dependencies instead:

```toml
dl-json = { path = "../DevLibrary/crates/dl-json" }
```

A taste of what that gives you, an invoice line priced exactly, serialized as JSON and
logged:

```rust,ignore
use dl_decimal::Decimal;
use dl_json::json;
use dl_log::{info, ConsoleSink, Logger, JsonFormat};

dl_log::init(Logger::builder().sink(ConsoleSink::stdout().with_format(JsonFormat::new())).build());

let unit: Decimal = "19990.50".parse()?;
let net = unit * Decimal::from(3);              // 59971.50, exact
let vat = net.percent_of(Decimal::from(8))?;    // 4797.7200
let id = dl_uuid::Uuid::now_v7();

let line = json!({ "id": id.to_string(), "net": net.to_string(), "vat": vat.round(0).to_string() });
info!("invoice line created"; line = line);
```

Each crate's README has a quick start, a guide with runnable examples, an API overview,
error tables, benchmarks and a list of known limitations.

### Requirements

- Rust **1.88** or newer (edition 2024). Install with [rustup](https://rustup.rs).
- For WebAssembly: `rustup target add wasm32-unknown-unknown`.

## Platform support

| Platform | Status |
|---|---|
| Linux (x86-64, AArch64; glibc and musl) | Tested in CI |
| macOS | Tested in CI |
| Windows 10+ | Tested in CI |
| `wasm32-unknown-unknown` (browser, Node) | Every crate builds; APIs that need a clock take the time as an argument there. `dl-rand` needs a one-line host import (see its README) |
| `wasm32-wasip1` | Supported by `dl-rand` (WASI `random_get`) and the crates that use it |
| `no_std` + `alloc` | Crates marked ✅ above; some need `default-features = false` |

## Building and testing

```sh
git clone https://github.com/vzeturn/DevLibrary
cd DevLibrary
cargo test --workspace                       # unit tests + every README example
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo build --workspace --exclude dl-bench --target wasm32-unknown-unknown
cargo bench -p dl-json                       # any crate; add `-- <filter>` to select cases
```

Linux from macOS or Windows, with Docker:

```sh
docker run --rm -v "$PWD":/w -w /w rust:latest cargo test --workspace
```

Some crates verify themselves against reference implementations in other languages.
Those scripts live next to the tests (`testdata/` or `tests/` folders, for example
`crates/dl-qr/tests/gen_vectors.py`); they are only needed to regenerate vectors, never to
build or use the crates.

## Design rules

The full list is in [CONTRIBUTING.md](CONTRIBUTING.md). In short:

1. No third-party crates, not even for tests or benchmarks.
2. Expected values in tests come from a standard or an independent implementation, never
   from the code under test.
3. No panics on untrusted input; one typed `Error` per crate with positions where useful.
4. Every public item is documented; every README example compiles and runs.
5. Every crate builds for `wasm32-unknown-unknown`.

## Status and roadmap

Version 0.1: APIs may still change before 1.0. Each crate's README lists what it does not
do yet. Planned next: HTTP/1.1 server and WebSocket, macOS TLS (Security.framework),
PostgreSQL driver (SCRAM-SHA-256), an embedded key-value store for agent outboxes,
deflate/gzip (and PNG output for `dl-qr`), RS256/ES256 for `dl-jwt`, Argon2id, IANA time
zones for `dl-time`, and bindings for Node.js and Python.

Cryptographic code here (`dl-hash`, `dl-jwt`, `dl-otp`) follows the standards and their
test vectors but **has not been independently audited**. See [SECURITY.md](SECURITY.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this project, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

---

## Tiếng Việt

DevLibrary là bộ thư viện Rust viết từ đầu, **không phụ thuộc thư viện bên thứ ba nào**,
dành cho phần mềm doanh nghiệp: hoá đơn điện tử, POS, quản lý khách sạn, chấm công,
thanh toán. Mỗi crate thay thế một gói quen thuộc trên NuGet, npm, Go hoặc PyPI.

**Điểm chính**

- Không có chuỗi cung ứng bên ngoài: bản build chỉ gồm mã trong repo này và thư viện
  chuẩn của Rust.
- Kiểm thử bằng bộ test vector chính thức (RFC, NIST, ISO) hoặc so với một bản cài đặt
  độc lập (Python `decimal`, segno, zxing, Node.js...).
- Chạy trên Linux, macOS, Windows và trình duyệt (WebAssembly).
- Có sẵn tiện ích cho Việt Nam trong [`dl-vn`](crates/dl-vn): đọc số tiền bằng chữ cho
  hoá đơn điện tử, bỏ dấu tiếng Việt, sắp xếp tên theo bảng chữ cái tiếng Việt, kiểm tra
  mã số thuế, chuẩn hoá số điện thoại, đọc số CCCD, tạo và đọc mã VietQR.
- [`dl-decimal`](crates/dl-decimal) tính tiền chính xác (VND, VAT 8%/10%, làm tròn tới
  500/1.000 đồng, chia hoá đơn); [`dl-time`](crates/dl-time) định dạng ngày giờ kiểu
  .NET kèm tiếng Việt (`"dddd, 'ngày' dd/MM/yyyy"` → `Chủ Nhật, ngày 15/11/2026`).

**Cài đặt nhanh**

```toml
[dependencies]
dl-vn      = { git = "https://github.com/vzeturn/DevLibrary" }
dl-decimal = { git = "https://github.com/vzeturn/DevLibrary" }
```

```rust,ignore
use dl_vn::words::vnd_to_words;
assert_eq!(vnd_to_words(1_250_000), "Một triệu hai trăm năm mươi nghìn đồng");
```

Hướng dẫn chi tiết, ví dụ chạy được và bảng API nằm trong README của từng crate (viết bằng
tiếng Anh để cộng đồng quốc tế cùng dùng). Báo lỗi hoặc đề xuất tính năng tại
[Issues](https://github.com/vzeturn/DevLibrary/issues); xem
[CONTRIBUTING.md](CONTRIBUTING.md) trước khi gửi pull request.
