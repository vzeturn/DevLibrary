//! TCVN3 (ABC, the `.Vn` fonts) ⇄ Unicode for legacy databases such as SMILE FO/BO/POS,
//! which keep Vietnamese as TCVN3 bytes in `char`/`varchar` columns.
//!
//! A database driver does not see TCVN3: it decodes the stored bytes through the column's
//! collation code page, so the same byte reaches the program as a different character on a
//! `SQL_Latin1_General_CP1_CI_AS` database (cp1252) than on a `Vietnamese_CI_AS` one (cp1258,
//! where byte `0xCC` becomes the combining grave accent U+0300). [`decode`] takes the
//! [`CodePage`] the text went through and recovers the bytes first; [`decode_bytes`] takes the
//! stored bytes directly (`CAST(col AS varbinary(max))`).
//!
//! TCVN3 only has lowercase toned vowels. Legacy entry screens store an uppercase toned vowel
//! as the uppercase base letter followed by the lowercase toned byte (`A` `0xB5` for `À`);
//! the decoder folds those pairs, and [`encode`] writes them.
//!
//! ```
//! use dl_vn::tcvn3::{self, CodePage};
//!
//! // "Phở Bò" as stored, read back through a cp1252 collation.
//! assert_eq!(tcvn3::decode_bytes(b"Ph\xEB B\xDF"), "Phở Bò");
//! assert_eq!(tcvn3::decode("Ph\u{EB} B\u{DF}", CodePage::Cp1252), "Phở Bò");
//! // Through Vietnamese_CI_AS (cp1258) byte 0xDD (í) arrives as `Ư`, 0xEC (ỡ) as U+0301.
//! assert_eq!(tcvn3::decode("b\u{B8}nh m\u{1AF}", CodePage::Cp1258), "bánh mí");
//! assert_eq!(tcvn3::decode("\u{301}", CodePage::Cp1258), "ỡ");
//! assert_eq!(tcvn3::encode("Đồ uống")?, b"\xA7\xE5 u\xE8ng");
//! # Ok::<(), dl_vn::Error>(())
//! ```

use alloc::string::String;
use alloc::vec::Vec;

use crate::Error;

/// The single-byte code page a `char`/`varchar` column was decoded through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodePage {
    /// Windows-1252 (`SQL_Latin1_General_CP1_*`, `Latin1_General_*`); bytes `0xA0..=0xFF`
    /// are Latin-1. Also right for text decoded as ISO-8859-1.
    Cp1252,
    /// Windows-1258 (`Vietnamese_*` collations).
    Cp1258,
}

impl CodePage {
    /// The code page with this number (`1252`, `1258`, or `28591` for Latin-1), as reported by
    /// `COLLATIONPROPERTY(collation, 'CodePage')`.
    #[must_use]
    pub fn from_number(n: u32) -> Option<Self> {
        match n {
            1252 | 28591 => Some(Self::Cp1252),
            1258 => Some(Self::Cp1258),
            _ => None,
        }
    }

    /// The byte this code page decodes to `c`, for bytes `0xA0..=0xFF` (the only ones TCVN3
    /// uses above ASCII).
    fn byte_of(self, c: char) -> Option<u8> {
        let u = c as u32;
        if self == Self::Cp1258 {
            if let Some(&(b, _)) = CP1258_HIGH.iter().find(|(_, x)| *x == c) {
                return Some(b);
            }
            // Positions cp1258 redefines never decode to their Latin-1 character.
            if (0xA0..=0xFF).contains(&u) && CP1258_HIGH.iter().any(|(b, _)| u32::from(*b) == u) {
                return None;
            }
        }
        // `u` is checked to be in 0xA0..=0xFF, so the cast is lossless.
        #[allow(clippy::cast_possible_truncation)]
        ((0xA0..=0xFF).contains(&u)).then_some(u as u8)
    }

    /// The character this code page decodes `b` (`0xA0..=0xFF`) to.
    fn char_of(self, b: u8) -> char {
        if self == Self::Cp1258
            && let Some(&(_, c)) = CP1258_HIGH.iter().find(|(x, _)| *x == b)
        {
            return c;
        }
        char::from(b)
    }
}

