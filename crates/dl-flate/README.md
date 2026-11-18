# dl-flate

DEFLATE (RFC 1951) compression and decompression, with the gzip (RFC 1952) and zlib
(RFC 1950) containers. One-shot functions over byte slices, `no_std` + `alloc`, no
`unsafe`, no dependencies outside DevLibrary (CRC-32 from `dl-hash`).

## Replaces

| Ecosystem | Packages |
|-----------|----------|
| Go        | `compress/flate`, `compress/gzip`, `compress/zlib` |
| .NET      | `System.IO.Compression.GZipStream`, `DeflateStream`, `ZLibStream` |
| Node.js   | `zlib.gzipSync` / `gunzipSync`, `pako` |
| Rust      | `flate2`, `miniz_oxide` |

## Usage

```rust
use dl_flate::{Level, gzip};

let frame = br#"{"type":"result","sets":[[{"Room":"101"},{"Room":"102"}]]}"#;
let gz = gzip::compress(frame, Level::Default);
// Always pass a limit: it is the guard against decompression bombs.
let back = gzip::decompress(&gz, 8 << 20)?;
assert_eq!(back, frame);
# Ok::<(), dl_flate::Error>(())
```

`deflate_raw` and `zlib` have the same `compress` / `decompress` pair;
`deflate_raw::decompress_with_end` also reports where the stream ended.

## Behaviour

- **Decompression** handles stored, fixed and dynamic blocks. It refuses
  over-subscribed or incomplete codes (a single one-bit code is allowed for the
  literal/length and distance codes, as zlib and Go allow), symbols 286/287 and 30/31,
  distances before the start of the output, stored-length mismatches and block type 3.
  Errors carry the input offset. The output limit is checked before every byte is
  produced. gzip members are checked against their CRC-32 and length; concatenated
  members are decoded one after the other, as Go and `gunzip` do. zlib checks the header,
  the Adler-32 and refuses a preset dictionary and trailing bytes.
- **Compression** is LZ77 over a 32 KiB window with hash chains (lazy matching for
  `Default` and `Best`), written in blocks of 16 384 symbols, each as whichever of a
  stored, fixed-Huffman or dynamic-Huffman block is smallest. Huffman codes are limited
  to 15 bits (7 for the code length code). The output is valid DEFLATE but not
  byte-identical to zlib's or Go's.
- Checked against streams from Go's `compress/*` at every level, Huffman-only and
  stored (`testdata/go_streams.txt`), and the other way round: streams from this crate
  decompress with Go (`DL_FLATE_EXPORT=dir cargo test -p dl-flate`, then
  `go run testdata/gen_go_streams.go -verify dir`).
- No streaming API: the whole input and output are in memory. No preset dictionaries.
