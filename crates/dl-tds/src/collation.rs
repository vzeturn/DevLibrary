//! Collations and CHAR/VARCHAR decoding.
//!
//! The code page is chosen exactly like go-mssqldb's `internal/cp`: first by SQL sort id
//! (the `SQL_*` collations), then by Windows LCID, defaulting to 1252.

use crate::Error;
use crate::codepages::*;

/// A SQL Server collation as sent on the wire: LCID + flags + version (4 bytes) and the
/// SQL sort id (1 byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Collation {
    /// LCID (bits 0–19), comparison flags (bits 20–27) and version (bits 28–31).
    pub lcid_and_flags: u32,
    /// SQL sort id; 0 for Windows collations.
    pub sort_id: u8,
}

/// How CHAR/VARCHAR bytes of a collation are turned into text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Charset {
    /// A single-byte code page with its 256-entry table.
    Single(u16, &'static [u16; 256]),
    /// A double-byte code page dl-tds does not embed.
    Double(u16),
    /// No code page: go-mssqldb passes the bytes through as a Go string; dl-tds decodes
    /// them as UTF-8 (invalid sequences become U+FFFD).
    Utf8,
}

const F_UTF8: u32 = 1 << 26;

impl Collation {
    /// Parses the 5-byte wire form.
    pub(crate) fn from_bytes(b: [u8; 5]) -> Collation {
        Collation {
            lcid_and_flags: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            sort_id: b[4],
        }
    }

    /// The 5-byte wire form.
    #[cfg(test)]
    pub(crate) fn to_bytes(self) -> [u8; 5] {
        let l = self.lcid_and_flags.to_le_bytes();
        [l[0], l[1], l[2], l[3], self.sort_id]
    }

    /// The Windows locale id.
    pub fn lcid(self) -> u32 {
        self.lcid_and_flags & 0x000F_FFFF
    }

    /// `true` for the SQL Server 2019+ `_UTF8` collations.
    pub fn is_utf8(self) -> bool {
        self.lcid_and_flags & F_UTF8 != 0
    }

    /// The Windows code page CHAR/VARCHAR data of this collation is stored in, or `None`
    /// for UTF-8 / Unicode-only locales.
    pub fn code_page(self) -> Option<u16> {
        match self.charset() {
            Charset::Single(cp, _) | Charset::Double(cp) => Some(cp),
            Charset::Utf8 => None,
        }
    }

