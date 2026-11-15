//! Mã số thuế (MST): Vietnamese tax identification numbers.
//!
//! Three forms are accepted by [`TaxCode::parse`]:
//!
//! - **10 digits**, an enterprise or organisation (`N1…N10`). For an enterprise this is also
//!   its enterprise registration number. `N1N2` is a range number, `N3…N9` a serial that is
//!   never `0000000`, and `N10` a check digit.
//! - **13 digits**, a dependent unit such as a branch: the parent's 10 digits plus a 3-digit
//!   suffix `001`–`999`, written `0101243150-001` or without the dash.
//! - **12 digits**, an individual, household or household business. Since 1 July 2025 the
//!   personal identification number (the 12-digit CCCD number) is used as the tax code,
//!   under Article 35(7) of the Law on Tax Administration 38/2019/QH14 and Articles 5 and 7
//!   of Circular 86/2024/TT-BTC, effective per its Article 39
//!   ([Báo Bắc Kạn, via vietnam.vn](https://www.vietnam.vn/de/su-dung-so-dinh-danh-ca-nhan-thay-cho-ma-so-thue-tu-01-7-2025-luu-y-voi-nguoi-nop-thue),
//!   [Government news portal](https://en.baochinhphu.vn/citizen-identification-cards-replace-social-insurance-codes-from-august-1-11125080616115797.htm)).
//!   It is validated with [`Cccd::parse`]; there is no check digit.
//!
//! Check digit of the 10-digit form:
//! `N10 = 10 − (31·N1 + 29·N2 + 23·N3 + 19·N4 + 17·N5 + 13·N6 + 7·N7 + 5·N8 + 3·N9) mod 11`.
//! When the remainder is 0 the result is 10, which is not a digit, so no valid code has that
//! prefix. The tax authority does not publish the formula in a legal text that could be
//! found. It is the formula of
//! [python-stdnum](https://github.com/arthurdejong/python-stdnum/blob/master/stdnum/vn/mst.py),
//! together with its other rules (serial not `0000000`, branch suffix not `000`), and the
//! tests check it against these published codes:
//!
//! | Code | Holder | Source |
//! |---|---|---|
//! | `0300588569` | Vinamilk | [2026 AGM minutes](https://static2.vietstock.vn/vietstock/2026/4/24/20260422___vnm___minutes_and_resolution_of_the_2026_agm.pdf) |
//! | `0100109106` | Viettel Group | [viettel.com.vn contact page](https://viettel.com.vn/en/contact/) |
//! | `0100109106-475` | Viettel Cyber Security (branch) | [company page](https://services.viettelcybersecurity.com/about-us) |
//! | `0101248141` | FPT Corporation | [enterprise registration certificate](https://static2.vietstock.vn/vietstock/2025/8/27/20250826___fpt___change_in_the_60th_certificate_of_enterprise_registration.pdf) |
//! | `0100112437` | Vietcombank | [2023 annual report](https://portal.vietcombank.com.vn/VCBDigital/2023/BCTN/en/about-vietcombank/) |
//! | `0100233488`, `0314409058-002` | python-stdnum examples | python-stdnum |
//!
//! Legacy 10-digit personal tax codes issued before July 2025 have the same shape as
//! enterprise codes and parse as [`TaxCode::Enterprise10`].
//!
//! ```
//! use dl_vn::tax::TaxCode;
//!
//! let code = TaxCode::parse("0101243150-001").unwrap();
//! assert!(matches!(code, TaxCode::Branch13(_)));
//! assert_eq!(code.to_string(), "0101243150-001");
//! assert_eq!(code.parent().unwrap().as_str(), "0101243150");
//! assert!(TaxCode::parse("0101243151").is_err());
//! ```

use crate::cccd::Cccd;
use crate::{Error, ascii_str};
use core::fmt;
use core::str::FromStr;

const WEIGHTS: [u32; 9] = [31, 29, 23, 19, 17, 13, 7, 5, 3];

