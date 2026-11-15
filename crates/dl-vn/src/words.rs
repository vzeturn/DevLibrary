//! Vietnamese number reading ("đọc số thành chữ"), for the amount-in-words line of invoices.
//!
//! Reading rules, from [Vietnamese numerals](https://en.wikipedia.org/wiki/Vietnamese_numerals):
//!
//! - Digits are grouped by thousands: nghìn (ngàn), triệu, tỷ. Above a billion the reading
//!   recurses on tỷ, so 10¹² is "một nghìn tỷ", 10¹⁵ "một triệu tỷ", 10¹⁸ "một tỷ tỷ" and
//!   1 500 × 10⁹ is "một nghìn năm trăm tỷ".
//! - 10 is "mười"; tens from 20 use "mươi" ("hai mươi").
//! - The unit 1 after "mươi" is "mốt" ("hai mươi mốt"), but "mười một".
//! - The unit 5 after "mười" or "mươi" is "lăm" ("mười lăm", "hai mươi lăm"), but "năm"
//!   elsewhere ("một trăm linh năm").
//! - A zero tens digit before a non-zero unit is read "linh" (northern) or "lẻ" (southern):
//!   "một trăm linh một".
//! - A group that follows a higher non-zero group always reads its hundreds, even when zero:
//!   "một nghìn không trăm linh năm", "hai triệu không trăm mười nghìn". Groups that are
//!   entirely zero are skipped: "một triệu", "một tỷ không trăm linh năm".
//!
//! The unit 4 after "mươi" can be "bốn" or "tư" ("hai mươi tư"). Wikipedia says "tư" is the
//! more common form there and that colloquial speech often uses "bốn"; the Unicode CLDR
//! spell-out rules ([`rbnf/vi.xml`](https://github.com/unicode-org/cldr/blob/main/common/rbnf/vi.xml))
//! print "tư". No legal text fixing the form on invoices was found. The default is "bốn",
//! the digit's own name, which reads the same in every position; select [`Four::Tu`] for
//! "tư". "mười bốn" keeps "bốn" in both modes.
//!
//! CLDR's rules also read 1 000 005 as "một triệu lẻ năm". This module keeps the "không
//! trăm" of every group after the first non-empty one ("một triệu không trăm linh năm"),
//! the form Wikipedia gives for 1 001 and 10 055, so that every group of three digits is
//! read in full.
//!
//! ```
//! use dl_vn::words::{self, Linh, Options, Thousand};
//!
//! assert_eq!(words::number_to_words(115), "một trăm mười lăm");
//! assert_eq!(
//!     words::vnd_to_words(1_234_567),
//!     "Một triệu hai trăm ba mươi bốn nghìn năm trăm sáu mươi bảy đồng"
//! );
//! let south = Options { linh: Linh::Le, thousand: Thousand::Ngan, ..Options::default() };
//! assert_eq!(words::number_to_words_with(1_005, &south), "một ngàn không trăm lẻ năm");
//! assert_eq!(
//!     words::amount_to_words("1234.56", "đô la Mỹ", Some(("xu", 2))).unwrap(),
//!     "Một nghìn hai trăm ba mươi bốn đô la Mỹ và năm mươi sáu xu"
//! );
//! ```

use crate::Error;
use alloc::string::String;

const DIGITS: [&str; 10] = [
    "không", "một", "hai", "ba", "bốn", "năm", "sáu", "bảy", "tám", "chín",
];

const BILLION: u64 = 1_000_000_000;

/// Word for a zero tens digit before a non-zero unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Linh {
    /// "linh", the northern form: "một trăm linh năm".
    #[default]
    Linh,
    /// "lẻ", the southern and central form: "một trăm lẻ năm".
    Le,
}

/// Word for 1 000.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Thousand {
    /// "nghìn", the northern form.
    #[default]
    Nghin,
    /// "ngàn", the southern form.
    Ngan,
}

/// Spelling of 10⁹.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Billion {
    /// "tỷ".
    #[default]
    Ty,
    /// "tỉ".
    Ti,
}

/// Word for the unit 4 after "mươi" (24, 34, ... 94).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Four {
    /// "bốn": "hai mươi bốn".
    #[default]
    Bon,
    /// "tư": "hai mươi tư".
    Tu,
}