/// Bytes `0xA0..=0xFF` where cp1258 differs from Latin-1.
const CP1258_HIGH: [(u8, char); 14] = [
    (0xC3, '\u{0102}'),
    (0xCC, '\u{0300}'),
    (0xD0, '\u{0110}'),
    (0xD2, '\u{0309}'),
    (0xD5, '\u{01A0}'),
    (0xDD, '\u{01AF}'),
    (0xDE, '\u{0303}'),
    (0xE3, '\u{0103}'),
    (0xEC, '\u{0301}'),
    (0xF0, '\u{0111}'),
    (0xF2, '\u{0323}'),
    (0xF5, '\u{01A1}'),
    (0xFD, '\u{01B0}'),
    (0xFE, '\u{20AB}'),
];

/// TCVN3 bytes `0xA1..=0xFE` → letter; `'\0'` where TCVN3 defines nothing.
const SINGLE: [char; 94] = {
    let mut t = ['\0'; 94];
    let mut i = 0;
    while i < LETTERS.len() {
        t[(LETTERS[i].0 - 0xA1) as usize] = LETTERS[i].1;
        i += 1;
    }
    t
};

const LETTERS: [(u8, char); 74] = [
    (0xA1, 'Ă'),
    (0xA2, 'Â'),
    (0xA3, 'Ê'),
    (0xA4, 'Ô'),
    (0xA5, 'Ơ'),
    (0xA6, 'Ư'),
    (0xA7, 'Đ'),
    (0xA8, 'ă'),
    (0xA9, 'â'),
    (0xAA, 'ê'),
    (0xAB, 'ô'),
    (0xAC, 'ơ'),
    (0xAD, 'ư'),
    (0xAE, 'đ'),
    (0xB5, 'à'),
    (0xB6, 'ả'),
    (0xB7, 'ã'),
    (0xB8, 'á'),
    (0xB9, 'ạ'),
    (0xBB, 'ằ'),
    (0xBC, 'ẳ'),
    (0xBD, 'ẵ'),
    (0xBE, 'ắ'),
    (0xC6, 'ặ'),
    (0xC7, 'ầ'),
    (0xC8, 'ẩ'),
    (0xC9, 'ẫ'),
    (0xCA, 'ấ'),
    (0xCB, 'ậ'),
    (0xCC, 'è'),
    (0xCE, 'ẻ'),
    (0xCF, 'ẽ'),
    (0xD0, 'é'),
    (0xD1, 'ẹ'),
    (0xD2, 'ề'),
    (0xD3, 'ể'),
    (0xD4, 'ễ'),
    (0xD5, 'ế'),
    (0xD6, 'ệ'),
    (0xD7, 'ì'),
    (0xD8, 'ỉ'),
    (0xDC, 'ĩ'),
    (0xDD, 'í'),
    (0xDE, 'ị'),
    (0xDF, 'ò'),
    (0xE1, 'ỏ'),
    (0xE2, 'õ'),
    (0xE3, 'ó'),
    (0xE4, 'ọ'),
    (0xE5, 'ồ'),
    (0xE6, 'ổ'),
    (0xE7, 'ỗ'),
    (0xE8, 'ố'),
    (0xE9, 'ộ'),
    (0xEA, 'ờ'),
    (0xEB, 'ở'),
    (0xEC, 'ỡ'),
    (0xED, 'ớ'),
    (0xEE, 'ợ'),
    (0xEF, 'ù'),
    (0xF1, 'ủ'),
    (0xF2, 'ũ'),
    (0xF3, 'ú'),
    (0xF4, 'ụ'),
    (0xF5, 'ừ'),
    (0xF6, 'ử'),
    (0xF7, 'ữ'),
    (0xF8, 'ứ'),
    (0xF9, 'ự'),
    (0xFA, 'ỳ'),
    (0xFB, 'ỷ'),
    (0xFC, 'ỹ'),
    (0xFD, 'ý'),
    (0xFE, 'ỵ'),
];

