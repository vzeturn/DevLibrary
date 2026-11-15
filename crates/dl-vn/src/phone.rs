//! Vietnamese phone numbers: parsing, normalisation to national and E.164 form, and the
//! carrier or area a number was allocated to.
//!
//! Numbering plan (country code 84, trunk prefix 0), from
//! [Telephone numbers in Vietnam](https://en.wikipedia.org/wiki/Telephone_numbers_in_Vietnam):
//!
//! - **Mobile**: 9 digits after the trunk 0, `0xx xxx xxx`, where the 2-digit network code
//!   is 3x, 5x, 7x, 8x or 9x. The former 11-digit ranges (012x, 016x, 018x, 0199) were
//!   converted to 10 digits from 15 September 2018 and are rejected.
//! - **Landline**: 10 digits after the trunk 0: area code `24` (Hà Nội) or `28`
//!   (Hồ Chí Minh City) with an 8-digit subscriber number, or a 3-digit `2xx` provincial
//!   code with a 7-digit subscriber number. The 63 area codes are the Wikipedia table.
//!
//! Network codes per carrier:
//!
//! | Carrier | Codes | Source |
//! |---|---|---|
//! | Viettel | 032–039, 086, 095, 096, 097, 098 | Wikipedia; 095 won at auction on 10 July 2026 ([Báo Tin tức](https://baotintuc.vn/dien-tu-vien-thong/dau-so-di-dong-095-cuoi-cung-duoc-dua-ra-dau-gia-thanh-cong-20260710191538115.htm)) |
//! | VinaPhone | 081–085, 088, 091, 094 | Wikipedia |
//! | MobiFone | 070, 076–079, 089, 090, 093 | Wikipedia |
//! | Vietnamobile | 052, 056, 058, 092 | Wikipedia |
//! | Gmobile | 059, 099 | Wikipedia |
//! | iTel (Indochina Telecom) | 087 | Wikipedia, [VnExpress](https://e.vnexpress.net/news/tech/vietnam-innovation/new-virtual-carrier-enters-vietnam-telecom-market-5109905.html) |
//! | Wintel (formerly Reddi) | 055 | Wikipedia, VnExpress |
//!
//! Limits: subscribers can move to another carrier and keep their number, so
//! [`Phone::carrier`] is the carrier the code was allocated to, not necessarily today's
//! network. Virtual operators that use part of a host carrier's range (Local on 089, VNSKY
//! on 077 and FPT on 0775, per VnExpress) are reported as the host. Short codes, 1800/1900
//! numbers, the 069 code of the defence and public security ministries and VSAT numbers are
//! not accepted.
//!
//! ```
//! use dl_vn::phone::{Carrier, Kind, normalize_phone};
//!
//! let p = normalize_phone("(+84)912-345-678").unwrap();
//! assert_eq!(p.national(), "0912345678");
//! assert_eq!(p.e164(), "+84912345678");
//! assert_eq!(p.carrier(), Some(Carrier::Vinaphone));
//!
//! let office = normalize_phone("024 3825 1234").unwrap();
//! assert_eq!(office.kind(), Kind::Landline);
//! assert_eq!(office.area_name(), Some("Hà Nội"));
//! ```

use crate::{Error, ascii_str};
use alloc::string::String;
use core::fmt;
use core::str::FromStr;

/// Mobile network operator a number range is allocated to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Carrier {
    /// Viettel.
    Viettel,
    /// VinaPhone (VNPT).
    Vinaphone,
    /// MobiFone.
    Mobifone,
    /// Vietnamobile.
    Vietnamobile,
    /// Gmobile (Gtel Mobile).
    Gmobile,
    /// iTel (Indochina Telecom), a virtual network operator.
    Itel,
    /// Wintel, formerly Reddi.
    Wintel,
}

impl Carrier {
    /// Brand name as the operator writes it.
    pub fn name(self) -> &'static str {
        match self {
            Carrier::Viettel => "Viettel",
            Carrier::Vinaphone => "VinaPhone",
            Carrier::Mobifone => "MobiFone",
            Carrier::Vietnamobile => "Vietnamobile",
            Carrier::Gmobile => "Gmobile",
            Carrier::Itel => "iTel",
            Carrier::Wintel => "Wintel",
        }
    }
}