/// Reading options. The default is the northern standard used in the examples above.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    /// "linh" or "lẻ".
    pub linh: Linh,
    /// "nghìn" or "ngàn".
    pub thousand: Thousand,
    /// "tỷ" or "tỉ".
    pub billion: Billion,
    /// "bốn" or "tư" after "mươi".
    pub four: Four,
    /// Money functions only: append " chẵn" ("even") when there is no minor part,
    /// as in "Năm trăm nghìn đồng chẵn". Off by default.
    pub chan: bool,
}

/// Reads `n` in Vietnamese with the default [`Options`].
pub fn number_to_words(n: u64) -> String {
    number_to_words_with(n, &Options::default())
}

/// Reads `n` in Vietnamese.
pub fn number_to_words_with(n: u64, opts: &Options) -> String {
    let mut out = String::with_capacity(64);
    if n == 0 {
        out.push_str(DIGITS[0]);
    } else {
        write_number(&mut out, n, opts);
    }
    out
}

/// Writes `n > 0`. Above a billion, the count of billions is read recursively.
fn write_number(out: &mut String, n: u64, opts: &Options) {
    if n >= BILLION {
        write_number(out, n / BILLION, opts);
        push_word(
            out,
            match opts.billion {
                Billion::Ty => "tỷ",
                Billion::Ti => "tỉ",
            },
        );
        let rest = n % BILLION;
        if rest > 0 {
            write_below_billion(out, rest, true, opts);
        }
    } else {
        write_below_billion(out, n, false, opts);
    }
}

/// Writes `0 < n < 10⁹`. `inner` is true when a higher part has already been written.
fn write_below_billion(out: &mut String, n: u64, mut inner: bool, opts: &Options) {
    let thousand = match opts.thousand {
        Thousand::Nghin => "nghìn",
        Thousand::Ngan => "ngàn",
    };
    let groups = [(n / 1_000_000) % 1000, (n / 1000) % 1000, n % 1000];
    let names = [Some("triệu"), Some(thousand), None];
    for (group, name) in groups.into_iter().zip(names) {
        if group == 0 {
            continue;
        }
        write_group(out, group as usize, inner, opts);
        if let Some(name) = name {
            push_word(out, name);
        }
        inner = true;
    }
}

/// Writes a group `0 < g < 1000`.
fn write_group(out: &mut String, g: usize, inner: bool, opts: &Options) {
    let (h, t, u) = (g / 100, (g / 10) % 10, g % 10);
    let hundreds = inner || h > 0;
    if hundreds {
        push_word(out, DIGITS[h]);
        push_word(out, "trăm");
    }
    match (t, u) {
        (0, 0) => {}
        (0, u) => {
            if hundreds {
                push_word(
                    out,
                    match opts.linh {
                        Linh::Linh => "linh",
                        Linh::Le => "lẻ",
                    },
                );
            }
            push_word(out, DIGITS[u]);
        }
        (1, u) => {
            push_word(out, "mười");
            match u {
                0 => {}
                5 => push_word(out, "lăm"),
                u => push_word(out, DIGITS[u]),
            }
        }
        (t, u) => {
            push_word(out, DIGITS[t]);
            push_word(out, "mươi");
            match (u, opts.four) {
                (0, _) => {}
                (1, _) => push_word(out, "mốt"),
                (4, Four::Tu) => push_word(out, "tư"),
                (5, _) => push_word(out, "lăm"),
                (u, _) => push_word(out, DIGITS[u]),
            }
        }
    }
}

fn push_word(out: &mut String, word: &str) {
    if !out.is_empty() {
        out.push(' ');
    }
    out.push_str(word);
}

/// Upper-cases the first character, as required at the start of the amount-in-words line.
fn capitalize_into(out: &mut String, s: &str) {
    let mut chars = s.chars();
    if let Some(first) = chars.next() {
        out.extend(first.to_uppercase());
        out.push_str(chars.as_str());
    }
}

/// Reads a VND amount for an invoice: capitalised, followed by " đồng".
///
/// `vnd_to_words(500_000)` is "Năm trăm nghìn đồng".
pub fn vnd_to_words(amount: u64) -> String {
    vnd_to_words_with(amount, &Options::default())
}

/// Reads a VND amount with options. With `opts.chan` the result ends in " đồng chẵn".
pub fn vnd_to_words_with(amount: u64, opts: &Options) -> String {
    let words = number_to_words_with(amount, opts);
    let mut out = String::with_capacity(words.len() + 16);
    capitalize_into(&mut out, &words);
    out.push_str(" đồng");
    if opts.chan {
        out.push_str(" chẵn");
    }
    out
}

