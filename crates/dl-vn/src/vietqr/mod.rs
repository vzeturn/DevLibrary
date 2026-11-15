//! VietQR: building and parsing NAPAS247 bank-transfer QR payloads.
//!
//! VietQR is NAPAS's profile of the EMVCo Merchant-Presented Mode QR code. The rules below
//! are from the NAPAS document "Quy định về định dạng mã VietQR trong Dịch vụ Chuyển nhanh
//! NAPAS247", version 1.0 (09/2021), published on vietqr.net
//! ([transcript](https://github.com/shinxz12/napas-qr-python/blob/main/docs/vietqr-spec.md)).
//!
//! A payload is a sequence of data objects, each a 2-digit ID, a 2-digit length `01`–`99`
//! counted in characters, and the value. ID `00` comes first, ID `63` last. This module
//! writes and reads:
//!
//! | ID | Content | Rule |
//! |---|---|---|
//! | `00` | Payload format indicator | `"01"` |
//! | `01` | Point of initiation | `"11"` static (reusable), `"12"` dynamic (one payment) |
//! | `38.00` | GUID | `"A000000727"` (NAPAS) |
//! | `38.01.00` | Beneficiary bank BIN | 6 digits |
//! | `38.01.01` | Account or card number | 1–19 characters |
//! | `38.02` | Service code | `QRIBFTTA` (to account) or `QRIBFTTC` (to card) |
//! | `53` | Currency | `"704"` (VND) |
//! | `54` | Amount, optional | 1–13 characters, digits with an optional trailing `.`, non-zero |
//! | `58` | Country | `"VN"` |
//! | `62.01` | Bill number, optional | 1–25 characters |
//! | `62.08` | Purpose of transaction, optional | 1–25 characters |
//! | `63` | CRC | CRC-16/CCITT-FALSE over everything before the CRC value, 4 hex digits |
//!
//! Fields `38.01.01`, `62.01` and `62.08` have the "ANS" format, the EMV common character
//! set: printable ASCII, U+0020 to U+007E. Vietnamese diacritics are not allowed there; pass
//! the text through [`crate::text::remove_diacritics`] first. The value `"***"` in `62.01` or
//! `62.08` asks the payer's app to prompt for it.
//!
//! [`VietQr::parse`] verifies the CRC and the fields above. Other data objects that the
//! format allows (merchant category `52`, name `59`, city `60`, other `62` sub-fields,
//! templates `64`–`99`, other merchant templates `26`–`51`) are checked for TLV structure and
//! then ignored.
//!
//! ```
//! use dl_vn::vietqr::{Initiation, Service, VietQr};
//!
//! let qr = VietQr {
//!     initiation: Initiation::Dynamic,
//!     bin: "970403".into(),
//!     account: "0011012345678".into(),
//!     service: Service::Account,
//!     amount: Some(180_000),
//!     bill_number: Some("NPS6869".into()),
//!     purpose: Some("thanh toan don hang".into()),
//! };
//! let payload = qr.to_payload().unwrap();
//! // Example 6.1.3 of the NAPAS specification.
//! assert_eq!(
//!     payload,
//!     "00020101021238570010A00000072701270006970403011300110123456780208QRIBFTTA\
//!      530370454061800005802VN62340107NPS68690819thanh toan don hang63042E2E"
//! );
//! assert_eq!(VietQr::parse(&payload).unwrap(), qr);
//! assert_eq!(qr.bank().unwrap().short_name, "Sacombank");
//! ```

mod banks;

pub use banks::{BANKS, Bank, bank_by_bin, bank_by_name};

use crate::Error;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;
use core::str::FromStr;

/// The NAPAS application identifier, the GUID in field `38.00`.
pub const NAPAS_GUID: &str = "A000000727";

/// Largest amount that fits the 13 characters of field `54`.
pub const MAX_AMOUNT: u64 = 9_999_999_999_999;

/// Maximum length in characters of `62.01` and `62.08`.
const MAX_ADDITIONAL: usize = 25;

/// Maximum length in characters of the account or card number, `38.01.01`.
const MAX_ACCOUNT: usize = 19;

/// CRC-16/CCITT-FALSE: polynomial 0x1021, initial value 0xFFFF, no reflection, no final XOR.
///
/// This is the checksum of field `63` (ISO/IEC 13239 per the NAPAS and EMVCo specifications).
///
/// ```
/// assert_eq!(dl_vn::vietqr::crc16(b"123456789"), 0x29B1);
/// ```
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Point of initiation method, field `01`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Initiation {
    /// `"11"`: the code can be paid many times, typically printed without an amount.
    #[default]
    Static,
    /// `"12"`: the code is for one payment, typically with an amount.
    Dynamic,
}

