//! Căn cước công dân (CCCD): the 12-digit personal identification number ("số định danh cá
//! nhân") printed on citizen ID cards.
//!
//! Structure, from Decree 137/2015/NĐ-CP and the province code table of Circular
//! 07/2016/TT-BCA, as reproduced by
//! [Wikipedia](https://vi.wikipedia.org/wiki/Căn_cước_công_dân_(Việt_Nam)) (the circular's
//! text itself could not be fetched for this release):
//!
//! | Digits | Meaning |
//! |---|---|
//! | 1–3 | Code of the province where the birth was registered, or of the country for a birth registered abroad |
//! | 4 | Century of birth and gender: 0/1 = 20th century male/female, 2/3 = 21st, 4/5 = 22nd, 6/7 = 23rd, 8/9 = 24th |
//! | 5–6 | Last two digits of the birth year |
//! | 7–12 | Random number, 000001–999999 |
//!
//! The century code maps to the years `1900 + 100 × (code / 2)` to `1999 + 100 × (code / 2)`,
//! so code 0 with year digits `99` is 1999 and code 2 with `05` is 2005. Wikipedia words the
//! 20th century as 1901–2000, which differs only for a year ending in `00`: this crate reads
//! code 0/1 with `00` as 1900 and code 2/3 with `00` as 2000.
//!
//! The province table is the 63-province list (codes 001–096); the other codes below 100
//! are marked reserved ("dự trữ") and rejected. Codes 100–999 are accepted without a name:
//! the decree allows a country code there, but no published country code table was found.
//!
//! The number does not change during a person's life (Wikipedia). No official statement on
//! new province codes after the July 2025 merger into 34 provinces could be found, so
//! [`Cccd::province`] returns the province of birth registration as named before the merger,
//! which may no longer exist as a province.
//!
//! There is no check digit, so a structurally valid number may still not be issued.
//!
//! ```
//! use dl_vn::cccd::{Cccd, Gender};
//!
//! let id = Cccd::parse("079203000123").unwrap();
//! assert_eq!(id.province(), Some("Hồ Chí Minh"));
//! assert_eq!(id.gender(), Gender::Male);
//! assert_eq!(id.birth_year(), 2003);
//! ```

use crate::{Error, ascii_str};
use core::fmt;
use core::str::FromStr;

/// Province codes of Circular 07/2016/TT-BCA (63 provinces before the 2025 merger).
#[rustfmt::skip]
const PROVINCES: [(u16, &str); 63] = [
    (1, "Hà Nội"), (2, "Hà Giang"), (4, "Cao Bằng"), (6, "Bắc Kạn"), (8, "Tuyên Quang"),
    (10, "Lào Cai"), (11, "Điện Biên"), (12, "Lai Châu"), (14, "Sơn La"), (15, "Yên Bái"),
    (17, "Hòa Bình"), (19, "Thái Nguyên"), (20, "Lạng Sơn"), (22, "Quảng Ninh"),
    (24, "Bắc Giang"), (25, "Phú Thọ"), (26, "Vĩnh Phúc"), (27, "Bắc Ninh"),
    (30, "Hải Dương"), (31, "Hải Phòng"), (33, "Hưng Yên"), (34, "Thái Bình"),
    (35, "Hà Nam"), (36, "Nam Định"), (37, "Ninh Bình"), (38, "Thanh Hóa"), (40, "Nghệ An"),
    (42, "Hà Tĩnh"), (44, "Quảng Bình"), (45, "Quảng Trị"), (46, "Thừa Thiên Huế"),
    (48, "Đà Nẵng"), (49, "Quảng Nam"), (51, "Quảng Ngãi"), (52, "Bình Định"),
    (54, "Phú Yên"), (56, "Khánh Hòa"), (58, "Ninh Thuận"), (60, "Bình Thuận"),
    (62, "Kon Tum"), (64, "Gia Lai"), (66, "Đắk Lắk"), (67, "Đắk Nông"), (68, "Lâm Đồng"),
    (70, "Bình Phước"), (72, "Tây Ninh"), (74, "Bình Dương"), (75, "Đồng Nai"),
    (77, "Bà Rịa - Vũng Tàu"), (79, "Hồ Chí Minh"), (80, "Long An"), (82, "Tiền Giang"),
    (83, "Bến Tre"), (84, "Trà Vinh"), (86, "Vĩnh Long"), (87, "Đồng Tháp"), (89, "An Giang"),
    (91, "Kiên Giang"), (92, "Cần Thơ"), (93, "Hậu Giang"), (94, "Sóc Trăng"),
    (95, "Bạc Liêu"), (96, "Cà Mau"),
];

