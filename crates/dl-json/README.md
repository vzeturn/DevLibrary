# dl-json

A strict RFC 8259 JSON parser, serializer and value model for Rust, with no dependencies
and `no_std` support.

## Replaces

| Ecosystem | Packages |
|-----------|----------|
| .NET      | `Newtonsoft.Json` (`JObject`, `JToken.SelectToken`), `System.Text.Json` (`JsonDocument`, `JsonNode`, `JsonSerializer`) |
| Node.js   | built-in `JSON.parse` / `JSON.stringify`, `json-bigint` / `lossless-json` (exact numbers), `json-pointer` |
| Go        | `encoding/json` (`map[string]any`, `json.Number`), `github.com/tidwall/gjson` |
| Python    | `json`, `simplejson` (`use_decimal=True`) |
| Rust      | `serde_json` (`Value`, `json!`, `Map` with `preserve_order`) |

## Features

- **Strict parser**: exactly the RFC 8259 grammar. No comments, trailing commas, single
  quotes, `NaN`/`Infinity`, leading zeros, `+1`, `.5` or `1.`; only space, tab, LF and CR
  count as whitespace. Checked against a JSONTestSuite-style table of 89 must-accept and
  127 must-reject inputs.
- **Useful errors**: an `ErrorKind`, byte offset, 1-based line and column
  (`unexpected character 'x' at line 3 column 7`). Conversion errors carry a JSON Pointer
  to the bad value (`expected u32, found string at /lines/1/quantity`).
- **Safe on untrusted input**: no panics, no `unsafe`, nesting limit (128 by default),
  lone surrogates and invalid UTF-8 rejected.
- **Ordered objects**: `Map` keeps insertion order and has `O(log n)` lookups on large
  objects.
- **Exact numbers when you need them**: integers are kept as `u64`/`i64`; with
  `keep_raw_numbers` every number keeps its source text (`"1999999.90"` stays
  `1999999.90`), which is what money amounts need.
- **JavaScript-compatible output**: floats print like `JSON.stringify` (shortest
  round-trip digits, `1e21`, `1.5e-8`), U+2028/U+2029 are escaped so output can be
  embedded in `<script>` tags.
- **Ergonomic access**: `v["lines"][0]["sku"]` (missing → `null`, never panics),
  `pointer("/lines/0/sku")` (RFC 6901), `as_str`/`as_u64`/... and `PartialEq` with Rust
  literals.
- **`json!` macro** and **`ToJson`/`FromJson` traits** for your own types.
- `#![no_std]` + `alloc`, builds for `wasm32-unknown-unknown`.

## Installation

The crate lives in the DevLibrary workspace; cargo finds the package by name inside the
repository:

```toml
[dependencies]
dl-json = { git = "https://github.com/vzeturn/devlibrary" }
```

Pin a revision for reproducible builds: `{ git = "...", rev = "<commit>" }`.

With a local checkout, use a path dependency:

```toml
[dependencies]
dl-json = { path = "../DevLibrary/crates/dl-json" }
```

The minimum supported Rust version is 1.88.

## Quick start

```rust
use dl_json::json;

let booking = dl_json::parse(r#"{
    "code": "BK-2024-118",
    "hotel": "Sông Hàn Riverside",
    "guest": {"name": "Nguyễn Văn An", "vip": true},
    "nights": 3,
    "rate": 1250000.5
}"#)?;

assert_eq!(booking["guest"]["name"], "Nguyễn Văn An");
assert_eq!(booking["nights"].as_u64(), Some(3));
assert!(booking["discount"].is_null()); // missing keys read as null

let reply = json!({"code": booking["code"], "status": "confirmed", "rooms": [1205, 1206]});
assert_eq!(
    reply.to_string(),
    r#"{"code":"BK-2024-118","status":"confirmed","rooms":[1205,1206]}"#
);
# Ok::<(), dl_json::Error>(())
```

## Usage guide

### Parsing

`parse` takes a `&str`, `parse_slice` takes bytes (for example a file or an HTTP body) and
validates UTF-8 first. Both skip a leading byte order mark, which RFC 8259 section 8.1
allows parsers to ignore. Any JSON value may be at the top level.

