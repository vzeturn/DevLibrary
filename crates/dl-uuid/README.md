# dl-uuid

UUIDs as defined by [RFC 9562](https://www.rfc-editor.org/rfc/rfc9562): generate random
(v4), time-ordered (v7) and name-based (v3, v5) IDs, parse and format every common text
form, and convert to and from the byte layout of .NET `Guid` and SQL Server
`uniqueidentifier`. No dependencies outside DevLibrary, no `unsafe`, and parsing,
formatting and name-based IDs work in `no_std` and WebAssembly.

```rust
use dl_uuid::Uuid;

let id = Uuid::now_v7(); // e.g. 019a6c0e-8f3b-7d41-a2c5-3f1e9b7d4a60
let again: Uuid = id.to_string().parse().unwrap();
assert_eq!(again, id);
```

## Replaces

| Ecosystem | Package | Notes |
|---|---|---|
| C# / .NET | `System.Guid` helpers: `Guid.NewGuid()`, `Guid.CreateVersion7()`, `Guid.Parse`, `ToString("D"/"N"/"B")`, `ToByteArray()`, `new Guid(byte[])`, `SqlGuid.CompareTo` | Same text forms; `to_bytes_le` / `from_bytes_le` give the `ToByteArray()` layout; `cmp_sqlserver` is the `SqlGuid` order |
| TypeScript / JavaScript | npm `uuid` (`v4`, `v7`, `v5`, `v3`, `parse`, `stringify`, `validate`, `version`) | Build for `wasm32-unknown-unknown`; see [Browser and WebAssembly](#browser-and-webassembly) |
| Go | `github.com/google/uuid` (`New`, `NewV7`, `NewSHA1`, `NewMD5`, `Parse`) | Same v3/v5 results; errors carry the byte offset |
| Python | `uuid` (`uuid4`, `uuid5`, `uuid3`, `UUID(...)`, `bytes_le`, `.time`) | Python generated the v3/v5 and `bytes_le` test vectors |
| Rust | the `uuid` crate | Similar names (`new_v4`, `now_v7`, `new_v5`, `hyphenated()`, `as_u128`) |

## Features

- `Uuid`: 16 bytes, `Copy`, `Eq`, `Ord` (byte order, so v7 IDs sort by time), `Hash`,
  `Default` (Nil). Constants `NIL`, `MAX`, `NAMESPACE_DNS`, `NAMESPACE_URL`,
  `NAMESPACE_OID`, `NAMESPACE_X500`.
- Generation: v4 from the OS CSPRNG; v7 with a monotonic counter (RFC 9562 §6.2
  Method 1) that keeps IDs strictly increasing within a process, also for millions
  of IDs per millisecond and when the clock goes backwards; v3 (MD5) and v5 (SHA-1)
  from a namespace and a name; v4, v7 and v8 from bytes you supply (`no_std`, WASM).
- Parsing: hyphenated, simple (32 hex digits), braced `{...}` and `urn:uuid:...`, any
  case; `const fn`; errors give the byte offset of the first bad character.
- Formatting: `Display` (lowercase hyphenated), `hyphenated()`, `simple()`,
  `braced()`, `urn()`, each with uppercase output (`{:X}`, `to_string_upper`) and
  allocation-free `encode_lower` / `encode_upper`; `encode_into(&mut [u8; 36])`.
- Inspection: `version()`, `variant()`, `is_nil()`, `is_max()`, `v7_timestamp_ms()`,
  and for v1/v6 `gregorian_timestamp()` and `unix_timestamp_ms()`.
- .NET and SQL Server: `from_bytes_le` / `to_bytes_le` (mixed-endian `Guid` layout),
  `from_fields` / `to_fields` (`Data1`..`Data4`), and `cmp_sqlserver` (the order SQL
  Server uses for `uniqueidentifier`).
- No panics on untrusted input, `unsafe` denied, every public item documented.

## Installation

From GitHub:

```toml
[dependencies]
dl-uuid = { git = "https://github.com/vzeturn/devlibrary" }
```

From a local checkout (or inside the DevLibrary workspace):

```toml
[dependencies]
dl-uuid = { path = "../DevLibrary/crates/dl-uuid" }
```

Without the standard library (parsing, formatting, v3/v5, and v4/v7/v8 from your own
random bytes and clock; needs `alloc`):

```toml
[dependencies]
dl-uuid = { git = "https://github.com/vzeturn/devlibrary", default-features = false }
```

| Feature | Default | Enables |
|---|---|---|
| `std` | yes | `new_v4`, `try_new_v4`, `now_v7`, `try_now_v7`, `V7Generator::{generate, try_generate, try_generate_at}`; pulls in `dl-rand` |

Minimum Rust version: 1.88 (edition 2024).

## Quick start

```rust
use dl_uuid::{Uuid, Version};

fn main() -> Result<(), dl_uuid::Error> {
    // Random (v4) and time-ordered (v7) IDs.
    let session = Uuid::new_v4();
    let order_id = Uuid::now_v7();
    assert_eq!(session.version(), Some(Version::V4));
    assert_eq!(order_id.version(), Some(Version::V7));
    assert!(order_id < Uuid::now_v7()); // v7 IDs increase

    // Deterministic (v5) IDs: the same namespace and name always give the same UUID.
    let host = Uuid::new_v5(&Uuid::NAMESPACE_DNS, "www.example.com");
    assert_eq!(host.to_string(), "2ed6657d-e927-568b-95e1-2665a8aea6a2");

    // Parse any common form, format in any form.
    let parsed: Uuid = "{2ED6657D-E927-568B-95E1-2665A8AEA6A2}".parse()?;
    assert_eq!(parsed, host);
    assert_eq!(parsed.simple().to_string(), "2ed6657de927568b95e12665a8aea6a2");
    assert_eq!(parsed.urn().to_string(), "urn:uuid:2ed6657d-e927-568b-95e1-2665a8aea6a2");
    assert_eq!(parsed.to_string_upper(), "2ED6657D-E927-568B-95E1-2665A8AEA6A2");

    // Bytes, integers and the .NET Guid byte layout.
    assert_eq!(Uuid::from_bytes(*host.as_bytes()), host);
    assert_eq!(Uuid::from_u128(host.as_u128()), host);
    assert_eq!(Uuid::from_bytes_le(host.to_bytes_le()), host);
    Ok(())
}
```

## Guide

### Choosing a version

| Need | Use | Why |
|---|---|---|
| Database primary keys, log/event IDs, anything sorted or indexed | **v7** `Uuid::now_v7()` | Starts with a millisecond timestamp, so new IDs sort after old ones and B-tree inserts stay at the end of the index. 74 bits are random or counter. Reveals the creation time. |
| Session IDs, API keys' public part, IDs that must not leak anything | **v4** `Uuid::new_v4()` | 122 random bits from the OS CSPRNG, no time information. Random index positions. |
| The same ID for the same input every time (imports, deduplication, URLs, cross-system keys) | **v5** `Uuid::new_v5(&namespace, name)` | SHA-1 of a namespace and a name; anyone with both can recompute it, so it is not secret. |
| Compatibility with a system that already uses MD5 name-based IDs | v3 `Uuid::new_v3` | Same as v5 with MD5. Prefer v5 for new designs (RFC 9562 §5.3). |
| Your own layout | v8 `Uuid::v8_from(bytes)` | 122 application-defined bits (RFC 9562 §5.8). |
| Reading IDs made by others | `parse`, `version()`, `v7_timestamp_ms()`, `unix_timestamp_ms()` (v1, v6, v7) | This crate does not generate v1, v2 or v6. |

### v7 as a database primary key

A v7 UUID is `unix_ts_ms (48 bits) | version | counter | random`. Comparing the bytes
left to right compares the timestamp first, so IDs created later sort later. A B-tree
index on such a key receives inserts at its right edge, as with an auto-increment
column, instead of at random pages, which keeps pages full and the working set small.

```rust
use dl_uuid::Uuid;

let ids: Vec<Uuid> = (0..10_000).map(|_| Uuid::now_v7()).collect();
assert!(ids.windows(2).all(|w| w[0] < w[1])); // strictly increasing, same process

let created = ids[0].v7_timestamp_ms().unwrap(); // Unix milliseconds
assert!(created > 1_700_000_000_000);
```

This works where the database compares the 16 bytes in order: PostgreSQL `uuid`,
`BINARY(16)` in MySQL and MariaDB, `BLOB` in SQLite, or a `CHAR(36)` column holding the
lowercase text (the text sorts in the same order). Store `as_bytes()` (not
`to_bytes_le()`) in binary columns.

`now_v7()` guarantees order within one process. IDs from different processes or
machines in the same millisecond interleave in random order, which still keeps index
inserts close to the end. **SQL Server `uniqueidentifier` is the exception; see the next
section.**

### SQL Server and C# `Guid` interop

**Text** is the same everywhere. `Display` prints what C# `Guid.ToString()` prints, and
the parser accepts the C# `"D"`, `"N"` and `"B"` formats in either case:

```rust
use dl_uuid::Uuid;

let id: Uuid = "35918BC9-196D-40EA-9779-889D79B753F0".parse().unwrap(); // SQL Server prints uppercase
assert_eq!(id.to_string(), "35918bc9-196d-40ea-9779-889d79b753f0"); // Guid.ToString() / "D"
assert_eq!(id.simple().to_string(), "35918bc9196d40ea9779889d79b753f0"); // ToString("N")
assert_eq!(id.braced().to_string(), "{35918bc9-196d-40ea-9779-889d79b753f0}"); // ToString("B")
```

**Bytes** differ. .NET stores a `Guid` as the Windows `GUID` struct, whose first three
fields are little-endian, and SQL Server stores `uniqueidentifier` the same way.
`Guid.ToByteArray()`, `new Guid(byte[])`, `CAST(id AS binary(16))` and the raw 16 bytes a
SQL Server driver sends or receives all use this mixed-endian layout. Use
`to_bytes_le()` / `from_bytes_le()` for them, and `as_bytes()` / `from_bytes()`
everywhere else (PostgreSQL, Go, Python `UUID.bytes`, npm `uuid.parse`):

```rust
use dl_uuid::Uuid;

// From the .NET documentation of Guid.ToByteArray():
//   Guid: 35918bc9-196d-40ea-9779-889d79b753f0
//   C9 8B 91 35 6D 19 EA 40 97 79 88 9D 79 B7 53 F0
let csharp_bytes = [
    0xC9, 0x8B, 0x91, 0x35, 0x6D, 0x19, 0xEA, 0x40,
    0x97, 0x79, 0x88, 0x9D, 0x79, 0xB7, 0x53, 0xF0,
];
let id = Uuid::from_bytes_le(csharp_bytes); // C#: new Guid(bytes)
assert_eq!(id.to_string(), "35918bc9-196d-40ea-9779-889d79b753f0");
assert_eq!(id.to_bytes_le(), csharp_bytes); // C#: guid.ToByteArray()

// The standard (RFC) byte order is the text order:
assert_eq!(id.as_bytes()[..4], [0x35, 0x91, 0x8B, 0xC9]);

// C#: new Guid(0x35918bc9, 0x196d, 0x40ea, new byte[] { 0x97, 0x79, 0x88, 0x9d, 0x79, 0xb7, 0x53, 0xf0 })
let same = Uuid::from_fields(0x35918bc9, 0x196d, 0x40ea, [0x97, 0x79, 0x88, 0x9d, 0x79, 0xb7, 0x53, 0xf0]);
assert_eq!(same, id);
```

**Ordering** differs too. SQL Server sorts `uniqueidentifier` by the last six bytes
first (the last group of the text form), then the fourth group, then the third, second
and first groups, each of the first three compared in stored (little-endian) byte
order. Microsoft documents that the last six bytes are the most significant
([Comparing GUID and uniqueidentifier values](https://learn.microsoft.com/en-us/sql/connect/ado-net/sql/compare-guid-uniqueidentifier-values));
the complete order is the one .NET `SqlGuid.CompareTo` implements and was measured on
SQL Server 2022 by
[dbi services](https://www.dbi-services.com/blog/why-uuidv7-does-not-reduce-fragmentation-in-sql-server/).
`cmp_sqlserver` reproduces it:

```rust
use core::cmp::Ordering;
use dl_uuid::{Uuid, V7Generator};

// Two v7 IDs one millisecond apart.
let mut generator = V7Generator::new();
let first = generator.generate_from(1_700_000_000_000, [0xFF; 10]);
let second = generator.generate_from(1_700_000_000_001, [0x00; 10]);
assert_eq!(first.cmp(&second), Ordering::Less); // byte order: time order
assert_eq!(first.cmp_sqlserver(&second), Ordering::Greater); // SQL Server: random order

// Sort the way `ORDER BY id` returns rows from SQL Server:
let mut rows = vec![first, second];
rows.sort_by(Uuid::cmp_sqlserver);
assert_eq!(rows, [second, first]);
```

So with SQL Server, a v7 UUID in a `uniqueidentifier` column gives **no** index
locality: the bytes SQL Server compares first are random. This also applies to .NET 9's
`Guid.CreateVersion7()`. Options:

- Keep `uniqueidentifier` as a `PRIMARY KEY NONCLUSTERED` and cluster the table on an
  `IDENTITY` (or other increasing) column. The UUID remains the public ID.
- Store the ID as `binary(16)` from `as_bytes()`. SQL Server compares `binary`
  byte by byte, so v7 order is preserved; convert with `from_bytes()` when reading.
- Use v4 if you keep a clustered `uniqueidentifier` key anyway: v7 gains nothing there
  and v4 reveals no timestamp.

### Parsing user input

`parse` is strict: one of the four forms, no whitespace. Trim input yourself, and use
the error offset to point at the problem:

```rust
use dl_uuid::{Error, Uuid};

fn read_id(input: &str) -> Result<Uuid, String> {
    Uuid::parse(input.trim()).map_err(|e| match e {
        Error::InvalidHexDigit { offset } | Error::ExpectedChar { offset, .. } => {
            format!("{e}\n  {}\n  {}^", input.trim(), " ".repeat(offset))
        }
        _ => e.to_string(),
    })
}

assert!(read_id("  67e55044-10b1-426f-9247-bb680e5fe0c8\n").is_ok());
assert!(read_id("urn:uuid:67E55044-10B1-426F-9247-BB680E5FE0C8").is_ok());
let err = read_id("67e55044-10b1-426f-9247-bb680e5fe0cx").unwrap_err();
assert!(err.starts_with("invalid UUID: expected a hex digit at offset 35"));
assert_eq!(
    read_id("67e55044").unwrap_err(),
    "invalid UUID length 8: expected 32, 36, 38 or 45 characters",
);
// Use parse_bytes for input that may not be UTF-8.
assert!(Uuid::parse_bytes(b"67e5504410b1426f9247bb680e5fe0c8").is_ok());
```

| Input | Result |
|---|---|
| `67e55044-10b1-426f-9247-bb680e5fe0c8` | ok (hyphenated) |
| `67E5504410B1426F9247BB680E5FE0C8` | ok (simple, any case) |
| `{67e55044-10b1-426f-9247-bb680e5fe0c8}` | ok (braced) |
| `urn:uuid:67e55044-10b1-426f-9247-bb680e5fe0c8`, `URN:UUID:...` | ok (URN, prefix case-insensitive per RFC 8141) |
| ` 67e55044-...` (leading space) | `InvalidLength { len: 37 }` |
| `67e55044_10b1-...` | `ExpectedChar { offset: 8, expected: '-' }` |
| `(67e55044-10b1-...)` (C# `"P"` format) | `ExpectedChar { offset: 0, expected: '{' }` |
| `{67e5504410b1426f9247bb680e5fe0c8}` | `InvalidLength { len: 34 }` |

### Deterministic IDs with v5

A v5 UUID is a hash of a namespace UUID and a name, so every system computes the same
ID for the same input without coordination: the same product imported twice, or a
record created in C#, Python and Rust from the same natural key. Python
`uuid.uuid5`, Go `uuid.NewSHA1` and npm `uuid.v5` give identical results; names are
hashed as UTF-8 bytes.

```rust
use dl_uuid::Uuid;

// Standard namespaces from RFC 9562 §6.6.
let page = Uuid::new_v5(&Uuid::NAMESPACE_URL, "https://shop.example/products/42");
assert_eq!(page, Uuid::new_v5(&Uuid::NAMESPACE_URL, "https://shop.example/products/42"));

// Your own namespace: generate one v4 UUID once and keep it as a constant.
const CUSTOMERS: Uuid = match Uuid::parse("9a4f1c7e-3b2d-4e8f-a1b0-5c6d7e8f9012") {
    Ok(id) => id,
    Err(_) => panic!("invalid namespace literal"),
};
let customer = Uuid::new_v5(&CUSTOMERS, "tax-code:0312345678");
assert_eq!(customer.version(), Some(dl_uuid::Version::V5));
assert_ne!(customer, Uuid::new_v5(&CUSTOMERS, "tax-code:0312345679"));
```

v5 IDs are not secret: anyone who knows the namespace and can guess the name can
recompute the ID. Normalize names first (case, whitespace, Unicode form) if the same
thing can be written in several ways.

### Browser and WebAssembly

The crate builds for `wasm32-unknown-unknown`. Two things differ there:

- **Random numbers** come from the `dl_rand.fill` host import, so `new_v4()` and
  `V7Generator::try_generate_at` work once JavaScript provides it:

  ```js
  const imports = { dl_rand: { fill: (ptr, len) =>
    crypto.getRandomValues(new Uint8Array(instance.exports.memory.buffer, ptr, len)) } };
  ```

- **The clock**: `std::time::SystemTime::now()` panics on this target, so `now_v7()`
  and `V7Generator::{generate, try_generate}` are not compiled there. Pass the time
  from JavaScript (`Date.now()`) instead:

```rust
use dl_uuid::V7Generator;
use std::sync::Mutex;

static IDS: Mutex<V7Generator> = Mutex::new(V7Generator::new());

/// Called from JavaScript with `Date.now()`.
pub fn new_order_id(unix_ms: f64) -> Result<String, dl_uuid::Error> {
    let mut generator = IDS.lock().unwrap_or_else(|e| e.into_inner());
    Ok(generator.try_generate_at(unix_ms as u64)?.to_string())
}

assert!(new_order_id(1_700_000_000_000.0).unwrap() < new_order_id(1_700_000_000_000.0).unwrap());
```

With `default-features = false` (no `dl-rand`), supply both the time and the random
bytes, for example from `crypto.getRandomValues`:

```rust
use dl_uuid::{Uuid, V7Generator};

let mut generator = V7Generator::new();
let random: [u8; 10] = [0x3c, 0x91, 0x07, 0xd2, 0x5e, 0xaa, 0x18, 0x60, 0xf4, 0x2b];
let id = generator.generate_from(1_700_000_000_000, random); // monotonic
let v4 = Uuid::v4_from([0x5a; 16]); // version and variant bits set for you
assert_eq!(id.v7_timestamp_ms(), Some(1_700_000_000_000));
assert_eq!(v4.to_string(), "5a5a5a5a-5a5a-4a5a-9a5a-5a5a5a5a5a5a");
```

## API overview

| Area | Items |
|---|---|
| Type and constants | `Uuid`, `Uuid::{NIL, MAX, NAMESPACE_DNS, NAMESPACE_URL, NAMESPACE_OID, NAMESPACE_X500}` |
| Bytes and integers | `from_bytes`, `as_bytes`, `into_bytes`, `from_u128`, `as_u128`, `from_fields`, `to_fields`, `From<[u8; 16]>`, `From<u128>` (both directions), `AsRef<[u8]>` |
| .NET / SQL Server | `from_bytes_le`, `to_bytes_le`, `cmp_sqlserver` |
| Generation (`std`) | `new_v4`, `try_new_v4`, `now_v7`, `try_now_v7` (not on `wasm32-unknown-unknown`) |
| Generation (always) | `new_v3`, `new_v5`, `v4_from`, `v7_from`, `v8_from`, `V7Generator::{new, generate_from}` |
| `V7Generator` (`std`) | `try_generate_at(unix_ms)`, `generate`, `try_generate` (not on `wasm32-unknown-unknown`) |
| Inspection | `version() -> Option<Version>`, `variant() -> Variant`, `is_nil`, `is_max`, `v7_timestamp_ms`, `gregorian_timestamp`, `unix_timestamp_ms` |
| Parsing | `parse` (`const`), `parse_bytes` (`const`), `FromStr`, `TryFrom<&str>` |
| Formatting | `Display`, `Debug`, `LowerHex`, `UpperHex`, `to_string_upper`, `encode_into`, `hyphenated()`, `simple()`, `braced()`, `urn()` returning `Hyphenated`, `Simple`, `Braced`, `Urn` (each with `LENGTH`, `encode_lower`, `encode_upper`, `to_string_upper`, `Display`, `{:X}`) |
| Enums | `Version::{Nil, V1..V8, Max}` with `to_u8`, `Variant::{Ncs, Rfc9562, Microsoft, Future}` |
| Errors | `Error::{InvalidLength { len }, InvalidHexDigit { offset }, ExpectedChar { offset, expected }, RandomSource { os_code }}` |

## Platform support

| Target | Parse, format, v3/v5, `*_from`, `generate_from` | `new_v4`, `try_generate_at` | `now_v7`, `V7Generator::generate` |
|---|---|---|---|
| Linux, macOS, Windows (x86-64, AArch64, 32-bit) | yes | yes (`getrandom`, `getentropy`, `ProcessPrng`) | yes |
| `wasm32-unknown-unknown` (browser, Node) | yes | yes, with the `dl_rand.fill` import | not compiled: no clock |
| `wasm32-wasip1` | yes | yes (WASI `random_get`) | yes |
| `no_std` + `alloc` (`default-features = false`) | yes | no: bring your own random bytes | no: bring your own clock |

## Performance

`cargo bench -p dl-uuid`: median time per operation, Intel Core i7-9850H (2.6 GHz),
Windows, rustc 1.100.

| Case | Time |
|---|---:|
| `Uuid::new_v4()` | 70 ns |
| `Uuid::now_v7()` | 118 ns |
| `V7Generator::generate()` (private generator, no lock) | 98 ns |
| `V7Generator::generate_from(ms, random)` (no OS calls) | 4 ns |
| `Uuid::new_v5(&NAMESPACE_DNS, "www.example.com")` | 260 ns |
| `Uuid::parse`, hyphenated | 31 ns |
| `Uuid::parse`, simple | 25 ns |
| `Uuid::parse`, URN with uppercase digits | 38 ns |
| `to_string()` | 84 ns |
| `to_string_upper()` | 72 ns |
| `encode_into(&mut [u8; 36])` (no allocation) | 17 ns |

The figures are the best of four runs on a shared machine; busy runs were up to 1.5x
slower. Almost all of the generation cost is the OS call: `new_v4` and `now_v7` read
the CSPRNG for every ID (16 and 10 bytes), and the generator itself takes 4 ns.
Both v7 paths also read the system clock; the process-wide lock of `now_v7` adds about
20 ns over a private `V7Generator`. `to_string()` goes through `core::fmt` and allocates;
`encode_into` writes the 36 bytes into a stack buffer.

## Standards and references

- [RFC 9562](https://www.rfc-editor.org/rfc/rfc9562), *Universally Unique IDentifiers
  (UUIDs)*: layout, versions 1–8, Nil and Max, namespaces (§6.6), v7 monotonicity
  (§6.2, Method 1). The Appendix A and B test vectors are unit tests.
- [RFC 8141](https://www.rfc-editor.org/rfc/rfc8141): the `urn:uuid:` prefix is
  matched case-insensitively.
- Python 3.13 `uuid` produced the v3/v5, `bytes_le`, field and version/variant
  vectors (`testdata/gen_vectors.py`).
- .NET [`Guid.ToByteArray`](https://learn.microsoft.com/en-us/dotnet/api/system.guid.tobytearray)
  for the mixed-endian byte layout; [`SqlGuid.CompareTo`](https://learn.microsoft.com/en-us/dotnet/api/system.data.sqltypes.sqlguid.compareto)
  and the measurements by [dbi services](https://www.dbi-services.com/blog/why-uuidv7-does-not-reduce-fragmentation-in-sql-server/)
  for the SQL Server order.

## Limitations

- No v1, v2 or v6 generation (they need a node ID or MAC address; RFC 9562 recommends
  v7 instead). v1 and v6 UUIDs can be parsed and their timestamps read.
- v7 order is guaranteed per generator: across threads for `now_v7()`, not across
  processes or machines. After the clock goes backwards, IDs keep the last timestamp
  until the clock catches up; if more than 2^41 IDs are made in one millisecond the
  timestamp runs ahead by 1 ms.
- The parser accepts exactly the four forms above. No surrounding whitespace, no C#
  `"P"` (`(...)`) or `"X"` (`{0x...}`) formats, and no hyphens in other positions
  (Python's `UUID()` is more lenient).
- No generator that puts the timestamp where SQL Server compares first (a "sequential
  GUID"/COMB layout); see the SQL Server options above.
- The full SQL Server order is not specified by Microsoft beyond "the last six bytes
  are most significant"; `cmp_sqlserver` follows `SqlGuid` and published measurements.
- No `serde` support (zero-dependency policy); use `Display` / `FromStr` or the bytes.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0) at your option.