/// Uppercase base byte, then the toned lowercase bytes in tone order
/// huyền, hỏi, ngã, sắc, nặng, and the precomposed uppercase letters in the same order.
const PAIRS: [(u8, [u8; 5], [char; 5]); 12] = [
    (
        b'A',
        [0xB5, 0xB6, 0xB7, 0xB8, 0xB9],
        ['À', 'Ả', 'Ã', 'Á', 'Ạ'],
    ),
    (
        0xA1,
        [0xBB, 0xBC, 0xBD, 0xBE, 0xC6],
        ['Ằ', 'Ẳ', 'Ẵ', 'Ắ', 'Ặ'],
    ),
    (
        0xA2,
        [0xC7, 0xC8, 0xC9, 0xCA, 0xCB],
        ['Ầ', 'Ẩ', 'Ẫ', 'Ấ', 'Ậ'],
    ),
    (
        b'E',
        [0xCC, 0xCE, 0xCF, 0xD0, 0xD1],
        ['È', 'Ẻ', 'Ẽ', 'É', 'Ẹ'],
    ),
    (
        0xA3,
        [0xD2, 0xD3, 0xD4, 0xD5, 0xD6],
        ['Ề', 'Ể', 'Ễ', 'Ế', 'Ệ'],
    ),
    (
        b'I',
        [0xD7, 0xD8, 0xDC, 0xDD, 0xDE],
        ['Ì', 'Ỉ', 'Ĩ', 'Í', 'Ị'],
    ),
    (
        b'O',
        [0xDF, 0xE1, 0xE2, 0xE3, 0xE4],
        ['Ò', 'Ỏ', 'Õ', 'Ó', 'Ọ'],
    ),
    (
        0xA4,
        [0xE5, 0xE6, 0xE7, 0xE8, 0xE9],
        ['Ồ', 'Ổ', 'Ỗ', 'Ố', 'Ộ'],
    ),
    (
        0xA5,
        [0xEA, 0xEB, 0xEC, 0xED, 0xEE],
        ['Ờ', 'Ở', 'Ỡ', 'Ớ', 'Ợ'],
    ),
    (
        b'U',
        [0xEF, 0xF1, 0xF2, 0xF3, 0xF4],
        ['Ù', 'Ủ', 'Ũ', 'Ú', 'Ụ'],
    ),
    (
        0xA6,
        [0xF5, 0xF6, 0xF7, 0xF8, 0xF9],
        ['Ừ', 'Ử', 'Ữ', 'Ứ', 'Ự'],
    ),
    (
        b'Y',
        [0xFA, 0xFB, 0xFC, 0xFD, 0xFE],
        ['Ỳ', 'Ỷ', 'Ỹ', 'Ý', 'Ỵ'],
    ),
];

fn single(b: u8) -> Option<char> {
    match b {
        0xA0 => Some(' '),
        0xA1..=0xFE => Some(SINGLE[usize::from(b - 0xA1)]).filter(|&c| c != '\0'),
        _ => None,
    }
}

fn pair(base: u8, tone: u8) -> Option<char> {
    let (_, tones, upper) = PAIRS.iter().find(|(b, _, _)| *b == base)?;
    tones.iter().position(|&t| t == tone).map(|i| upper[i])
}

/// One input unit: a byte the code page produced, or a character it could not have produced.
#[derive(Clone, Copy)]
enum Unit {
    Byte(u8),
    Other(char),
}

fn decode_units(units: &[Unit]) -> String {
    let mut out = String::with_capacity(units.len() + 8);
    let mut i = 0;
    while i < units.len() {
        if let (Unit::Byte(b), Some(Unit::Byte(t))) = (units[i], units.get(i + 1))
            && let Some(c) = pair(b, *t)
        {
            out.push(c);
            i += 2;
            continue;
        }
        match units[i] {
            Unit::Byte(b) if b < 0x80 => out.push(char::from(b)),
            Unit::Byte(b) => out.push(single(b).unwrap_or(char::from(b))),
            Unit::Other(c) => out.push(c),
        }
        i += 1;
    }
    out
}