    pub(crate) fn charset(self) -> Charset {
        if self.is_utf8() {
            return Charset::Utf8;
        }
        let single = |cp: u16| {
            let t: &'static [u16; 256] = match cp {
                437 => &CP437,
                850 => &CP850,
                874 => &CP874,
                1250 => &CP1250,
                1251 => &CP1251,
                1253 => &CP1253,
                1254 => &CP1254,
                1255 => &CP1255,
                1256 => &CP1256,
                1257 => &CP1257,
                1258 => &CP1258,
                _ => &CP1252,
            };
            Charset::Single(cp, t)
        };
        // http://msdn.microsoft.com/en-us/library/ms144250.aspx (SQL collations).
        match self.sort_id {
            30..=34 => return single(437),
            40..=42 | 44 | 49 | 55..=61 => return single(850),
            50..=54 | 71..=75 => return single(1252),
            80..=96 => return single(1250),
            104..=108 => return single(1251),
            112..=114 | 121 | 124 => return single(1253),
            128..=130 => return single(1254),
            136..=138 => return single(1255),
            144..=146 => return single(1256),
            152..=160 => return single(1257),
            183..=186 => return single(1252),
            192 | 193 | 200 => return Charset::Double(932),
            194 | 195 | 201 => return Charset::Double(949),
            196 | 197 | 202 => return Charset::Double(950),
            198 | 199 | 203 => return Charset::Double(936),
            204..=206 => return single(874),
            210..=217 => return single(1252),
            _ => {}
        }
        match self.lcid() {
            0x001e | 0x041e => single(874),
            0x0411 | 0x10411 | 0x40411 => Charset::Double(932),
            0x0804 | 0x1004 | 0x20804 => Charset::Double(936),
            0x0012 | 0x0412 => Charset::Double(949),
            0x0404 | 0x1404 | 0x0c04 | 0x7c04 | 0x30404 | 0x21404 => Charset::Double(950),
            0x041c | 0x041a | 0x0405 | 0x040e | 0x104e | 0x0415 | 0x0418 | 0x041b | 0x0424
            | 0x1040e | 0x0442 | 0x081A | 0x141A => single(1250),
            0x0423 | 0x0402 | 0x042f | 0x0419 | 0x0c1a | 0x0422 | 0x043f | 0x0444 | 0x082c
            | 0x046D | 0x0485 | 0x201A => single(1251),
            0x0408 => single(1253),
            0x041f | 0x042c | 0x0443 => single(1254),
            0x040d => single(1255),
            0x0401 | 0x0801 | 0xc01 | 0x1001 | 0x1401 | 0x1801 | 0x1c01 | 0x2001 | 0x2401
            | 0x2801 | 0x2c01 | 0x3001 | 0x3401 | 0x3801 | 0x3c01 | 0x4001 | 0x0429 | 0x0420
            | 0x0480 | 0x048C => single(1256),
            0x0425 | 0x0426 | 0x0427 | 0x0827 => single(1257),
            0x042a => single(1258),
            0x0439 | 0x045a | 0x0465 | 0x043A | 0x0445 | 0x044D | 0x0451 | 0x0453 | 0x0454
            | 0x0461 | 0x0463 | 0x0481 => Charset::Utf8,
            _ => single(1252),
        }
    }

    /// Decodes CHAR/VARCHAR/TEXT bytes stored under this collation, byte-for-byte like
    /// go-mssqldb's `cp.CharsetToUTF8`.
    ///
    /// # Errors
    /// [`Error::UnsupportedCodePage`] for the double-byte code pages 932/936/949/950.
    pub fn decode(self, bytes: &[u8]) -> Result<String, Error> {
        match self.charset() {
            Charset::Single(_, table) => Ok(decode_single(table, bytes)),
            Charset::Double(code_page) => Err(Error::UnsupportedCodePage { code_page }),
            Charset::Utf8 => Ok(String::from_utf8_lossy(bytes).into_owned()),
        }
    }
}

