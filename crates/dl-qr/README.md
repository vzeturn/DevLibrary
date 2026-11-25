# dl-qr

QR Code Model 2 encoder (ISO/IEC 18004) for Rust, with SVG and terminal output. All 40
versions, the four error correction levels, numeric / alphanumeric / byte modes with optimal
mixed-mode segmentation, and automatic mask selection. `#![no_std]` + `alloc`, no
dependencies, no `unsafe`, builds for `wasm32-unknown-unknown`.

Part of [DevLibrary](https://github.com/vzeturn/devlibrary).

## Replaces

| Ecosystem | Package | Notes |
|-----------|---------|-------|
| C# / .NET | [QRCoder](https://github.com/codebude/QRCoder) | `QRCodeGenerator` + `SvgQRCode` / `AsciiQRCode` |
| TypeScript / JS | [`qrcode`](https://www.npmjs.com/package/qrcode), [`qrcode.react`](https://www.npmjs.com/package/qrcode.react) | `toString(..., { type: "svg" })`, `<QRCodeSVG>` (via WASM, or by serving the SVG) |
| Python | [`qrcode`](https://pypi.org/project/qrcode/), [`segno`](https://pypi.org/project/segno/) | Generation only; no PNG yet (see [Limitations](#limitations--roadmap)) |

## Features

- Versions 1 to 40, error correction levels L, M, Q, H.
- Numeric, alphanumeric and byte (UTF-8) modes. `encode_text` picks the mix of segments
  with the shortest bit stream (dynamic programming, as in Nayuki's generator); the result
  is never longer than any single-mode encoding.
- Optional ECI designator for UTF-8 (assignment 26), off by default.
- Reed-Solomon over GF(256) (polynomial `0x11D`), block tables for every version and level,
  codeword interleaving, function patterns, zig-zag placement.
- All 8 mask patterns scored with the 4 penalty rules; ties go to the lowest mask. The mask,
  version range and ECC level can be forced, and ECC can be boosted to the highest level
  that still fits the chosen version.
- SVG output as a single `<path>` of merged horizontal runs with `shape-rendering="crispEdges"`,
  terminal output with Unicode half blocks, raw module matrix.
- Never panics on input: oversized data returns `Error::DataTooLong` after an O(n),
  allocation-free length check.
- Output is bit-exact with [segno](https://github.com/heuer/segno) and decodes with
  [zxing-cpp](https://github.com/zxing-cpp/zxing-cpp) (see [Verification](#how-correctness-is-verified)).

## Installation

```toml
[dependencies]
dl-qr = { git = "https://github.com/vzeturn/devlibrary" }
```

From a local checkout of the repository:

```toml
[dependencies]
dl-qr = { path = "../DevLibrary/crates/dl-qr" }
```

Inside the DevLibrary workspace, use `dl-qr.workspace = true`.

## Quick start

```rust
use dl_qr::{Ecc, QrCode, SvgOptions};

fn main() -> Result<(), dl_qr::Error> {
    let qr = QrCode::encode_text("https://example.com", Ecc::Medium)?;
    assert_eq!((qr.version(), qr.size()), (2, 25));

    let svg = qr.to_svg(&SvgOptions::default()); // 4-module quiet zone, black on white
    assert!(svg.starts_with("<svg") && svg.contains("<path"));

    // Every option at once.
    let qr = QrCode::builder()
        .ecc(Ecc::Quartile)
        .min_version(2)
        .max_version(10)
        .mask(Some(3))
        .boost_ecc(true)
        .encode_text("CHECKIN-000123")?;
    assert_eq!((qr.version(), qr.mask()), (2, 3));
    assert_eq!(qr.ecc(), Ecc::High); // boosted: the data still fits version 2 at level H

    // Raw bytes, one byte-mode segment.
    let qr = QrCode::encode_bytes(&[0x00, 0xFF, 0x10], Ecc::Low)?;
    assert!(qr.get(0, 0)); // top-left finder pattern is dark
    assert_eq!(qr.to_matrix().len(), qr.size());
    Ok(())
}
```

## Guide

### SVG for the web

`to_svg` returns a standalone SVG document. Without `module_size` it has only a `viewBox`
(coordinates in modules), so it scales to whatever box CSS gives it. Set `module_size` to
get fixed `width` / `height` attributes in pixels.

```rust
use dl_qr::{Ecc, QrCode, SvgOptions};

fn main() -> Result<(), dl_qr::Error> {
    let qr = QrCode::encode_text("https://example.com/checkin?event=42", Ecc::Medium)?;
    let svg = qr.to_svg(&SvgOptions {
        border: 4,                // quiet zone in modules; the standard asks for 4
        dark: "currentColor",     // follows the CSS `color` of the parent element
        light: "",                // empty = no background rectangle (transparent)
        module_size: Some(4),     // 4 px per module: width = height = 4 * (size + 8)
    });
    assert_eq!(qr.version(), 3); // 36 bytes at level M
    assert!(svg.contains("width=\"148\" height=\"148\""));
    let html = format!(r#"<div class="qr" style="color:#0b3d91">{svg}</div>"#);
    assert!(html.contains("fill=\"currentColor\""));
    Ok(())
}
```

Colours are XML-escaped, so they are safe to take from configuration. If the background is
transparent, make sure the page behind the code is light: scanners need dark modules on a
light background and a light quiet zone.

### Inline in an invoice

For HTML invoices and e-mails rendered to PDF, embed the SVG directly and size it with CSS
in physical units. Keep at least about 0.4 mm per module for laser print, more for thermal
receipt printers.

```rust
use dl_qr::{Ecc, QrCode, SvgOptions};

fn invoice_html(number: &str, payment_payload: &str) -> Result<String, dl_qr::Error> {
    let qr = QrCode::encode_text(payment_payload, Ecc::Medium)?;
    let svg = qr.to_svg(&SvgOptions::default());
    Ok(format!(
        "<section class=\"pay\">\
           <h2>Invoice {number}</h2>\
           <div style=\"width:35mm;height:35mm\">{svg}</div>\
           <p>Scan with your banking app</p>\
         </section>"
    ))
}

fn main() -> Result<(), dl_qr::Error> {
    let html = invoice_html("HD2024-000123", "00020101021238570010A000000727")?;
    assert!(html.contains("<svg"));
    Ok(())
}
```

`number` is interpolated as-is in this sketch; escape it if it comes from user input.

### Terminal output

`to_text(border)` packs two module rows into each line with the half blocks `█ ▀ ▄`.

```rust
use dl_qr::{Ecc, QrCode};

fn main() -> Result<(), dl_qr::Error> {
    let qr = QrCode::encode_text("otpauth://totp/DevLibrary?secret=JBSWY3DPEHPK3PXP", Ecc::Low)?;
    let text = qr.to_text(2);
    // print!("{text}");
    assert_eq!(text.lines().count(), (qr.size() + 4).div_ceil(2));
    Ok(())
}
```

The blocks are the dark modules, which assumes a light terminal background. On a dark theme
the code appears inverted; many phone scanners still read it, some do not.

### Choosing the ECC level

| Level | Recovers about | Use it for |
|-------|----------------|------------|
| `Ecc::Low` | 7 % of codewords | Screens, clean digital display, maximum capacity |
| `Ecc::Medium` | 15 % | General default: URLs, tickets, payment codes (VietQR, EMVCo) |
| `Ecc::Quartile` | 25 % | Printed labels, receipts, codes that get scratched or creased |
| `Ecc::High` | 30 % | Harsh environments, or a small logo drawn over the centre |

Higher levels make the symbol larger for the same data. With `boost_ecc(true)` the encoder
first picks the smallest version for the requested level, then raises the level as far as
the data still fits that version, so the extra protection costs no size.

### Version and capacity

The version (1..=40) sets the size: `4 * version + 17` modules per side. By default the
smallest version that fits is used. Maximum characters per version and level
(numeric / alphanumeric / byte):

| Version | Size | L | M | Q | H |
|--------:|-----:|---|---|---|---|
| 1 | 21 | 41 / 25 / 17 | 34 / 20 / 14 | 27 / 16 / 11 | 17 / 10 / 7 |
| 2 | 25 | 77 / 47 / 32 | 63 / 38 / 26 | 48 / 29 / 20 | 34 / 20 / 14 |
| 5 | 37 | 255 / 154 / 106 | 202 / 122 / 84 | 144 / 87 / 60 | 106 / 64 / 44 |
| 7 | 45 | 370 / 224 / 154 | 293 / 178 / 122 | 207 / 125 / 86 | 154 / 93 / 64 |
| 10 | 57 | 652 / 395 / 271 | 513 / 311 / 213 | 364 / 221 / 151 | 288 / 174 / 119 |
| 20 | 97 | 2061 / 1249 / 858 | 1600 / 970 / 666 | 1159 / 702 / 482 | 919 / 557 / 382 |
| 40 | 177 | 7089 / 4296 / 2953 | 5596 / 3391 / 2331 | 3993 / 2420 / 1663 | 3057 / 1852 / 1273 |

Byte capacity counts UTF-8 bytes: "Thanh toán hóa đơn 123" is 22 characters but 26 bytes.
Mixed content (for example a URL with a long digit run) usually fits more than the byte
column, because `encode_text` switches to numeric or alphanumeric mode where that is shorter.

Pin the size to keep a layout stable, and handle the error when data does not fit:

```rust
use dl_qr::{Ecc, Error, QrCode};

fn main() {
    let fixed = QrCode::builder().version(5).encode_text("short").unwrap();
    assert_eq!(fixed.size(), 37);

    let err = QrCode::builder()
        .ecc(Ecc::High)
        .max_version(1)
        .encode_text("this text is too long for version 1-H")
        .unwrap_err();
    assert!(matches!(err, Error::DataTooLong { bits, capacity } if bits > capacity));
    assert_eq!(
        QrCode::builder().min_version(0).encode_text("x"),
        Err(Error::InvalidVersion { min: 0, max: 40 })
    );
}
```

### Forcing the mask

Automatic selection (the default) tries all 8 masks and keeps the one with the lowest
penalty. Forcing a mask is useful for reproducing another encoder's output exactly, or to
skip the evaluation when encoding speed matters more than the last bit of scan robustness.

```rust
use dl_qr::{Ecc, Error, QrCode};

fn main() -> Result<(), Error> {
    let auto = QrCode::encode_text("mask", Ecc::Medium)?;
    let forced = QrCode::builder().mask(Some(auto.mask())).encode_text("mask")?;
    assert_eq!(auto, forced);
    assert_eq!(
        QrCode::builder().mask(Some(8)).encode_text("x"),
        Err(Error::InvalidMask(8))
    );
    Ok(())
}
```

### Forcing modes and ECI

`QrBuilder::encode_segments` encodes the given segments in order, without re-segmenting:

```rust
use dl_qr::{Ecc, QrCode, Segment};

fn main() -> Result<(), dl_qr::Error> {
    let segments = [
        Segment::eci(26)?, // following bytes are UTF-8
        Segment::bytes("Mã đơn: ".as_bytes()),
        Segment::numeric("20240512000001")?,
    ];
    let qr = QrCode::builder().ecc(Ecc::Quartile).encode_segments(&segments)?;
    assert_eq!(qr.version(), 2); // 12 + 100 + 61 = 173 of 176 bits
    // ECI prefix with automatic segmentation: 12 more bits than without.
    let text = "Thanh toán hóa đơn 123"; // 26 UTF-8 bytes
    assert_eq!(QrCode::encode_text(text, Ecc::Medium)?.version(), 2); // 220 of 224 bits
    let qr = QrCode::builder().eci_utf8(true).encode_text(text)?;
    assert_eq!(qr.version(), 3);
    Ok(())
}
```

Most phone readers already assume UTF-8 for byte mode, and some older readers mishandle
ECI, which is why it is off by default.

### Serving SVG from a web handler

The encoder is framework-agnostic: map the result to a status, a content type and a body.
Bound the input length before encoding; anything over 7089 characters can never fit.

```rust
use dl_qr::{Ecc, Error, QrCode, SvgOptions};

/// `GET /qr?data=...` -> (status, content type, body), for any HTTP framework.
fn qr_endpoint(data: &str) -> (u16, &'static str, String) {
    if data.len() > 2048 {
        return (413, "text/plain; charset=utf-8", "data too long".into());
    }
    match QrCode::encode_text(data, Ecc::Medium) {
        Ok(qr) => (200, "image/svg+xml", qr.to_svg(&SvgOptions::default())),
        Err(e @ Error::DataTooLong { .. }) => (413, "text/plain; charset=utf-8", e.to_string()),
        Err(e) => (400, "text/plain; charset=utf-8", e.to_string()),
    }
}

fn main() {
    let (status, content_type, body) = qr_endpoint("https://example.com/t/8f3a");
    assert_eq!((status, content_type), (200, "image/svg+xml"));
    assert!(body.ends_with("</svg>\n"));
    assert_eq!(qr_endpoint(&"9".repeat(3000)).0, 413);
}
```

The output depends only on the input and options, so responses can be cached with a long
`Cache-Control: max-age` keyed on the data.

### Using with WASM in a browser

dl-qr has no platform code and builds for `wasm32-unknown-unknown` as is:

```sh
cargo build -p dl-qr --target wasm32-unknown-unknown --release
```

To call it from JavaScript, export a function from your application crate. With
[wasm-bindgen](https://rustwasm.github.io/wasm-bindgen/) in that crate (dl-qr itself does not
depend on it):

```rust,ignore
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn qr_svg(text: &str, ecc: u8) -> Result<String, JsError> {
    let ecc = match ecc {
        0 => dl_qr::Ecc::Low,
        2 => dl_qr::Ecc::Quartile,
        3 => dl_qr::Ecc::High,
        _ => dl_qr::Ecc::Medium,
    };
    let qr = dl_qr::QrCode::encode_text(text, ecc).map_err(|e| JsError::new(&e.to_string()))?;
    Ok(qr.to_svg(&dl_qr::SvgOptions::default()))
}
```

```js
import init, { qr_svg } from "./pkg/app.js";

await init();
document.querySelector("#qr").innerHTML = qr_svg("https://example.com", 1);
```

This replaces `qrcode.react`'s `<QRCodeSVG value=... />`: render the returned string into a
container element. The workspace's `min-size` profile (`--profile min-size`) optimises the
`.wasm` for size.

## API overview

| Item | Purpose |
|------|---------|
| `QrCode::encode_text(&str, Ecc)` | Smallest version, optimal segmentation, automatic mask |
| `QrCode::encode_bytes(&[u8], Ecc)` | One byte-mode segment, smallest version, automatic mask |
| `QrCode::builder() -> QrBuilder` | Options: `ecc`, `min_version`, `max_version`, `version`, `mask`, `boost_ecc`, `eci_utf8` |
| `QrBuilder::encode_text` / `encode_bytes` / `encode_segments` | Encode with those options |
| `QrCode::version`, `size`, `ecc`, `mask` | Symbol parameters actually used |
| `QrCode::get(x, y)` | Module at column `x`, row `y` (`true` = dark, `false` outside) |
| `QrCode::to_matrix()` | `Vec<Vec<bool>>`, rows top to bottom, no quiet zone |
| `QrCode::to_svg(&SvgOptions)` | SVG document; `SvgOptions { border, dark, light, module_size }` |
| `QrCode::to_text(border)` | Unicode half-block text, 2 module rows per line |
| `Segment::numeric` / `alphanumeric` / `bytes` / `eci` | Hand-built segments; `Mode` |
| `Ecc` | `Low`, `Medium`, `Quartile`, `High` (ordered) |
| `Error` | `DataTooLong { bits, capacity }`, `InvalidVersion { min, max }`, `InvalidMask(u8)`, `InvalidCharacter { index, mode }`, `InvalidEci(u32)` |

`Error` implements `Display` and `core::error::Error`. `QrCode` implements `Clone`, `Eq` and
`Hash`; its `Debug` output shows the parameters without the modules.

## How correctness is verified

- **Bit-exact with segno.** `tests/vectors.txt` holds 85 symbols produced by segno 1.6.6 with
  a forced version, level, segment modes and mask: every ECC level; numeric, alphanumeric,
  byte and mixed-mode streams; ECI; Vietnamese UTF-8 ("Thanh toán hóa đơn 123"); VietQR
  payloads; one case per version 1..=40; and symbols filled exactly to capacity at versions
  1, 2, 7, 10, 20 and 40. `tests/segno.rs` rebuilds each one and compares every module. 83
  of the 85 (all but the ECI cases, which it does not support) are also built by
  python-qrcode 8.2 and must be identical to segno's.
- **Mask selection.** For every vector the generator scores the 8 finished symbols with
  segno's penalty function; dl-qr's automatic choice must equal the lowest-scoring mask in
  all 85 cases. Each of the four penalty rules is also compared separately with segno's
  `mask_scores`.
- **Decoding.** The `qr_dump` example writes 24 symbols (VietQR payload of 144 characters
  at M and boosted ECC, version-40 symbols at full capacity in all three modes, Vietnamese
  UTF-8 with and without ECI, mixed segments, all 8 masks, versions 7 to 33, empty input)
  and `tests/decode_check.py` decodes them with zxing-cpp and compares payload and ECC
  level: 24 of 24 decode. All but the empty symbol are found by zxing's camera detector;
  the empty one (a data area of nothing but pad codewords) only by its pure-barcode
  detector, where it decodes with no error.
- **Unit tests** cover GF(256) arithmetic, RS generator polynomials, the "HELLO WORLD" 1-Q
  codewords, format and version information BCH codes (ISO/IEC 18004 Annex C and D),
  block tables (compared with segno for all 160 version/level pairs), alignment pattern
  positions, capacity limits, the segmentation DP against brute force, the single-pass
  penalty scan against a direct transcription of rules 1 and 3 on 20 000 random lines, and
  the SVG / text renderers (rasterised back and compared module by module).

Two documented differences from segno, both in segno:

1. segno 1.6.6 appends a whole `0x00` codeword when the bit stream already ends on a byte
   boundary after the terminator; ISO/IEC 18004 7.4.10 adds padding bits only up to the
   boundary. The generator patches that step (see `_iso_padding_bits`); python-qrcode, ZXing
   and Nayuki's library agree with the patched behaviour.
2. segno scores masks before drawing format and version information, so its own automatic
   choice differs from the finished-symbol score in 18 of the 85 cases. dl-qr, like ZXing,
   QRCoder and Nayuki's library, scores the finished symbol. segno's rule-3 search also
   skips a finder-like pattern that overlaps a counted one; dl-qr counts every occurrence.
   This changed the selected mask in none of the vectors.

Rerun everything (the Python packages are for verification only, never a dependency):

```sh
python -m pip install --user segno==1.6.6 qrcode==8.2 zxing-cpp numpy
python crates/dl-qr/tests/gen_vectors.py              # regenerate vectors.txt / tables.txt
cargo test -p dl-qr
cargo run -p dl-qr --example qr_dump -- target/qr-decode
python crates/dl-qr/tests/decode_check.py target/qr-decode
```

## Performance

`cargo bench -p dl-qr`: median time per operation, Intel Core i7-9850H (2.6 GHz),
Windows, rustc 1.100.

| Case | Symbol | Time |
|------|--------|-----:|
| `encode_text`, 100-character URL, level M | version 6, 41 x 41 | 230 µs |
| `encode_text`, same URL, mask forced | version 6 | 10 µs |
| `encode_bytes`, 1000 bytes, level L | version 22, 105 x 105 | 1.4 ms |
| `to_svg`, URL symbol | 41 x 41 | 46 µs |
| `to_svg`, 1000-byte symbol | 105 x 105 | 380 µs |
| `to_text`, URL symbol | 41 x 41 | 6 µs |

The figures are the best of several runs on a shared machine; busy runs were up to 3x
slower. About 95 % of the encoding time is mask selection (8 full penalty evaluations), so
forcing a mask with `QrBuilder::mask` makes encoding about 20x faster.

## Standards

- ISO/IEC 18004:2015, *QR Code bar code symbology specification* (Model 2 symbols).
- AIM ECI specification for the ECI designator (assignment 26 = UTF-8).

## Limitations / roadmap

- No raster output yet. PNG needs a deflate encoder and will come with a DevLibrary
  compression crate; until then use the SVG, or `to_matrix` with your own image writer.
- No Kanji mode: Shift JIS text is encoded in byte mode.
- No Micro QR, rMQR, or structured append.
- Encoding only, no decoder.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0) at your option.
