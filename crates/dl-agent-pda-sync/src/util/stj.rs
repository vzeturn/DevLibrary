//! JSON output byte-identical to .NET's `System.Text.Json` (`Utf8JsonWriter`), which the
//! C# agent and the cloud use: the default encoder's escaping (everything outside
//! printable ASCII plus `"&'+<>\`` as `\uXXXX` with upper-case hex), the relaxed encoder
//! used by the log formatter, `double` and `DateTimeOffset` text, and indented output
//! (two spaces, `"key": value`, the platform newline).

use dl_json::Value;

/// Which `JavaScriptEncoder` the C# side writes with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Escaping {
    /// `JavaScriptEncoder.Default` (no encoder configured): only printable ASCII except
    /// `"`, `&`, `'`, `+`, `<`, `>`, `` ` `` is written as is; `"` becomes `\u0022`.
    Default,
    /// `JavaScriptEncoder.UnsafeRelaxedJsonEscaping`: BMP text as is; `"` and `\` with a
    /// backslash; control characters, U+2028/U+2029 and characters outside the BMP escaped.
    Relaxed,
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn push_u(out: &mut String, unit: u16) {
    out.push_str("\\u");
    for shift in [12, 8, 4, 0] {
        out.push(HEX[usize::from((unit >> shift) & 0xF)] as char);
    }
}

fn push_escaped_char(out: &mut String, c: char) {
    let mut units = [0u16; 2];
    for u in c.encode_utf16(&mut units) {
        push_u(out, *u);
    }
}

fn relaxed_escapes(c: char) -> bool {
    let cp = u32::from(c);
    if cp < 0x20 || cp == 0x7F {
        return true;
    }
    if cp < 0x80 {
        return false;
    }
    let Ok(u) = u16::try_from(cp) else {
        // Outside the BMP: always written as a surrogate pair escape.
        return true;
    };
    let table = super::stj_relaxed::RELAXED_ESCAPED;
    table
        .binary_search_by(|&(lo, hi)| {
            if hi < u {
                std::cmp::Ordering::Less
            } else if lo > u {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Appends `s` as a JSON string literal (with quotes).
pub fn write_str(out: &mut String, s: &str, escaping: Escaping) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '"' if escaping == Escaping::Relaxed => out.push_str("\\\""),
            _ => {
                let escape = match escaping {
                    Escaping::Default => {
                        !(' '..='\u{7e}').contains(&c)
                            || matches!(c, '"' | '&' | '\'' | '+' | '<' | '>' | '`')
                    }
                    Escaping::Relaxed => relaxed_escapes(c),
                };
                if escape {
                    push_escaped_char(out, c);
                } else {
                    out.push(c);
                }
            }
        }
    }
    out.push('"');
}

/// A JSON string literal with the default encoder.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    write_str(&mut out, s, Escaping::Default);
    out
}

/// `double` as `Utf8JsonWriter.WriteNumberValue(double)` writes it (.NET Core 3.0+
/// shortest round-trip text): fixed notation for decimal exponents -4..=16, otherwise
/// `d.dddE+XX`. `None` for NaN and infinities, which System.Text.Json refuses to write.
pub fn f64_text(v: f64) -> Option<String> {
    if !v.is_finite() {
        return None;
    }
    if v == 0.0 {
        return Some(if v.is_sign_negative() { "-0" } else { "0" }.to_owned());
    }
    // Rust's `{:e}` is the shortest round-trip digit string, like .NET's "R".
    let sci = format!("{:e}", v.abs());
    let (mantissa, exp) = sci.split_once('e')?;
    let exp: i32 = exp.parse().ok()?;
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let mut out = String::new();
    if v < 0.0 {
        out.push('-');
    }
    // .NET: scientific when the scale (exp + 1) exceeds max(digits, 17) or is below -3.
    if (-4..17).contains(&exp) {
        if exp < 0 {
            out.push_str("0.");
            for _ in 0..(-exp - 1) {
                out.push('0');
            }
            out.push_str(&digits);
        } else {
            let int_len = usize::try_from(exp).ok()? + 1;
            if digits.len() <= int_len {
                out.push_str(&digits);
                for _ in digits.len()..int_len {
                    out.push('0');
                }
            } else {
                out.push_str(&digits[..int_len]);
                out.push('.');
                out.push_str(&digits[int_len..]);
            }
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('E');
        out.push(if exp < 0 { '-' } else { '+' });
        let a = exp.unsigned_abs();
        if a < 10 {
            out.push('0');
        }
        out.push_str(&a.to_string());
    }
    Some(out)
}

/// Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `yyyy-MM-ddTHH:mm:ss[.FFFFFFF]` of a wall-clock instant given as nanoseconds since
/// 1970-01-01T00:00:00 (sub-tick digits are dropped, as .NET ticks are 100 ns).
fn wall_clock(out: &mut String, wall_nanos: i128) {
    let secs = wall_nanos.div_euclid(1_000_000_000) as i64;
    let ticks = (wall_nanos.rem_euclid(1_000_000_000) / 100) as u32;
    let (y, mo, d) = civil(secs.div_euclid(86_400));
    let sod = secs.rem_euclid(86_400);
    out.push_str(&format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}",
        sod / 3600,
        (sod / 60) % 60,
        sod % 60
    ));
    if ticks != 0 {
        let frac = format!("{ticks:07}");
        out.push('.');
        out.push_str(frac.trim_end_matches('0'));
    }
}

/// A `DateTimeOffset` as System.Text.Json writes it: local time at `offset_minutes`,
/// trailing zero fraction digits trimmed, offset as `+HH:mm` (`+00:00` for UTC).
pub fn date_time_offset(unix_nanos: i128, offset_minutes: i16) -> String {
    let mut out = String::with_capacity(33);
    wall_clock(
        &mut out,
        unix_nanos + i128::from(offset_minutes) * 60_000_000_000,
    );
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let a = offset_minutes.unsigned_abs();
    out.push_str(&format!("{sign}{:02}:{:02}", a / 60, a % 60));
    out
}

/// A `DateTime` of kind UTC as System.Text.Json writes it (`...Z`).
pub fn date_time_utc(unix_nanos: i128) -> String {
    let mut out = String::with_capacity(28);
    wall_clock(&mut out, unix_nanos);
    out.push('Z');
    out
}