/// Computes the MST check digit (`N10`) from the first nine digits, given as values 0–9.
///
/// Returns `None` when the weighted sum is a multiple of 11 (no 10-digit code exists) or
/// when an input value is not a digit.
///
/// ```
/// assert_eq!(dl_vn::tax::check_digit(&[0, 3, 0, 0, 5, 8, 8, 5, 6]), Some(9));
/// ```
pub fn check_digit(first9: &[u8; 9]) -> Option<u8> {
    if first9.iter().any(|&d| d > 9) {
        return None;
    }
    let sum: u32 = first9
        .iter()
        .zip(WEIGHTS)
        .map(|(&d, w)| u32::from(d) * w)
        .sum();
    match sum % 11 {
        0 => None,
        // r is 1..=10, so 10 - r is 0..=9.
        r => Some((10 - r) as u8),
    }
}

/// A validated 10-digit tax code of an enterprise or organisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Mst {
    /// ASCII digits; validated by [`TaxCode::parse`].
    digits: [u8; 10],
}

impl Mst {
    /// The 10 digits.
    pub fn as_str(&self) -> &str {
        ascii_str(&self.digits)
    }
}

impl fmt::Display for Mst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A validated 13-digit tax code of a branch or other dependent unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BranchMst {
    /// ASCII digits without the dash; validated by [`TaxCode::parse`].
    digits: [u8; 13],
}

impl BranchMst {
    /// The 13 digits, without the dash.
    pub fn as_str(&self) -> &str {
        ascii_str(&self.digits)
    }

    /// The parent enterprise's code (the first 10 digits).
    pub fn parent(&self) -> Mst {
        let mut digits = [0u8; 10];
        digits.copy_from_slice(&self.digits[..10]);
        Mst { digits }
    }

    /// The branch number, 1–999.
    pub fn branch(&self) -> u16 {
        self.digits[10..]
            .iter()
            .fold(0, |acc, &b| acc * 10 + u16::from(b.wrapping_sub(b'0')))
    }
}

impl fmt::Display for BranchMst {
    /// Formats with the conventional dash: `0101243150-001`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(ascii_str(&self.digits[..10]))?;
        f.write_str("-")?;
        f.write_str(ascii_str(&self.digits[10..]))
    }
}

/// A validated tax code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaxCode {
    /// 10-digit code of an enterprise or organisation, or a legacy personal code.
    Enterprise10(Mst),
    /// 13-digit code of a branch or other dependent unit.
    Branch13(BranchMst),
    /// 12-digit personal identification number used as an individual's tax code.
    PersonalId12(Cccd),
}

impl TaxCode {
    /// Parses a tax code of 10, 12 or 13 ASCII digits. A 13-digit code may have a single `-`
    /// after the 10th digit. No other separators or whitespace are accepted; trim user input
    /// first.
    ///
    /// Errors: [`Error::Empty`], [`Error::InvalidCharacter`], [`Error::InvalidLength`],
    /// [`Error::Checksum`], [`Error::InvalidComponent`] (all-zero serial or `000` branch), and
    /// the [`Cccd::parse`] errors for 12-digit codes.
    pub fn parse(s: &str) -> Result<Self, Error> {
        if s.is_empty() {
            return Err(Error::Empty);
        }
        let mut buf = [0u8; 13];
        let mut len = 0usize;
        let mut dash = None;
        for (index, ch) in s.char_indices() {
            match ch {
                '0'..='9' => {
                    if let Some(slot) = buf.get_mut(len) {
                        *slot = ch as u8;
                    }
                    len += 1;
                }
                '-' if len == 10 && dash.is_none() => dash = Some(index),
                _ => return Err(Error::InvalidCharacter { index, ch }),
            }
        }
        if let Some(index) = dash
            && len != 13
        {
            return Err(Error::InvalidCharacter { index, ch: '-' });
        }
        match len {
            10 => {
                let mut digits = [0u8; 10];
                digits.copy_from_slice(&buf[..10]);
                validate10(&digits)?;
                Ok(TaxCode::Enterprise10(Mst { digits }))
            }
            12 => Cccd::parse(ascii_str(&buf[..12])).map(TaxCode::PersonalId12),
            13 => {
                let mut parent = [0u8; 10];
                parent.copy_from_slice(&buf[..10]);
                validate10(&parent)?;
                if &buf[10..] == b"000" {
                    return Err(Error::InvalidComponent);
                }
                Ok(TaxCode::Branch13(BranchMst { digits: buf }))
            }
            0 => Err(Error::Empty),
            len => Err(Error::InvalidLength { len }),
        }
    }