/// Mobile network codes (the two digits after the trunk 0), sorted.
#[rustfmt::skip]
const MOBILE: [(u8, Carrier); 37] = [
    (32, Carrier::Viettel), (33, Carrier::Viettel), (34, Carrier::Viettel),
    (35, Carrier::Viettel), (36, Carrier::Viettel), (37, Carrier::Viettel),
    (38, Carrier::Viettel), (39, Carrier::Viettel),
    (52, Carrier::Vietnamobile), (55, Carrier::Wintel), (56, Carrier::Vietnamobile),
    (58, Carrier::Vietnamobile), (59, Carrier::Gmobile),
    (70, Carrier::Mobifone), (76, Carrier::Mobifone), (77, Carrier::Mobifone),
    (78, Carrier::Mobifone), (79, Carrier::Mobifone),
    (81, Carrier::Vinaphone), (82, Carrier::Vinaphone), (83, Carrier::Vinaphone),
    (84, Carrier::Vinaphone), (85, Carrier::Vinaphone), (86, Carrier::Viettel),
    (87, Carrier::Itel), (88, Carrier::Vinaphone), (89, Carrier::Mobifone),
    (90, Carrier::Mobifone), (91, Carrier::Vinaphone), (92, Carrier::Vietnamobile),
    (93, Carrier::Mobifone), (94, Carrier::Vinaphone), (95, Carrier::Viettel),
    (96, Carrier::Viettel), (97, Carrier::Viettel), (98, Carrier::Viettel),
    (99, Carrier::Gmobile),
];

/// Landline area codes (digits after the trunk 0) and the province each is assigned to, from
/// the Wikipedia table. Names are the 63 provinces before the 2025 merger.
#[rustfmt::skip]
const AREAS: [(u16, &str); 63] = [
    (24, "Hà Nội"), (28, "Hồ Chí Minh"),
    (203, "Quảng Ninh"), (204, "Bắc Giang"), (205, "Lạng Sơn"), (206, "Cao Bằng"),
    (207, "Tuyên Quang"), (208, "Thái Nguyên"), (209, "Bắc Kạn"),
    (210, "Phú Thọ"), (211, "Vĩnh Phúc"), (212, "Sơn La"), (213, "Lai Châu"),
    (214, "Lào Cai"), (215, "Điện Biên"), (216, "Yên Bái"), (218, "Hòa Bình"),
    (219, "Hà Giang"),
    (220, "Hải Dương"), (221, "Hưng Yên"), (222, "Bắc Ninh"), (225, "Hải Phòng"),
    (226, "Hà Nam"), (227, "Thái Bình"), (228, "Nam Định"), (229, "Ninh Bình"),
    (232, "Quảng Bình"), (233, "Quảng Trị"), (234, "Thừa Thiên Huế"), (235, "Quảng Nam"),
    (236, "Đà Nẵng"), (237, "Thanh Hóa"), (238, "Nghệ An"), (239, "Hà Tĩnh"),
    (251, "Đồng Nai"), (252, "Bình Thuận"), (254, "Bà Rịa - Vũng Tàu"), (255, "Quảng Ngãi"),
    (256, "Bình Định"), (257, "Phú Yên"), (258, "Khánh Hòa"), (259, "Ninh Thuận"),
    (260, "Kon Tum"), (261, "Đắk Nông"), (262, "Đắk Lắk"), (263, "Lâm Đồng"),
    (269, "Gia Lai"),
    (270, "Vĩnh Long"), (271, "Bình Phước"), (272, "Long An"), (273, "Tiền Giang"),
    (274, "Bình Dương"), (275, "Bến Tre"), (276, "Tây Ninh"), (277, "Đồng Tháp"),
    (290, "Cà Mau"), (291, "Bạc Liêu"), (292, "Cần Thơ"), (293, "Hậu Giang"),
    (294, "Trà Vinh"), (296, "An Giang"), (297, "Kiên Giang"), (299, "Sóc Trăng"),
];

fn mobile_carrier(code: u8) -> Option<Carrier> {
    MOBILE
        .iter()
        .find(|&&(c, _)| c == code)
        .map(|&(_, carrier)| carrier)
}

/// Kind of number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Mobile number, 10 digits in national form.
    Mobile,
    /// Fixed line, 11 digits in national form.
    Landline,
}

/// A validated Vietnamese phone number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Phone {
    /// National significant number (without trunk 0), ASCII digits.
    nsn: [u8; 10],
    len: u8,
    kind: Kind,
}