/// Reads a decimal amount string in words with the default [`Options`].
///
/// See [`amount_to_words_with`].
pub fn amount_to_words(
    amount: &str,
    major: &str,
    minor: Option<(&str, u32)>,
) -> Result<String, Error> {
    amount_to_words_with(amount, major, minor, &Options::default())
}

/// Reads a decimal amount string such as `"1234.56"` or `"-15000"` in words.
///
/// - `major` is the main unit name, e.g. `"đồng"` or `"đô la Mỹ"`.
/// - `minor` is `Some((name, digits))` for a currency with a minor unit, e.g. `("xu", 2)`
///   for cents, where `digits` (1..=18) is the number of decimal places of the minor unit.
///   With `None` the amount must be whole; trailing zero decimals (`"1000.00"`) are accepted.
///
/// The input is parsed exactly, without floating point. The syntax is an optional `-`, at
/// least one ASCII digit, then optionally `.` and at least one digit. Thousands separators,
/// spaces and `+` are rejected. Decimal digits beyond `digits` must be zero.
///
/// The result is capitalised. A negative non-zero amount starts with "Âm". A non-zero minor
/// part is appended as `" và {minor in words} {minor name}"`; otherwise `opts.chan` appends
/// `" chẵn"`.
///
/// ```
/// use dl_vn::words::amount_to_words;
///
/// assert_eq!(
///     amount_to_words("-20.5", "đô la Mỹ", Some(("xu", 2))).unwrap(),
///     "Âm hai mươi đô la Mỹ và năm mươi xu"
/// );
/// assert_eq!(amount_to_words("1000.00", "đồng", None).unwrap(), "Một nghìn đồng");
/// assert!(amount_to_words("1,000", "đồng", None).is_err());
/// ```
pub fn amount_to_words_with(
    amount: &str,
    major: &str,
    minor: Option<(&str, u32)>,
    opts: &Options,
) -> Result<String, Error> {
    let digits = match minor {
        None => 0,
        Some((_, d @ 1..=18)) => d,
        Some((_, d)) => return Err(Error::InvalidMinorDigits { digits: d }),
    };
    let parsed = parse_amount(amount, digits)?;

    let mut out = String::with_capacity(96);
    let major_words = number_to_words_with(parsed.major, opts);
    if parsed.negative && (parsed.major != 0 || parsed.minor != 0) {
        out.push_str("Âm ");
        out.push_str(&major_words);
    } else {
        capitalize_into(&mut out, &major_words);
    }
    if !major.is_empty() {
        out.push(' ');
        out.push_str(major);
    }
    match minor {
        Some((name, _)) if parsed.minor != 0 => {
            out.push_str(" và ");
            out.push_str(&number_to_words_with(parsed.minor, opts));
            if !name.is_empty() {
                out.push(' ');
                out.push_str(name);
            }
        }
        _ => {
            if opts.chan {
                out.push_str(" chẵn");
            }
        }
    }
    Ok(out)
}

struct Amount {
    negative: bool,
    major: u64,
    minor: u64,
}

/// Parses `-?\d+(\.\d+)?`, keeping `digits` decimal places as the minor value.
fn parse_amount(s: &str, digits: u32) -> Result<Amount, Error> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let negative = bytes.first() == Some(&b'-');
    if negative {
        i = 1;
    }
    let int_start = i;
    let mut major: u64 = 0;
    while let Some(&b) = bytes.get(i)
        && b.is_ascii_digit()
    {
        major = major
            .checked_mul(10)
            .and_then(|m| m.checked_add(u64::from(b - b'0')))
            .ok_or(Error::Overflow)?;
        i += 1;
    }
    if i == int_start {
        return match next_char(s, i) {
            None => Err(Error::Empty),
            Some(ch) => Err(Error::InvalidCharacter { index: i, ch }),
        };
    }
    let mut minor: u64 = 0;
    let mut taken = 0u32;
    if bytes.get(i) == Some(&b'.') {
        let dot = i;
        i += 1;
        let frac_start = i;
        while let Some(&b) = bytes.get(i)
            && b.is_ascii_digit()
        {
            if taken < digits {
                // At most 18 digits, so this cannot overflow a u64.
                minor = minor * 10 + u64::from(b - b'0');
                taken += 1;
            } else if b != b'0' {
                return Err(Error::TooManyDecimals { index: i });
            }
            i += 1;
        }
        if i == frac_start {
            return Err(Error::InvalidCharacter {
                index: dot,
                ch: '.',
            });
        }
    }
    if let Some(ch) = next_char(s, i) {
        return Err(Error::InvalidCharacter { index: i, ch });
    }
    // "1234.5" with two minor digits is 50 cents.
    minor *= 10u64.pow(digits - taken);
    Ok(Amount {
        negative,
        major,
        minor,
    })
}

