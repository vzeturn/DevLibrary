//! Vietnamese text helpers: diacritics removal, URL slugs, name casing and collation.
//!
//! Every function accepts both precomposed (NFC) and decomposed (NFD) input, and functions
//! that return Vietnamese text return precomposed letters.
//!
//! Collation follows the Unicode CLDR `vi` "standard" tailoring
//! (<https://unicode.org/cldr/charts/45/collation/vi.html>):
//!
//! - letters in alphabet order a ă â b c d đ e ê g h i k l m n o ô ơ p q r s t u ư v x y,
//!   with the non-native f, j, w, z at their Latin positions;
//! - tone marks are a secondary difference, ordered none, huyền, hỏi, ngã, sắc, nặng
//!   (`a à ả ã á ạ`);
//! - case is a tertiary difference, lowercase first.
//!
//! ```
//! use dl_vn::text;
//!
//! assert_eq!(text::remove_diacritics("Nguyễn Văn Đức"), "Nguyen Van Duc");
//! assert_eq!(text::normalize_name("  nguyễn   văn AN "), "Nguyễn Văn An");
//!
//! let mut names = ["Đào", "Dũng", "Ân", "An", "Bà", "Ba"];
//! names.sort_by(|a, b| text::compare_vi(a, b));
//! assert_eq!(names, ["An", "Ân", "Ba", "Bà", "Dũng", "Đào"]);
//! ```

use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

/// The 12 Vietnamese vowel letters, in alphabet order. Row order of [`LOWER`] and [`UPPER`].
#[cfg(test)]
const VOWELS: [char; 12] = ['a', 'ă', 'â', 'e', 'ê', 'i', 'o', 'ô', 'ơ', 'u', 'ư', 'y'];

/// ASCII letter each vowel reduces to when diacritics are removed.
const VOWEL_ASCII: [char; 12] = ['a', 'a', 'a', 'e', 'e', 'i', 'o', 'o', 'o', 'u', 'u', 'y'];

/// Position of each vowel in the alphabet table [`ALPHABET`].
const VOWEL_RANK: [u8; 12] = [0, 1, 2, 7, 8, 12, 18, 19, 20, 26, 27, 31];

/// Lowercase vowels by tone: none, huyền, hỏi, ngã, sắc, nặng (the CLDR order).
#[rustfmt::skip]
const LOWER: [[char; 6]; 12] = [
    ['a', 'à', 'ả', 'ã', 'á', 'ạ'],
    ['ă', 'ằ', 'ẳ', 'ẵ', 'ắ', 'ặ'],
    ['â', 'ầ', 'ẩ', 'ẫ', 'ấ', 'ậ'],
    ['e', 'è', 'ẻ', 'ẽ', 'é', 'ẹ'],
    ['ê', 'ề', 'ể', 'ễ', 'ế', 'ệ'],
    ['i', 'ì', 'ỉ', 'ĩ', 'í', 'ị'],
    ['o', 'ò', 'ỏ', 'õ', 'ó', 'ọ'],
    ['ô', 'ồ', 'ổ', 'ỗ', 'ố', 'ộ'],
    ['ơ', 'ờ', 'ở', 'ỡ', 'ớ', 'ợ'],
    ['u', 'ù', 'ủ', 'ũ', 'ú', 'ụ'],
    ['ư', 'ừ', 'ử', 'ữ', 'ứ', 'ự'],
    ['y', 'ỳ', 'ỷ', 'ỹ', 'ý', 'ỵ'],
];

/// Uppercase counterpart of [`LOWER`].
#[rustfmt::skip]
const UPPER: [[char; 6]; 12] = [
    ['A', 'À', 'Ả', 'Ã', 'Á', 'Ạ'],
    ['Ă', 'Ằ', 'Ẳ', 'Ẵ', 'Ắ', 'Ặ'],
    ['Â', 'Ầ', 'Ẩ', 'Ẫ', 'Ấ', 'Ậ'],
    ['E', 'È', 'Ẻ', 'Ẽ', 'É', 'Ẹ'],
    ['Ê', 'Ề', 'Ể', 'Ễ', 'Ế', 'Ệ'],
    ['I', 'Ì', 'Ỉ', 'Ĩ', 'Í', 'Ị'],
    ['O', 'Ò', 'Ỏ', 'Õ', 'Ó', 'Ọ'],
    ['Ô', 'Ồ', 'Ổ', 'Ỗ', 'Ố', 'Ộ'],
    ['Ơ', 'Ờ', 'Ở', 'Ỡ', 'Ớ', 'Ợ'],
    ['U', 'Ù', 'Ủ', 'Ũ', 'Ú', 'Ụ'],
    ['Ư', 'Ừ', 'Ử', 'Ữ', 'Ứ', 'Ự'],
    ['Y', 'Ỳ', 'Ỷ', 'Ỹ', 'Ý', 'Ỵ'],
];