/// Looks up a province by its 3-digit code (1 = Hà Nội, 79 = Hồ Chí Minh).
pub fn province_name(code: u16) -> Option<&'static str> {
    PROVINCES
        .iter()
        .find(|&&(c, _)| c == code)
        .map(|&(_, name)| name)
}

/// Gender encoded in the 4th digit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gender {
    /// Even century code.
    Male,
    /// Odd century code.
    Female,
}

/// A structurally valid 12-digit personal identification number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cccd {
    digits: [u8; 12],
}

impl Cccd {
    /// Parses exactly 12 ASCII digits.
    ///
    /// Errors: [`Error::Empty`], [`Error::InvalidCharacter`], [`Error::InvalidLength`],
    /// [`Error::UnknownCode`] for a reserved province code, [`Error::InvalidComponent`] for
    /// the serial `000000`.
    pub fn parse(s: &str) -> Result<Self, Error> {
        if s.is_empty() {
            return Err(Error::Empty);
        }
        if let Some((index, ch)) = s.char_indices().find(|&(_, c)| !c.is_ascii_digit()) {
            return Err(Error::InvalidCharacter { index, ch });
        }
        let bytes = s.as_bytes();
        let digits: [u8; 12] = bytes
            .try_into()
            .map_err(|_| Error::InvalidLength { len: bytes.len() })?;
        let id = Self { digits };
        let code = id.place_code();
        if code < 100 && province_name(code).is_none() {
            return Err(Error::UnknownCode {
                code: u32::from(code),
            });
        }
        if &digits[6..] == b"000000" {
            return Err(Error::InvalidComponent);
        }
        Ok(id)
    }

    /// The 12 digits.
    pub fn as_str(&self) -> &str {
        ascii_str(&self.digits)
    }

    /// The first three digits as a number: a province code (below 100), otherwise a code
    /// outside the province table (see the module documentation).
    pub fn place_code(&self) -> u16 {
        self.digits[..3]
            .iter()
            .fold(0, |acc, &d| acc * 10 + u16::from(d - b'0'))
    }

    /// Province of birth registration, as named before the 2025 merger. `None` for a place
    /// code of 100 or more.
    pub fn province(&self) -> Option<&'static str> {
        province_name(self.place_code())
    }

    fn century_code(&self) -> u16 {
        u16::from(self.digits[3] - b'0')
    }

    /// Gender from the 4th digit.
    pub fn gender(&self) -> Gender {
        if self.century_code().is_multiple_of(2) {
            Gender::Male
        } else {
            Gender::Female
        }
    }

    /// Century of birth: 20 for 1900–1999, 21 for 2000–2099, up to 24.
    pub fn century(&self) -> u8 {
        20 + (self.century_code() / 2) as u8
    }

    /// Full birth year, e.g. 1999 or 2005.
    pub fn birth_year(&self) -> u16 {
        let yy = u16::from(self.digits[4] - b'0') * 10 + u16::from(self.digits[5] - b'0');
        1900 + 100 * (self.century_code() / 2) + yy
    }

    /// The last six (random) digits.
    pub fn serial(&self) -> &str {
        ascii_str(&self.digits[6..])
    }
}

impl fmt::Display for Cccd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Cccd {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn decodes_fields() {
        let id = Cccd::parse("001099012345").unwrap();
        assert_eq!(id.as_str(), "001099012345");
        assert_eq!(id.place_code(), 1);
        assert_eq!(id.province(), Some("Hà Nội"));
        assert_eq!(id.gender(), Gender::Male);
        assert_eq!(id.century(), 20);
        assert_eq!(id.birth_year(), 1999);
        assert_eq!(id.serial(), "012345");