/// A `DateTime` of kind Unspecified (no suffix); `wall_nanos` counts from
/// 1970-01-01T00:00:00 on the same wall clock.
pub fn date_time_unspecified(wall_nanos: i128) -> String {
    let mut out = String::with_capacity(27);
    wall_clock(&mut out, wall_nanos);
    out
}

/// Layout of a [`Writer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// No whitespace (`JsonSerializer` default).
    Compact,
    /// `WriteIndented = true`: two spaces per level and the platform newline
    /// (`Environment.NewLine`: CRLF on Windows, LF elsewhere).
    Indented,
}

/// The newline `Utf8JsonWriter` uses by default on this platform.
pub const NEWLINE: &str = if cfg!(windows) { "\r\n" } else { "\n" };

/// A streaming JSON writer with System.Text.Json's layout and escaping.
///
/// Calls must form valid JSON (a key before each object member value); the writer does
/// not validate.
#[derive(Debug)]
pub struct Writer {
    out: String,
    style: Style,
    escaping: Escaping,
    /// One entry per open container: whether it already has an item.
    stack: Vec<bool>,
    after_key: bool,
}

impl Writer {
    /// A writer with the default encoder.
    pub fn new(style: Style) -> Writer {
        Writer::with_escaping(style, Escaping::Default)
    }

    /// A writer with an explicit encoder.
    pub fn with_escaping(style: Style, escaping: Escaping) -> Writer {
        Writer {
            out: String::new(),
            style,
            escaping,
            stack: Vec::new(),
            after_key: false,
        }
    }

    fn newline_indent(&mut self) {
        self.out.push_str(NEWLINE);
        for _ in 0..self.stack.len() {
            self.out.push_str("  ");
        }
    }

    /// Separator and indentation before a value or a key.
    fn before_item(&mut self) {
        if self.after_key {
            self.after_key = false;
            return;
        }
        if let Some(has) = self.stack.last_mut() {
            let had = *has;
            *has = true;
            if had {
                self.out.push(',');
            }
            if self.style == Style::Indented {
                self.newline_indent();
            }
        }
    }

    /// An object member name; the next call writes its value.
    pub fn key(&mut self, name: &str) -> &mut Self {
        self.before_item();
        write_str(&mut self.out, name, self.escaping);
        self.out.push(':');
        if self.style == Style::Indented {
            self.out.push(' ');
        }
        self.after_key = true;
        self
    }

    fn open(&mut self, c: char) -> &mut Self {
        self.before_item();
        self.out.push(c);
        self.stack.push(false);
        self
    }

    fn close(&mut self, c: char) -> &mut Self {
        let had = self.stack.pop().unwrap_or(false);
        if had && self.style == Style::Indented {
            self.newline_indent();
        }
        self.out.push(c);
        self
    }

    /// `{`.
    pub fn begin_object(&mut self) -> &mut Self {
        self.open('{')
    }

    /// `}`.
    pub fn end_object(&mut self) -> &mut Self {
        self.close('}')
    }

    /// `[`.
    pub fn begin_array(&mut self) -> &mut Self {
        self.open('[')
    }

    /// `]`.
    pub fn end_array(&mut self) -> &mut Self {
        self.close(']')
    }

    /// A string value.
    pub fn string(&mut self, s: &str) -> &mut Self {
        self.before_item();
        write_str(&mut self.out, s, self.escaping);
        self
    }

    /// A string value written without the encoder, the way `Utf8JsonWriter` writes
    /// `DateTime`, `DateTimeOffset` and `Guid` values: the `+` of an offset stays `+`
    /// (a [`Writer::string`] would write `\u002B`). Only for such ASCII texts without
    /// `"` or `\`.
    pub fn unescaped(&mut self, s: &str) -> &mut Self {
        self.before_item();
        self.out.push('"');
        self.out.push_str(s);
        self.out.push('"');
        self
    }

    /// A `DateTimeOffset` value (see [`date_time_offset`]), written like
    /// `Utf8JsonWriter.WriteStringValue(DateTimeOffset)`.
    pub fn date_time_offset(&mut self, unix_nanos: i128, offset_minutes: i16) -> &mut Self {
        self.unescaped(&date_time_offset(unix_nanos, offset_minutes))
    }

    /// An integer value.
    pub fn int(&mut self, n: i64) -> &mut Self {
        self.before_item();
        self.out.push_str(&n.to_string());
        self
    }

    /// An unsigned integer value.
    pub fn uint(&mut self, n: u64) -> &mut Self {
        self.before_item();
        self.out.push_str(&n.to_string());
        self
    }

    /// A `double` value; NaN and infinities are written as `null` (System.Text.Json
    /// would throw: callers must not pass them).
    pub fn f64(&mut self, v: f64) -> &mut Self {
        self.before_item();
        self.out.push_str(f64_text(v).as_deref().unwrap_or("null"));
        self
    }

    /// A boolean value.
    pub fn bool(&mut self, b: bool) -> &mut Self {
        self.before_item();
        self.out.push_str(if b { "true" } else { "false" });
        self
    }

    /// `null`.
    pub fn null(&mut self) -> &mut Self {
        self.before_item();
        self.out.push_str("null");
        self
    }

    /// Already encoded JSON text for one value, written as is (`WriteRawValue`).
    pub fn raw(&mut self, json: &str) -> &mut Self {
        self.before_item();
        self.out.push_str(json);
        self
    }