/// Decodes stored TCVN3 bytes. ASCII and bytes TCVN3 leaves undefined pass through as
/// Latin-1; `0xA0` (no-break space) becomes a space.
///
/// ```
/// assert_eq!(dl_vn::tcvn3::decode_bytes(b"\xA7\xE5 u\xE8ng"), "Đồ uống");
/// assert_eq!(dl_vn::tcvn3::decode_bytes(b"A\xB5N"), "ÀN");
/// ```
#[must_use]
pub fn decode_bytes(raw: &[u8]) -> String {
    let units: Vec<Unit> = raw.iter().map(|&b| Unit::Byte(b)).collect();
    decode_units(&units)
}

/// Decodes TCVN3 text that a driver already decoded through `cp`. Characters `cp` cannot
/// produce from a single byte (text that is already Unicode, `€` from cp1252 `0x80`) pass
/// through unchanged.
#[must_use]
pub fn decode(s: &str, cp: CodePage) -> String {
    let units: Vec<Unit> = s
        .chars()
        .map(|c| match c {
            c if c.is_ascii() => Unit::Byte(c as u8),
            // Only bytes TCVN3 gives a meaning; any other character stays as decoded.
            c => cp
                .byte_of(c)
                .filter(|&b| single(b).is_some())
                .map_or(Unit::Other(c), Unit::Byte),
        })
        .collect();
    decode_units(&units)
}

/// [`decode`], then [`trim_padding`](crate::text::trim_padding): the value of a TCVN3
/// `char(n)` / `varchar` column as it compares in SQL Server.
#[must_use]
pub fn decode_column(s: &str, cp: CodePage) -> String {
    let mut out = decode(s, cp);
    out.truncate(crate::text::trim_padding(&out).len());
    out
}

fn encode_char(c: char, out: &mut Vec<u8>) -> bool {
    if c.is_ascii() {
        out.push(c as u8);
        return true;
    }
    if let Some((b, _)) = LETTERS.iter().find(|(_, x)| *x == c) {
        out.push(*b);
        return true;
    }
    for (base, tones, upper) in &PAIRS {
        if let Some(i) = upper.iter().position(|&u| u == c) {
            out.extend_from_slice(&[*base, tones[i]]);
            return true;
        }
    }
    if c == '\u{A0}' {
        out.push(0xA0);
        return true;
    }
    false
}

/// Encodes Unicode text (precomposed or decomposed) as TCVN3 bytes, writing uppercase toned
/// vowels as base-plus-tone pairs.
///
/// # Errors
/// [`Error::InvalidCharacter`] for a character TCVN3 cannot represent, with its byte offset in
/// the precomposed form of `s`.
pub fn encode(s: &str) -> Result<Vec<u8>, Error> {
    let s = crate::text::compose(s);
    let mut out = Vec::with_capacity(s.len());
    for (index, ch) in s.char_indices() {
        if !encode_char(ch, &mut out) {
            return Err(Error::InvalidCharacter { index, ch });
        }
    }
    Ok(out)
}