/// Collation alphabet (CLDR `vi` standard): Vietnamese letters plus f, j, w, z.
const ALPHABET: [char; 33] = [
    'a', 'ă', 'â', 'b', 'c', 'd', 'đ', 'e', 'ê', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o',
    'ô', 'ơ', 'p', 'q', 'r', 's', 't', 'u', 'ư', 'v', 'w', 'x', 'y', 'z',
];

/// A Vietnamese vowel letter split into base vowel, tone and case.
#[derive(Clone, Copy)]
struct Vowel {
    /// Index into [`VOWELS`].
    base: usize,
    /// 0 none, 1 huyền, 2 hỏi, 3 ngã, 4 sắc, 5 nặng.
    tone: usize,
    upper: bool,
}

impl Vowel {
    fn from_char(c: char) -> Option<Self> {
        if c.is_ascii() && !c.is_ascii_alphabetic() {
            return None;
        }
        for (table, upper) in [(&LOWER, false), (&UPPER, true)] {
            for (base, row) in table.iter().enumerate() {
                if let Some(tone) = row.iter().position(|&x| x == c) {
                    return Some(Self { base, tone, upper });
                }
            }
        }
        None
    }

    fn to_char(self) -> char {
        let table = if self.upper { &UPPER } else { &LOWER };
        table[self.base][self.tone]
    }

    /// Applies a combining mark (NFD input). Returns false when the mark does not combine.
    fn apply(&mut self, mark: char) -> bool {
        let tone = match mark {
            '\u{300}' | '\u{340}' => 1,
            '\u{309}' => 2,
            '\u{303}' => 3,
            '\u{301}' | '\u{341}' => 4,
            '\u{323}' => 5,
            _ => 0,
        };
        if tone != 0 {
            if self.tone != 0 {
                return false;
            }
            self.tone = tone;
            return true;
        }
        // Base indexes: a 0, ă 1, â 2, e 3, ê 4, o 6, ô 7, ơ 8, u 9, ư 10.
        let base = match (mark, self.base) {
            ('\u{306}', 0) => 1,
            ('\u{302}', 0) => 2,
            ('\u{302}', 3) => 4,
            ('\u{302}', 6) => 7,
            ('\u{31B}', 6) => 8,
            ('\u{31B}', 9) => 10,
            _ => return false,
        };
        self.base = base;
        true
    }
}

fn is_combining_mark(c: char) -> bool {
    ('\u{300}'..='\u{36F}').contains(&c)
}

/// Removes Vietnamese diacritics: every vowel with any tone in both cases becomes its ASCII
/// letter, `đ`/`Đ` become `d`/`D`, and combining marks U+0300–U+036F are dropped (so NFD
/// input works). Other characters are kept unchanged.
///
/// ```
/// assert_eq!(dl_vn::text::remove_diacritics("ĐẶNG THỊ HƯƠNG"), "DANG THI HUONG");
/// ```
pub fn remove_diacritics(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii() {
            out.push(c);
        } else if is_combining_mark(c) {
            continue;
        } else if c == 'đ' {
            out.push('d');
        } else if c == 'Đ' {
            out.push('D');
        } else if let Some(v) = Vowel::from_char(c) {
            let ascii = VOWEL_ASCII[v.base];
            out.push(if v.upper {
                ascii.to_ascii_uppercase()
            } else {
                ascii
            });
        } else {
            out.push(c);
        }
    }
    out
}

