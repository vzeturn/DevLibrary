# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/); before 1.0, minor versions may change APIs.

## [0.2.0] - 2026-11-15

### Added

- `dl-ffi`: C ABI (`include/dl_ffi.h`, ABI version 1) so other languages call the Rust
  implementation instead of porting it: amount in words (every `i64`, linh/lẻ, nghìn/ngàn,
  bốn/tư, tỷ/tỉ, with or without "Âm", "chẵn"), diacritics removal, VietQR payload build
  (with optional folding of the message to the printable ASCII field 62 requires) and
  verification. Caller-owned buffers only; no allocation crosses the boundary. About
  130 KB native, 45 KB WebAssembly.
- `bindings/dotnet`: `DevLibrary` NuGet package for .NET 8 and 10 (`VnWords`, `VnText`,
  `VietQr`), source-generated `LibraryImport`, no dependencies, AOT compatible. Tested on
  Windows and Linux.
- Release workflow: native libraries for win-x64, win-arm64, linux-x64, linux-arm64,
  osx-x64 and osx-arm64, the WebAssembly module and the NuGet package are attached to
  every tagged release.

## [0.1.0] - 2026-11-15

First public release. All crates have no third-party dependencies and build for Linux,
macOS, Windows and `wasm32-unknown-unknown`.

### Added

- `dl-json`: strict RFC 8259 parser and serializer, ordered `Map`, raw exact numbers,
  RFC 6901 JSON Pointer, `json!` macro, `ToJson` / `FromJson`.
- `dl-decimal`: 38-digit decimal with scale 0–28, seven rounding modes, money helpers
  (`percent_of`, `split`, `allocate`, `round_to_multiple`), grouped formatting; tested
  against 8,641 Python `decimal` vectors.
- `dl-time`: `Date`, `Time`, `DateTime`, `Offset`, `OffsetDateTime`, `Duration`;
  RFC 3339, ISO 8601, HTTP dates; .NET-style patterns with English and Vietnamese
  locales.
- `dl-log`: structured logging macros, `RUST_LOG`-style filters, text / JSON / CLEF /
  logfmt formats, console, rolling file, memory and async sinks.
- `dl-uuid`: RFC 9562 UUIDs (v3, v4, v5, v7, v8), all text forms, .NET `Guid` byte layout
  and SQL Server ordering.
- `dl-jwt`: HS256 / HS384 / HS512 JWTs with strict validation and `kid` key selection.
- `dl-otp`: HOTP / TOTP, `otpauth://` URIs, replay protection, recovery codes.
- `dl-qr`: QR Code Model 2 encoder (versions 1–40), SVG and text output; bit-exact with
  segno on 85 vectors and decoded by zxing-cpp.
- `dl-cli`: argument parser with subcommands, environment fallbacks, suggestions and
  generated help.
- `dl-vn`: Vietnamese amount in words, diacritics, collation, tax codes, phone numbers,
  CCCD and VietQR.
- `dl-hash`, `dl-encoding`, `dl-rand`, `dl-bench`: foundation crates.

[0.2.0]: https://github.com/vzeturn/DevLibrary/releases/tag/v0.2.0
[0.1.0]: https://github.com/vzeturn/DevLibrary/releases/tag/v0.1.0