impl Initiation {
    /// The 2-digit value of field `01`.
    pub fn code(self) -> &'static str {
        match self {
            Initiation::Static => "11",
            Initiation::Dynamic => "12",
        }
    }
}

/// NAPAS247 service code, field `38.02`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Service {
    /// `QRIBFTTA`: transfer to a bank account number.
    #[default]
    Account,
    /// `QRIBFTTC`: transfer to a card number.
    Card,
}

impl Service {
    /// The service code written in field `38.02`.
    pub fn code(self) -> &'static str {
        match self {
            Service::Account => "QRIBFTTA",
            Service::Card => "QRIBFTTC",
        }
    }
}

/// A VietQR transfer: beneficiary, optional amount and optional message.
///
/// Build a payload with [`VietQr::to_payload`] and read one with [`VietQr::parse`]. Fields are
/// validated when a payload is built, so a struct literal with invalid values is not an error
/// until then.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VietQr {
    /// Static or dynamic code (field `01`).
    pub initiation: Initiation,
    /// 6-digit BIN of the beneficiary bank (field `38.01.00`); see [`BANKS`].
    pub bin: String,
    /// Beneficiary account or card number (field `38.01.01`), 1–19 printable ASCII characters.
    pub account: String,
    /// Transfer to an account or to a card (field `38.02`).
    pub service: Service,
    /// Amount in đồng (field `54`), 1 to [`MAX_AMOUNT`]. `None` lets the payer enter it.
    pub amount: Option<u64>,
    /// Bill number (field `62.01`), 1–25 printable ASCII characters.
    pub bill_number: Option<String>,
    /// Purpose of transaction, the transfer message (field `62.08`), 1–25 printable ASCII
    /// characters.
    pub purpose: Option<String>,
}

impl VietQr {
    /// A static transfer to an account, without amount or message.
    pub fn new(bin: &str, account: &str) -> Self {
        Self {
            initiation: Initiation::Static,
            bin: bin.into(),
            account: account.into(),
            service: Service::Account,
            amount: None,
            bill_number: None,
            purpose: None,
        }
    }