```rust
use dl_json::{ErrorKind, ParseOptions};

let body: &[u8] = b"\xEF\xBB\xBF{\"invoice\": \"INV-1\", \"paid\": false}";
let v = dl_json::parse_slice(body)?;
assert_eq!(v["paid"], false);

// The nesting limit protects against stack exhaustion on hostile input.
let opts = ParseOptions { max_depth: 2, ..ParseOptions::default() };
let err = dl_json::parse_with("[[[1]]]", opts).unwrap_err();
assert_eq!(err.kind(), &ErrorKind::DepthLimitExceeded);

// `str::parse` works too.
let n: dl_json::Value = "42".parse()?;
assert_eq!(n, 42);
# Ok::<(), dl_json::Error>(())
```

When an object repeats a key, the last value wins and the entry keeps the position of the
first occurrence (the same as calling `Map::insert` twice):

```rust
let v = dl_json::parse(r#"{"status": "pending", "id": 7, "status": "paid"}"#)?;
assert_eq!(v.to_string(), r#"{"status":"paid","id":7}"#);
# Ok::<(), dl_json::Error>(())
```

### Reading values

Indexing with a key or position returns `&Value::Null` when the value is missing or has
another type, so lookups chain without panicking. `get`/`get_index` return `Option`
instead. `pointer` takes an RFC 6901 JSON Pointer (`~1` is `/`, `~0` is `~`).

```rust
let invoice = dl_json::parse(r#"{
    "number": "INV-2024-0042",
    "customer": {"name": "Công ty TNHH Minh Phát", "tax_code": "0312345678"},
    "lines": [
        {"sku": "CF-01", "description": "Cà phê sữa đá", "quantity": 2, "unit_price": 29000},
        {"sku": "TR-07", "description": "Trà đào", "quantity": 1, "unit_price": 35000}
    ]
}"#)?;

assert_eq!(invoice["lines"][1]["sku"], "TR-07");
assert_eq!(invoice.pointer("/customer/tax_code").and_then(|v| v.as_str()), Some("0312345678"));
assert!(invoice["lines"][5]["sku"].is_null());
assert!(invoice.get("discount").is_none());

let total: u64 = invoice["lines"]
    .as_array()
    .map(|lines| {
        lines.iter().map(|l| l["quantity"].as_u64().unwrap_or(0) * l["unit_price"].as_u64().unwrap_or(0)).sum()
    })
    .unwrap_or(0);
assert_eq!(total, 93000);
assert_eq!(invoice["customer"].type_name(), "object");
# Ok::<(), dl_json::Error>(())
```

### Modifying values

```rust
use dl_json::{Value, json};

let mut booking = json!({"code": "BK-7", "rooms": [{"number": 301, "nights": 2}]});

*booking.pointer_mut("/rooms/0/nights").unwrap() = 3.into();
if let Some(rooms) = booking.get_mut("rooms").and_then(Value::as_array_mut) {
    rooms.push(json!({"number": 302, "nights": 3}));
}
if let Some(obj) = booking.as_object_mut() {
    obj.insert("status", "checked_in");
    obj.remove("code");
}
let rooms = booking["rooms"].clone();
assert_eq!(rooms[1]["number"], 302);
assert_eq!(booking.to_string(), r#"{"rooms":[{"number":301,"nights":3},{"number":302,"nights":3}],"status":"checked_in"}"#);

// take() moves a value out and leaves null behind.
let status = booking.get_mut("status").map(Value::take);
assert_eq!(status, Some(Value::from("checked_in")));
assert!(booking["status"].is_null());
```

### Building values

`json!` accepts JSON syntax with Rust expressions in value position. Expressions are
converted by reference through `ToJson`, so variables stay usable afterwards. Keys can be
any expression that converts into `String` when wrapped in parentheses.