    /// The digits without separators.
    pub fn as_str(&self) -> &str {
        match self {
            TaxCode::Enterprise10(m) => m.as_str(),
            TaxCode::Branch13(b) => b.as_str(),
            TaxCode::PersonalId12(id) => id.as_str(),
        }
    }

    /// For a branch code, the parent enterprise's 10-digit code.
    pub fn parent(&self) -> Option<TaxCode> {
        match self {
            TaxCode::Branch13(b) => Some(TaxCode::Enterprise10(b.parent())),
            _ => None,
        }
    }

    /// For a branch code, the 3-digit branch number (1–999).
    pub fn branch(&self) -> Option<u16> {
        match self {
            TaxCode::Branch13(b) => Some(b.branch()),
            _ => None,
        }
    }
}

/// Checks the serial and check digit of 10 ASCII digits.
fn validate10(d: &[u8; 10]) -> Result<(), Error> {
    if d[2..9].iter().all(|&b| b == b'0') {
        return Err(Error::InvalidComponent);
    }
    let mut first9 = [0u8; 9];
    for (v, &b) in first9.iter_mut().zip(d.iter()) {
        *v = b.wrapping_sub(b'0');
    }
    if check_digit(&first9) == Some(d[9].wrapping_sub(b'0')) {
        Ok(())
    } else {
        Err(Error::Checksum)
    }
}

/// True when `s` is a valid tax code in any of the accepted forms.
pub fn is_valid_tax_code(s: &str) -> bool {
    TaxCode::parse(s).is_ok()
}

impl fmt::Display for TaxCode {
    /// Formats with the conventional dash for branch codes: `0101243150-001`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaxCode::Enterprise10(m) => m.fmt(f),
            TaxCode::Branch13(b) => b.fmt(f),
            TaxCode::PersonalId12(id) => id.fmt(f),
        }
    }
}

impl FromStr for TaxCode {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    /// Published 10-digit codes; sources in the module documentation.
    const REAL: &[(&str, &str)] = &[
        ("0300588569", "Vinamilk"),
        ("0100109106", "Viettel"),
        ("0101248141", "FPT"),
        ("0100112437", "Vietcombank"),
        ("0100233488", "python-stdnum example"),
    ];

    #[test]
    fn real_codes_validate() {
        for &(code, who) in REAL {
            let parsed = TaxCode::parse(code).unwrap_or_else(|e| panic!("{who}: {e}"));
            assert!(matches!(parsed, TaxCode::Enterprise10(_)));
            assert_eq!(parsed.as_str(), code);
            assert_eq!(parsed.to_string(), code);
            assert_eq!(parsed.branch(), None);
            assert_eq!(parsed.parent(), None);
        }
    }

    #[test]
    fn single_digit_changes_are_detected() {
        for &(code, _) in REAL {
            let digits = code.as_bytes();
            for pos in 0..10 {
                for d in b'0'..=b'9' {
                    if d == digits[pos] {
                        continue;
                    }
                    let mut m = [0u8; 10];
                    m.copy_from_slice(digits);
                    m[pos] = d;
                    assert!(
                        TaxCode::parse(ascii_str(&m)).is_err(),
                        "{:?}",
                        ascii_str(&m)
                    );
                }
            }
        }
    }

    #[test]
    fn check_digit_values() {
        assert_eq!(check_digit(&[0, 3, 0, 0, 5, 8, 8, 5, 6]), Some(9));
        assert_eq!(check_digit(&[0, 1, 0, 1, 2, 4, 3, 1, 5]), Some(0));
        // 31 + 29 + 23 + 19 + 17 + 13 + 7 + 5 + 3 = 147 = 11 * 13 + 4.
        assert_eq!(check_digit(&[1; 9]), Some(6));
        // A weighted sum of 0 is a multiple of 11.
        assert_eq!(check_digit(&[0; 9]), None);
        // 29 + 23 + 3 = 55 = 11 * 5.
        assert_eq!(check_digit(&[0, 1, 1, 0, 0, 0, 0, 0, 1]), None);
        assert_eq!(check_digit(&[10, 0, 0, 0, 0, 0, 0, 0, 0]), None);
        // Every prefix with a multiple-of-11 sum is rejected whatever the 10th digit.
        for last in b'0'..=b'9' {
            let mut code = *b"0110000010";
            code[9] = last;
            assert_eq!(TaxCode::parse(ascii_str(&code)), Err(Error::Checksum));
        }
    }