    /// The bank with this BIN in [`BANKS`], if listed.
    pub fn bank(&self) -> Option<&'static Bank> {
        bank_by_bin(&self.bin)
    }

    /// Builds the payload string to encode in the QR code, CRC included.
    ///
    /// Fields are written in the order of the NAPAS examples: `00 01 38 53 54 58 62 63`.
    ///
    /// Errors: [`Error::InvalidField`] with the tag of the first invalid field
    /// (`"38.01.00"`, `"38.01.01"`, `"54"`, `"62.01"` or `"62.08"`).
    ///
    /// ```
    /// use dl_vn::vietqr::{Initiation, VietQr};
    ///
    /// let mut qr = VietQr::new("970436", "1031933430");
    /// qr.initiation = Initiation::Dynamic;
    /// qr.purpose = Some("Thanh toan hoa don".into());
    /// // The README example of the napas-qr-python library.
    /// assert_eq!(
    ///     qr.to_payload().unwrap(),
    ///     "00020101021238540010A00000072701240006970436011010319334300208QRIBFTTA\
    ///      53037045802VN62220818Thanh toan hoa don6304E8A4"
    /// );
    ///
    /// qr.purpose = Some("Tiền phòng".into());
    /// assert!(qr.to_payload().is_err());
    /// ```
    pub fn to_payload(&self) -> Result<String, Error> {
        if !is_bin(&self.bin) {
            return Err(Error::InvalidField { tag: "38.01.00" });
        }
        if !is_ans(&self.account, MAX_ACCOUNT) {
            return Err(Error::InvalidField { tag: "38.01.01" });
        }
        if let Some(amount) = self.amount
            && !(1..=MAX_AMOUNT).contains(&amount)
        {
            return Err(Error::InvalidField { tag: "54" });
        }
        if let Some(bill) = &self.bill_number
            && !is_ans(bill, MAX_ADDITIONAL)
        {
            return Err(Error::InvalidField { tag: "62.01" });
        }
        if let Some(purpose) = &self.purpose
            && !is_ans(purpose, MAX_ADDITIONAL)
        {
            return Err(Error::InvalidField { tag: "62.08" });
        }

        // Every value below is ASCII and at most 99 characters long (the longest, field 38,
        // is 14 + 4 + 33 + 12 = 63), so lengths are byte lengths and fit two digits.
        let mut beneficiary = String::with_capacity(33);
        push_tlv(&mut beneficiary, 0, &self.bin);
        push_tlv(&mut beneficiary, 1, &self.account);
        let mut merchant = String::with_capacity(63);
        push_tlv(&mut merchant, 0, NAPAS_GUID);
        push_tlv(&mut merchant, 1, &beneficiary);
        push_tlv(&mut merchant, 2, self.service.code());

        let mut out = String::with_capacity(160);
        push_tlv(&mut out, 0, "01");
        push_tlv(&mut out, 1, self.initiation.code());
        push_tlv(&mut out, 38, &merchant);
        push_tlv(&mut out, 53, "704");
        if let Some(amount) = self.amount {
            let mut digits = String::with_capacity(13);
            // Writing to a String cannot fail.
            let _ = write!(digits, "{amount}");
            push_tlv(&mut out, 54, &digits);
        }
        push_tlv(&mut out, 58, "VN");
        if self.bill_number.is_some() || self.purpose.is_some() {
            let mut extra = String::with_capacity(58);
            if let Some(bill) = &self.bill_number {
                push_tlv(&mut extra, 1, bill);
            }
            if let Some(purpose) = &self.purpose {
                push_tlv(&mut extra, 8, purpose);
            }
            push_tlv(&mut out, 62, &extra);
        }
        out.push_str("6304");
        let crc = crc16(out.as_bytes());
        let _ = write!(out, "{crc:04X}");
        Ok(out)
    }

    /// Parses and validates a VietQR payload, as read from a QR code.
    ///
    /// The whole input is the payload: no surrounding whitespace is accepted. The CRC is
    /// checked before the fields. A missing service code (`38.02`) is an error, as the
    /// specification marks it conditional and this module only handles the two transfer
    /// services.
    ///
    /// Errors:
    /// - [`Error::Empty`] for an empty string;
    /// - [`Error::MalformedTlv`] for a data object without a 2-digit ID and a 2-digit length
    ///   `01`–`99`, a value running past the end of its template, or a repeated ID;
    /// - [`Error::MissingField`] and [`Error::InvalidField`] with the dotted tag, for example
    ///   `"00"` when the payload does not start with the format indicator, `"63"` when the
    ///   CRC is not the last 4-hex-digit object, `"62.08"` for a purpose that is not 1–25
    ///   printable ASCII characters;
    /// - [`Error::CrcMismatch`];
    /// - [`Error::Unsupported`] for a QR code that is valid EMVCo but not a NAPAS247 transfer
    ///   in đồng (another GUID, currency or service code).
    ///
    /// ```
    /// use dl_vn::vietqr::{Service, VietQr};
    ///
    /// // Example 6.1.2 of the NAPAS specification: static code to a card.
    /// let qr = VietQr::parse(
    ///     "00020101021138600010A00000072701300006970403011697040311012345670208QRIBFTTC\
    ///      53037045802VN63044F52",
    /// )
    /// .unwrap();
    /// assert_eq!(qr.service, Service::Card);
    /// assert_eq!(qr.account, "9704031101234567");
    /// assert_eq!(qr.amount, None);
    /// ```
    pub fn parse(payload: &str) -> Result<Self, Error> {
        if payload.is_empty() {
            return Err(Error::Empty);
        }
        let root = parse_tlvs(payload, 0)?;

        match root.first() {
            Some(first) if first.tag == 0 => {}
            _ if find(&root, 0).is_some() => return Err(Error::InvalidField { tag: "00" }),
            _ => return Err(Error::MissingField { tag: "00" }),
        }
        let crc_obj = match root.last() {
            Some(last) if last.tag == 63 => last,
            _ if find(&root, 63).is_some() => return Err(Error::InvalidField { tag: "63" }),
            _ => return Err(Error::MissingField { tag: "63" }),
        };
        let found = parse_hex4(crc_obj.value).ok_or(Error::InvalidField { tag: "63" })?;
        // The CRC covers everything up to and including "6304", i.e. up to the value.
        let covered = payload
            .get(..crc_obj.value_at)
            .ok_or(Error::InvalidField { tag: "63" })?;
        let expected = crc16(covered.as_bytes());
        if expected != found {
            return Err(Error::CrcMismatch { expected, found });
        }

        if required(&root, 0, "00")?.value != "01" {
            return Err(Error::InvalidField { tag: "00" });
        }
        let initiation = match required(&root, 1, "01")?.value {
            "11" => Initiation::Static,
            "12" => Initiation::Dynamic,
            _ => return Err(Error::InvalidField { tag: "01" }),
        };

        let merchant_obj = required(&root, 38, "38")?;
        let merchant = parse_tlvs(merchant_obj.value, merchant_obj.value_at)?;
        if required(&merchant, 0, "38.00")?.value != NAPAS_GUID {
            return Err(Error::Unsupported {
                what: "merchant account GUID other than NAPAS",
            });
        }
        let beneficiary_obj = required(&merchant, 1, "38.01")?;
        let beneficiary = parse_tlvs(beneficiary_obj.value, beneficiary_obj.value_at)?;
        let bin = required(&beneficiary, 0, "38.01.00")?.value;
        if !is_bin(bin) {
            return Err(Error::InvalidField { tag: "38.01.00" });
        }
        let account = required(&beneficiary, 1, "38.01.01")?.value;
        if !is_ans(account, MAX_ACCOUNT) {
            return Err(Error::InvalidField { tag: "38.01.01" });
        }
        let service = match required(&merchant, 2, "38.02")?.value {
            "QRIBFTTA" => Service::Account,
            "QRIBFTTC" => Service::Card,
            _ => {
                return Err(Error::Unsupported {
                    what: "service code other than QRIBFTTA/QRIBFTTC",
                });
            }
        };

        if required(&root, 53, "53")?.value != "704" {
            return Err(Error::Unsupported {
                what: "currency other than VND",
            });
        }
        let amount = match find(&root, 54) {
            Some(obj) => Some(parse_amount(obj.value).ok_or(Error::InvalidField { tag: "54" })?),
            None => None,
        };
        if required(&root, 58, "58")?.value != "VN" {
            return Err(Error::InvalidField { tag: "58" });
        }

        let (mut bill_number, mut purpose) = (None, None);
        if let Some(extra_obj) = find(&root, 62) {
            let extra = parse_tlvs(extra_obj.value, extra_obj.value_at)?;
            if let Some(obj) = find(&extra, 1) {
                if !is_ans(obj.value, MAX_ADDITIONAL) {
                    return Err(Error::InvalidField { tag: "62.01" });
                }
                bill_number = Some(obj.value.into());
            }
            if let Some(obj) = find(&extra, 8) {
                if !is_ans(obj.value, MAX_ADDITIONAL) {
                    return Err(Error::InvalidField { tag: "62.08" });
                }
                purpose = Some(obj.value.into());
            }
        }

        Ok(Self {
            initiation,
            bin: bin.into(),
            account: account.into(),
            service,
            amount,
            bill_number,
            purpose,
        })
    }
}