```rust
use dl_json::{Map, Value, json};

let guest = String::from("Trần Thị Bình");
let nights = 2u32;
let rate = 980_000u64;
let extras = vec!["breakfast", "airport_pickup"];

let booking = json!({
    "guest": guest,
    "nights": nights,
    "total": nights as u64 * rate,
    "extras": extras,
    "deposit": null,
    "flags": {"vip": false, "smoking": false},
    (format!("room_{}", 1205)): true,
});
assert_eq!(booking["total"], 1_960_000);
assert_eq!(booking["room_1205"], true);
assert_eq!(guest, "Trần Thị Bình"); // not moved

// Without the macro: From conversions and Map.
let mut line = Map::new();
line.insert("sku", "CF-01");
line.insert("quantity", 2);
line.insert("unit_price", 29_000);
let lines = Value::from(vec![Value::from(line)]);
assert_eq!(lines.to_string(), r#"[{"sku":"CF-01","quantity":2,"unit_price":29000}]"#);

// Non-finite floats have no JSON form and become null, like JSON.stringify.
assert_eq!(Value::from(f64::NAN), Value::Null);
assert_eq!(Value::from(None::<u32>), Value::Null);
```

`json!` limits: it recurses once per token, so very large literals may need a higher
`#![recursion_limit]`, and a value expression that contains a top-level comma (such as a
generic call with two type arguments) must be wrapped in parentheses.

### Ordered maps

`Map` keeps insertion order. `insert` on an existing key replaces the value in place and
returns the old one; `remove` keeps the order of the remaining entries. Equality ignores
order, because JSON objects are unordered.

```rust
use dl_json::{Map, Value};

let mut room: Map = [("number", Value::from(1205)), ("type", "deluxe".into())].into_iter().collect();
room.insert("rate", 1_250_000);
assert_eq!(room.insert("type", "suite"), Some(Value::from("deluxe")));
assert_eq!(room.keys().map(String::as_str).collect::<Vec<_>>(), ["number", "type", "rate"]);

room.remove("number");
room.extend([("view", "river"), ("floor", "12")]);
assert_eq!(Value::from(room.clone()).to_string(), r#"{"type":"suite","rate":1250000,"view":"river","floor":"12"}"#);

for (key, value) in &room {
    assert!(!key.is_empty() && !value.is_null());
}
```

### Numbers and money

Integers that fit are stored exactly as `u64` (non-negative) or `i64` (negative). Other
numbers become `f64`; integers beyond 64 bits are converted to the nearest `f64`, and a
float that overflows to infinity (`1e400`) is rejected with `InvalidNumber`.

Binary floats cannot represent most decimal amounts exactly. For money, parse with
`keep_raw_numbers: true`: every number keeps its source text, is serialized verbatim, and
can be handed to a decimal type. `Number::from_raw` creates such a number for output.

```rust
use dl_json::{Number, ParseOptions, Value, json};

let opts = ParseOptions { keep_raw_numbers: true, ..ParseOptions::default() };
let payment = dl_json::parse_with(r#"{"amount": 1999999.90, "fee": 0.1, "count": 3}"#, opts)?;

let amount = payment["amount"].as_number().unwrap();
assert_eq!(amount.as_raw_str(), Some("1999999.90")); // exact text, trailing zero kept
assert_eq!(payment["count"].as_u64(), Some(3)); // raw integers still convert
assert_eq!(payment.to_string(), r#"{"amount":1999999.90,"fee":0.1,"count":3}"#);

// Convert to integer minor units without going through f64.
let (whole, cents) = amount.as_raw_str().unwrap().split_once('.').unwrap();
let minor: u64 = format!("{whole}{cents:0<2}").parse().unwrap();
assert_eq!(minor, 199_999_990);

// Emit an exact amount.
let total = Number::from_raw("123456789012345678.05").unwrap();
assert_eq!(json!({"total": total}).to_string(), r#"{"total":123456789012345678.05}"#);

// Equality is numeric across representations.
assert_eq!(dl_json::parse("1.0")?, 1);
assert_eq!(Value::from(Number::from_raw("10.50").unwrap()), 10.5);
# Ok::<(), dl_json::Error>(())
```

### Serializing

`to_string` writes compact JSON, `to_string_pretty` uses a 2-space indent with the same
layout as `JSON.stringify(value, null, 2)`, and `to_string_pretty_with` takes another
indent unit. `write_to`/`write_pretty_to` append to an existing `String`. `Value`
implements `Display` (compact, or pretty with `{:#}`).