/// [`encode`], then each byte as the character `cp` decodes it to: the string to send as an
/// `nvarchar` parameter so that SQL Server stores the TCVN3 bytes in a `varchar` column of
/// that code page.
///
/// # Errors
/// As [`encode`].
pub fn encode_for(s: &str, cp: CodePage) -> Result<String, Error> {
    Ok(encode(s)?
        .into_iter()
        .map(|b| {
            if b < 0xA0 {
                char::from(b)
            } else {
                cp.char_of(b)
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every byte through `cp` as the driver would decode it.
    fn through(raw: &[u8], cp: CodePage) -> String {
        raw.iter()
            .map(|&b| {
                if b < 0xA0 {
                    char::from(b)
                } else {
                    cp.char_of(b)
                }
            })
            .collect()
    }

    #[test]
    fn table_bytes_and_letters_are_unique() {
        for (i, (b, c)) in LETTERS.iter().enumerate() {
            assert!(
                LETTERS[i + 1..].iter().all(|(x, y)| x != b && y != c),
                "{b:#x}"
            );
        }
    }

    #[test]
    fn decodes_live_smile_samples_through_both_code_pages() {
        let samples: [(&[u8], &str); 5] = [
            (b"Doanh Thu Th\xF8c \xA8n", "Doanh Thu Thức ăn"),
            (b"\xA8n s\xB8ng kh\xB8ch ph\xDFng", "ăn sáng khách phòng"),
            (b"M\xE3n ch\xDDnh", "Món chính"),
            (b"C\xACm tr\xF8ng", "Cơm trứng"),
            (b"Ch\xCC \xAE\xE7 \xAEen", "Chè đỗ đen"),
        ];
        for (raw, want) in samples {
            assert_eq!(decode_bytes(raw), want);
            for cp in [CodePage::Cp1252, CodePage::Cp1258] {
                assert_eq!(decode(&through(raw, cp), cp), want, "{cp:?}");
            }
        }
    }

    #[test]
    fn every_letter_round_trips_through_both_code_pages() {
        let mut all = String::new();
        for (_, c) in &LETTERS {
            all.push(*c);
        }
        for (_, _, upper) in &PAIRS {
            all.extend(upper.iter());
        }
        let raw = encode(&all).unwrap();
        assert_eq!(decode_bytes(&raw), all);
        for cp in [CodePage::Cp1252, CodePage::Cp1258] {
            let sent = encode_for(&all, cp).unwrap();
            assert_eq!(through(&raw, cp), sent);
            assert_eq!(decode(&sent, cp), all, "{cp:?}");
        }
    }

    #[test]
    fn cp1258_combining_marks_are_tcvn3_bytes_not_accents() {
        // 0xCC è, 0xD2 ề, 0xDE ị, 0xEC ỡ, 0xF2 ũ arrive as bare combining marks.
        assert_eq!(
            decode("\u{300}\u{309}\u{303}\u{301}\u{323}", CodePage::Cp1258),
            "èềịỡũ"
        );
        // Through cp1252 those code points are not single bytes: left alone.
        assert_eq!(decode("e\u{301}", CodePage::Cp1252), "e\u{301}");
        assert_eq!(CodePage::from_number(1258), Some(CodePage::Cp1258));
        assert_eq!(CodePage::from_number(932), None);
    }

    #[test]
    fn uppercase_pairs_fold_and_padding_is_trimmed() {
        assert_eq!(decode_bytes(b"A\xB5N"), "ÀN");
        assert_eq!(decode_bytes(b"\xA7\xA2\xCAT"), "ĐẤT");
        assert_eq!(decode_bytes(b"H\xF1\xA0\xA0"), "Hủ  ");
        assert_eq!(decode_column("H\u{F1}\u{A0}\u{A0}", CodePage::Cp1252), "Hủ");
        assert_eq!(decode_column("100001      ", CodePage::Cp1258), "100001");
        assert_eq!(decode_column("   ", CodePage::Cp1252), "");
        assert_eq!(encode("ĐẤT").unwrap(), b"\xA7\xA2\xCAT");
    }

    #[test]
    fn unicode_text_and_foreign_symbols_pass_through() {
        assert_eq!(decode("Phở € 日本", CodePage::Cp1252), "Phở € 日本");
        assert_eq!(decode("Phở", CodePage::Cp1258), "Phở");
        // A byte TCVN3 leaves undefined keeps the code page's character.
        assert_eq!(decode("\u{102}", CodePage::Cp1258), "Ă");
        assert_eq!(decode_bytes(b"\xC3"), "\u{C3}");
        assert_eq!(
            encode("Giá 5€"),
            Err(Error::InvalidCharacter {
                index: 6, ch: '€'
            })
        );
        // Decomposed input encodes like precomposed.
        assert_eq!(encode("Pho\u{31B}\u{309}").unwrap(), b"Ph\xEB");
    }
}
