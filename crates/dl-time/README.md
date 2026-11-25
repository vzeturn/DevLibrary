# dl-time

Dates, times, fixed UTC offsets and durations for Rust, with RFC 3339, ISO 8601, HTTP dates
and .NET-style custom patterns in English and Vietnamese. No dependencies, `no_std`
compatible, and builds for `wasm32-unknown-unknown`.

It is written for business software that runs in Vietnam (UTC+07:00): hotel PMS, POS,
attendance and invoicing, where you need correct calendar arithmetic, exact timestamps and
`dd/MM/yyyy` formatting rather than a time zone database.

## Replaces

| Ecosystem | Package / API |
|---|---|
| TypeScript / JavaScript | `date-fns`, `dayjs`, `moment` (formatting, parsing, add/diff) |
| C# / .NET | `DateTime`, `DateOnly`, `TimeOnly`, `DateTimeOffset`, `TimeSpan`, `ToString("dd/MM/yyyy")`, `ParseExact` |
| Python | `datetime.date`, `time`, `datetime`, `timezone`, `timedelta`, `isoformat`, `fromisoformat` |
| Go | `time.Time` with fixed zones, `time.Duration`, `http.TimeFormat` |
| Rust | the common subset of `time` and `chrono` with fixed offsets |

## Features

- `Date` (years -9999 to 9999, proleptic Gregorian), `Time` (nanoseconds), `DateTime`
  (wall clock), `Offset` (fixed, ±18:00), `OffsetDateTime` (an instant), signed `Duration`.
- Calendar arithmetic: add days, months (clamped to month end) and years; ISO weeks;
  ordinals; start of week; first and last day of month.
- Unix timestamps in seconds, milliseconds (JavaScript `Date.now()`) and nanoseconds.
- RFC 3339 / ISO 8601 parsing and formatting, HTTP dates (RFC 9110), ISO 8601 durations.
- .NET-style custom patterns (`dd/MM/yyyy HH:mm:ss`) for formatting and parsing, with
  `Locale::En` and `Locale::Vi`.
- Every fallible operation returns `Result` or `Option`; parsers report the byte offset of
  the error. No `unsafe`.

## Installation

From GitHub:

```toml
[dependencies]
dl-time = { git = "https://github.com/vzeturn/devlibrary" }
```

Inside the DevLibrary workspace or from a local checkout:

```toml
[dependencies]
dl-time = { path = "../DevLibrary/crates/dl-time" }
```

For `no_std` (needs `alloc`), turn off the default `std` feature. This only removes
`OffsetDateTime::now_utc()` and `OffsetDateTime::now()`:

```toml
[dependencies]
dl-time = { git = "https://github.com/vzeturn/devlibrary", default-features = false }
```

## Quick start

```rust
use dl_time::{Date, Duration, Locale, Offset, OffsetDateTime};

// Parse an API timestamp and show it to a Vietnamese user.
let t = OffsetDateTime::parse_rfc3339("2026-11-15T03:20:30Z")?;
let local = t.to_offset(Offset::VIETNAM);
assert_eq!(local.to_rfc3339(), "2026-11-15T10:20:30+07:00");
assert_eq!(local.format("dd/MM/yyyy HH:mm:ss")?, "15/11/2026 10:20:30");
assert_eq!(
    local.format_locale("dddd, 'ngày' dd/MM/yyyy", Locale::Vi)?,
    "Chủ Nhật, ngày 15/11/2026"
);

// Calendar arithmetic.
let invoice = Date::new(2026, 11, 15)?;
assert_eq!(invoice.add_days(30).to_string(), "2026-12-15");

// Unix timestamps and durations.
assert_eq!(t.unix_seconds(), 1_794_712_830);
assert_eq!((t + Duration::from_hours(8)).to_rfc3339(), "2026-11-15T11:20:30Z");
# Ok::<(), dl_time::Error>(())
```

## Usage guide

### Getting the current time

`now_utc()` and `now(offset)` read `std::time::SystemTime`. They need the `std` feature and
are not compiled for `wasm32-unknown-unknown`, which has no clock. In the browser, pass
JavaScript's `Date.now()` to `OffsetDateTime::from_unix_millis`.