```rust
use dl_json::json;

let invoice = json!({"number": "INV-9", "lines": [{"sku": "A", "qty": 2}], "tags": [], "note": null});
assert_eq!(
    dl_json::to_string_pretty(&invoice),
    r#"{
  "number": "INV-9",
  "lines": [
    {
      "sku": "A",
      "qty": 2
    }
  ],
  "tags": [],
  "note": null
}"#
);
assert_eq!(dl_json::to_string_pretty_with(&json!([1]), "\t"), "[\n\t1\n]");

let mut buf = String::from("data: ");
dl_json::write_to(&json!({"event": "paid"}), &mut buf);
assert_eq!(buf, r#"data: {"event":"paid"}"#);
assert_eq!(format!("{}", json!([true, null])), "[true,null]");
```

Strings escape `"`, `\`, control characters (`\b \f \n \r \t`, others as `\u00XX`) and
U+2028/U+2029 (as `\u2028`/`\u2029`, so the output is also valid inside JavaScript
source). Other characters, including all non-ASCII text, are written as UTF-8.

```rust
use dl_json::Value;

let s = Value::from("Phòng \"VIP\"\n\u{2028}tab\there");
assert_eq!(s.to_string(), r#""Phòng \"VIP\"\n\u2028tab\there""#);
```

Floats are written like JavaScript's `Number.prototype.toString`, which is what
`JSON.stringify` uses: the shortest digits that read back as the same `f64`, plain
decimal for `1e-6 <= |x| < 1e21` and exponent form otherwise. Integral floats print
without `.0`. Three deliberate differences, all valid JSON: positive exponents have no
`+` (`1e21` where JS writes `1e+21`); `-0.0` prints as `-0` (JS prints `0`); and an
integral float between 2^53 and 2^64 whose zero-padded digits would read back as a
different 64-bit integer is written in exponent form (`12345678901234567168.0` prints as
`1.2345678901234567e19`, where JS writes `12345678901234567000`).

```rust
use dl_json::Value;

let cases = [
    (0.1 + 0.2, "0.30000000000000004"),
    (100.0, "100"),
    (1e21, "1e21"),
    (1.5e-8, "1.5e-8"),
    (0.000001, "0.000001"),
    (1e-7, "1e-7"),
    (5e-324, "5e-324"),
    (-0.0, "-0"),
];
for (f, text) in cases {
    assert_eq!(Value::from(f).to_string(), text);
}
```

### Your own types: `ToJson` and `FromJson`

Implement `ToJson` (usually with `json!`) and `FromJson` (usually with `Value::field`).
Built-in conversions exist for `bool`, all integer types (range-checked), `f32`, `f64`,
`String`/`str`, `Option<T>`, `Vec<T>`/slices/arrays, `BTreeMap<String, T>`, `Box<T>`,
`Map`, `Number` and `Value`. `from_str` parses and converts in one step; `to_string_of`
converts and serializes.

`Value::field::<T>("name")` reports a missing member as `MissingField("name")`, except
for types that accept `null` (`Option<T>` gives `None`). Errors from nested conversions
get the member name or array index prepended to their JSON Pointer path.

```rust
use dl_json::{Error, ErrorKind, FromJson, ToJson, Value, json};

#[derive(Debug, PartialEq)]
struct Line {
    sku: String,
    description: String,
    quantity: u32,
    unit_price: u64, // VND has no minor unit
}

#[derive(Debug, PartialEq)]
struct Invoice {
    number: String,
    customer: String,
    lines: Vec<Line>,
    note: Option<String>,
}

impl Invoice {
    fn total(&self) -> u64 {
        self.lines.iter().map(|l| l.quantity as u64 * l.unit_price).sum()
    }
}

impl ToJson for Line {
    fn to_json(&self) -> Value {
        json!({
            "sku": self.sku,
            "description": self.description,
            "quantity": self.quantity,
            "unit_price": self.unit_price,
        })
    }
}

impl FromJson for Line {
    fn from_json(v: &Value) -> Result<Self, Error> {
        let line = Line {
            sku: v.field("sku")?,
            description: v.field("description")?,
            quantity: v.field("quantity")?,
            unit_price: v.field("unit_price")?,
        };
        if line.quantity == 0 {
            return Err(Error::custom("quantity must be at least 1"));
        }
        Ok(line)
    }
}

impl ToJson for Invoice {
    fn to_json(&self) -> Value {
        json!({
            "number": self.number,
            "customer": self.customer,
            "lines": self.lines,
            "note": self.note,
            "total": self.total(),
        })
    }
}

impl FromJson for Invoice {
    fn from_json(v: &Value) -> Result<Self, Error> {
        Ok(Invoice {
            number: v.field("number")?,
            customer: v.field("customer")?,
            lines: v.field("lines")?,
            note: v.field("note")?, // Option: missing or null -> None
        })
    }
}

let input = r#"{
    "number": "INV-2024-0042",
    "customer": "Công ty TNHH Minh Phát",
    "lines": [
        {"sku": "CF-01", "description": "Cà phê sữa đá", "quantity": 2, "unit_price": 29000},
        {"sku": "TR-07", "description": "Trà đào", "quantity": 1, "unit_price": 35000}
    ]
}"#;
let invoice: Invoice = dl_json::from_str(input)?;
assert_eq!(invoice.total(), 93_000);
assert_eq!(invoice.note, None);