fn decode_single(table: &[u16; 256], bytes: &[u8]) -> String {
    if bytes.is_ascii() {
        // Every table maps 0x00-0x7F to itself (checked by a unit test).
        return bytes.iter().map(|&b| b as char).collect();
    }
    let mut s = String::with_capacity(bytes.len() + bytes.len() / 2);
    for &b in bytes {
        // The tables hold BMP scalar values only (no surrogates), so this never fails.
        s.push(char::from_u32(table[b as usize] as u32).unwrap_or(char::REPLACEMENT_CHARACTER));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SQL_Latin1_General_CP1_CI_AS as sent by SQL Server: LCID 0x0409, flags 0xD0, sort 52.
    pub(crate) const SQL_LATIN1: Collation = Collation {
        lcid_and_flags: 0x00D0_0409,
        sort_id: 52,
    };
    /// Vietnamese_CI_AS: LCID 0x042A, sort 0.
    pub(crate) const VIETNAMESE: Collation = Collation {
        lcid_and_flags: 0x00D0_042A,
        sort_id: 0,
    };

    #[test]
    fn code_page_selection() {
        assert_eq!(SQL_LATIN1.code_page(), Some(1252));
        assert_eq!(VIETNAMESE.code_page(), Some(1258));
        let c = |lcid: u32, sort: u8| Collation {
            lcid_and_flags: lcid,
            sort_id: sort,
        };
        assert_eq!(c(0x0409, 0).code_page(), Some(1252)); // Latin1_General
        assert_eq!(c(0x0409, 30).code_page(), Some(437)); // SQL_Latin1_General_CP437_BIN
        assert_eq!(c(0x0409, 40).code_page(), Some(850));
        assert_eq!(c(0x041E, 0).code_page(), Some(874)); // Thai
        assert_eq!(c(0x0419, 0).code_page(), Some(1251)); // Cyrillic
        assert_eq!(c(0x0405, 0).code_page(), Some(1250)); // Czech
        assert_eq!(c(0x0408, 0).code_page(), Some(1253));
        assert_eq!(c(0x041F, 0).code_page(), Some(1254));
        assert_eq!(c(0x040D, 0).code_page(), Some(1255));
        assert_eq!(c(0x0401, 0).code_page(), Some(1256));
        assert_eq!(c(0x0425, 0).code_page(), Some(1257));
        assert_eq!(c(0x0411, 0).code_page(), Some(932));
        assert_eq!(c(0x0439, 0).code_page(), None); // Hindi: Unicode-only
        assert_eq!(c(0x0409 | F_UTF8, 0).code_page(), None);
        assert_eq!(Collation::default().code_page(), Some(1252));
        let b = VIETNAMESE.to_bytes();
        assert_eq!(Collation::from_bytes(b), VIETNAMESE);
    }

    #[test]
    fn tables_are_ascii_compatible() {
        for t in [
            &CP437, &CP850, &CP874, &CP1250, &CP1251, &CP1252, &CP1253, &CP1254, &CP1255, &CP1256,
            &CP1257, &CP1258,
        ] {
            for b in 0..0x80u16 {
                assert_eq!(t[b as usize], b);
            }
            assert!(t.iter().all(|&u| !(0xD800..0xE000).contains(&u)));
        }
    }

    /// Spot checks against Python: `bytes(range(0x80, 0x100)).decode('cp1252', 'replace')`.
    #[test]
    fn cp1252_and_cp1258_spot_checks() {
        let s = SQL_LATIN1
            .decode(&[0x80, 0x81, 0x8A, 0x9F, 0xA0, 0xC0, 0xE9, 0xFF])
            .unwrap();
        assert_eq!(
            s,
            "\u{20AC}\u{FFFD}\u{0160}\u{0178}\u{00A0}\u{00C0}\u{00E9}\u{00FF}"
        );
        // cp1258: combining tone marks and Vietnamese letters.
        let s = VIETNAMESE
            .decode(&[0xC3, 0xD0, 0xD2, 0xDE, 0xEC, 0xF2, 0xF5, 0xFD, 0xFE, 0x81])
            .unwrap();
        assert_eq!(
            s,
            "\u{0102}\u{0110}\u{0309}\u{0303}\u{0301}\u{0323}\u{01A1}\u{01B0}\u{20AB}\u{FFFD}"
        );
        // cp874 (Thai) undefined 0x81 and THAI CHARACTER KO KAI.
        let thai = Collation {
            lcid_and_flags: 0x041E,
            sort_id: 0,
        };
        assert_eq!(
            thai.decode(&[0xA1, 0x81, 0x80]).unwrap(),
            "\u{0E01}\u{FFFD}\u{20AC}"
        );
        // cp437 box drawing.
        let oem = Collation {
            lcid_and_flags: 0x0409,
            sort_id: 30,
        };
        assert_eq!(oem.decode(&[0xC9, 0xB0]).unwrap(), "\u{2554}\u{2591}");
        assert_eq!(SQL_LATIN1.decode(b"plain").unwrap(), "plain");
    }

    #[test]
    fn dbcs_is_reported() {
        let jp = Collation {
            lcid_and_flags: 0x0411,
            sort_id: 0,
        };
        assert!(matches!(
            jp.decode(b"x"),
            Err(Error::UnsupportedCodePage { code_page: 932 })
        ));
        let utf8 = Collation {
            lcid_and_flags: 0x0409 | F_UTF8,
            sort_id: 0,
        };
        assert_eq!(utf8.decode("chào".as_bytes()).unwrap(), "chào");
    }
}