```rust
use dl_time::{Offset, OffsetDateTime};

# #[cfg(all(feature = "std", not(all(target_arch = "wasm32", target_os = "unknown"))))]
# {
let now = OffsetDateTime::now(Offset::VIETNAM);
let today = now.date();
println!("{} {}", today, now.to_rfc3339());
# }

// Browser (wasm32): the value of Date.now() passed in from JavaScript.
let js_now: i64 = 1_794_712_830_120;
let t = OffsetDateTime::from_unix_millis(js_now)?.to_offset(Offset::VIETNAM);
assert_eq!(t.to_rfc3339(), "2026-11-15T10:20:30.120+07:00");
# Ok::<(), dl_time::Error>(())
```

### Hotel check-in and check-out

Nights are counted between calendar dates, so the clock time of arrival does not matter.

```rust
use dl_time::{DateTime, Duration, Time};

let check_in = DateTime::parse("15/11/2026 14:00", "dd/MM/yyyy HH:mm")?;
let check_out = DateTime::parse("18/11/2026 13:30", "dd/MM/yyyy HH:mm")?;

let nights = check_in.date().days_until(check_out.date());
assert_eq!(nights, 3);

// Late check-out after 12:00 costs extra.
let late = check_out.time() > Time::NOON;
assert!(late);
assert_eq!(check_out - check_out.start_of_day(), Duration::from_minutes(13 * 60 + 30));
# Ok::<(), dl_time::Error>(())
```

### Invoice due dates

`add_days` is exact. `add_months` clamps to the end of the month, like .NET `AddMonths` and
`date-fns` `addMonths`: January 31 + 1 month is February 28 (29 in leap years).

```rust
use dl_time::Date;

let issued = Date::parse("31/01/2026", "dd/MM/yyyy")?;
assert_eq!(issued.add_days(30), Date::new(2026, 3, 2)?);       // net 30
assert_eq!(issued.add_months(1), Date::new(2026, 2, 28)?);     // clamped
assert_eq!(issued.add_months(1).last_day_of_month(), Date::new(2026, 2, 28)?);
assert_eq!(Date::new(2024, 2, 29)?.add_years(1), Date::new(2025, 2, 28)?);

let today = Date::new(2026, 3, 5)?;
let overdue_days = issued.add_days(30).days_until(today);
assert_eq!(overdue_days, 3);
# Ok::<(), dl_time::Error>(())
```

### Shift schedules and attendance

`DateTime + Duration` crosses midnight and month or year ends. ISO weeks group timesheets.

```rust
use dl_time::{Date, Duration, Weekday};

let start = Date::new(2026, 12, 31)?.and_hms(22, 0, 0)?;   // night shift
let end = start + Duration::from_hours(8);
assert_eq!(end.to_string(), "2027-01-01T06:00:00");

let week_start = start.date().start_of_week(Weekday::Monday);
assert_eq!(week_start, Date::new(2026, 12, 28)?);
assert_eq!(start.date().iso_week(), (2026, 53));

// Worked time from punch-in / punch-out records.
let punch_in = Date::new(2026, 11, 16)?.and_hms(7, 58, 12)?;
let punch_out = Date::new(2026, 11, 16)?.and_hms(17, 3, 40)?;
let worked = punch_out - punch_in;
assert_eq!(worked.whole_minutes(), 545);
assert_eq!(worked.to_string(), "9h5m28s");
assert_eq!(worked.to_iso8601(), "PT9H5M28S");
# Ok::<(), dl_time::Error>(())
```

### Vietnamese formatting

`Locale::Vi` uses `Tháng 1` to `Tháng 12`, `Thứ Hai` to `Thứ Bảy`, `Chủ Nhật`, and `SA` / `CH`
for AM / PM. Literal words go in quotes. Parsing accepts the same names, ignoring case.

```rust
use dl_time::{DateTime, Locale};

let dt = DateTime::parse_iso("2026-11-16T15:05:00")?;
let vi = |p| dt.format_locale(p, Locale::Vi);
assert_eq!(vi("dddd, 'ngày' dd/MM/yyyy")?, "Thứ Hai, ngày 16/11/2026");
assert_eq!(vi("'Ngày' dd 'tháng' MM 'năm' yyyy")?, "Ngày 16 tháng 11 năm 2026");
assert_eq!(vi("hh:mm tt, ddd dd MMMM")?, "03:05 CH, T2 16 Tháng 11");

let parsed = DateTime::parse_locale("thứ hai, 16/11/2026 15:05", "dddd, dd/MM/yyyy HH:mm", Locale::Vi)?;
assert_eq!(parsed, dt);
# Ok::<(), dl_time::Error>(())
```