impl FromStr for VietQr {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        Self::parse(s)
    }
}

/// One data object. Offsets are byte offsets into the whole payload.
struct Tlv<'a> {
    tag: u8,
    value: &'a str,
    /// Offset of the value (just after the 4-digit header).
    value_at: usize,
}

/// Appends a data object. Callers pass ASCII values of 1–99 characters.
fn push_tlv(out: &mut String, tag: u8, value: &str) {
    let _ = write!(out, "{tag:02}{:02}", value.chars().count());
    out.push_str(value);
}

/// Splits `s` (which starts at byte `base` of the payload) into data objects. Lengths count
/// characters, as in the EMVCo specification.
fn parse_tlvs(s: &str, base: usize) -> Result<Vec<Tlv<'_>>, Error> {
    let mut out: Vec<Tlv<'_>> = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let malformed = Error::MalformedTlv { index: base + i };
        let header = s.as_bytes().get(i..i + 4).ok_or(malformed)?;
        if !header.iter().all(u8::is_ascii_digit) {
            return Err(malformed);
        }
        let tag = (header[0] - b'0') * 10 + (header[1] - b'0');
        let len = usize::from((header[2] - b'0') * 10 + (header[3] - b'0'));
        if len == 0 || out.iter().any(|t| t.tag == tag) {
            return Err(malformed);
        }
        // The header is 4 ASCII bytes, so `i + 4` is a char boundary.
        let rest = s.get(i + 4..).ok_or(malformed)?;
        let mut chars = rest.char_indices().map(|(at, _)| at).chain([rest.len()]);
        let end = chars.nth(len).ok_or(malformed)?;
        let value = rest.get(..end).ok_or(malformed)?;
        out.push(Tlv {
            tag,
            value,
            value_at: base + i + 4,
        });
        i += 4 + end;
    }
    Ok(out)
}

fn find<'a, 'b>(objs: &'b [Tlv<'a>], tag: u8) -> Option<&'b Tlv<'a>> {
    objs.iter().find(|t| t.tag == tag)
}