/// Parses and validates a phone number. Accepted forms:
///
/// - national: `0912345678`, `0912 345 678`, `024 3825 1234`;
/// - international: `+84912345678`, `+84 912 345 678`, `84912345678`, `0084912345678`,
///   `(+84)912-345-678`, and `+84 (0) 912 345 678` with a redundant trunk 0.
///
/// Spaces, `-`, `.`, `(` and `)` are ignored; `+` is allowed only before the first digit.
/// A number without the trunk 0 or the country code (`912345678`) is rejected as ambiguous.
///
/// Errors: [`Error::Empty`], [`Error::InvalidCharacter`], [`Error::InvalidLength`],
/// [`Error::UnknownCode`] for an unallocated mobile network or area code, and
/// [`Error::Unsupported`] for a foreign country code or a number without a prefix.
pub fn normalize_phone(s: &str) -> Result<Phone, Error> {
    let mut digits = [0u8; 16];
    let mut n = 0usize;
    let mut plus = false;
    for (index, ch) in s.char_indices() {
        match ch {
            '0'..='9' => {
                if let Some(slot) = digits.get_mut(n) {
                    *slot = ch as u8;
                }
                n += 1;
            }
            '+' if n == 0 && !plus => plus = true,
            ' ' | '-' | '.' | '(' | ')' => {}
            _ => return Err(Error::InvalidCharacter { index, ch }),
        }
    }
    if n == 0 {
        return Err(Error::Empty);
    }
    if n > digits.len() {
        return Err(Error::InvalidLength { len: n });
    }
    let d = &digits[..n];
    let mut nsn = if plus {
        d.strip_prefix(b"84").ok_or(Error::Unsupported {
            what: "foreign country code",
        })?
    } else if let Some(rest) = d.strip_prefix(b"00") {
        rest.strip_prefix(b"84").ok_or(Error::Unsupported {
            what: "foreign country code",
        })?
    } else if let Some(rest) = d.strip_prefix(b"0") {
        rest
    } else if let Some(rest) = d.strip_prefix(b"84") {
        rest
    } else {
        return Err(Error::Unsupported {
            what: "number without trunk 0 or country code 84",
        });
    };
    if (plus || d.starts_with(b"84") || d.starts_with(b"00"))
        && let Some(rest) = nsn.strip_prefix(b"0")
    {
        // "+84 (0) 912 ..." : a trunk 0 after the country code is redundant.
        nsn = rest;
    }
    classify(nsn, n)
}

fn classify(nsn: &[u8], total: usize) -> Result<Phone, Error> {
    let Some(&first) = nsn.first() else {
        return Err(Error::InvalidLength { len: total });
    };
    let code2 = || u32::from(area_number(nsn, 2));
    let kind = match (first, nsn.len()) {
        (b'3' | b'5' | b'7' | b'8' | b'9', 9) => {
            let code = area_number(nsn, 2) as u8;
            if mobile_carrier(code).is_none() {
                return Err(Error::UnknownCode {
                    code: u32::from(code),
                });
            }
            Kind::Mobile
        }
        (b'2', 10) => {
            if area_of(nsn).is_none() {
                return Err(Error::UnknownCode {
                    code: u32::from(area_number(nsn, 3)),
                });
            }
            Kind::Landline
        }
        (b'0' | b'1' | b'4' | b'6', 9 | 10) => return Err(Error::UnknownCode { code: code2() }),
        _ => return Err(Error::InvalidLength { len: total }),
    };
    let mut buf = [0u8; 10];
    buf[..nsn.len()].copy_from_slice(nsn);
    Ok(Phone {
        nsn: buf,
        len: nsn.len() as u8,
        kind,
    })
}

fn area_number(nsn: &[u8], len: usize) -> u16 {
    nsn.iter()
        .take(len)
        .fold(0, |acc, &b| acc * 10 + u16::from(b - b'0'))
}

/// Area code length and table entry of a 10-digit landline NSN.
fn area_of(nsn: &[u8]) -> Option<(usize, &'static str)> {
    let two = area_number(nsn, 2);
    let (len, code) = if two == 24 || two == 28 {
        (2, two)
    } else {
        (3, area_number(nsn, 3))
    };
    AREAS
        .iter()
        .find(|&&(c, _)| c == code)
        .map(|&(_, name)| (len, name))
}