### Timestamps for APIs, logs and HTTP

```rust
use dl_time::{Offset, OffsetDateTime};

let t = OffsetDateTime::parse_rfc3339("2026-11-15T10:20:30.5+07:00")?;
assert_eq!(t.to_rfc3339(), "2026-11-15T10:20:30.500+07:00");   // 0, 3, 6 or 9 digits
assert_eq!(t.to_utc().to_string(), "2026-11-15T03:20:30.500Z");
assert_eq!(t.unix_millis(), 1_794_712_830_500);

// Equality compares instants, not fields.
assert_eq!(t, OffsetDateTime::parse_rfc3339("2026-11-15T03:20:30.5Z")?);

// HTTP Date / Last-Modified / Expires headers.
assert_eq!(t.to_http_date(), "Sun, 15 Nov 2026 03:20:30 GMT");
let h = OffsetDateTime::parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT")?;
assert_eq!(h.unix_seconds(), 784_111_777);
# Ok::<(), dl_time::Error>(())
```

### Durations

`Duration` is signed with nanosecond precision. ISO 8601 durations accept weeks, days, hours,
minutes and seconds. Years and months are rejected because their length varies; use
`Date::add_months` / `Date::add_years` instead.

```rust
use dl_time::Duration;

let d: Duration = "PT1H30M".parse()?;
assert_eq!(d, Duration::from_minutes(90));
assert_eq!("P2W".parse::<Duration>()?, Duration::from_days(14));
assert_eq!((-d).to_iso8601(), "-PT1H30M");
assert_eq!(Duration::from_millis(93_784_500).to_iso8601(), "P1DT2H3M4.5S");
assert!("P1M".parse::<Duration>().is_err());
assert_eq!(Duration::MAX.checked_add(Duration::SECOND), None);
# Ok::<(), dl_time::Error>(())
```

## Format tokens

Patterns follow .NET custom date and time format strings. `format` returns an error for an
unknown ASCII letter, so text such as `ngày` must be quoted.

| Token | Meaning | Example (2026-11-05 14:07:09.120 +07:00) |
|---|---|---|
| `yyyy` | Year, at least 4 digits (`-` for negative years) | `2026` |
| `yy` | Year modulo 100, 2 digits | `26` |
| `y`, `yyy`, `yyyyy` | Year mod 100 unpadded; year padded to 3 or 5 digits | `26`, `2026`, `02026` |
| `MM`, `M` | Month, 2 digits / no padding | `11` |
| `MMM`, `MMMM` | Abbreviated / full month name | `Nov`, `November` (Vi: `Thg 11`, `Tháng 11`) |
| `dd`, `d` | Day of month, 2 digits / no padding | `05`, `5` |
| `ddd`, `dddd` | Abbreviated / full weekday name | `Thu`, `Thursday` (Vi: `T5`, `Thứ Năm`) |
| `HH`, `H` | Hour 0–23, 2 digits / no padding | `14` |
| `hh`, `h` | Hour 1–12, 2 digits / no padding | `02`, `2` |
| `mm`, `m` | Minute | `07`, `7` |
| `ss`, `s` | Second | `09`, `9` |
| `f` … `fffffffff` | Fraction of a second, exactly 1–9 digits (truncated) | `fff` → `120` |
| `F` … `FFFFFFFFF` | Like `f` without trailing zeros; nothing (and no preceding `.`) when zero | `FFF` → `12` |
| `tt`, `t` | AM/PM designator, full / first letter | `PM`, `P` (Vi: `CH`, `C`) |
| `zzz` | Offset `±HH:MM` | `+07:00` |
| `zz`, `z` | Offset hours, 2 digits / no padding | `+07`, `+7` |
| `K` | `Z` for UTC, otherwise `±HH:MM` | `+07:00` |
| `'text'`, `"text"` | Literal text | `'ngày'` → `ngày` |
| `\c` | The single character `c` as a literal | `\h` → `h` |
| any other non-letter | Copied as is (`/`, `:`, `-`, `,`, spaces, non-ASCII) | |

Rules and differences from .NET:

- `/` and `:` are always literal (no culture-specific separators).
- A pattern can only use fields the value has: `HH` on a `Date` and `zzz` on a `DateTime`
  are `Error::InvalidPattern`, like `DateOnly` and `TimeOnly` in .NET.
