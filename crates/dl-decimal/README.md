# dl-decimal

Exact decimal arithmetic for money: invoices, VAT, POS and VND amounts. A `Decimal` is
an `i128` mantissa (up to 38 digits) and a scale of 0 to 28 decimal places, so
`0.1 + 0.2` is exactly `0.3` and `1.005` rounds to `1.01`. No dependencies, `no_std`,
no `unsafe`, and the same results on Linux, macOS, Windows and WebAssembly.

```rust
use dl_decimal::Decimal;

let a: Decimal = "0.1".parse().unwrap();
let b: Decimal = "0.2".parse().unwrap();
assert_eq!(a + b, "0.3".parse::<Decimal>().unwrap());
assert_eq!((a + b).to_string(), "0.3");
```

## Replaces

| Ecosystem | Package | Notes |
|---|---|---|
| C# / .NET | `System.Decimal` (`decimal`), `Math.Round(d, n, MidpointRounding)` | Same scale range (0–28) and the same "round half-even to fit" rule; 38 digits instead of ~29 |
| Go | `github.com/shopspring/decimal`, `github.com/ericlagergren/decimal` | Fixed size and `Copy`, no allocation per operation |
| TypeScript / JavaScript | `decimal.js`, `big.js`, `bignumber.js` | Build for `wasm32-unknown-unknown` and call it from the web |
| Python | `decimal.Decimal` for business amounts | Behaves like a fixed `decimal` context (see [Arithmetic rules](#arithmetic-rules)); Python's `decimal` generated the test vectors |

## Features

- Exact `+`, `-`, `*`, `%`; division rounded once to 28 decimal places or 38
  significant digits. A result is only rounded when it does not fit, and then once,
  half-even.
- 7 rounding modes (`HalfEven`, `HalfUp`, `HalfDown`, `Up`, `Down`, `Ceiling`, `Floor`),
  rounding to decimal places or to a multiple (`round_to_multiple`, e.g. VND cash to 500
  or 1,000 đồng).
- Strict parser (`"12.50"`, `".5"`, `"5."`, `"1.5e3"`) with byte offsets in errors, and
  a rounding parser for inputs with too many digits.
- Exact `Display` (`1.50` stays `1.50`), `{:.2}`, `{:+}`, width and fill; grouped
  output such as `1.234.567,50` (Vietnam) or `1,234,567.50` (US).
- Money helpers: `percent_of`, `split` and `allocate` (parts always add up to the
  total, e.g. 100 / 3 = 34 + 33 + 33).
- Equality, ordering and hashing by value: `1.5 == 1.50`, and both hash the same.
- Checked (`checked_add`, ...) and panicking (`+`, ...) forms, like Rust integers.
- Conversions: all integer types, `f64`/`f32` (shortest round-trip form), `to_f64`,
  `to_i64`, `to_i128`.
- `#![no_std]` (needs `alloc`), zero dependencies, `unsafe` denied.

## Installation

From GitHub:

```toml
[dependencies]
dl-decimal = { git = "https://github.com/vzeturn/DevLibrary" }
```

From a local checkout (or inside the DevLibrary workspace):

```toml
[dependencies]
dl-decimal = { path = "../DevLibrary/crates/dl-decimal" }
```

Minimum Rust version: 1.88 (edition 2024).

## Quick start

```rust
use dl_decimal::{Decimal, Error, RoundingMode};

fn main() -> Result<(), Error> {
    let price: Decimal = "19990.50".parse()?;
    let qty = Decimal::from(3);
    let net = price * qty; // 59971.50, exact
    let vat = net.percent_of(Decimal::TEN)?; // 10% VAT = 5997.1500
    let total = (net + vat).round(0); // 65968.65 -> 65969 (half-up, like Excel ROUND)

    assert_eq!(net.to_string(), "59971.50");
    assert_eq!(vat.to_string(), "5997.1500");
    assert_eq!(total.to_string(), "65969");
    assert_eq!(total.to_string_grouped(0, '.', ',', RoundingMode::HalfUp), "65.969");

    // Constants, without a macro:
    const VAT_8: Decimal = Decimal::from_i128_scale(8, 2); // 0.08
    assert_eq!((net * VAT_8).round(0).to_string(), "4798");
    Ok(())
}
```

## Usage guide

### Creating decimals

```rust
use dl_decimal::{Decimal, Error};

// From strings: the written scale is kept.
let a: Decimal = "12.50".parse().unwrap();
assert_eq!((a.mantissa(), a.scale()), (1250, 2));
assert_eq!(".5".parse::<Decimal>().unwrap().to_string(), "0.5"); // like C# decimal.Parse
assert_eq!("5.".parse::<Decimal>().unwrap().to_string(), "5");
assert_eq!("1.5e3".parse::<Decimal>().unwrap().to_string(), "1500");
assert_eq!("2E-2".parse::<Decimal>().unwrap().to_string(), "0.02");

// From a mantissa and a scale: 12.50.
assert_eq!(Decimal::new(1250, 2), Ok(a));
assert_eq!(Decimal::new(1, 29), Err(Error::ScaleTooLarge));
// In constants (a bad argument is a compile-time error there).
const PRICE: Decimal = Decimal::from_i128_scale(1250, 2);
assert_eq!(PRICE, a);

// From integers (exact) and floats (the shortest form that round-trips).
assert_eq!(Decimal::from(25_000u32).to_string(), "25000");
assert_eq!(Decimal::try_from(0.1f64).unwrap().to_string(), "0.1");
assert_eq!(Decimal::try_from(f64::NAN), Err(Error::NotFinite));
assert_eq!(Decimal::try_from(1e40), Err(Error::Overflow));

// Back to primitives.
assert_eq!(a.to_f64(), 12.5);
assert_eq!("-12.99".parse::<Decimal>().unwrap().to_i64(), Some(-12)); // truncates
```

Parsing is strict: no whitespace, no thousands separators, no `NaN`/`Infinity`, at
most 38 digits and at most 28 decimal places (`"1e-29"` fails with `ScaleTooLarge`,
even `"0.000…0"` with 29 zeros). `Decimal::parse_rounded(s, mode)` rounds instead:

```rust
use dl_decimal::{Decimal, RoundingMode};

let x = Decimal::parse_rounded("0.123456789012345678901234567891", RoundingMode::HalfUp);
assert_eq!(x.unwrap().to_string(), "0.1234567890123456789012345679");
```

### Arithmetic rules

```rust
use dl_decimal::{Decimal, RoundingMode};

let d = |s: &str| s.parse::<Decimal>().unwrap();

// + - * are exact; the scale of a product is the sum of the scales.
assert_eq!((d("1.10") + d("2.205")).to_string(), "3.305");
assert_eq!((d("1.5") * d("1.5")).to_string(), "2.25");
// Division: 28 decimal places or 38 significant digits, half-even, trailing zeros removed.
assert_eq!((d("1") / d("3")).to_string(), "0.3333333333333333333333333333");
assert_eq!((d("2") / d("3")).to_string(), "0.6666666666666666666666666667");
assert_eq!((d("10") / d("4")).to_string(), "2.5");
assert_eq!((d("108000") / d("1.08")).to_string(), "100000");
// div_round: choose the decimal places and the rounding mode yourself.
let unit = d("1000000").div_round(d("7"), 0, RoundingMode::HalfUp).unwrap();
assert_eq!(unit.to_string(), "142857");
// The remainder has the sign of the dividend, like Rust and C#.
assert_eq!((d("-7") % d("3")).to_string(), "-1");
// Checked forms return None instead of panicking.
assert_eq!(Decimal::MAX.checked_add(Decimal::ONE), None);
assert_eq!(Decimal::ONE.checked_div(Decimal::ZERO), None);
```

The exact rules:

- `+`, `-`, `*` compute the exact result. If it needs more than 28 decimal places or
  more than 38 digits, it is rounded **once**, half-even, to the largest scale that
  fits (what C# `decimal` does). This is Python `decimal` with `prec=38`,
  `rounding=ROUND_HALF_EVEN`, `Emax=37`, plus a cap of 28 decimal places.
- `/` rounds the exact quotient half-even to 28 decimal places or 38 significant
  digits, whichever is fewer, then removes trailing zeros.
- `%` is exact, with the scale of the more precise operand.
- Overflow means the integer part needs more than 38 digits. Operators panic on
  overflow and division by zero (like integer operators); `checked_*` return `None`.
- `checked_pow` is exact whenever the exact power fits; otherwise each step of the
  square-and-multiply loop rounds like `*`.

### Rounding

```rust
use dl_decimal::{Decimal, RoundingMode};

let x: Decimal = "2.345".parse().unwrap();
assert_eq!(x.round(2).to_string(), "2.35"); // HalfUp: Excel ROUND, invoices
assert_eq!(x.round_dp(2, RoundingMode::HalfEven).to_string(), "2.34"); // banker's
assert_eq!(x.trunc(1).to_string(), "2.3");
assert_eq!("-2.1".parse::<Decimal>().unwrap().floor().to_string(), "-3");
assert_eq!("-2.9".parse::<Decimal>().unwrap().ceil().to_string(), "-2");
// Fixed number of decimals (pads with zeros or rounds):
assert_eq!("1.5".parse::<Decimal>().unwrap().rescale(2, RoundingMode::HalfUp).unwrap().to_string(), "1.50");
```

`round(dp)` uses `HalfUp` (ties away from zero) because that is what people and
invoices usually expect: Excel and Google Sheets `ROUND` round 0.5 away from zero, and
so do most accounting and Vietnamese e-invoice systems. Arithmetic results use
`HalfEven` instead, which does not bias long sums; pass a mode to `round_dp` to choose.

### Invoice lines and VAT (8% / 10%)

Rounding each line and rounding once per VAT rate can differ by a few đồng. The
library makes both explicit; use the rule your tax software or authority expects.

```rust
use dl_decimal::{Decimal, RoundingMode};

let d = |s: &str| s.parse::<Decimal>().unwrap();
// (quantity, unit price in VND, VAT rate in %)
let lines = [
    (d("1.5"), d("33333"), d("8")),
    (d("2"), d("18519"), d("8")),
    (d("3"), d("12345"), d("10")),
    (d("0.75"), d("45678"), d("10")),
    (d("7"), d("4321"), d("8")),
];

// Line amounts are rounded to whole đồng.
let nets: Vec<Decimal> = lines.iter().map(|(q, p, _)| (*q * *p).round(0)).collect();
let net_total: Decimal = nets.iter().sum();
assert_eq!(net_total, Decimal::from(188_579));

// Rule A: VAT per line, rounded per line.
let vat_per_line: Decimal = nets
    .iter()
    .zip(&lines)
    .map(|(n, (_, _, rate))| n.percent_of(*rate).unwrap().round(0))
    .sum();
assert_eq!(vat_per_line, Decimal::from(16_513));

// Rule B: VAT per rate group, rounded once per group.
let group = |r: &str| -> Decimal {
    let base: Decimal = nets.iter().zip(&lines).filter(|(_, l)| l.2 == d(r)).map(|(n, _)| *n).sum();
    base.percent_of(d(r)).unwrap().round(0)
};
let vat_per_group = group("8") + group("10"); // 9382.80 -> 9383, 7129.40 -> 7129
assert_eq!(vat_per_group, Decimal::from(16_512));

let total = net_total + vat_per_group;
assert_eq!(total.to_string_grouped(0, '.', ',', RoundingMode::HalfUp), "205.091");
```

A complete program that prints such an invoice is in
[`examples/vat_invoice.rs`](examples/vat_invoice.rs)
(`cargo run -p dl-decimal --example vat_invoice`).

### VND cash rounding (to 500 or 1,000 đồng)

```rust
use dl_decimal::{Decimal, RoundingMode};

let total = Decimal::from(187_499);
let to = |step: i64, mode| total.round_to_multiple(Decimal::from(step), mode).unwrap();
assert_eq!(to(500, RoundingMode::HalfUp), Decimal::from(187_500));
assert_eq!(to(1000, RoundingMode::HalfUp), Decimal::from(187_000));
assert_eq!(to(1000, RoundingMode::Up), Decimal::from(188_000));
assert_eq!(to(1000, RoundingMode::Down), Decimal::from(187_000));
// Works for prices too: to the nearest 0.05.
let p: Decimal = "1.234".parse().unwrap();
assert_eq!(p.round_to_multiple("0.05".parse().unwrap(), RoundingMode::HalfUp).unwrap().to_string(), "1.25");
```

### Splitting a bill and allocating amounts

`split` and `allocate` hand out the remainder unit by unit, so the parts always add up
exactly to the total.

```rust
use dl_decimal::Decimal;

let bill = Decimal::from(1_234_567);
let parts = bill.split(4, 0).unwrap();
assert_eq!(parts, [308_642, 308_642, 308_642, 308_641].map(Decimal::from));
assert_eq!(parts.iter().sum::<Decimal>(), bill);

assert_eq!(Decimal::from(100).split(3, 0).unwrap(), [34, 33, 33].map(Decimal::from));
let usd: Decimal = "100".parse().unwrap();
assert_eq!(usd.split(3, 2).unwrap()[0].to_string(), "33.34");

// Proportional: 2 shares for one person, 1 each for two others.
let parts = Decimal::from(1_000_000).allocate(&[2, 1, 1], 0).unwrap();
assert_eq!(parts, [500_000, 250_000, 250_000].map(Decimal::from));

// Spread a 50,000 đồng invoice discount over line amounts.
let lines = ["120000", "45500", "9900"].map(|s| s.parse::<Decimal>().unwrap());
let discount = Decimal::from(50_000).allocate(&lines, 0).unwrap();
assert_eq!(discount, [34_208, 12_970, 2_822].map(Decimal::from));
```

### Formatting

```rust
use dl_decimal::{Decimal, RoundingMode};

let x: Decimal = "1234567.505".parse().unwrap();
assert_eq!(x.to_string(), "1234567.505"); // exact, never scientific notation
assert_eq!(format!("{x:.2}"), "1234567.51"); // {:.N} rounds half-up
assert_eq!(format!("{:+}", x), "+1234567.505");
assert_eq!(format!("{:>12.1}", "-1.25".parse::<Decimal>().unwrap()), "        -1.3");
assert_eq!(x.to_string_grouped(2, '.', ',', RoundingMode::HalfUp), "1.234.567,51"); // VN
assert_eq!(x.to_string_grouped(2, ',', '.', RoundingMode::HalfUp), "1,234,567.51"); // US
assert_eq!(x.to_string_grouped(0, '.', ',', RoundingMode::HalfUp), "1.234.568");
```

`Debug` prints the same as `Display`.

### Comparing and hashing

```rust
use dl_decimal::Decimal;
use std::collections::HashSet;

let a: Decimal = "1.5".parse().unwrap();
let b: Decimal = "1.500".parse().unwrap();
assert_eq!(a, b);
assert_eq!(a.to_string(), "1.5");
assert_eq!(b.to_string(), "1.500");
assert_eq!(b.normalize().to_string(), "1.5");
assert_eq!(HashSet::from([a, b]).len(), 1);
assert!("-0.01".parse::<Decimal>().unwrap() < Decimal::ZERO);
assert_eq!(a.max(Decimal::ONE), a);
```

## Why not f64

A binary float cannot store most decimal fractions, so money computed in `f64` drifts
and rounds the wrong way at ties:

```rust
use dl_decimal::Decimal;

// 0.1 + 0.2
assert_ne!(0.1f64 + 0.2, 0.3);
let d = |s: &str| s.parse::<Decimal>().unwrap();
assert_eq!(d("0.1") + d("0.2"), d("0.3"));

// Rounding 1.005 to 2 decimals: f64 holds 1.00499999999999989..., so it rounds down.
assert_eq!((1.005f64 * 100.0).round() / 100.0, 1.0);
assert_eq!(d("1.005").round(2).to_string(), "1.01");

// Adding 0.1 ten times.
assert_ne!((0..10).map(|_| 0.1f64).sum::<f64>(), 1.0);
assert_eq!((0..10).map(|_| d("0.1")).sum::<Decimal>(), Decimal::ONE);
```

Use `Decimal::try_from(f64)` only at boundaries where you receive floats (JSON from
other systems, sensors); it takes the shortest decimal that round-trips (`0.1`, not
`0.1000000000000000055511151231257827`).

## API overview

| Area | Items |
|---|---|
| Type and constants | `Decimal`, `Decimal::{ZERO, ONE, TEN, ONE_HUNDRED, MAX, MIN, MAX_SCALE}`, `MAX_SCALE`, `MAX_DIGITS` |
| Construction | `new(mantissa, scale)`, `from_parts(magnitude, negative, scale)`, `const fn from_i128_scale`, `From<i8..i64, u8..u64, isize, usize>`, `TryFrom<i128, u128, f64, f32>`, `FromStr`, `parse_rounded` |
| Conversion | `to_f64`, `to_i64`, `to_i128`, `Display`, `to_string_grouped(dp, group_sep, decimal_sep, mode)` |
| Inspection | `mantissa`, `scale`, `digits`, `is_zero`, `is_negative`, `is_positive`, `signum` |
| Arithmetic | `+ - * / %` and `+= -= *= /= %=` (also with `&Decimal` and `i32/i64/u32/u64/usize`), unary `-`, `checked_add/sub/mul/div/rem`, `div_round`, `checked_pow`, `abs`, `Sum`, `Product`, `checked_sum` |
| Rounding | `RoundingMode`, `round_dp`, `round` (half-up), `trunc`, `floor`, `ceil`, `round_to_multiple`, `rescale` / `with_scale`, `normalize`, `integer_part`, `fract` |
| Comparison | `Eq`, `Ord` (`min`, `max`, `clamp`), `Hash`, all by value |
| Money | `percent_of`, `split(n, dp)`, `allocate(ratios, dp)` |
| Errors | `Error::{InvalidFormat { offset }, Overflow, DivisionByZero, ScaleTooLarge, NotFinite}` |

## Rounding modes

| Mode | 2.5 | 1.5 | -2.5 | 2.4 | -2.6 | 2.345 → 2 dp | Python | C# `MidpointRounding` |
|---|---|---|---|---|---|---|---|---|
| `HalfEven` | 2 | 2 | -2 | 2 | -3 | 2.34 | `ROUND_HALF_EVEN` | `ToEven` |
| `HalfUp` | 3 | 2 | -3 | 2 | -3 | 2.35 | `ROUND_HALF_UP` | `AwayFromZero` |
| `HalfDown` | 2 | 1 | -2 | 2 | -3 | 2.34 | `ROUND_HALF_DOWN` | – |
| `Up` | 3 | 2 | -3 | 3 | -3 | 2.35 | `ROUND_UP` | – |
| `Down` | 2 | 1 | -2 | 2 | -2 | 2.34 | `ROUND_DOWN` | `ToZero` |
| `Ceiling` | 3 | 2 | -2 | 3 | -2 | 2.35 | `ROUND_CEILING` | `ToPositiveInfinity` |
| `Floor` | 2 | 1 | -3 | 2 | -3 | 2.34 | `ROUND_FLOOR` | `ToNegativeInfinity` |

"Up" and "Down" refer to the magnitude (away from / toward zero); "Ceiling" and "Floor"
to the number line. `HalfUp` is the mode of Excel `ROUND`.

## Error handling

Every fallible function returns `Result<_, dl_decimal::Error>` (or `Option` for
`checked_*`, like std integers). `Error` implements `Display` and `core::error::Error`.

```rust
use dl_decimal::{Decimal, Error};

match "12,5".parse::<Decimal>() {
    Err(Error::InvalidFormat { offset }) => assert_eq!(offset, 2),
    other => panic!("unexpected {other:?}"),
}
assert_eq!("1e38".parse::<Decimal>(), Err(Error::Overflow));
assert_eq!("0.00000000000000000000000000001".parse::<Decimal>(), Err(Error::ScaleTooLarge));
assert_eq!(Decimal::ONE.div_round(Decimal::ZERO, 2, dl_decimal::RoundingMode::HalfUp), Err(Error::DivisionByZero));
assert_eq!(Error::Overflow.to_string(), "decimal overflow: more than 38 digits needed");
```

| Variant | When |
|---|---|
| `InvalidFormat { offset }` | Not a decimal number; `offset` is the byte offset of the first unexpected byte, or the input length if it ended too early |
| `Overflow` | The integer part needs more than 38 digits, or a parsed/padded mantissa would exceed 38 digits |
| `DivisionByZero` | Division by zero, `split(0, _)`, `allocate` with ratios summing to 0 |
| `ScaleTooLarge` | More than 28 decimal places written or requested, or `split`/`allocate` of an amount with more decimals than `dp` |
| `NotFinite` | `TryFrom<f64>` of NaN or ±infinity |

Parsing and every `checked_*` / `Result` method never panic. The operators panic in
the same situations as the integer operators (see `# Panics` in the docs).

## Platform support

- Linux, macOS and Windows on any 64-bit or 32-bit target Rust supports.
- WebAssembly: `cargo build -p dl-decimal --target wasm32-unknown-unknown`.
- `#![no_std]` with `alloc` (needed for `to_string_grouped`, `split`, `allocate` and
  `Display` with padding). No platform-specific code, no `unsafe`, no dependencies.

## Performance

Measured with `cargo bench -p dl-decimal` on an Intel Core i7-9850H laptop (Windows,
x86-64, release profile with LTO). Each number is the better median of two runs; the
machine was running other builds at the same time, so expect variation of ±30%.

| Operation | Example | Time |
|---|---|---|
| Parse | `"1234567.89"` | 45 ns |
| Parse | 38 digits | 89 ns |
| Format into a reused `String` | `1234567.89` | 59 ns |
| Format into a reused `String` | 38 digits | 154 ns |
| `to_string` (includes one heap allocation) | `1234567.89` / 38 digits | 483 ns / 210 ns |
| `to_string_grouped(2, '.', ',', HalfUp)` | `1234567.89` | 485 ns |
| Add, same scale | `1234567.89 + 1234567.89` | 5.3 ns |
| Add, mixed scale | `1234567.89 + 3.5` | 13 ns |
| Add that must round | 38 digits + `1/3` | 178 ns |
| Multiply | `1234567.89 × 3.5` | 7.1 ns |
| Multiply that must round | 38 digits × `1/3` | 163 ns |
| Divide | `1234567.89 / 3.5` | 94 ns |
| Divide | 38 digits / `1/3` | 180 ns |
| `div_round(_, 2, HalfUp)` | `1234567.89 / 3.5` | 53 ns |
| `round_dp(2, HalfUp)` | `1/3` (scale 28) | 52 ns |
| `round_dp(0, HalfEven)` | `1234567.89` | 25 ns |
| Compare, mixed scale | `1234567.89 < 3.5` | 19 ns |

Money-sized values take `i128` fast paths; values that need rounding at 38 digits go
through a 256-bit intermediate. The `to_string` rows are dominated by the Windows heap
allocator; format into a reused buffer with `write!` in hot loops.

## Standards and references

- C# `System.Decimal`: same scale range (0–28), same "round half-even to the largest
  scale that fits" behaviour for `+ - *`, remainder sign rule, `MidpointRounding` names.
  The mantissa is 38 decimal digits here instead of 96 bits (~28.9 digits).
- Python `decimal` (IBM General Decimal Arithmetic specification): the reference for
  every expected value. `testdata/gen_vectors.py` computes 8,641 vectors (add, sub,
  mul, div, rem, rounding in every mode, `div_round`, `round_to_multiple`,
  `percent_of`, `checked_pow`, comparisons, strict and rounding parsing, `f64`
  conversions) with `decimal` under explicit contexts and cross-checks each one against
  an independent `fractions.Fraction` implementation and, where they apply, Python's
  own `prec=38, ROUND_HALF_EVEN, Emax=37` context. Regenerate with
  `python crates/dl-decimal/testdata/gen_vectors.py`.
- `f64` conversions use Rust's shortest round-trip formatting and correctly rounded
  parsing (IEEE 754 binary64, ties to even).

## Limitations and roadmap

- Range: 38 significant digits and at most 28 decimal places. Results that need more
  integer digits are errors; results that need more decimals are rounded.
- No NaN, infinity or negative zero (`-0` parses as `0`).
- `checked_pow` rounds at each step when the exact power does not fit, so the last
  digit can differ from a correctly rounded power. No `sqrt`, `ln`, `exp`.
- The parser does not accept locale formats (`1.234,5`, spaces, currency symbols);
  strip them first. A locale-aware parser is on the roadmap.
- No `serde` support (zero-dependency policy); use `Display`/`FromStr` or
  `mantissa()`/`scale()` to serialize.

## License

Licensed under either of MIT or Apache-2.0, at your option.