impl Phone {
    fn digits(&self) -> &[u8] {
        &self.nsn[..usize::from(self.len)]
    }

    /// National format with the trunk 0 and no spaces: `0912345678`, `02438251234`.
    pub fn national(&self) -> String {
        let mut s = String::with_capacity(12);
        s.push('0');
        s.push_str(ascii_str(self.digits()));
        s
    }

    /// E.164 format: `+84912345678`.
    pub fn e164(&self) -> String {
        let mut s = String::with_capacity(14);
        s.push_str("+84");
        s.push_str(ascii_str(self.digits()));
        s
    }

    /// Mobile or landline.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// True for a mobile number.
    pub fn is_mobile(&self) -> bool {
        self.kind == Kind::Mobile
    }

    /// Carrier the mobile network code was allocated to. `None` for landlines.
    pub fn carrier(&self) -> Option<Carrier> {
        match self.kind {
            Kind::Mobile => mobile_carrier((self.nsn[0] - b'0') * 10 + (self.nsn[1] - b'0')),
            Kind::Landline => None,
        }
    }

    /// Landline area code without the trunk 0, e.g. `"24"` or `"236"`.
    pub fn area_code(&self) -> Option<&str> {
        match self.kind {
            Kind::Landline => area_of(self.digits()).map(|(len, _)| ascii_str(&self.nsn[..len])),
            Kind::Mobile => None,
        }
    }

    /// Province the landline area code was assigned to (names before the 2025 merger).
    pub fn area_name(&self) -> Option<&'static str> {
        match self.kind {
            Kind::Landline => area_of(self.digits()).map(|(_, name)| name),
            Kind::Mobile => None,
        }
    }
}

impl fmt::Display for Phone {
    /// Formats as E.164.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("+84")?;
        f.write_str(ascii_str(self.digits()))
    }
}