/// Removes the trailing spaces (U+0020 only) SQL Server pads `char(n)` values with and
/// ignores when comparing text, so `'100001'` and `'100001      '` stay one key once they leave
/// the database. Leading and inner spaces, tabs and newlines are kept; an all-space value
/// becomes empty.
///
/// ```
/// assert_eq!(dl_vn::text::trim_padding("100001      "), "100001");
/// assert_eq!(dl_vn::text::trim_padding("  a b\t "), "  a b\t");
/// assert_eq!(dl_vn::text::trim_padding("   "), "");
/// ```
#[must_use]
pub fn trim_padding(s: &str) -> &str {
    s.trim_end_matches(' ')
}

/// Builds a URL slug: diacritics removed, ASCII lowercase, every run of other characters
/// replaced by one `-`, no leading or trailing `-`. Characters that are still not ASCII
/// letters or digits after [`remove_diacritics`] (other scripts, and precomposed Latin letters
/// that Vietnamese does not use, such as `ñ`) act as separators.
///
/// ```
/// assert_eq!(
///     dl_vn::text::slugify("Khách sạn Đà Nẵng - Phòng 101!"),
///     "khach-san-da-nang-phong-101"
/// );
/// ```
pub fn slugify(s: &str) -> String {
    let plain = remove_diacritics(s);
    let mut out = String::with_capacity(plain.len());
    let mut gap = false;
    for c in plain.chars() {
        if c.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('-');
            }
            gap = false;
            out.push(c.to_ascii_lowercase());
        } else {
            gap = true;
        }
    }
    out
}

/// Recomposes Vietnamese vowels written with combining marks into precomposed letters.
pub(crate) fn compose(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending: Option<Vowel> = None;
    for c in s.chars() {
        if let Some(v) = pending.as_mut()
            && v.apply(c)
        {
            continue;
        }
        if let Some(v) = pending.take() {
            out.push(v.to_char());
        }
        match Vowel::from_char(c) {
            Some(v) => pending = Some(v),
            None => out.push(c),
        }
    }
    if let Some(v) = pending {
        out.push(v.to_char());
    }
    out
}

/// Normalises a person's name: trims, collapses whitespace, and writes each word with an
/// uppercase first letter and lowercase rest. A letter after `-` or an apostrophe is also
/// capitalised, as in the Ê Đê name "H'Hen". Output letters are precomposed.
///
/// ```
/// use dl_vn::text::normalize_name;
///
/// assert_eq!(normalize_name("nguyễn văn AN"), "Nguyễn Văn An");
/// assert_eq!(normalize_name("h'hen  NIÊ"), "H'Hen Niê");
/// ```
pub fn normalize_name(s: &str) -> String {
    let composed = compose(s);
    let mut out = String::with_capacity(composed.len());
    for word in composed.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        let mut start = true;
        for c in word.chars() {
            if start {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            start = matches!(c, '-' | '\'' | '\u{2019}');
        }
    }
    out
}

/// Primary classes, in collation order: spaces, punctuation and symbols, digits, Latin
/// letters, everything else.
const CLASS_SPACE: u32 = 1 << 24;
const CLASS_PUNCT: u32 = 2 << 24;
const CLASS_DIGIT: u32 = 3 << 24;
const CLASS_LETTER: u32 = 4 << 24;
const CLASS_OTHER: u32 = 5 << 24;

/// One collation element: primary weight, tone (secondary) and case (tertiary).
#[derive(Clone, Copy)]
struct Element {
    primary: u32,
    tone: u8,
    upper: bool,
}

impl Element {
    fn of_vowel(v: Vowel) -> Self {
        Self {
            primary: CLASS_LETTER | u32::from(VOWEL_RANK[v.base]),
            tone: v.tone as u8,
            upper: v.upper,
        }
    }

    fn of_char(c: char) -> Self {
        let lower = c.to_lowercase().next().unwrap_or(c);
        let upper = lower != c;
        let primary = if let Some(rank) = ALPHABET.iter().position(|&x| x == lower) {
            // The alphabet has 33 entries.
            CLASS_LETTER | rank as u32
        } else if c.is_whitespace() {
            CLASS_SPACE
        } else if c.is_ascii_digit() {
            CLASS_DIGIT | u32::from(c)
        } else if c.is_alphanumeric() {
            CLASS_OTHER | u32::from(lower)
        } else {
            CLASS_PUNCT | u32::from(c)
        };
        Self {
            primary,
            tone: 0,
            upper,
        }
    }
}