fn required<'a, 'b>(
    objs: &'b [Tlv<'a>],
    tag: u8,
    path: &'static str,
) -> Result<&'b Tlv<'a>, Error> {
    find(objs, tag).ok_or(Error::MissingField { tag: path })
}

fn is_bin(s: &str) -> bool {
    s.len() == 6 && s.bytes().all(|b| b.is_ascii_digit())
}

/// 1..=`max` characters of the EMV common character set (printable ASCII).
fn is_ans(s: &str, max: usize) -> bool {
    (1..=max).contains(&s.len()) && s.bytes().all(|b| (0x20..=0x7E).contains(&b))
}

/// Field 54 for VND (ISO 4217 exponent 0): 1–13 characters, digits with an optional
/// trailing `.`, non-zero.
fn parse_amount(s: &str) -> Option<u64> {
    if s.len() > 13 {
        return None;
    }
    let digits = s.strip_suffix('.').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // At most 13 digits, so this cannot overflow.
    let n = digits
        .bytes()
        .fold(0u64, |acc, b| acc * 10 + u64::from(b - b'0'));
    (n != 0).then_some(n)
}

fn parse_hex4(s: &str) -> Option<u16> {
    if s.len() != 4 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(s, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::ToString;

    /// Examples 6.1.2 to 6.1.4 of the NAPAS VietQR specification v1.0.
    const NAPAS_EXAMPLES: [&str; 3] = [
        "00020101021138600010A00000072701300006970403011697040311012345670208QRIBFTTC53037045802VN63044F52",
        "00020101021238570010A00000072701270006970403011300110123456780208QRIBFTTA530370454061800005802VN62340107NPS68690819thanh toan don hang63042E2E",
        "00020101021238600010A00000072701300006970403011697040311012345670208QRIBFTTC530370454061800005802VN62340107NPS68690819thanh toan don hang6304A203",
    ];

    #[test]
    fn crc_check_value() {
        // CRC-16/CCITT-FALSE check value from the CRC catalogue.
        assert_eq!(crc16(b"123456789"), 0x29B1);
        assert_eq!(crc16(b""), 0xFFFF);
    }

    #[test]
    fn crc_of_napas_examples() {
        for p in NAPAS_EXAMPLES {
            let (body, crc) = p.split_at(p.len() - 4);
            assert_eq!(format!("{:04X}", crc16(body.as_bytes())), crc, "{p}");
        }
        // Example 6.1.1 as printed: its CRC matches, but field 38.01 declares 27 characters
        // with a 12-character account inside, which leaves one stray character.
        let p = "00020101021138570010A00000072701270006970403011200110123456780208QRIBFTTA53037045802VN6304F4E5";
        let (body, crc) = p.split_at(p.len() - 4);
        assert_eq!(format!("{:04X}", crc16(body.as_bytes())), crc);
        assert!(matches!(VietQr::parse(p), Err(Error::MalformedTlv { .. })));
    }

    #[test]
    fn napas_examples_round_trip() {
        for p in NAPAS_EXAMPLES {
            let qr = VietQr::parse(p).unwrap_or_else(|e| panic!("{p}: {e}"));
            assert_eq!(qr.bin, "970403");
            assert_eq!(qr.to_payload().unwrap(), p);
        }
        let qr = VietQr::parse(NAPAS_EXAMPLES[1]).unwrap();
        assert_eq!(
            qr,
            VietQr {
                initiation: Initiation::Dynamic,
                bin: "970403".into(),
                account: "0011012345678".into(),
                service: Service::Account,
                amount: Some(180_000),
                bill_number: Some("NPS6869".into()),
                purpose: Some("thanh toan don hang".into()),
            }
        );
        let card = VietQr::parse(NAPAS_EXAMPLES[2]).unwrap();
        assert_eq!(
            (card.service, card.account.as_str()),
            (Service::Card, "9704031101234567")
        );
    }

    #[test]
    fn third_party_payloads() {
        // Published examples of open-source VietQR libraries.
        let p = "00020101021238530010A0000007270123000697041601092576788590208QRIBFTTA5303704540410005802VN62150811Chuyen tien6304BBB8";
        let qr = VietQr::parse(p).unwrap();
        assert_eq!(
            (qr.bin.as_str(), qr.account.as_str()),
            ("970416", "257678859")
        );
        assert_eq!(
            (qr.amount, qr.purpose.as_deref()),
            (Some(1000), Some("Chuyen tien"))
        );
        assert_eq!(qr.bank().unwrap().short_name, "ACB");
        assert_eq!(qr.to_payload().unwrap(), p);

        let p = "00020101021238530010A0000007270123000697041601092245284790208QRIBFTTA53037045405100005802VN62340107NPS68690819TRANSFER TO SOMEONE6304AC13";
        let qr = VietQr::parse(p).unwrap();
        assert_eq!(qr.amount, Some(10_000));
        assert_eq!(qr.to_payload().unwrap(), p);

        let p = "00020101021238630010A00000072701330006970436011997043686255816010180208QRIBFTTC5303704540720000005802VN6304A2DC";
        let qr = VietQr::parse(p).unwrap();
        assert_eq!((qr.service, qr.amount), (Service::Card, Some(2_000_000)));
        assert_eq!(qr.to_payload().unwrap(), p);
    }

    #[test]
    fn build_then_parse() {
        let long_bill = "*".repeat(25);
        let long_purpose = "~".repeat(25);
        let extras: [(Option<&str>, Option<&str>); 5] = [
            (None, None),
            (Some("HD-0001"), None),
            (None, Some("Tien phong 101")),
            (Some(&long_bill), Some(&long_purpose)),
            (Some("***"), Some("***")),
        ];
        let mut count = 0;
        for initiation in [Initiation::Static, Initiation::Dynamic] {
            for service in [Service::Account, Service::Card] {
                for amount in [None, Some(1), Some(55_000), Some(MAX_AMOUNT)] {
                    for account in ["1", "1234567890123456789", "VQR 0001-ab"] {
                        for (bill, purpose) in extras {
                            let qr = VietQr {
                                initiation,
                                bin: "970436".into(),
                                account: account.into(),
                                service,
                                amount,
                                bill_number: bill.map(Into::into),
                                purpose: purpose.map(Into::into),
                            };
                            let payload = qr.to_payload().unwrap();
                            assert_eq!(VietQr::parse(&payload).unwrap(), qr, "{payload}");
                            assert_eq!(payload.parse::<VietQr>().unwrap(), qr);
                            count += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(count, 240);
    }

    #[test]
    fn build_errors() {
        let ok = VietQr::new("970436", "1031933430");
        assert!(ok.to_payload().is_ok());
        let check = |f: &dyn Fn(&mut VietQr), tag| {
            let mut qr = ok.clone();
            f(&mut qr);
            assert_eq!(qr.to_payload(), Err(Error::InvalidField { tag }));
        };
        check(&|q| q.bin = "97043".into(), "38.01.00");
        check(&|q| q.bin = "97043a".into(), "38.01.00");
        check(&|q| q.bin = "９７０４３６".into(), "38.01.00");
        check(&|q| q.account = String::new(), "38.01.01");
        check(&|q| q.account = "1".repeat(20), "38.01.01");
        check(&|q| q.account = "123\n".into(), "38.01.01");
        check(&|q| q.amount = Some(0), "54");
        check(&|q| q.amount = Some(MAX_AMOUNT + 1), "54");
        check(&|q| q.amount = Some(u64::MAX), "54");
        check(&|q| q.bill_number = Some(String::new()), "62.01");
        check(&|q| q.purpose = Some("x".repeat(26)), "62.08");
        check(&|q| q.purpose = Some("Tiền nhà".into()), "62.08");
        check(&|q| q.purpose = Some("tab\there".into()), "62.08");
    }

    /// Appends `6304` and the correct CRC, so that the field checks are reached.
    fn with_crc(body: &str) -> String {
        let mut s = format!("{body}6304");
        let crc = crc16(s.as_bytes());
        s.push_str(&format!("{crc:04X}"));
        s
    }

    /// Field 38 for account 1031933430 at Vietcombank, and variants missing one part.
    const M38: &str = "38540010A00000072701240006970436011010319334300208QRIBFTTA";
    const M38_NO_GUID: &str = "384001240006970436011010319334300208QRIBFTTA";
    const M38_NO_SERVICE: &str = "38420010A0000007270124000697043601101031933430";
    const M38_NO_ACCOUNT: &str = "38400010A000000727011000069704360208QRIBFTTA";

    #[test]
    fn parse_field_errors() {
        let err = |body: &str| VietQr::parse(&with_crc(body)).unwrap_err();
        let base = format!("000201010211{M38}53037045802VN");
        assert_eq!(
            VietQr::parse(&with_crc(&base)).unwrap(),
            VietQr::new("970436", "1031933430")
        );

        assert_eq!(
            err(&format!("010211{M38}000201")),
            Error::InvalidField { tag: "00" }
        );
        assert_eq!(
            err(&format!("010211{M38}")),
            Error::MissingField { tag: "00" }
        );
        assert_eq!(
            err(&base.replacen("000201", "000202", 1)),
            Error::InvalidField { tag: "00" }
        );
        assert_eq!(
            err(&base.replacen("010211", "010213", 1)),
            Error::InvalidField { tag: "01" }
        );
        assert_eq!(
            err(&base.replacen("010211", "", 1)),
            Error::MissingField { tag: "01" }
        );
        assert_eq!(
            err(&base.replace("5303704", "5303840")),
            Error::Unsupported {
                what: "currency other than VND"
            }
        );
        assert_eq!(
            err(&base.replace("5303704", "")),
            Error::MissingField { tag: "53" }
        );
        assert_eq!(
            err(&base.replace("5802VN", "5802TH")),
            Error::InvalidField { tag: "58" }
        );
        assert_eq!(
            err(&base.replace("5802VN", "")),
            Error::MissingField { tag: "58" }
        );
        assert_eq!(
            err(&base.replace(M38, "")),
            Error::MissingField { tag: "38" }
        );
        assert!(matches!(
            err(&base.replace("A000000727", "A000000677")),
            Error::Unsupported { .. }
        ));
        assert!(matches!(
            err(&base.replace("QRIBFTTA", "QRPUSHAA")),
            Error::Unsupported { .. }
        ));
        assert_eq!(
            err(&base.replace(M38, M38_NO_GUID)),
            Error::MissingField { tag: "38.00" }
        );
        assert_eq!(
            err(&base.replace(M38, M38_NO_SERVICE)),
            Error::MissingField { tag: "38.02" }
        );
        assert_eq!(
            err(&base.replace(M38, M38_NO_ACCOUNT)),
            Error::MissingField { tag: "38.01.01" }
        );
        assert_eq!(
            err(&base.replace("0006970436", "0006970A36")),
            Error::InvalidField { tag: "38.01.00" }
        );
        assert_eq!(
            err(&base.replace("1031933430", "103193343\u{1}")),
            Error::InvalidField { tag: "38.01.01" }
        );

        for bad in [
            "0",
            "0.",
            "00",
            "1.5",
            "1,000",
            "-5",
            "1e3",
            ".",
            "12345678901234",
            "1 000",
        ] {
            let body = format!("{base}54{:02}{bad}", bad.len());
            assert_eq!(err(&body), Error::InvalidField { tag: "54" }, "{bad:?}");
        }
        for (good, n) in [
            ("1", 1),
            ("50000.", 50_000),
            ("9999999999999", MAX_AMOUNT),
            ("007", 7),
        ] {
            let body = format!("{base}54{:02}{good}", good.len());
            assert_eq!(VietQr::parse(&with_crc(&body)).unwrap().amount, Some(n));
        }

        let long = "x".repeat(26);
        assert_eq!(
            err(&format!("{base}62300826{long}")),
            Error::InvalidField { tag: "62.08" }
        );
        assert_eq!(
            err(&format!("{base}62300126{long}")),
            Error::InvalidField { tag: "62.01" }
        );
        // "Tiền" is 4 characters: lengths count characters, and the value is not ASCII.
        assert_eq!(
            err(&format!("{base}62080804Tiền")),
            Error::InvalidField { tag: "62.08" }
        );
        // Other sub-fields of 62 and other root objects are checked for structure, then ignored.
        let qr = VietQr::parse(&with_crc(&format!(
            "{base}520458125906Pho 2462150703T010804note"
        )))
        .unwrap();
        assert_eq!(
            (qr.purpose.as_deref(), qr.bill_number),
            (Some("note"), None)
        );
        let qr = VietQr::parse(&with_crc(&format!("{base}5906Phở Bò"))).unwrap();
        assert_eq!(qr.purpose, None);
        assert!(matches!(
            err(&format!("{base}5907Phở Bò")),
            Error::MalformedTlv { .. }
        ));
    }

    #[test]
    fn parse_structure_errors() {
        let good = NAPAS_EXAMPLES[1];
        assert_eq!(VietQr::parse(""), Err(Error::Empty));
        // Truncated anywhere: always an error, never a panic.
        for i in 1..good.len() {
            assert!(VietQr::parse(&good[..i]).is_err(), "{i}");
        }
        // Every single-character change is detected.
        for i in 0..good.len() {
            for replacement in ["0", "9", "A", " ", "é"] {
                let s = format!("{}{replacement}{}", &good[..i], &good[i + 1..]);
                if s != good {
                    assert!(VietQr::parse(&s).is_err(), "{s}");
                }
            }
        }
        assert_eq!(
            VietQr::parse(&format!("{good} ")),
            Err(Error::MalformedTlv { index: good.len() })
        );
        assert_eq!(
            VietQr::parse(&format!(" {good}")),
            Err(Error::MalformedTlv { index: 0 })
        );

        assert_eq!(
            VietQr::parse(&good.replace("2E2E", "2E2F")),
            Err(Error::CrcMismatch {
                expected: 0x2E2E,
                found: 0x2E2F
            })
        );
        // Lowercase hex digits in the CRC are accepted.
        assert!(VietQr::parse(&good.replace("2E2E", "2e2e")).is_ok());
        assert_eq!(
            VietQr::parse(&good.replace("63042E2E", "6304+E2E")),
            Err(Error::InvalidField { tag: "63" })
        );
        assert_eq!(
            VietQr::parse(&good.replace("63042E2E", "63032E2")),
            Err(Error::InvalidField { tag: "63" })
        );
        // CRC present but not last, and CRC absent.
        assert_eq!(
            VietQr::parse(&format!("000201010211{M38}53037046304ABCD5802VN")),
            Err(Error::InvalidField { tag: "63" })
        );
        assert_eq!(
            VietQr::parse(&format!("000201010211{M38}53037045802VN")),
            Err(Error::MissingField { tag: "63" })
        );
        // Length 00, header cut short, repeated ID.
        assert_eq!(VietQr::parse("0000"), Err(Error::MalformedTlv { index: 0 }));
        assert_eq!(
            VietQr::parse("00020A02"),
            Err(Error::MalformedTlv { index: 6 })
        );
        assert_eq!(
            VietQr::parse("000201000201"),
            Err(Error::MalformedTlv { index: 6 })
        );
        // A malformed nested template reports its offset in the whole payload.
        let nested = with_crc(&format!(
            "000201010211{}",
            M38.replace("0010A000", "00X0A000")
        ));
        assert_eq!(
            VietQr::parse(&nested),
            Err(Error::MalformedTlv { index: 16 })
        );
        assert_eq!(VietQr::parse("é"), Err(Error::MalformedTlv { index: 0 }));
        assert_eq!(
            VietQr::parse("0002é"),
            Err(Error::MalformedTlv { index: 0 })
        );
        assert_eq!(
            VietQr::parse("0002éé"),
            Err(Error::MissingField { tag: "63" })
        );
    }

    #[test]
    fn codes_and_bank_lookup() {
        assert_eq!(Initiation::Static.code(), "11");
        assert_eq!(Initiation::Dynamic.code(), "12");
        assert_eq!(Service::Account.code(), "QRIBFTTA");
        assert_eq!(Service::Card.code(), "QRIBFTTC");
        assert_eq!(
            VietQr::new("970436", "1").bank().map(|b| b.code),
            Some("VCB")
        );
        assert_eq!(VietQr::new("123456", "1").bank(), None);
        assert_eq!(
            Error::InvalidField { tag: "54" }.to_string(),
            "invalid value in field 54"
        );
    }

    #[test]
    fn bank_table() {
        assert_eq!(BANKS.len(), 76);
        for (i, b) in BANKS.iter().enumerate() {
            assert!(is_bin(b.bin), "{}", b.bin);
            assert!(
                !BANKS[i + 1..].iter().any(|o| o.bin == b.bin),
                "duplicate {}",
                b.bin
            );
            assert!(!b.code.is_empty() && !b.short_name.is_empty() && !b.name.is_empty());
            assert_eq!(b.name.trim(), b.name);
        }
        let vcb = bank_by_bin("970436").unwrap();
        assert_eq!((vcb.code, vcb.short_name), ("VCB", "Vietcombank"));
        assert_eq!(bank_by_name("vcb").unwrap().bin, "970436");
        assert_eq!(bank_by_name("techcombank").unwrap().bin, "970407");
        assert_eq!(bank_by_name("MBBank").unwrap().bin, "970422");
        assert_eq!(bank_by_name("ICB").unwrap().short_name, "VietinBank");
        assert_eq!(bank_by_bin("970405").unwrap().short_name, "Agribank");
        assert_eq!(bank_by_bin("970418").unwrap().short_name, "BIDV");
        assert_eq!(bank_by_bin("970416").unwrap().short_name, "ACB");
        assert!(bank_by_bin("000000").is_none());
        assert!(bank_by_name("nope").is_none());
    }
}