    #[test]
    fn branch_codes() {
        for input in ["0101243150-001", "0101243150001"] {
            let code = TaxCode::parse(input).unwrap();
            assert_eq!(code.as_str(), "0101243150001");
            assert_eq!(code.to_string(), "0101243150-001");
            assert_eq!(code.branch(), Some(1));
            assert_eq!(code.parent(), Some(TaxCode::parse("0101243150").unwrap()));
            let TaxCode::Branch13(b) = code else {
                panic!("expected a branch")
            };
            assert_eq!(b.parent().as_str(), "0101243150");
            assert_eq!(b.to_string(), "0101243150-001");
        }
        // Viettel Cyber Security, a published branch of Viettel.
        let code: TaxCode = "0100109106-475".parse().unwrap();
        assert_eq!(code.branch(), Some(475));
        assert_eq!(code.parent().unwrap().as_str(), "0100109106");
        // python-stdnum example.
        assert_eq!(TaxCode::parse("0314409058-002").unwrap().branch(), Some(2));
        assert_eq!(
            TaxCode::parse("0100109106-999").unwrap().branch(),
            Some(999)
        );
        assert_eq!(
            TaxCode::parse("0101243150-000"),
            Err(Error::InvalidComponent)
        );
        assert_eq!(TaxCode::parse("0101243151-001"), Err(Error::Checksum));
    }

    #[test]
    fn personal_codes() {
        let code = TaxCode::parse("001099012345").unwrap();
        let TaxCode::PersonalId12(id) = code else {
            panic!("expected a personal code")
        };
        assert_eq!(id.province(), Some("Hà Nội"));
        assert_eq!(code.as_str(), "001099012345");
        assert_eq!(code.to_string(), "001099012345");
        assert_eq!(code.parent(), None);
        assert_eq!(
            TaxCode::parse("003099012345"),
            Err(Error::UnknownCode { code: 3 })
        );
        assert_eq!(TaxCode::parse("001099000000"), Err(Error::InvalidComponent));
    }

    #[test]
    fn errors() {
        assert_eq!(TaxCode::parse(""), Err(Error::Empty));
        assert_eq!(
            TaxCode::parse("-"),
            Err(Error::InvalidCharacter { index: 0, ch: '-' })
        );
        assert_eq!(
            TaxCode::parse("030058856"),
            Err(Error::InvalidLength { len: 9 })
        );
        assert_eq!(
            TaxCode::parse("03005885690"),
            Err(Error::InvalidLength { len: 11 })
        );
        assert_eq!(
            TaxCode::parse("01012431500011"),
            Err(Error::InvalidLength { len: 14 })
        );
        assert_eq!(
            TaxCode::parse("0101243150001123456789"),
            Err(Error::InvalidLength { len: 22 })
        );
        assert_eq!(TaxCode::parse("0300588568"), Err(Error::Checksum));
        assert_eq!(TaxCode::parse("1234567890"), Err(Error::Checksum));
        assert_eq!(TaxCode::parse("0100000000"), Err(Error::InvalidComponent));
        assert_eq!(
            TaxCode::parse("0300588569-"),
            Err(Error::InvalidCharacter { index: 10, ch: '-' })
        );
        assert_eq!(
            TaxCode::parse("030058856-9"),
            Err(Error::InvalidCharacter { index: 9, ch: '-' })
        );
        assert_eq!(
            TaxCode::parse("0101243150--001"),
            Err(Error::InvalidCharacter { index: 11, ch: '-' })
        );
        assert_eq!(
            TaxCode::parse(" 0300588569"),
            Err(Error::InvalidCharacter { index: 0, ch: ' ' })
        );
        assert_eq!(
            TaxCode::parse("03005885６9"),
            Err(Error::InvalidCharacter {
                index: 8, ch: '６'
            })
        );
        assert!(!is_valid_tax_code("0300588560"));
        assert!(is_valid_tax_code("0300588569"));
    }
}