    /// A parsed value the way `JsonElement.WriteTo` re-emits it: member order kept,
    /// numbers as their source text (parse with `keep_raw_numbers`), strings re-escaped
    /// with this writer's encoder, laid out in this writer's style.
    pub fn value(&mut self, v: &Value) -> &mut Self {
        match v {
            Value::Null => self.null(),
            Value::Bool(b) => self.bool(*b),
            Value::Number(n) => {
                let text = n
                    .as_raw_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| n.to_string());
                self.raw(&text)
            }
            Value::String(s) => self.string(s),
            Value::Array(items) => {
                self.begin_array();
                for item in items {
                    self.value(item);
                }
                self.end_array()
            }
            Value::Object(m) => {
                self.begin_object();
                for (k, item) in m.iter() {
                    self.key(k);
                    self.value(item);
                }
                self.end_object()
            }
        }
    }

    /// Optional string member, skipped when `None` (`WhenWritingNull`).
    pub fn opt_str_member(&mut self, name: &str, v: Option<&str>) -> &mut Self {
        if let Some(s) = v {
            self.key(name).string(s);
        }
        self
    }

    /// Optional integer member, skipped when `None` (`WhenWritingNull`).
    pub fn opt_int_member(&mut self, name: &str, v: Option<i64>) -> &mut Self {
        if let Some(n) = v {
            self.key(name).int(n);
        }
        self
    }

    /// The text written so far.
    pub fn as_str(&self) -> &str {
        &self.out
    }

    /// The finished text.
    pub fn finish(self) -> String {
        self.out
    }
}

impl Writer {
    /// A `decimal` value as `Utf8JsonWriter.WriteNumberValue(decimal)` writes it: the
    /// invariant text with its scale kept (`1.50`, `100000.00`, `0.0`).
    pub fn decimal(&mut self, d: &dl_decimal::Decimal) -> &mut Self {
        self.raw(&d.to_string())
    }
}

/// `float` (`System.Single`) as .NET Core 3.0+ `ToString("R")` / `Utf8JsonWriter` write
/// it: shortest round-trip digits, fixed notation for decimal exponents -4..=8, otherwise
/// `d.dddE+XX`. `None` for NaN and infinities.
pub fn f32_text(v: f32) -> Option<String> {
    if !v.is_finite() {
        return None;
    }
    if v == 0.0 {
        return Some(if v.is_sign_negative() { "-0" } else { "0" }.to_owned());
    }
    let sci = format!("{:e}", v.abs());
    shortest_text(&sci, v < 0.0, 9)
}

/// Formats Rust `{:e}` output (`d.ddde±x`) the way .NET's "R" does, with fixed notation
/// for decimal exponents `-4..precision`.
fn shortest_text(sci: &str, negative: bool, precision: i32) -> Option<String> {
    let (mantissa, exp) = sci.split_once('e')?;
    let exp: i32 = exp.parse().ok()?;
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if (-4..precision).contains(&exp) {
        if exp < 0 {
            out.push_str("0.");
            for _ in 0..(-exp - 1) {
                out.push('0');
            }
            out.push_str(&digits);
        } else {
            let int_len = usize::try_from(exp).ok()? + 1;
            if digits.len() <= int_len {
                out.push_str(&digits);
                for _ in digits.len()..int_len {
                    out.push('0');
                }
            } else {
                out.push_str(&digits[..int_len]);
                out.push('.');
                out.push_str(&digits[int_len..]);
            }
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('E');
        out.push(if exp < 0 { '-' } else { '+' });
        let a = exp.unsigned_abs();
        if a < 10 {
            out.push('0');
        }
        out.push_str(&a.to_string());
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------
// Reading: `JsonSerializer.Deserialize` semantics.
// ---------------------------------------------------------------------------------------

/// A document or value System.Text.Json refuses (`JsonException`); `path` is the JSON
/// path of the offending value (`$.rows[0].key`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadError {
    /// JSON path of the value, `$` for the root.
    pub path: String,
    /// What was wrong.
    pub message: String,
}

impl ReadError {
    /// An error at `path`.
    pub fn new(path: &str, message: impl Into<String>) -> ReadError {
        ReadError {
            path: path.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} Path: {}", self.message, self.path)
    }
}

impl std::error::Error for ReadError {}

/// Property naming policy of a `JsonSerializerOptions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Naming {
    /// `JsonKnownNamingPolicy.SnakeCaseLower` (the wire contexts): `src_ver`.
    SnakeLower,
    /// `JsonNamingPolicy.CamelCase` (`JsonSerializerDefaults.Web`): `srcVer`.
    Camel,
}

impl Naming {
    /// The member name for a snake_case name under this policy.
    pub fn name(self, snake: &str) -> std::borrow::Cow<'_, str> {
        match self {
            Naming::SnakeLower => std::borrow::Cow::Borrowed(snake),
            Naming::Camel => {
                let mut out = String::with_capacity(snake.len());
                let mut upper = false;
                for c in snake.chars() {
                    if c == '_' {
                        upper = true;
                    } else if upper {
                        out.extend(c.to_uppercase());
                        upper = false;
                    } else {
                        out.push(c);
                    }
                }
                std::borrow::Cow::Owned(out)
            }
        }
    }
}

/// The reading half of a `JsonSerializerOptions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadOptions {
    /// Member naming policy.
    pub naming: Naming,
    /// `PropertyNameCaseInsensitive` (the last matching member wins).
    pub case_insensitive: bool,
    /// `JsonNumberHandling.AllowReadingFromString` (`"5"` reads as 5).
    pub numbers_from_strings: bool,
}

/// The source-generated wire contexts (`SnakeCaseLower`, case-sensitive, strict numbers).
pub const SNAKE: ReadOptions = ReadOptions {
    naming: Naming::SnakeLower,
    case_insensitive: false,
    numbers_from_strings: false,
};

/// `new JsonSerializerOptions(JsonSerializerDefaults.Web)`: camelCase, case-insensitive,
/// numbers also read from strings.
pub const WEB: ReadOptions = ReadOptions {
    naming: Naming::Camel,
    case_insensitive: true,
    numbers_from_strings: true,
};

/// `JsonSerializerOptions.MaxDepth` default.
pub const MAX_DEPTH: usize = 64;

/// Parses a document like `JsonSerializer.Deserialize(string)`: strict RFC 8259, nesting
/// up to 64, numbers kept as their source text (for `decimal` and `JsonElement`), and a
/// leading byte order mark refused (System.Text.Json reads it as an invalid token).
pub fn parse(json: &str) -> Result<Value, ReadError> {
    if json.starts_with('\u{feff}') {
        return Err(ReadError::new(
            "$",
            "'0xEF' is an invalid start of a value.",
        ));
    }
    dl_json::parse_with(
        json,
        dl_json::ParseOptions {
            max_depth: MAX_DEPTH,
            keep_raw_numbers: true,
        },
    )
    .map_err(|e| ReadError::new("$", e.to_string()))
}