/// The character starting at byte `i`. Every caller passes an offset reached by stepping over
/// ASCII bytes only, so `i` is a char boundary.
fn next_char(s: &str, i: usize) -> Option<char> {
    s.get(i..).and_then(|rest| rest.chars().next())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rustfmt::skip]
    const CASES: &[(u64, &str)] = &[
        (0, "không"),
        (1, "một"),
        (2, "hai"),
        (3, "ba"),
        (4, "bốn"),
        (5, "năm"),
        (6, "sáu"),
        (7, "bảy"),
        (8, "tám"),
        (9, "chín"),
        (10, "mười"),
        (11, "mười một"),
        (12, "mười hai"),
        (14, "mười bốn"),
        (15, "mười lăm"),
        (16, "mười sáu"),
        (19, "mười chín"),
        (20, "hai mươi"),
        (21, "hai mươi mốt"),
        (22, "hai mươi hai"),
        (24, "hai mươi bốn"),
        (25, "hai mươi lăm"),
        (30, "ba mươi"),
        (31, "ba mươi mốt"),
        (44, "bốn mươi bốn"),
        (45, "bốn mươi lăm"),
        (50, "năm mươi"),
        (51, "năm mươi mốt"),
        (55, "năm mươi lăm"),
        (69, "sáu mươi chín"),
        (71, "bảy mươi mốt"),
        (88, "tám mươi tám"),
        (99, "chín mươi chín"),
        (100, "một trăm"),
        (101, "một trăm linh một"),
        (104, "một trăm linh bốn"),
        (105, "một trăm linh năm"),
        (110, "một trăm mười"),
        (111, "một trăm mười một"),
        (115, "một trăm mười lăm"),
        (121, "một trăm hai mươi mốt"),
        (125, "một trăm hai mươi lăm"),
        (200, "hai trăm"),
        (205, "hai trăm linh năm"),
        (250, "hai trăm năm mươi"),
        (305, "ba trăm linh năm"),
        (555, "năm trăm năm mươi lăm"),
        (605, "sáu trăm linh năm"),
        (999, "chín trăm chín mươi chín"),
        (1_000, "một nghìn"),
        (1_001, "một nghìn không trăm linh một"),
        (1_005, "một nghìn không trăm linh năm"),
        (1_010, "một nghìn không trăm mười"),
        (1_015, "một nghìn không trăm mười lăm"),
        (1_021, "một nghìn không trăm hai mươi mốt"),
        (1_100, "một nghìn một trăm"),
        (1_234, "một nghìn hai trăm ba mươi bốn"),
        (2_000, "hai nghìn"),
        (5_555, "năm nghìn năm trăm năm mươi lăm"),
        (10_000, "mười nghìn"),
        (10_005, "mười nghìn không trăm linh năm"),
        (10_055, "mười nghìn không trăm năm mươi lăm"),
        (11_000, "mười một nghìn"),
        (15_000, "mười lăm nghìn"),
        (21_000, "hai mươi mốt nghìn"),
        (25_500, "hai mươi lăm nghìn năm trăm"),
        (100_000, "một trăm nghìn"),
        (101_000, "một trăm linh một nghìn"),
        (105_000, "một trăm linh năm nghìn"),
        (110_000, "một trăm mười nghìn"),
        (999_999, "chín trăm chín mươi chín nghìn chín trăm chín mươi chín"),
        (1_000_000, "một triệu"),
        (1_000_001, "một triệu không trăm linh một"),
        (1_000_010, "một triệu không trăm mười"),
        (1_001_000, "một triệu không trăm linh một nghìn"),
        (1_010_000, "một triệu không trăm mười nghìn"),
        (1_100_000, "một triệu một trăm nghìn"),
        (1_234_567, "một triệu hai trăm ba mươi bốn nghìn năm trăm sáu mươi bảy"),
        (2_010_000, "hai triệu không trăm mười nghìn"),
        (10_000_000, "mười triệu"),
        (15_000_000, "mười lăm triệu"),
        (100_000_000, "một trăm triệu"),
        (105_000_000, "một trăm linh năm triệu"),
        (999_999_999, "chín trăm chín mươi chín triệu chín trăm chín mươi chín nghìn chín trăm chín mươi chín"),
        (1_000_000_000, "một tỷ"),
        (1_000_000_005, "một tỷ không trăm linh năm"),
        (1_000_005_000, "một tỷ không trăm linh năm nghìn"),
        (1_005_000_000, "một tỷ không trăm linh năm triệu"),
        (1_200_000_000, "một tỷ hai trăm triệu"),
        (2_000_000_000, "hai tỷ"),
        (2_500_000_000, "hai tỷ năm trăm triệu"),
        (10_000_000_000, "mười tỷ"),
        (21_000_000_000, "hai mươi mốt tỷ"),
        (100_000_000_000, "một trăm tỷ"),
        (1_000_000_000_000, "một nghìn tỷ"),
        (1_000_000_000_001, "một nghìn tỷ không trăm linh một"),
        (1_000_001_000_000, "một nghìn tỷ không trăm linh một triệu"),
        (1_500_000_000_000, "một nghìn năm trăm tỷ"),
        (1_234_567_890_123, "một nghìn hai trăm ba mươi bốn tỷ năm trăm sáu mươi bảy triệu tám trăm chín mươi nghìn một trăm hai mươi ba"),
        (1_000_000_000_000_000, "một triệu tỷ"),
        (5_000_000_000_000_000, "năm triệu tỷ"),
        (1_000_000_000_000_000_000, "một tỷ tỷ"),
        (1_000_000_000_000_000_005, "một tỷ tỷ không trăm linh năm"),
        (1_000_000_001_000_000_000, "một tỷ không trăm linh một tỷ"),
        (10_000_000_000_000_000_000, "mười tỷ tỷ"),
        (u64::MAX, "mười tám tỷ bốn trăm bốn mươi sáu triệu bảy trăm bốn mươi bốn nghìn không trăm bảy mươi ba tỷ bảy trăm linh chín triệu năm trăm năm mươi mốt nghìn sáu trăm mười lăm"),
    ];

    #[test]
    fn reading_table() {
        assert!(CASES.len() >= 80);
        for &(n, expected) in CASES {
            assert_eq!(number_to_words(n), expected, "n = {n}");
        }
    }

    #[test]
    fn options() {
        let le = Options {
            linh: Linh::Le,
            ..Options::default()
        };
        assert_eq!(number_to_words_with(105, &le), "một trăm lẻ năm");
        assert_eq!(
            number_to_words_with(1_001, &le),
            "một nghìn không trăm lẻ một"
        );

        let ngan = Options {
            thousand: Thousand::Ngan,
            ..Options::default()
        };
        assert_eq!(number_to_words_with(1_000, &ngan), "một ngàn");

        let ti = Options {
            billion: Billion::Ti,
            thousand: Thousand::Ngan,
            ..Options::default()
        };
        assert_eq!(number_to_words_with(2_000_000_000, &ti), "hai tỉ");
        assert_eq!(number_to_words_with(1_000_000_000_000, &ti), "một ngàn tỉ");

        let tu = Options {
            four: Four::Tu,
            ..Options::default()
        };
        assert_eq!(number_to_words_with(24, &tu), "hai mươi tư");
        assert_eq!(number_to_words_with(94_000, &tu), "chín mươi tư nghìn");
        assert_eq!(number_to_words_with(14, &tu), "mười bốn");
        assert_eq!(number_to_words_with(104, &tu), "một trăm linh bốn");
        assert_eq!(number_to_words_with(4, &tu), "bốn");
    }

    #[test]
    fn no_double_spaces_and_lowercase() {
        let mut n = 1u64;
        for _ in 0..200 {
            let s = number_to_words(n);
            assert!(
                !s.contains("  ") && !s.starts_with(' ') && !s.ends_with(' '),
                "{s}"
            );
            n = n
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
        }
    }

    #[test]
    fn vnd() {
        assert_eq!(
            vnd_to_words(1_234_567),
            "Một triệu hai trăm ba mươi bốn nghìn năm trăm sáu mươi bảy đồng"
        );
        assert_eq!(vnd_to_words(0), "Không đồng");
        assert_eq!(vnd_to_words(500_000), "Năm trăm nghìn đồng");
        let chan = Options {
            chan: true,
            ..Options::default()
        };
        assert_eq!(
            vnd_to_words_with(500_000, &chan),
            "Năm trăm nghìn đồng chẵn"
        );
    }

    #[test]
    fn decimal_amounts() {
        let usd = |s| amount_to_words(s, "đô la Mỹ", Some(("xu", 2)));
        assert_eq!(
            usd("1234.56").unwrap(),
            "Một nghìn hai trăm ba mươi bốn đô la Mỹ và năm mươi sáu xu"
        );
        assert_eq!(
            usd("1234.5").unwrap(),
            "Một nghìn hai trăm ba mươi bốn đô la Mỹ và năm mươi xu"
        );
        assert_eq!(
            usd("1234.05").unwrap(),
            "Một nghìn hai trăm ba mươi bốn đô la Mỹ và năm xu"
        );
        assert_eq!(usd("1234.560").unwrap(), usd("1234.56").unwrap());
        assert_eq!(
            usd("-1234.56").unwrap(),
            "Âm một nghìn hai trăm ba mươi bốn đô la Mỹ và năm mươi sáu xu"
        );
        assert_eq!(usd("0.56").unwrap(), "Không đô la Mỹ và năm mươi sáu xu");
        assert_eq!(usd("-0.00").unwrap(), "Không đô la Mỹ");
        assert_eq!(usd("12").unwrap(), "Mười hai đô la Mỹ");
        assert_eq!(
            amount_to_words("1000", "đồng", None).unwrap(),
            "Một nghìn đồng"
        );
        assert_eq!(
            amount_to_words("1000.000", "đồng", None).unwrap(),
            "Một nghìn đồng"
        );
        assert_eq!(
            amount_to_words("18446744073709551615", "đồng", None).unwrap(),
            alloc::format!(
                "{} đồng",
                "Mười tám tỷ bốn trăm bốn mươi sáu triệu bảy trăm bốn mươi bốn nghìn không trăm bảy mươi ba tỷ bảy trăm linh chín triệu năm trăm năm mươi mốt nghìn sáu trăm mười lăm"
            )
        );
        let chan = Options {
            chan: true,
            ..Options::default()
        };
        let eur = |s| amount_to_words_with(s, "euro", Some(("cent", 2)), &chan);
        assert_eq!(eur("100").unwrap(), "Một trăm euro chẵn");
        assert_eq!(eur("100.01").unwrap(), "Một trăm euro và một cent");
        assert_eq!(
            amount_to_words("0.000000000000000001", "x", Some(("y", 18))).unwrap(),
            "Không x và một y"
        );
    }

    #[test]
    fn decimal_errors() {
        let usd = |s| amount_to_words(s, "đô la Mỹ", Some(("xu", 2)));
        assert_eq!(usd(""), Err(Error::Empty));
        assert_eq!(usd("-"), Err(Error::Empty));
        assert_eq!(
            usd("12a"),
            Err(Error::InvalidCharacter { index: 2, ch: 'a' })
        );
        assert_eq!(
            usd(" 12"),
            Err(Error::InvalidCharacter { index: 0, ch: ' ' })
        );
        assert_eq!(
            usd("12 "),
            Err(Error::InvalidCharacter { index: 2, ch: ' ' })
        );
        assert_eq!(
            usd("+12"),
            Err(Error::InvalidCharacter { index: 0, ch: '+' })
        );
        assert_eq!(
            usd("1,000"),
            Err(Error::InvalidCharacter { index: 1, ch: ',' })
        );
        assert_eq!(
            usd("1."),
            Err(Error::InvalidCharacter { index: 1, ch: '.' })
        );
        assert_eq!(
            usd(".5"),
            Err(Error::InvalidCharacter { index: 0, ch: '.' })
        );
        assert_eq!(
            usd("1.2.3"),
            Err(Error::InvalidCharacter { index: 3, ch: '.' })
        );
        assert_eq!(
            usd("--1"),
            Err(Error::InvalidCharacter { index: 1, ch: '-' })
        );
        assert_eq!(
            usd("١٢"),
            Err(Error::InvalidCharacter { index: 0, ch: '١' })
        );
        assert_eq!(
            usd("1é"),
            Err(Error::InvalidCharacter { index: 1, ch: 'é' })
        );
        assert_eq!(usd("1234.567"), Err(Error::TooManyDecimals { index: 7 }));
        assert_eq!(usd("18446744073709551616"), Err(Error::Overflow));
        assert_eq!(
            amount_to_words("1000.50", "đồng", None),
            Err(Error::TooManyDecimals { index: 5 })
        );
        assert_eq!(
            amount_to_words("1", "x", Some(("y", 0))),
            Err(Error::InvalidMinorDigits { digits: 0 })
        );
        assert_eq!(
            amount_to_words("1", "x", Some(("y", 19))),
            Err(Error::InvalidMinorDigits { digits: 19 })
        );
    }
}