- Parsing is exact: literals must match, `dd`/`MM`/`HH`/`yyyy` need exactly 2 or 4 digits,
  `d`/`M`/`H` accept 1 or 2. Names are matched case-insensitively (input must be NFC for
  Vietnamese). A parsed weekday must match the date.
- Two-digit years (`yy`) map 00–49 to 2000–2049 and 50–99 to 1950–1999, as .NET does.
- When parsing, the year is required, a missing month or day is 1, missing time fields are 0,
  `hh` without `tt` is AM, and `OffsetDateTime::parse` requires an offset token.

## API overview

| Type | Highlights |
|---|---|
| `Date` | `new`, `from_ordinal`, `from_iso_week`, `from_unix_days` / `to_unix_days`, `year` `month` `day` `ordinal` `weekday` `iso_week`, `is_leap_year` `days_in_month` `days_in_year`, `add_days` `add_months` `add_years` (+ `checked_*`), `first_day_of_month` `last_day_of_month` `start_of_week`, `succ` `pred`, `days_until`, `with_*`, `at` `and_hms` `midnight`, `parse_iso`, `format` `parse` |
| `Time` | `new`, `from_hms` `from_hms_milli` `from_hms_micro` `from_hms_nano`, `MIDNIGHT` `NOON` `MAX`, accessors, `seconds_from_midnight`, `nanos_of_day`, `with_*`, `parse_iso`, `format` `parse` |
| `DateTime` | `new`, accessors, `with_*`, `start_of_day`, `+`/`-` `Duration` (+ `checked_add` `checked_sub`), `-` → `Duration`, `duration_since`, `assume_offset` `assume_utc`, `parse_iso`, `format` `parse` |
| `Offset` | `UTC`, `VIETNAM`, `from_seconds`, `from_hms`, `whole_seconds` `whole_minutes` `whole_hours` `as_hms`, `Display` / `FromStr` |
| `OffsetDateTime` | `from_unix_seconds` `from_unix_millis` `from_unix_nanos`, `unix_seconds` `unix_millis` `unix_nanos`, `now_utc` `now`, `to_offset` `checked_to_offset` `to_utc`, local `date` `time` `datetime` and accessors, `+`/`-` `Duration`, `duration_since`, `parse_rfc3339` `to_rfc3339`, `parse_http_date` `to_http_date`, `format` `parse` |
| `Duration` | `from_weeks` … `from_nanos`, `ZERO` `SECOND` … `WEEK` `MIN` `MAX`, `whole_*`, `subsec_nanoseconds`, `as_seconds_f64`, `abs`, `checked_add` `checked_sub` `checked_mul` `checked_div`, operators, `to_iso8601` `parse_iso8601`, `TryFrom<core::time::Duration>` |
| `Month`, `Weekday` | numbers (`number`, `iso_number`, `days_from_monday`, `days_from_sunday`), English names, `succ` `pred`, `FromStr` |
| `Locale` | `En`, `Vi`; `month_name`, `weekday_name`, `am_pm` and abbreviations |
| `is_leap_year` | free function |

All types are `Copy`, `Eq`, `Ord`, `Hash` and `Debug`. `Display` writes ISO 8601 / RFC 3339
(`2026-11-15`, `10:20:30.500`, `2026-11-15T10:20:30`, `2026-11-15T10:20:30+07:00`) with the
shortest exact fraction of 0, 3, 6 or 9 digits, and `FromStr` parses the same forms.
`Duration` displays as `1d2h3m4.5s` and parses ISO 8601.

`OffsetDateTime` equality, ordering and hashing use the instant: `10:00+07:00 == 03:00Z`.
Compare `datetime()` and `offset()` if you need the fields to match too.

## Error handling

There is one error type, `dl_time::Error`:

| Variant | When |
|---|---|
| `InvalidComponent { name, value }` | A value out of range: month 13, February 30, second 60, offset +19:00 |
| `Parse { offset, expected }` | Input does not match the syntax; `offset` is the byte position |
| `InvalidPattern { offset, reason }` | Bad custom pattern, or a field the value does not have |
| `Overflow` | Result outside years -9999..=9999 or the `Duration` range |

```rust
use dl_time::{Date, Error, OffsetDateTime};

assert_eq!(Date::new(2026, 2, 30), Err(Error::InvalidComponent { name: "day", value: 30 }));
assert_eq!(
    OffsetDateTime::parse_rfc3339("2026-11-15 10:20"),
    Err(Error::Parse { offset: 16, expected: "':' after minute" })
);
// Pattern errors are reported before the input is read.
let e = Date::parse("15/11/2026", "dd/MM/yyyy ngày").unwrap_err();
assert_eq!(e.to_string(), "invalid format pattern at byte 11: unknown pattern letter (quote literal text)");
assert_eq!(Date::MAX.checked_add_days(1), None);
```