/// The source text of a number token (raw when parsed with [`parse`]).
fn number_text(n: &dl_json::Number) -> String {
    n.as_raw_str()
        .map(str::to_owned)
        .unwrap_or_else(|| n.to_string())
}

/// The number text of `v`: a number token, or a string under `numbers_from_strings`.
fn numeric_text(v: &Value, opts: ReadOptions, path: &str) -> Result<String, ReadError> {
    match v {
        Value::Number(n) => Ok(number_text(n)),
        Value::String(s) if opts.numbers_from_strings => Ok(s.clone()),
        _ => Err(ReadError::new(
            path,
            format!("The JSON value could not be converted ({}).", v.type_name()),
        )),
    }
}

/// `-?digits` as an integer (System.Text.Json refuses fractions and exponents).
fn parse_integer(text: &str) -> Option<i128> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || digits.len() > 30 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A JSON integer that fits `T` (`Int32`, `Int64`, ...).
pub fn read_int<T: TryFrom<i128>>(
    v: &Value,
    opts: ReadOptions,
    path: &str,
) -> Result<T, ReadError> {
    let text = numeric_text(v, opts, path)?;
    parse_integer(&text)
        .and_then(|n| T::try_from(n).ok())
        .ok_or_else(|| ReadError::new(path, format!("'{text}' is not a valid integer here.")))
}

/// A JSON number as `double` (`1e400` reads as infinity, as in .NET).
pub fn read_f64(v: &Value, opts: ReadOptions, path: &str) -> Result<f64, ReadError> {
    let text = numeric_text(v, opts, path)?;
    if !is_json_number(&text) {
        return Err(ReadError::new(path, format!("'{text}' is not a number.")));
    }
    text.parse()
        .map_err(|_| ReadError::new(path, format!("'{text}' is not a number.")))
}

fn is_json_number(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if b.first() == Some(&b'-') {
        i = 1;
    }
    let start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == start || (b[start] == b'0' && i - start > 1) {
        return false;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let f = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == f {
            return false;
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let e = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == e {
            return false;
        }
    }
    i == b.len()
}

/// The largest `System.Decimal` mantissa (2^96 - 1).
const DOTNET_DECIMAL_MAX: u128 = (1u128 << 96) - 1;

/// Fits a decimal into `System.Decimal` (96-bit mantissa, scale 0..=28): extra digits
/// are rounded half-even, as `decimal.Parse` does; `None` when the integer part is too
/// large.
pub fn fit_dotnet_decimal(d: dl_decimal::Decimal) -> Option<dl_decimal::Decimal> {
    let mut d = d;
    while d.mantissa().unsigned_abs() > DOTNET_DECIMAL_MAX {
        if d.scale() == 0 {
            return None;
        }
        d = d.round_dp(d.scale() - 1, dl_decimal::RoundingMode::HalfEven);
    }
    Some(d)
}

/// Decimal text in JSON number syntax (or any `[+-]digits[.digits][e[+-]digits]` when
/// `lenient`) as `System.Decimal`; `None` when it is not a number or overflows.
pub fn parse_dotnet_decimal(text: &str) -> Option<dl_decimal::Decimal> {
    let d = dl_decimal::Decimal::parse_rounded(text, dl_decimal::RoundingMode::HalfEven).ok()?;
    fit_dotnet_decimal(d)
}

/// A JSON number as `decimal` (scale kept; exponents allowed; overflow refused).
pub fn read_decimal(
    v: &Value,
    opts: ReadOptions,
    path: &str,
) -> Result<dl_decimal::Decimal, ReadError> {
    let text = numeric_text(v, opts, path)?;
    if !is_json_number(&text) {
        return Err(ReadError::new(path, format!("'{text}' is not a number.")));
    }
    parse_dotnet_decimal(&text)
        .ok_or_else(|| ReadError::new(path, format!("'{text}' does not fit a decimal.")))
}

/// `true` / `false` (strings are refused).
pub fn read_bool(v: &Value, path: &str) -> Result<bool, ReadError> {
    v.as_bool()
        .ok_or_else(|| ReadError::new(path, "The JSON value could not be converted to Boolean."))
}

/// A string, `None` for JSON null (any other token is refused).
pub fn read_opt_string(v: &Value, path: &str) -> Result<Option<String>, ReadError> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        _ => Err(ReadError::new(
            path,
            format!(
                "Cannot get the value of a token type '{}' as a string.",
                v.type_name()
            ),
        )),
    }
}

/// A `Guid`: exactly the 36-character `"D"` form, any case.
pub fn read_guid(v: &Value, path: &str) -> Result<dl_uuid::Uuid, ReadError> {
    match v {
        Value::String(s) if s.len() == 36 => dl_uuid::Uuid::parse(s)
            .map_err(|_| ReadError::new(path, "The JSON value is not in a supported Guid format.")),
        _ => Err(ReadError::new(
            path,
            "The JSON value is not in a supported Guid format.",
        )),
    }
}

/// The offset part of an ISO 8601 date and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsoOffset {
    /// No offset (local time).
    None,
    /// `Z`.
    Utc,
    /// `+HH:mm` / `-HH:mm` / `+HH`, in minutes.
    Minutes(i16),
}

/// An ISO 8601 date and time as System.Text.Json reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsoDateTime {
    /// The written wall clock as 100 ns ticks since 0001-01-01T00:00:00.
    pub ticks: i64,
    /// The written offset.
    pub offset: IsoOffset,
}

/// Days from 0001-01-01 to the given proleptic Gregorian date (`None` if invalid).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> Option<i64> {
    if !(1..=9999).contains(&y) || !(1..=12).contains(&m) || d == 0 {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if d > dim[(m - 1) as usize] {
        return None;
    }
    let yy = y - 1;
    let mut days = yy * 365 + yy / 4 - yy / 100 + yy / 400;
    for len in dim.iter().take((m - 1) as usize) {
        days += i64::from(*len);
    }
    Some(days + i64::from(d) - 1)
}