impl FromStr for Phone {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        normalize_phone(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::ToString;

    #[test]
    fn accepted_forms() {
        for input in [
            "0912 345 678",
            "0912345678",
            "+84 912345678",
            "+84912345678",
            "84912345678",
            "0084912345678",
            "(+84)912-345-678",
            "+84 (0) 912 345 678",
            "0912.345.678",
            "+840912345678",
        ] {
            let p = normalize_phone(input).unwrap_or_else(|e| panic!("{input}: {e}"));
            assert_eq!(p.national(), "0912345678", "{input}");
            assert_eq!(p.e164(), "+84912345678", "{input}");
            assert_eq!(p.to_string(), "+84912345678");
            assert_eq!(p.kind(), Kind::Mobile);
            assert!(p.is_mobile());
            assert_eq!(p.carrier(), Some(Carrier::Vinaphone));
            assert_eq!(p.area_code(), None);
        }
    }

    #[test]
    fn carriers() {
        let cases = [
            ("0987654321", Carrier::Viettel),
            ("0321234567", Carrier::Viettel),
            ("0391234567", Carrier::Viettel),
            ("0861234567", Carrier::Viettel),
            ("0961234567", Carrier::Viettel),
            ("0951234567", Carrier::Viettel),
            ("0811234567", Carrier::Vinaphone),
            ("0881234567", Carrier::Vinaphone),
            ("0941234567", Carrier::Vinaphone),
            ("0701234567", Carrier::Mobifone),
            ("0761234567", Carrier::Mobifone),
            ("0891234567", Carrier::Mobifone),
            ("0901234567", Carrier::Mobifone),
            ("0931234567", Carrier::Mobifone),
            ("0521234567", Carrier::Vietnamobile),
            ("0561234567", Carrier::Vietnamobile),
            ("0581234567", Carrier::Vietnamobile),
            ("0921234567", Carrier::Vietnamobile),
            ("0591234567", Carrier::Gmobile),
            ("0991234567", Carrier::Gmobile),
            ("0871234567", Carrier::Itel),
            ("0551234567", Carrier::Wintel),
        ];
        for (input, carrier) in cases {
            assert_eq!(
                normalize_phone(input).unwrap().carrier(),
                Some(carrier),
                "{input}"
            );
        }
        assert_eq!(Carrier::Vinaphone.name(), "VinaPhone");
        // Every allocated code is a 3x/5x/7x/8x/9x code and appears once.
        let codes: alloc::vec::Vec<u8> = MOBILE.iter().map(|&(c, _)| c).collect();
        assert_eq!(codes.len(), 37);
        assert!(codes.windows(2).all(|w| w[0] < w[1]), "sorted and unique");
        for c in &codes {
            assert!(matches!(c / 10, 3 | 5 | 7 | 8 | 9));
            let p = normalize_phone(&format!("0{c}1234567")).unwrap();
            assert!(p.carrier().is_some());
        }
    }

    #[test]
    fn landlines() {
        let p = normalize_phone("024 3825 1234").unwrap();
        assert_eq!(p.kind(), Kind::Landline);
        assert!(!p.is_mobile());
        assert_eq!(p.national(), "02438251234");
        assert_eq!(p.e164(), "+842438251234");
        assert_eq!(p.area_code(), Some("24"));
        assert_eq!(p.area_name(), Some("Hà Nội"));
        assert_eq!(p.carrier(), None);

        let p = normalize_phone("+84 28 3829 1234").unwrap();
        assert_eq!(
            (p.area_code(), p.area_name()),
            (Some("28"), Some("Hồ Chí Minh"))
        );

        let p = normalize_phone("0236 3821 234").unwrap();
        assert_eq!(
            (p.area_code(), p.area_name()),
            (Some("236"), Some("Đà Nẵng"))
        );
        assert_eq!(p.national(), "02363821234");

        assert_eq!(AREAS.len(), 63);
        for &(code, _) in &AREAS {
            let rest = if code < 100 { "12345678" } else { "1234567" };
            let p = normalize_phone(&format!("0{code}{rest}")).unwrap();
            assert_eq!(p.area_code(), Some(code.to_string().as_str()));
        }
    }

    #[test]
    fn errors() {
        assert_eq!(normalize_phone(""), Err(Error::Empty));
        assert_eq!(normalize_phone(" - "), Err(Error::Empty));
        assert_eq!(
            normalize_phone("abc"),
            Err(Error::InvalidCharacter { index: 0, ch: 'a' })
        );
        assert_eq!(
            normalize_phone("0912+345678"),
            Err(Error::InvalidCharacter { index: 4, ch: '+' })
        );
        assert_eq!(
            normalize_phone("++84912345678"),
            Err(Error::InvalidCharacter { index: 1, ch: '+' })
        );
        assert_eq!(
            normalize_phone("0311234567"),
            Err(Error::UnknownCode { code: 31 })
        );
        assert_eq!(
            normalize_phone("0801234567"),
            Err(Error::UnknownCode { code: 80 })
        );
        assert_eq!(
            normalize_phone("0691234567"),
            Err(Error::UnknownCode { code: 69 })
        );
        assert_eq!(
            normalize_phone("01681234567"),
            Err(Error::UnknownCode { code: 16 })
        );
        assert_eq!(
            normalize_phone("0612345678"),
            Err(Error::UnknownCode { code: 61 })
        );
        assert_eq!(
            normalize_phone("02171234567"),
            Err(Error::UnknownCode { code: 217 })
        );
        assert_eq!(
            normalize_phone("02512345678"),
            Ok(normalize_phone("+842512345678").unwrap())
        );
        assert_eq!(
            normalize_phone("091234567"),
            Err(Error::InvalidLength { len: 9 })
        );
        assert_eq!(
            normalize_phone("09123456789"),
            Err(Error::InvalidLength { len: 11 })
        );
        assert_eq!(
            normalize_phone("0243825123"),
            Err(Error::InvalidLength { len: 10 })
        );
        assert_eq!(
            normalize_phone("01234567890"),
            Err(Error::UnknownCode { code: 12 })
        );
        assert_eq!(normalize_phone("+84"), Err(Error::InvalidLength { len: 2 }));
        assert_eq!(
            normalize_phone("012345678901234567"),
            Err(Error::InvalidLength { len: 18 })
        );
        assert_eq!(
            normalize_phone("+1 650 253 0000"),
            Err(Error::Unsupported {
                what: "foreign country code"
            })
        );
        assert_eq!(
            normalize_phone("0065 6123 4567"),
            Err(Error::Unsupported {
                what: "foreign country code"
            })
        );
        assert!(matches!(
            normalize_phone("912345678"),
            Err(Error::Unsupported { .. })
        ));
    }
}
