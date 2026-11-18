//! TCVN3 (ABC) → Unicode for SMILE text, a port of `tcvn3.go`.
//!
//! SMILE stores Vietnamese in TCVN3 bytes inside cp1252 varchar columns, so the driver
//! hands us the cp1252 characters of those bytes (U+00A1..U+00FE map to themselves).

use crate::domain::CollectedInvoice;

/// TCVN3 byte → Vietnamese letter (the table of `tcvn3Byte`).
const TABLE: &[(u8, char)] = &[
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

fn map(c: char) -> Option<char> {
    let u = c as u32;
    if !(0xA1..=0xFE).contains(&u) {
        return None;
    }
    TABLE
        .iter()
        .find(|(b, _)| u32::from(*b) == u)
        .map(|&(_, v)| v)
}

/// `maybeTCVN3`: converts every TCVN3 letter, turns NBSP into a space and trims trailing
/// spaces.
pub fn convert(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if let Some(v) = map(c) {
            out.push(v);
        } else if c == '\u{A0}' {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    let trimmed = out.trim_end_matches(' ').len();
    out.truncate(trimmed);
    out
}

/// `convertInvoiceTCVN3`: the free-text fields of headers, lines and items.
pub fn convert_invoice(inv: &mut CollectedInvoice) {
    let h = &mut inv.header;
    for f in [
        &mut h.company_name,
        &mut h.customer_addr,
        &mut h.guest_name,
        &mut h.address2,
        &mut h.inv_content,
        &mut h.buyer_contact_name,
    ] {
        *f = convert(f);
    }
    for ln in &mut inv.lines {
        ln.merged.unit = convert(&ln.merged.unit);
        for t in &mut ln.merged.desc_defaults {
            t.text = convert(&t.text);
        }
        for it in &mut ln.detail {
            it.group = convert(&it.group);
            it.unit = convert(&it.unit);
            for t in &mut it.name_defaults {
                t.text = convert(&t.text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_samples() {
        assert_eq!(convert("Doanh Thu Th\u{f8}c \u{a8}n"), "Doanh Thu Thức ăn");
        assert_eq!(
            convert("\u{a8}n s\u{b8}ng kh\u{b8}ch ph\u{df}ng"),
            "ăn sáng khách phòng"
        );
        assert_eq!(convert("H\u{f1}\u{a0}\u{a0}"), "Hủ");
        // Conversion is unconditional, as in Go: a cp1252 'é' (0xE9) is TCVN3 'ộ'.
        assert_eq!(convert("日本 Café  "), "日本 Cafộ");
    }
}