/// Parses the ISO 8601-1:2019 extended profile System.Text.Json accepts:
/// `yyyy-MM-dd`, `yyyy-MM-ddTHH:mm`, `yyyy-MM-ddTHH:mm:ss[.fffffff…]`, each time
/// optionally followed by `Z`, `±HH:mm` or `±HH`. `T` and `Z` are upper case only; more
/// than 7 fraction digits are truncated (at most 16 accepted); offsets beyond ±14:00 are
/// refused.
pub fn parse_iso_date_time(s: &str) -> Option<IsoDateTime> {
    let b = s.as_bytes();
    if b.len() < 10 || b.len() > 42 {
        return None;
    }
    let num = |from: usize, len: usize| -> Option<u32> {
        let part = b.get(from..from + len)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        Some(part.iter().fold(0u32, |a, c| a * 10 + u32::from(c - b'0')))
    };
    let year = num(0, 4)?;
    if b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let month = num(5, 2)?;
    let day = num(8, 2)?;
    let days = days_from_civil(i64::from(year), month, day)?;
    let mut ticks = days * 864_000_000_000;
    let mut i = 10;
    let mut offset = IsoOffset::None;
    if i < b.len() {
        if b[i] != b'T' {
            return None;
        }
        let hour = num(i + 1, 2)?;
        if b.get(i + 3) != Some(&b':') {
            return None;
        }
        let minute = num(i + 4, 2)?;
        i += 6;
        let mut second = 0;
        let mut frac_ticks = 0i64;
        if b.get(i) == Some(&b':') {
            second = num(i + 1, 2)?;
            i += 3;
            if b.get(i) == Some(&b'.') {
                i += 1;
                let start = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                let digits = &b[start..i];
                if digits.len() > 16 {
                    return None;
                }
                let mut f = 0i64;
                for k in 0..7 {
                    f = f * 10 + digits.get(k).map_or(0, |c| i64::from(c - b'0'));
                }
                frac_ticks = f;
            }
        }
        if hour > 23 || minute > 59 || second > 59 {
            return None;
        }
        ticks += (i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second)) * 10_000_000
            + frac_ticks;
        if i < b.len() {
            match b[i] {
                b'Z' => {
                    offset = IsoOffset::Utc;
                    i += 1;
                }
                b'+' | b'-' => {
                    let neg = b[i] == b'-';
                    let oh = num(i + 1, 2)?;
                    i += 3;
                    let mut om = 0;
                    if i < b.len() {
                        if b[i] != b':' {
                            return None;
                        }
                        om = num(i + 1, 2)?;
                        i += 3;
                    }
                    if om > 59 {
                        return None;
                    }
                    let total = i16::try_from(oh * 60 + om).ok()?;
                    if total > 14 * 60 {
                        return None;
                    }
                    offset = IsoOffset::Minutes(if neg { -total } else { total });
                }
                _ => return None,
            }
        }
        if i != b.len() {
            return None;
        }
    }
    Some(IsoDateTime { ticks, offset })
}

/// Reads JSON objects with a [`ReadOptions`]: member lookup by snake_case name (mapped
/// through the naming policy, case-insensitive when the options say so) and typed
/// getters with System.Text.Json's record rules: a missing member takes the default,
/// `null` for a non-nullable value type is an error, unknown members are ignored.
#[derive(Debug, Clone)]
pub struct Obj<'a> {
    map: &'a dl_json::Map,
    opts: ReadOptions,
    path: String,
}

impl<'a> Obj<'a> {
    /// `None` for JSON null (a null object reference); an error for any other non-object.
    pub fn of(v: &'a Value, opts: ReadOptions, path: &str) -> Result<Option<Obj<'a>>, ReadError> {
        match v {
            Value::Null => Ok(None),
            Value::Object(map) => Ok(Some(Obj {
                map,
                opts,
                path: path.to_owned(),
            })),
            _ => Err(ReadError::new(
                path,
                format!(
                    "The JSON value could not be converted (found {}).",
                    v.type_name()
                ),
            )),
        }
    }

    /// Like [`Obj::of`] but JSON null is an error too (a top-level DTO).
    pub fn require(v: &'a Value, opts: ReadOptions, path: &str) -> Result<Obj<'a>, ReadError> {
        Obj::of(v, opts, path)?.ok_or_else(|| ReadError::new(path, "The JSON value is null."))
    }

    /// The options this reader was made with.
    pub fn options(&self) -> ReadOptions {
        self.opts
    }

    /// The path of a member.
    pub fn path_of(&self, snake: &str) -> String {
        format!("{}.{}", self.path, self.opts.naming.name(snake))
    }

    /// The member named `snake` (under the naming policy), if present.
    pub fn get(&self, snake: &str) -> Option<&'a Value> {
        let name = self.opts.naming.name(snake);
        if self.opts.case_insensitive {
            let mut found = None;
            for (k, v) in self.map.iter() {
                if eq_ignore_case(k, &name) {
                    found = Some(v);
                }
            }
            found
        } else {
            self.map.get(&name)
        }
    }

    /// The member, `None` when absent or JSON null.
    pub fn get_non_null(&self, snake: &str) -> Option<&'a Value> {
        self.get(snake).filter(|v| !v.is_null())
    }

    /// An integer member (`Int32`, `Int64`, ...): 0 when missing, error when null.
    pub fn int<T: TryFrom<i128> + Default>(&self, snake: &str) -> Result<T, ReadError> {
        self.int_or(snake, T::default())
    }

    /// An integer member with a constructor default for a missing member.
    pub fn int_or<T: TryFrom<i128>>(&self, snake: &str, default: T) -> Result<T, ReadError> {
        match self.get(snake) {
            None => Ok(default),
            Some(v) => read_int(v, self.opts, &self.path_of(snake)),
        }
    }

    /// A nullable integer member (`int?`, `long?`).
    pub fn opt_int<T: TryFrom<i128>>(&self, snake: &str) -> Result<Option<T>, ReadError> {
        match self.get_non_null(snake) {
            None => Ok(None),
            Some(v) => read_int(v, self.opts, &self.path_of(snake)).map(Some),
        }
    }