let json = dl_json::to_string_of(&invoice);
assert!(json.ends_with(r#""note":null,"total":93000}"#));
let again: Invoice = dl_json::from_str(&json)?; // the extra "total" member is ignored
assert_eq!(again, invoice);

// Errors say what was wrong and where.
let bad = input.replace(r#""quantity": 1"#, r#""quantity": "1""#);
let err = dl_json::from_str::<Invoice>(&bad).unwrap_err();
assert_eq!(err.to_string(), "expected u32, found string at /lines/1/quantity");
assert_eq!(err.path(), "/lines/1/quantity");

let err = dl_json::from_str::<Invoice>(r#"{"number": "INV-1", "lines": []}"#).unwrap_err();
assert_eq!(err.kind(), &ErrorKind::MissingField("customer".into()));

let zero = input.replace(r#""quantity": 2"#, r#""quantity": 0"#);
let err = dl_json::from_str::<Invoice>(&zero).unwrap_err();
assert_eq!(err.to_string(), "quantity must be at least 1 at /lines/0");
# Ok::<(), dl_json::Error>(())
```

## API overview

| Item | Purpose |
|------|---------|
| `parse(&str)`, `parse_slice(&[u8])` | Parse with default options (`parse_slice` validates UTF-8) |
| `parse_with`, `parse_slice_with`, `ParseOptions { max_depth, keep_raw_numbers }` | Parse with a nesting limit and/or exact raw numbers |
| `to_string`, `to_string_pretty`, `to_string_pretty_with(v, indent)` | Serialize compact / 2-space / custom indent |
| `write_to(v, &mut String)`, `write_pretty_to(v, &mut String, indent)` | Append to an existing buffer |
| `Value` | `Null`, `Bool`, `Number`, `String`, `Array(Vec<Value>)`, `Object(Map)`; `Display` (`{:#}` = pretty), `FromStr`, `Default` (null) |
| `Value::get`, `get_mut`, `get_index`, `get_index_mut`, `v["key"]`, `v[0]` | Member / element access (`Index` returns `null` when missing) |
| `Value::pointer`, `pointer_mut` | RFC 6901 JSON Pointer lookup |
| `Value::as_str`, `as_bool`, `as_i64`, `as_u64`, `as_f64`, `as_number`, `as_array(_mut)`, `as_object(_mut)` | Typed views |
| `Value::is_null`, `is_boolean`, `is_number`, `is_string`, `is_array`, `is_object`, `type_name`, `take` | Inspection and moving out |
| `Value::field::<T>(name)` | Convert an object member with `FromJson` |
| `Map` | Ordered object: `new`, `with_capacity`, `len`, `is_empty`, `get`, `get_mut`, `contains_key`, `insert`, `remove`, `iter`, `iter_mut`, `keys`, `values`, `values_mut`, `IntoIterator`, `FromIterator`, `Extend`, `Index<&str>` |
| `Number` | `as_i64`, `as_u64`, `as_f64`, `is_i64`, `is_u64`, `is_f64`, `is_raw`, `as_raw_str`, `from_f64`, `from_raw`, `From<integers>`; `Display` |
| `ToJson`, `FromJson`, `from_str::<T>`, `to_string_of` | Conversions for your own types |
| `json!` | Build values with JSON syntax |
| `Error`, `ErrorKind`, `Position` | Errors with position (syntax) or JSON Pointer path (conversion) |

`From<T> for Value` exists for `bool`, all integer types (128-bit values outside the
64-bit range become raw numbers), `f32`/`f64` (non-finite → `null`), `&str`, `String`,
`&String`, `Cow<str>`, `Number`, `Map`, `Vec<T>`, `&[T]` and `Option<T>`. `Value`
compares with `==` against `str`, `&str`, `String`, `bool`, all 8- to 64-bit integers,
`f32` and `f64`, in both directions.

## Error handling

Every fallible function returns `Result<_, dl_json::Error>`. `Error` is one pointer wide
and implements `Display`, `Debug`, `Clone`, `PartialEq` and `core::error::Error`.

| `ErrorKind` | Raised when |
|-------------|-------------|
| `UnexpectedEof` | The input ends inside a value, or is empty |
| `UnexpectedChar(char)` | A character not allowed at that point (`[1,]`, `{'a':1}`, `[1 2]`) |
| `InvalidNumber` | `01`, `1.`, `-`, `1e`, `-Infinity`, or a float that overflows `f64` |
| `InvalidEscape` | `\x`, `\'`, a backslash before a tab, ... |
| `InvalidUnicode` | Bad `\u` hex digits or an unpaired surrogate (`"\ud800"`) |
| `ControlCharInString` | A raw U+0000..U+001F inside a string |
| `TrailingCharacters` | Anything but whitespace after the top-level value |
| `DepthLimitExceeded` | Nesting deeper than `max_depth` |
| `InvalidUtf8` | `parse_slice` input is not UTF-8 |
| `Type { expected, found }` | `FromJson`: wrong JSON type or out-of-range number |
| `MissingField(name)` | `FromJson`: required member missing |
| `Custom(message)` | `FromJson`: created with `Error::custom` |

Syntax errors carry a `Position` (byte `offset`, 1-based `line`, 1-based `column` counted
in characters; only `\n` starts a new line; a skipped BOM counts in `offset` but not in
`column`). Conversion errors carry a JSON Pointer `path` instead.

```rust
use dl_json::ErrorKind;

let err = dl_json::parse("{\n  \"room\": 1205,\n  \"guest\": Bình\n}").unwrap_err();
assert_eq!(err.kind(), &ErrorKind::UnexpectedChar('B'));
assert_eq!((err.line(), err.column(), err.offset()), (3, 12, 29));
assert_eq!(err.to_string(), "unexpected character 'B' at line 3 column 12");

let err = dl_json::from_str::<Vec<u8>>("[1, 2, 300]").unwrap_err();
assert_eq!(err.to_string(), "expected u8, found out-of-range integer at /2");
assert!(err.position().is_none());
```

## Platform support

- Linux, macOS and Windows (tested in CI on all three), `wasm32-unknown-unknown`.
- `#![no_std]`: only `core` and `alloc` are used, so the crate works in embedded and
  browser builds. No `unsafe` code, no platform-specific code, no build script.

## Performance

The parser makes one pass over the input: plain string runs are found 8 bytes at a time
and copied in one `push_str`, strings without escapes are allocated once at their exact
size, arrays and objects are collected on a shared scratch stack and moved into exactly
sized vectors, and common numbers are converted without building a lexeme (integers up
to 19 digits directly; floats with Clinger's exact fast path, falling back to `core`'s
correctly rounded parser). The serializer writes into a single `String` without
intermediate allocations.

Measured with `cargo bench -p dl-json` on an Intel Core i7-9850H (2.6 GHz), Windows 11,
Rust 1.100, release profile with fat LTO. The main document is 2,393 hotel bookings
(1.0 MB compact, 1.56 MB pretty) with nested objects, escaped and Vietnamese strings,
integers and floats; it is built in `benches/json.rs`.

| Case | Input | Median | Throughput |
|------|-------|-------:|-----------:|
| `parse`, compact document | 1.00 MB | 10.2 ms | 93 MiB/s |
| `parse`, pretty document | 1.56 MB | 10.8 ms | 138 MiB/s |
| `parse_slice` (includes UTF-8 validation) | 1.00 MB | 10.8 ms | 88 MiB/s |
| `parse_with`, `keep_raw_numbers: true` | 1.00 MB | 12.8 ms | 75 MiB/s |
| `to_string` | 1.00 MB | 3.2 ms | 298 MiB/s |
| `to_string_pretty` | 1.56 MB | 4.6 ms | 327 MiB/s |
| Baseline: `clone` + drop of the parsed tree | 1.00 MB | 7.2 ms | 133 MiB/s |
| `parse`, array of 100,000 integers and floats | 0.97 MB | 7.3 ms | 127 MiB/s |
| `parse`, array of 1,000 long strings with escapes | 1.19 MB | 2.8 ms | 403 MiB/s |
| `parse`, small invoice | 87 B | 1.7 µs | |
| `to_string`, small invoice | 87 B | 640 ns | |

The machine was running other builds at the same time, and repeated runs varied by up
to 3x; the table is the quietest run. Compare rows within one run rather than across
machines.

Most of the parse time on the main document is memory allocation: it holds about
90,000 strings, arrays and objects, and cloning plus dropping the finished tree (the
baseline row) already takes 70% of the parse time with the Windows system allocator.
Documents with fewer, longer values parse faster, as the long-strings row shows. Floats
with 17 significant digits (typical for computed values) take the slower general
conversion path, which dominates the numbers row.

Reproduce with:

```sh
cargo bench -p dl-json
cargo bench -p dl-json -- parse   # only cases whose name contains "parse"
```

## Standards and test vectors

- [RFC 8259](https://www.rfc-editor.org/rfc/rfc8259): the JSON grammar. The section 13
  examples and the section 7 surrogate-pair example are unit tests.
- [RFC 6901](https://www.rfc-editor.org/rfc/rfc6901): JSON Pointer. All section 5
  examples are unit tests.
- [JSONTestSuite](https://github.com/nst/JSONTestSuite): the accept/reject tables in
  `src/parse/tests.rs` reproduce its `y_`, `n_` and selected `i_` cases inline, named
  after the original files.
- [ECMA-262 Number::toString](https://tc39.es/ecma262/#sec-numeric-types-number-tostring):
  float formatting. Expected strings were produced by Node.js `JSON.stringify` with
  `testdata/float_vectors.js` and are checked bit-exactly; 20,000 random `f64` values
  are also checked to round-trip.
- [RFC 3629](https://www.rfc-editor.org/rfc/rfc3629): UTF-8 (overlong forms, encoded
  surrogates and code points above U+10FFFF are rejected by `parse_slice`).

## Limitations and roadmap

- The whole document is parsed into memory; there is no streaming or event (SAX) API
  and no zero-copy `Value` that borrows from the input.
- There is no derive macro: implement `ToJson`/`FromJson` by hand (a `dl-*` derive crate
  is on the roadmap).
- The parser enforces `max_depth`, but the serializer, `Clone` and `Drop` are recursive:
  a `Value` built by hand with tens of thousands of nesting levels can overflow the
  stack.
- Duplicate object keys are accepted (last wins); there is no option to reject them yet.
- Inserting a new key into a large `Map` shifts its index (`O(n)` moves of `usize`),
  and `remove` is `O(n)`. Build large maps with `collect` or `extend`, which sort once.
- Number equality is numeric (`1 == 1.0`). Comparing a raw number with a non-raw one
  converts the raw text first, so `PartialEq` is not transitive in rare precision edge
  cases; `Value` does not implement `Eq` or `Hash`.
- Built-in integer conversions reject floats, even integral ones such as `3.0`.
- Columns in error positions count Unicode scalar values, not UTF-16 units or grapheme
  clusters.
- No JSON5, comments, NDJSON helpers or schema validation.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0) at your option.
