# dl-zip

ZIP archives without dependencies: read archives from any `Read + Seek` source (stored and
deflate entries, ZIP64, UTF-8 and CP437 names, data descriptors), extract them safely, and
write archives to any `Write` stream. Built for agents that ship diagnostic bundles, upload
legacy data bundles and unpack self-update packages made by .NET or PowerShell.
Compression comes from `dl-flate`, CRC-32 from `dl-hash`; no `unsafe`.

## Replaces

| Ecosystem | Packages |
|-----------|----------|
| Go        | `archive/zip` |
| .NET      | `System.IO.Compression.ZipArchive`, `ZipFile`, PowerShell `Compress-Archive` / `Expand-Archive` |
| Rust      | `zip` |

## Write a bundle

```rust
use std::io::Cursor;
use dl_zip::{Level, Method, Modified, Options, ZipWriter};

let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
let opts = Options {
    method: Method::Deflate(Level::Default),
    modified: Modified::Unix(1_714_979_290),
};
zip.add_directory("logs")?;
zip.add_file("logs/agent.log", b"2024-05-06 07:08:10 started\n", opts)?;
zip.add_file("Hóa đơn/tháng 5.json", br#"{"total":"1250000"}"#, opts)?;
let bytes = zip.finish()?.into_inner();
# assert_eq!(&bytes[..4], b"PK\x03\x04");
# Ok::<(), dl_zip::Error>(())
```

`ZipWriter` takes any `Write` (a `File`, a `BufWriter`, a `Vec<u8>`, an HTTP body) and
never seeks. Each entry is compressed in memory, so sizes and CRC-32 go in the local
header and no data descriptors are needed. `Method::Deflate` falls back to stored when
compression does not help (already compressed data, empty files). Names use `/`
(backslashes are converted); the UTF-8 flag is set for non-ASCII names. ZIP64 records are
written automatically for 65 535 entries or more, or a size or offset of 4 GiB or more.

## Read and extract

```rust,no_run
use std::fs::File;
use dl_zip::{ExtractLimits, ZipArchive};

let mut zip = ZipArchive::new(File::open("update.zip")?)?;
for e in zip.entries() {
    println!("{} {} bytes{}", e.name(), e.size(), if e.is_dir() { " (dir)" } else { "" });
}
// Always pass a limit: entries above it are refused before decompression.
let manifest = zip.read_by_name("manifest.json", 1 << 20)?;

// Refuses the whole archive if any name escapes the directory (zip slip).
zip.extract_to("C:/ProgramData/Agent/staging", &ExtractLimits::default())?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`read` and `read_by_name` check the CRC-32 and the declared size. `extract_to` checks
every name and the limits (entry count, size per entry, total size) before writing
anything, and refuses absolute paths, drive letters, `..` components (with `/` or `\`
separators), NUL and control characters; on Windows also `:`, components ending in a dot
or space, and device names such as `CON` or `NUL`. `Entry::enclosed_name` exposes the
same check for callers that extract on their own.

## Behaviour

- **Reader**: finds the end of central directory record in the last 65 557 bytes, then
  the ZIP64 locator and record when present, and ZIP64 extra fields for sizes and
  offsets. Data in front of the archive (self-extractor stubs) is allowed. Names are UTF-8
  when flag bit 11 is set, else from a matching Info-ZIP Unicode Path field (0x7075), else
  CP437. Sizes and CRC-32 come from the central directory, so entries with data
  descriptors (flag bit 3, written by .NET to unseekable streams) read correctly.
  Directories are recognised by a trailing `/` or the directory attribute (MS-DOS or Unix
  mode). Malformed input gives `Error::Malformed` with the offset; it never panics
  (tested with every truncation and random byte changes).
- **Times**: `DosDateTime` is the two-second, zone-less header time. `Modified::Unix`
  writes the UTC date-time and an extended timestamp field (0x5455), which
  `Entry::modified_unix` reads back.
- Checked against .NET: archives written by `System.IO.Compression` (deflate, stored,
  unseekable output with data descriptors, CP437 names, 65 600 entries with ZIP64) are in
  `testdata/` and read in the tests; archives written by this crate open, read and
  extract with .NET (`DL_ZIP_EXPORT=dir cargo test -p dl-zip -- --ignored
  export_for_dotnet`, then `dotnet run testdata/gen_dotnet_zips.cs -- verify dir`).

## Limits

- No encryption (encrypted entries give `Error::Encrypted`) and only methods 0 (stored)
  and 8 (deflate); others give `Error::UnsupportedMethod`.
- No multi-disk (split) archives.
- No streaming read of unseekable input: the reader needs `Seek` to reach the central
  directory.
- Entries are compressed and decompressed in memory, one at a time; no streaming
  per-entry API.
- No archive or entry comments on write, no Unix permissions other than 0644 / 0755, no
  symbolic links (extracted as regular files).