    /// A `double` member (0 when missing).
    pub fn f64(&self, snake: &str) -> Result<f64, ReadError> {
        match self.get(snake) {
            None => Ok(0.0),
            Some(v) => read_f64(v, self.opts, &self.path_of(snake)),
        }
    }

    /// A `decimal` member (0 when missing).
    pub fn decimal(&self, snake: &str) -> Result<dl_decimal::Decimal, ReadError> {
        match self.get(snake) {
            None => Ok(dl_decimal::Decimal::ZERO),
            Some(v) => read_decimal(v, self.opts, &self.path_of(snake)),
        }
    }

    /// A `bool` member (false when missing).
    pub fn bool(&self, snake: &str) -> Result<bool, ReadError> {
        match self.get(snake) {
            None => Ok(false),
            Some(v) => read_bool(v, &self.path_of(snake)),
        }
    }

    /// A `bool?` member.
    pub fn opt_bool(&self, snake: &str) -> Result<Option<bool>, ReadError> {
        match self.get_non_null(snake) {
            None => Ok(None),
            Some(v) => read_bool(v, &self.path_of(snake)).map(Some),
        }
    }

    /// A `string?` member.
    pub fn opt_string(&self, snake: &str) -> Result<Option<String>, ReadError> {
        match self.get(snake) {
            None => Ok(None),
            Some(v) => read_opt_string(v, &self.path_of(snake)),
        }
    }

    /// A non-nullable `string` member. System.Text.Json leaves a missing or null member
    /// null; the Rust port reads it as `""`.
    pub fn string(&self, snake: &str) -> Result<String, ReadError> {
        Ok(self.opt_string(snake)?.unwrap_or_default())
    }

    /// A `Guid` member (`Guid.Empty` when missing).
    pub fn guid(&self, snake: &str) -> Result<dl_uuid::Uuid, ReadError> {
        match self.get(snake) {
            None => Ok(dl_uuid::Uuid::NIL),
            Some(v) => read_guid(v, &self.path_of(snake)),
        }
    }

    /// A `Guid?` member.
    pub fn opt_guid(&self, snake: &str) -> Result<Option<dl_uuid::Uuid>, ReadError> {
        match self.get_non_null(snake) {
            None => Ok(None),
            Some(v) => read_guid(v, &self.path_of(snake)).map(Some),
        }
    }

    /// A date-time member as written (`None` when missing; error when null or not ISO).
    pub fn iso(&self, snake: &str) -> Result<Option<IsoDateTime>, ReadError> {
        match self.get(snake) {
            None => Ok(None),
            Some(v) => read_iso(v, &self.path_of(snake)).map(Some),
        }
    }

    /// A nullable date-time member.
    pub fn opt_iso(&self, snake: &str) -> Result<Option<IsoDateTime>, ReadError> {
        match self.get_non_null(snake) {
            None => Ok(None),
            Some(v) => read_iso(v, &self.path_of(snake)).map(Some),
        }
    }