Arithmetic operators (`+`, `-` on dates, date-times and durations), `add_days`,
`add_months`, `succ`, `to_offset` and `Duration::from_days` and similar panic when the result
leaves the supported range, as integer overflow does. Each has a `checked_*` variant
returning `Option`; use those for values derived from user input. Parsers never panic.

## Platform support

| Platform | Status |
|---|---|
| Linux | Supported, tested in CI |
| macOS, Windows | Supported, tested in CI |
| `wasm32-unknown-unknown` | Supported, built in CI. `now_utc()` / `now()` are compiled out: pass `Date.now()` to `OffsetDateTime::from_unix_millis` |
| `no_std` + `alloc` | Supported with `default-features = false` (no `now()`) |

MSRV: Rust 1.88.

## Performance

Measured with `cargo bench -p dl-time` (in-house `dl-bench`, median of 30 samples,
release profile with fat LTO) on an Intel Core i7-9850H, Windows 11, Rust 1.100:

| Operation | Time |
|---|---|
| `parse_rfc3339` (`…30.123456789+07:00`) | 53 ns |
| `parse_rfc3339` (`…03:20:30Z`) | 29 ns |
| `to_rfc3339` (9-digit fraction, allocates) | 99 ns |
| `from_unix_seconds` | 20 ns |
| `unix_seconds` | 6 ns |
| `to_offset(Offset::VIETNAM)` | 37 ns |
| `to_http_date` | 123 ns |
| `parse_http_date` | 44 ns |
| `format("dd/MM/yyyy HH:mm:ss")` | 219 ns |
| `format_locale("dddd, 'ngày' dd/MM/yyyy", Vi)` | 189 ns |
| `DateTime::parse(…, "dd/MM/yyyy HH:mm:ss")` | 148 ns |
| `Date::add_months` | 6 ns |
| `Date::iso_week` | 14 ns |
| `Duration::parse_iso8601("P1DT2H3M4.5S")` | 68 ns |

Run `cargo bench -p dl-time -- rfc3339` to filter by name.

## Standards

- [RFC 3339](https://www.rfc-editor.org/rfc/rfc3339): date and time on the Internet.
  Leap seconds (`:60`) are rejected because they cannot be represented; at most 9 fraction
  digits; `-00:00` is read as UTC; `T`, `t` or a space separate date and time.
- [ISO 8601](https://www.iso.org/iso-8601-date-and-time-format.html): calendar dates,
  ordinal dates, week dates (`iso_week`, `from_iso_week`) and durations (`PnWnDTnHnMnS`).
  Years before 1 are proleptic Gregorian with a year 0, written `-YYYY`.
- [RFC 9110 §5.6.7](https://www.rfc-editor.org/rfc/rfc9110#section-5.6.7): HTTP dates.
  `to_http_date` writes IMF-fixdate; `parse_http_date` also reads RFC 850 and `asctime`.
- Calendar algorithms: Howard Hinnant,
  [`chrono`-Compatible Low-Level Date Algorithms](https://howardhinnant.github.io/date_algorithms.html).
- Custom patterns: [.NET custom date and time format strings](https://learn.microsoft.com/dotnet/standard/base-types/custom-date-and-time-format-strings).

Tests check every day from -9999-01-01 to 9999-12-31, 500 dates and 400 Unix timestamps
against Python's `datetime`, and the RFC 3339 / RFC 9110 examples.

## Limitations and roadmap

- **Fixed offsets only.** There is no IANA time zone database yet, so no daylight saving
  rules (`Europe/Berlin`, `America/New_York`). Vietnam has no DST, so `Offset::VIETNAM` is
  exact for current dates. A `tz` module reading the IANA database is on the roadmap.
- No leap seconds: like Unix time, every day has 86 400 seconds.
- Years are limited to -9999..=9999. RFC 3339 output is only valid RFC 3339 for years
  0000–9999 and whole-minute offsets.
- Two locales (`En`, `Vi`). `Locale` is `#[non_exhaustive]` so more can be added.
- ISO 8601 durations with years or months are rejected (no fixed length).
- Two-digit years use a fixed 2049 pivot rather than the current date.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0) at your option.