fn elements(s: &str) -> Vec<Element> {
    let mut out = Vec::with_capacity(s.len());
    let mut pending: Option<Vowel> = None;
    for c in s.chars() {
        if let Some(v) = pending.as_mut()
            && v.apply(c)
        {
            continue;
        }
        if let Some(v) = pending.take() {
            out.push(Element::of_vowel(v));
        }
        match Vowel::from_char(c) {
            Some(v) => pending = Some(v),
            None => out.push(Element::of_char(c)),
        }
    }
    if let Some(v) = pending {
        out.push(Element::of_vowel(v));
    }
    out
}

/// Compares two strings in Vietnamese dictionary order, for sorting customer names.
///
/// Letters compare first (a < ă < â < b ... d < đ ...), then tones left to right (none <
/// huyền < hỏi < ngã < sắc < nặng), then case (lowercase first). Whitespace sorts before
/// punctuation, punctuation before digits, digits before letters. Precomposed and decomposed
/// spellings of the same text compare `Equal`.
///
/// The whole string is compared as given. To sort by given name, pass the last word.
pub fn compare_vi(a: &str, b: &str) -> Ordering {
    let (x, y) = (elements(a), elements(b));
    let primary = x.iter().map(|e| e.primary).cmp(y.iter().map(|e| e.primary));
    if primary != Ordering::Equal {
        return primary;
    }
    let tone = x.iter().map(|e| e.tone).cmp(y.iter().map(|e| e.tone));
    if tone != Ordering::Equal {
        return tone;
    }
    x.iter().map(|e| e.upper).cmp(y.iter().map(|e| e.upper))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    const TONE_MARKS: [&str; 6] = ["", "\u{300}", "\u{309}", "\u{303}", "\u{301}", "\u{323}"];
    const BASE_NFD: [&str; 12] = [
        "a", "a\u{306}", "a\u{302}", "e", "e\u{302}", "i", "o", "o\u{302}", "o\u{31B}", "u",
        "u\u{31B}", "y",
    ];

    #[test]
    fn tables_are_consistent() {
        for (base, row) in LOWER.iter().enumerate() {
            assert_eq!(row[0], VOWELS[base]);
            assert_eq!(ALPHABET[usize::from(VOWEL_RANK[base])], VOWELS[base]);
            for (tone, &c) in row.iter().enumerate() {
                let up = UPPER[base][tone];
                assert_eq!(c.to_uppercase().next(), Some(up));
                assert_eq!(up.to_lowercase().next(), Some(c));
                let v = Vowel::from_char(c).unwrap();
                assert_eq!((v.base, v.tone, v.upper), (base, tone, false));
                assert_eq!(v.to_char(), c);
            }
        }
    }

    #[test]
    fn remove_all_vowels_and_tones() {
        for base in 0..12 {
            for tone in 0..6 {
                let lower = LOWER[base][tone];
                let upper = UPPER[base][tone];
                let mut s = String::new();
                s.push(lower);
                s.push(upper);
                let mut expected = String::new();
                expected.push(VOWEL_ASCII[base]);
                expected.push(VOWEL_ASCII[base].to_ascii_uppercase());
                assert_eq!(remove_diacritics(&s), expected, "{s}");
                // NFD spelling, with the tone either after or before the vowel modifier.
                let nfd = alloc::format!("{}{}", BASE_NFD[base], TONE_MARKS[tone]);
                assert_eq!(remove_diacritics(&nfd), VOWEL_ASCII[base].to_string());
            }
        }
    }

    #[test]
    fn remove_diacritics_examples() {
        assert_eq!(remove_diacritics("Nguyễn Văn Đức"), "Nguyen Van Duc");
        assert_eq!(remove_diacritics("ĐẶNG THỊ HƯƠNG"), "DANG THI HUONG");
        assert_eq!(remove_diacritics("Tiếng Việt có dấu"), "Tieng Viet co dau");
        assert_eq!(remove_diacritics("Nguye\u{302}\u{303}n"), "Nguyen");
        assert_eq!(remove_diacritics("đường Lê Lợi, Q.1"), "duong Le Loi, Q.1");
        assert_eq!(remove_diacritics("東京 €5"), "東京 €5");
        assert_eq!(remove_diacritics(""), "");
    }

    #[test]
    fn slugs() {
        assert_eq!(
            slugify("Khách sạn Đà Nẵng - Phòng 101!"),
            "khach-san-da-nang-phong-101"
        );
        assert_eq!(slugify("  --Hello---World--  "), "hello-world");
        assert_eq!(slugify("Phở Bò 100%"), "pho-bo-100");
        assert_eq!(slugify("Pho\u{31B}\u{309} bo\u{300}"), "pho-bo");
        assert_eq!(slugify("Cà phê 東京"), "ca-phe");
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn compose_every_nfd_spelling() {
        for base in 0..12 {
            for tone in 0..6 {
                let expected = LOWER[base][tone].to_string();
                let modifier_first = alloc::format!("{}{}", BASE_NFD[base], TONE_MARKS[tone]);
                assert_eq!(compose(&modifier_first), expected);
                // Canonical order puts the dot below (and horn) before other marks.
                let mut chars = BASE_NFD[base].chars();
                let letter = chars.next().unwrap();
                let tone_first = alloc::format!("{letter}{}{}", TONE_MARKS[tone], chars.as_str());
                assert_eq!(compose(&tone_first), expected);
            }
        }
        // A precomposed vowel modifier followed by a combining tone.
        assert_eq!(compose("a\u{302}\u{301}"), "ấ");
        assert_eq!(compose("\u{302}a"), "\u{302}a");
        assert_eq!(compose("à\u{301}"), "à\u{301}");
    }

    #[test]
    fn names() {
        assert_eq!(normalize_name("nguyễn văn AN"), "Nguyễn Văn An");
        assert_eq!(
            normalize_name("  trần   thị   BÍCH  ngọc "),
            "Trần Thị Bích Ngọc"
        );
        assert_eq!(normalize_name("ĐẶNG"), "Đặng");
        assert_eq!(normalize_name("ưng hoàng PHÚC"), "Ưng Hoàng Phúc");
        assert_eq!(normalize_name("nguye\u{302}\u{303}n"), "Nguyễn");
        assert_eq!(normalize_name("h'hen niê"), "H'Hen Niê");
        assert_eq!(normalize_name("lê-nguyễn"), "Lê-Nguyễn");
        assert_eq!(normalize_name("\u{a0}an\tbình\n"), "An Bình");
        assert_eq!(normalize_name(""), "");
    }

    #[test]
    fn sorts_customer_names() {
        let expected = vec![
            "An", "Anh", "Ánh", "Ăn", "Ân", "Ba", "Bà", "Bả", "Bã", "Bá", "Bạ", "Bác", "Bạc",
            "Bàn", "Cường", "Dũng", "Đào", "Đạt", "Giang", "Hà", "Hải", "Le", "Lê", "Lệ", "Ngô",
            "Nguyễn", "Nhung", "Oanh", "Ông", "Ơn", "Út", "Uyên", "Ưng", "Vũ", "Xuân", "Yến",
        ];
        let mut shuffled = expected.clone();
        shuffled.reverse();
        shuffled.swap(3, 17);
        shuffled.swap(0, 30);
        shuffled.sort_by(|a, b| compare_vi(a, b));
        assert_eq!(shuffled, expected);
        // Plain byte order gets this wrong: "Ánh" sorts after "Yến".
        let mut bytes = expected.clone();
        bytes.sort();
        assert_ne!(bytes, expected);
    }

    #[test]
    fn collation_levels() {
        use Ordering::*;
        assert_eq!(compare_vi("an", "An"), Less);
        assert_eq!(compare_vi("An", "án"), Less);
        assert_eq!(compare_vi("Nguyễn", "Nguye\u{302}\u{303}n"), Equal);
        assert_eq!(compare_vi("Nguyễn Văn An", "Nguyễn Văn Anh"), Less);
        assert_eq!(compare_vi("Nguyễn Văn Anh", "Nguyễn Văn Ân"), Less);
        assert_eq!(compare_vi("Lê An", "Lêa"), Less);
        assert_eq!(compare_vi("Phòng 2", "Phòng 10"), Greater);
        assert_eq!(compare_vi("A-1", "A1"), Less);
        assert_eq!(compare_vi("", "a"), Less);
        assert_eq!(compare_vi("zoo", "đá"), Greater);
        assert_eq!(compare_vi("Ý", "Ỳ"), Greater);
        assert_eq!(compare_vi("Trần", "Trần"), Equal);
    }
}