    /// A list member read with `item` (empty when missing or null: the Rust port of a
    /// null `IReadOnlyList`).
    pub fn list<T>(
        &self,
        snake: &str,
        item: impl Fn(&'a Value, ReadOptions, &str) -> Result<T, ReadError>,
    ) -> Result<Vec<T>, ReadError> {
        Ok(self.opt_list(snake, item)?.unwrap_or_default())
    }

    /// A nullable list member.
    pub fn opt_list<T>(
        &self,
        snake: &str,
        item: impl Fn(&'a Value, ReadOptions, &str) -> Result<T, ReadError>,
    ) -> Result<Option<Vec<T>>, ReadError> {
        let Some(v) = self.get_non_null(snake) else {
            return Ok(None);
        };
        let path = self.path_of(snake);
        let Some(items) = v.as_array() else {
            return Err(ReadError::new(&path, "The JSON value is not an array."));
        };
        items
            .iter()
            .enumerate()
            .map(|(i, x)| item(x, self.opts, &format!("{path}[{i}]")))
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// A `Dictionary<string, string?>` member in document order (empty when missing or
    /// null). Keys are kept as written (no naming policy applies to dictionary keys).
    pub fn str_dict(&self, snake: &str) -> Result<Vec<(String, Option<String>)>, ReadError> {
        let Some(v) = self.get_non_null(snake) else {
            return Ok(Vec::new());
        };
        let path = self.path_of(snake);
        let Some(map) = v.as_object() else {
            return Err(ReadError::new(&path, "The JSON value is not an object."));
        };
        map.iter()
            .map(|(k, v)| Ok((k.clone(), read_opt_string(v, &format!("{path}.{k}"))?)))
            .collect()
    }

    /// A nested object member read with `item` (`None` when missing or null).
    pub fn opt_obj<T>(
        &self,
        snake: &str,
        item: impl Fn(Obj<'a>) -> Result<T, ReadError>,
    ) -> Result<Option<T>, ReadError> {
        let Some(v) = self.get(snake) else {
            return Ok(None);
        };
        match Obj::of(v, self.opts, &self.path_of(snake))? {
            None => Ok(None),
            Some(o) => item(o).map(Some),
        }
    }
}

/// A date-time string member value.
pub fn read_iso(v: &Value, path: &str) -> Result<IsoDateTime, ReadError> {
    match v {
        Value::String(s) => parse_iso_date_time(s).ok_or_else(|| {
            ReadError::new(
                path,
                "The JSON value is not in a supported DateTime format.",
            )
        }),
        _ => Err(ReadError::new(
            path,
            "The JSON value is not in a supported DateTime format.",
        )),
    }
}

/// An enum through `JsonStringEnumConverter`: a name (trimmed, case-insensitive), a
/// comma-separated list of names and numbers OR-ed together, an integer, or a string
/// holding an integer. `names` lists the C# members with their values. Values without a
/// member (which .NET would keep) are refused.
pub fn read_enum<T: Copy>(v: &Value, path: &str, names: &[(&str, i64, T)]) -> Result<T, ReadError> {
    let bad = || ReadError::new(path, "The JSON value could not be converted to the enum.");
    let value = match v {
        Value::Number(n) => parse_integer(&number_text(n))
            .and_then(|n| i64::try_from(n).ok())
            .ok_or_else(bad)?,
        Value::String(s) => {
            let mut acc = 0i64;
            if s.trim().is_empty() {
                return Err(bad());
            }
            for part in s.split(',') {
                let part = part.trim();
                if let Some(n) = parse_integer(part).and_then(|n| i64::try_from(n).ok()) {
                    acc |= n;
                } else if let Some((_, n, _)) =
                    names.iter().find(|(name, _, _)| eq_ignore_case(name, part))
                {
                    acc |= n;
                } else {
                    return Err(bad());
                }
            }
            acc
        }
        _ => return Err(bad()),
    };
    names
        .iter()
        .find(|(_, n, _)| *n == value)
        .map(|(_, _, t)| *t)
        .ok_or_else(bad)
}

/// `StringComparison.OrdinalIgnoreCase`: equal after simple upper-casing of each char.
pub fn eq_ignore_case(a: &str, b: &str) -> bool {
    if a.len() == b.len() && a.eq_ignore_ascii_case(b) {
        return true;
    }
    let up = |c: char| {
        let mut u = c.to_uppercase();
        match (u.next(), u.next()) {
            (Some(x), None) => x,
            _ => c,
        }
    };
    a.chars().map(up).eq(b.chars().map(up))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_helpers_follow_stj() {
        let v = parse(r#"{"a":1,"b":1.0,"c":"7","d":null,"e":1e3,"f":"x"}"#).unwrap();
        let o = Obj::require(&v, SNAKE, "$").unwrap();
        assert_eq!(o.int::<i32>("a").unwrap(), 1);
        assert!(o.int::<i32>("b").is_err());
        assert!(o.int::<i32>("c").is_err());
        assert!(o.int::<i64>("d").is_err());
        assert_eq!(o.int::<i64>("missing").unwrap(), 0);
        assert_eq!(o.int_or::<i32>("missing", 1).unwrap(), 1);
        assert_eq!(o.opt_int::<i64>("d").unwrap(), None);
        assert_eq!(o.decimal("e").unwrap().to_string(), "1000");
        assert_eq!(o.decimal("b").unwrap().to_string(), "1.0");
        assert!(o.opt_string("a").is_err());
        assert_eq!(o.string("missing").unwrap(), "");
        let w = Obj::require(&v, WEB, "$").unwrap();
        assert_eq!(w.int::<i64>("c").unwrap(), 7);
        assert!(Obj::require(&v, SNAKE, "$").unwrap().get("A").is_none());
        let v = parse(r#"{"rows":5,"ROWS":6,"manifestSha256":"h"}"#).unwrap();
        let w = Obj::require(&v, WEB, "$").unwrap();
        assert_eq!(w.int::<i64>("rows").unwrap(), 6);
        assert_eq!(w.string("manifest_sha256").unwrap(), "h");
        // Depth limit 64 as JsonSerializerOptions.MaxDepth.
        assert!(parse(&format!("{}{}", "[".repeat(64), "]".repeat(64))).is_ok());
        assert!(parse(&format!("{}{}", "[".repeat(65), "]".repeat(65))).is_err());
        assert!(parse("\u{feff}{}").is_err());
        assert!(parse(r#"{"a":5} x"#).is_err());
    }

    #[test]
    fn decimals_fit_system_decimal() {
        let p = |s: &str| parse_dotnet_decimal(s).map(|d| d.to_string());
        assert_eq!(
            p("1.23456789012345678901234567891").as_deref(),
            Some("1.2345678901234567890123456789")
        );
        assert_eq!(
            p("79228162514264337593543950335").as_deref(),
            Some("79228162514264337593543950335")
        );
        assert_eq!(p("79228162514264337593543950336"), None);
        assert_eq!(p("1e3").as_deref(), Some("1000"));
    }

    #[test]
    fn enums_like_json_string_enum_converter() {
        #[derive(Debug, Clone, Copy, PartialEq)]
        enum M {
            D,
            S,
            R,
        }
        let names = [
            ("Delta", 0, M::D),
            ("Snapshot", 1, M::S),
            ("Repair", 2, M::R),
        ];
        let r = |j: &str| read_enum(&parse(j).unwrap(), "$", &names).ok();
        assert_eq!(r("\"Delta\""), Some(M::D));
        assert_eq!(r("\"snapshot\""), Some(M::S));
        assert_eq!(r("\"REPAIR\""), Some(M::R));
        assert_eq!(r("1"), Some(M::S));
        assert_eq!(r("\"1\""), Some(M::S));
        assert_eq!(r("\" Delta \""), Some(M::D));
        assert_eq!(r("\"Delta, Snapshot\""), Some(M::S));
        assert_eq!(r("\"Nope\""), None);
        assert_eq!(r("null"), None);
        assert_eq!(r("1.0"), None);
        assert_eq!(r("\"\""), None);
    }

    #[test]
    fn iso_dates_like_stj() {
        let p = |s: &str| parse_iso_date_time(s);
        assert_eq!(p("2026-10-24").unwrap().offset, IsoOffset::None);
        assert!(p("2026-10-24T01:02").is_some());
        let x = p("2026-10-24T01:02:03.12345678Z").unwrap();
        assert_eq!(x.offset, IsoOffset::Utc);
        assert_eq!(x.ticks % 10_000_000, 1_234_567);
        assert!(p("2026-10-24T01:02:03.1234567890123456Z").is_some());
        assert_eq!(
            p("2026-10-24T01:02:03+07").unwrap().offset,
            IsoOffset::Minutes(420)
        );
        assert_eq!(
            p("2026-10-24T01:02:03-14:00").unwrap().offset,
            IsoOffset::Minutes(-840)
        );
        assert!(p("2026-10-24T01:02:03.Z").is_some());
        for bad in [
            "2026-10-24T01:02:03+0700",
            "2026-10-24t01:02:03z",
            "2026-10-24 01:02:03Z",
            "2026-10-24T24:00:00Z",
            "2026-10-24T01:02:60Z",
            "2026-10-24T01:02:03+14:01",
            "2026-10-24T01:02:03,5Z",
            "2026-10-24T01Z",
            "2026-02-30",
        ] {
            assert!(p(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn floats_like_dotnet_r() {
        let t = |v: f32| f32_text(v).unwrap();
        assert_eq!(t(1.5), "1.5");
        assert_eq!(t(0.1), "0.1");
        assert_eq!(t(1e8), "100000000");
        assert_eq!(t(1e9), "1E+09");
        assert_eq!(t(123_456_789.0), "123456790");
        assert_eq!(t(1.0 / 3.0), "0.33333334");
        assert_eq!(t(1e-5), "1E-05");
        assert_eq!(t(0.0001), "0.0001");
        assert_eq!(t(3.402_823_5e38), "3.4028235E+38");
        assert_eq!(Naming::Camel.name("manifest_sha256"), "manifestSha256");
    }

    #[test]
    fn default_escaping_matches_stj() {
        assert_eq!(quote("a\"b"), r#""a\u0022b""#);
        assert_eq!(quote("<&>'+`"), r#""\u003C\u0026\u003E\u0027\u002B\u0060""#);
        assert_eq!(quote("Phở 1\n\t\\"), r#""Ph\u1EDF 1\n\t\\""#);
        assert_eq!(quote("\u{1}\u{7f}"), r#""\u0001\u007F""#);
        assert_eq!(quote("😀"), r#""\uD83D\uDE00""#);
    }

    #[test]
    fn relaxed_escaping() {
        let mut s = String::new();
        write_str(&mut s, "Phở \"x\" <b> \u{1} 😀", Escaping::Relaxed);
        assert_eq!(s, r#""Phở \"x\" <b> \u0001 \uD83D\uDE00""#);
        // .NET 10 output for "\u2028\u2029\uFEFF\uFFFD\u0085\u00A0\u200B\u00AD".
        let mut s = String::new();
        write_str(
            &mut s,
            "\u{2028}\u{2029}\u{feff}\u{fffd}\u{85}\u{a0}\u{200b}\u{ad}",
            Escaping::Relaxed,
        );
        assert_eq!(
            s,
            "\"\\u2028\\u2029\\uFEFF\u{fffd}\\u0085\\u00A0\u{200b}\u{ad}\""
        );
    }

    #[test]
    fn doubles_like_dotnet() {
        let t = |v: f64| f64_text(v).unwrap();
        assert_eq!(t(0.0), "0");
        assert_eq!(t(1.5), "1.5");
        assert_eq!(t(-12.25), "-12.25");
        assert_eq!(t(0.1), "0.1");
        assert_eq!(t(0.0001), "0.0001");
        assert_eq!(t(0.00001), "1E-05");
        assert_eq!(t(1.5e-7), "1.5E-07");
        assert_eq!(t(1e14), "100000000000000");
        assert_eq!(t(1e15), "1000000000000000");
        assert_eq!(t(1e16), "10000000000000000");
        assert_eq!(t(1e17), "1E+17");
        assert_eq!(t(123456789012345.6), "123456789012345.6");
        assert_eq!(t(5e-324), "5E-324");
        assert_eq!(t(1.2345678901234568e17), "1.2345678901234568E+17");
        assert_eq!(t(1e300), "1E+300");
        assert_eq!(t(42.0), "42");
        assert!(f64_text(f64::NAN).is_none());
    }

    #[test]
    fn dates_like_dotnet() {
        // 2026-11-04T03:00:00Z
        let n = 1_793_761_200i128 * 1_000_000_000;
        assert_eq!(date_time_offset(n, 0), "2026-11-04T03:00:00+00:00");
        assert_eq!(date_time_offset(n, 420), "2026-11-04T10:00:00+07:00");
        assert_eq!(
            date_time_offset(n + 1_230_000_000 / 10, -150),
            "2026-11-04T00:30:00.123-02:30"
        );
        assert_eq!(
            date_time_utc(n + 1_234_567_800),
            "2026-11-04T03:00:01.2345678Z"
        );
        assert_eq!(date_time_utc(n + 99), "2026-11-04T03:00:00Z");
        assert_eq!(date_time_unspecified(0), "1970-01-01T00:00:00");
        assert_eq!(date_time_utc(-1_000_000_000), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn dates_are_written_without_the_encoder() {
        // .NET 10: {"a":"2026-01-02T03:04:05+07:00","d":"\u002B"}
        let n = 1_767_297_845i128 * 1_000_000_000;
        let mut w = Writer::new(Style::Compact);
        w.begin_object();
        w.key("a").date_time_offset(n, 420);
        w.key("d").string("+");
        w.end_object();
        assert_eq!(
            w.finish(),
            r#"{"a":"2026-01-02T03:04:05+07:00","d":"\u002B"}"#
        );
    }

    #[test]
    fn writer_layouts() {
        let mut w = Writer::new(Style::Compact);
        w.begin_object()
            .key("a")
            .int(1)
            .key("b")
            .begin_array()
            .string("x")
            .bool(true)
            .end_array();
        w.key("e")
            .begin_object()
            .end_object()
            .key("z")
            .begin_array()
            .end_array()
            .end_object();
        assert_eq!(w.finish(), r#"{"a":1,"b":["x",true],"e":{},"z":[]}"#);

        let mut w = Writer::new(Style::Indented);
        w.begin_object()
            .key("a")
            .int(1)
            .key("b")
            .begin_array()
            .string("x")
            .null()
            .end_array();
        w.key("e").begin_object().end_object().end_object();
        let nl = NEWLINE;
        assert_eq!(
            w.finish(),
            format!(
                "{{{nl}  \"a\": 1,{nl}  \"b\": [{nl}    \"x\",{nl}    null{nl}  ],{nl}  \"e\": {{}}{nl}}}"
            )
        );
    }

    #[test]
    fn value_keeps_number_text_and_reescapes() {
        let v = dl_json::parse_with(
            r#"{"n":1.50,"s":"a\u00e9<","l":[1e3]}"#,
            dl_json::ParseOptions {
                keep_raw_numbers: true,
                ..dl_json::ParseOptions::default()
            },
        )
        .unwrap();
        let mut w = Writer::new(Style::Compact);
        w.value(&v);
        assert_eq!(w.finish(), r#"{"n":1.50,"s":"a\u00E9\u003C","l":[1e3]}"#);
    }
}