        let id = Cccd::parse("096305000001").unwrap();
        assert_eq!(id.province(), Some("Cà Mau"));
        assert_eq!(id.gender(), Gender::Female);
        assert_eq!(id.century(), 21);
        assert_eq!(id.birth_year(), 2005);

        let id: Cccd = "048000999999".parse().unwrap();
        assert_eq!(id.province(), Some("Đà Nẵng"));
        assert_eq!((id.gender(), id.birth_year()), (Gender::Male, 1900));
        assert_eq!(format!("{id}"), "048000999999");

        let id = Cccd::parse("048100999999").unwrap();
        assert_eq!((id.gender(), id.birth_year()), (Gender::Female, 1900));

        let id = Cccd::parse("079200000001").unwrap();
        assert_eq!(
            (id.gender(), id.century(), id.birth_year()),
            (Gender::Male, 21, 2000)
        );
    }

    #[test]
    fn published_decoder_example() {
        // README example of github.com/duydev/vietnam-personal-id-number-explain.
        let id = Cccd::parse("037153000257").unwrap();
        assert_eq!(id.province(), Some("Ninh Bình"));
        assert_eq!(
            (id.gender(), id.century(), id.birth_year()),
            (Gender::Female, 20, 1953)
        );
        assert_eq!(id.serial(), "000257");
    }

    #[test]
    fn every_century_code() {
        let expected = [
            (Gender::Male, 20, 1987),
            (Gender::Female, 20, 1987),
            (Gender::Male, 21, 2087),
            (Gender::Female, 21, 2087),
            (Gender::Male, 22, 2187),
            (Gender::Female, 22, 2187),
            (Gender::Male, 23, 2287),
            (Gender::Female, 23, 2287),
            (Gender::Male, 24, 2387),
            (Gender::Female, 24, 2387),
        ];
        for (code, &(gender, century, year)) in expected.iter().enumerate() {
            let id = Cccd::parse(&format!("037{code}87000257")).unwrap();
            assert_eq!(
                (id.gender(), id.century(), id.birth_year()),
                (gender, century, year)
            );
            assert_eq!(id.province(), Some("Ninh Bình"));
        }
    }

    #[test]
    fn province_table() {
        assert_eq!(PROVINCES.len(), 63);
        for pair in PROVINCES.windows(2) {
            assert!(pair[0].0 < pair[1].0, "table must be sorted and unique");
        }
        for &(code, _) in &PROVINCES {
            assert!(Cccd::parse(&format!("{code:03}090000001")).is_ok());
        }
        assert_eq!(province_name(79), Some("Hồ Chí Minh"));
        assert_eq!(province_name(3), None);
    }

    #[test]
    fn codes_outside_the_province_table() {
        let id = Cccd::parse("175090000001").unwrap();
        assert_eq!(id.province(), None);
        assert_eq!(id.place_code(), 175);
    }

    #[test]
    fn errors() {
        assert_eq!(Cccd::parse(""), Err(Error::Empty));
        assert_eq!(
            Cccd::parse("00109901234"),
            Err(Error::InvalidLength { len: 11 })
        );
        assert_eq!(
            Cccd::parse("0010990123456"),
            Err(Error::InvalidLength { len: 13 })
        );
        assert_eq!(
            Cccd::parse("001 099012345"),
            Err(Error::InvalidCharacter { index: 3, ch: ' ' })
        );
        assert_eq!(
            Cccd::parse("00109901234５"),
            Err(Error::InvalidCharacter {
                index: 11,
                ch: '５'
            })
        );
        assert_eq!(
            Cccd::parse("003099012345"),
            Err(Error::UnknownCode { code: 3 })
        );
        assert_eq!(
            Cccd::parse("000099012345"),
            Err(Error::UnknownCode { code: 0 })
        );
        assert_eq!(
            Cccd::parse("028099012345"),
            Err(Error::UnknownCode { code: 28 })
        );
        assert_eq!(Cccd::parse("001099000000"), Err(Error::InvalidComponent));
    }
}
