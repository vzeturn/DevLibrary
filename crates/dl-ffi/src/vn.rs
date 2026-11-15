//! Vietnam functions: amount in words, diacritics removal, VietQR.

use crate::abi::{DL_ERR_INVALID_INPUT, DL_OK, input, output};
use alloc::format;
use alloc::string::{String, ToString};
use dl_vn::text;
use dl_vn::vietqr::{Initiation, Service, VietQr};
use dl_vn::words::{self, Billion, Four, Linh, Options, Thousand};

/// Read "lẻ" instead of "linh" (một trăm lẻ năm).
pub const DL_WORDS_LE: u32 = 1;
/// Read "ngàn" instead of "nghìn".
pub const DL_WORDS_NGAN: u32 = 1 << 1;
/// Read "tư" instead of "bốn" after "mươi".
pub const DL_WORDS_TU: u32 = 1 << 2;
/// Write "tỉ" instead of "tỷ".
pub const DL_WORDS_TI: u32 = 1 << 3;
/// Read a negative amount by its magnitude, without "Âm".
pub const DL_WORDS_UNSIGNED: u32 = 1 << 4;
/// Append " chẵn" after the unit.
pub const DL_WORDS_CHAN: u32 = 1 << 5;

fn options(flags: u32) -> Options {
    Options {
        linh: if flags & DL_WORDS_LE != 0 {
            Linh::Le
        } else {
            Linh::Linh
        },
        thousand: if flags & DL_WORDS_NGAN != 0 {
            Thousand::Ngan
        } else {
            Thousand::Nghin
        },
        billion: if flags & DL_WORDS_TI != 0 {
            Billion::Ti
        } else {
            Billion::Ty
        },
        four: if flags & DL_WORDS_TU != 0 {
            Four::Tu
        } else {
            Four::Bon
        },
        chan: false,
    }
}

/// Reads `n` in lower-case Vietnamese: 1050000 → "một triệu không trăm năm mươi nghìn".
///
/// # Safety
/// See the buffer conventions in the crate documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dl_vn_number_words(
    n: u64,
    flags: u32,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    let text = words::number_to_words_with(n, &options(flags));
    // SAFETY: forwarded caller contract.
    unsafe { output(&text, DL_OK, out, cap, out_len) }
}

/// Reads an amount for the "Số tiền bằng chữ" line: capitalised, followed by `unit`
/// (UTF-8, may be empty), "Âm" in front of a negative amount unless
/// [`DL_WORDS_UNSIGNED`] is set. 1050000, "đồng" → "Một triệu không trăm năm mươi nghìn
/// đồng". Accepts every `i64`.
///
/// # Safety
/// See the buffer conventions in the crate documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dl_vn_amount_words(
    amount: i64,
    unit: *const u8,
    unit_len: usize,
    flags: u32,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    // SAFETY: forwarded caller contract for `unit`.
    let unit = match unsafe { input(unit, unit_len) } {
        Ok(u) => u,
        Err(e) => return e,
    };
    let negative = amount < 0 && flags & DL_WORDS_UNSIGNED == 0;
    let reading = words::number_to_words_with(amount.unsigned_abs(), &options(flags));
    let mut text = String::with_capacity(reading.len() + unit.len() + 16);
    if negative {
        text.push_str("Âm ");
        text.push_str(&reading);
    } else {
        // Every reading starts with an ASCII lower-case letter.
        let mut chars = reading.chars();
        if let Some(first) = chars.next() {
            text.extend(first.to_uppercase());
            text.push_str(chars.as_str());
        }
    }
    if !unit.is_empty() {
        text.push(' ');
        text.push_str(unit);
    }
    if flags & DL_WORDS_CHAN != 0 {
        text.push_str(" chẵn");
    }
    // SAFETY: forwarded caller contract.
    unsafe { output(&text, DL_OK, out, cap, out_len) }
}

/// Removes Vietnamese diacritics (all tones, đ/Đ, NFC or NFD input): "Bàn 5" → "Ban 5".
///
/// # Safety
/// See the buffer conventions in the crate documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dl_vn_remove_diacritics(
    text_ptr: *const u8,
    text_len: usize,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    // SAFETY: forwarded caller contract.
    let s = match unsafe { input(text_ptr, text_len) } {
        Ok(s) => s,
        Err(e) => return e,
    };
    let folded = text::remove_diacritics(s);
    // SAFETY: forwarded caller contract.
    unsafe { output(&folded, DL_OK, out, cap, out_len) }
}

/// Build a dynamic code (field 01 = "12", one payment) instead of a static one ("11").
pub const DL_VIETQR_DYNAMIC: u32 = 1;
/// Transfer to a card number (QRIBFTTC) instead of an account number (QRIBFTTA).
pub const DL_VIETQR_CARD: u32 = 1 << 1;
/// Make the message and bill number valid instead of rejecting them: remove diacritics,
/// replace characters outside printable ASCII with spaces, collapse spaces, and cut to
/// 25 characters. A text that becomes empty is omitted.
pub const DL_VIETQR_FOLD_TEXT: u32 = 1 << 2;

/// Longest message or bill number a VietQR field `62.xx` holds.
const MAX_ADDITIONAL: usize = 25;

fn fold_field(s: &str) -> Option<String> {
    let folded = text::remove_diacritics(s);
    let mut out = String::with_capacity(MAX_ADDITIONAL);
    let mut space = false;
    for c in folded.chars() {
        let c = if (' '..='~').contains(&c) { c } else { ' ' };
        if c == ' ' {
            space = !out.is_empty();
            continue;
        }
        // `out` is ASCII, so bytes are characters. Stop before the separator and the
        // character would exceed the limit, so the result never ends with a space.
        let need = 1 + usize::from(space);
        if out.len() + need > MAX_ADDITIONAL {
            break;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    (!out.is_empty()).then_some(out)
}

fn optional(s: &str, fold: bool) -> Option<String> {
    if fold {
        fold_field(s)
    } else {
        (!s.is_empty()).then(|| s.to_string())
    }
}

/// Builds a NAPAS VietQR payload (EMVCo MPM), CRC included, ready to encode as a QR code.
///
/// `amount` is in đồng; 0 means "no amount" (the payer enters it). `bill` (field 62.01) and
/// `purpose` (62.08) may be empty. Without [`DL_VIETQR_FOLD_TEXT`], text with diacritics or
/// longer than 25 characters is rejected, as the specification requires.
///
/// Returns [`DL_ERR_INVALID_INPUT`] with a message naming the field for an invalid BIN,
/// account, amount (above 9 999 999 999 999) or text.
///
/// # Safety
/// See the buffer conventions in the crate documentation.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn dl_vietqr_payload(
    bin: *const u8,
    bin_len: usize,
    account: *const u8,
    account_len: usize,
    amount: u64,
    flags: u32,
    bill: *const u8,
    bill_len: usize,
    purpose: *const u8,
    purpose_len: usize,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    // SAFETY: forwarded caller contract for every input pair.
    let fields = unsafe {
        (
            input(bin, bin_len),
            input(account, account_len),
            input(bill, bill_len),
            input(purpose, purpose_len),
        )
    };
    let (bin, account, bill, purpose) = match fields {
        (Ok(a), Ok(b), Ok(c), Ok(d)) => (a, b, c, d),
        (Err(e), ..) | (_, Err(e), ..) | (_, _, Err(e), _) | (.., Err(e)) => return e,
    };
    let fold = flags & DL_VIETQR_FOLD_TEXT != 0;
    let qr = VietQr {
        initiation: if flags & DL_VIETQR_DYNAMIC != 0 {
            Initiation::Dynamic
        } else {
            Initiation::Static
        },
        bin: bin.to_string(),
        account: account.to_string(),
        service: if flags & DL_VIETQR_CARD != 0 {
            Service::Card
        } else {
            Service::Account
        },
        amount: (amount != 0).then_some(amount),
        bill_number: optional(bill, fold),
        purpose: optional(purpose, fold),
    };
    let (text, status) = match qr.to_payload() {
        Ok(p) => (p, DL_OK),
        Err(e) => (format!("{e}"), DL_ERR_INVALID_INPUT),
    };
    // SAFETY: forwarded caller contract.
    unsafe { output(&text, status, out, cap, out_len) }
}

/// Checks a scanned or stored VietQR payload: structure, every field and the CRC.
/// Returns [`DL_OK`] or [`DL_ERR_INVALID_INPUT`] with the reason in `out`.
///
/// # Safety
/// See the buffer conventions in the crate documentation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dl_vietqr_verify(
    payload: *const u8,
    payload_len: usize,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    // SAFETY: forwarded caller contract.
    let p = match unsafe { input(payload, payload_len) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    let (text, status) = match VietQr::parse(p) {
        Ok(_) => (String::new(), DL_OK),
        Err(e) => (format!("{e}"), DL_ERR_INVALID_INPUT),
    };
    // SAFETY: forwarded caller contract.
    unsafe { output(&text, status, out, cap, out_len) }
}

#[cfg(test)]
#[allow(clippy::undocumented_unsafe_blocks)] // tests pass valid local buffers
mod tests {
    use super::*;
    use crate::abi::*;
    use std::vec::Vec;

    fn call(f: impl Fn(*mut u8, usize, *mut usize) -> i32) -> (i32, String) {
        let mut len = 0usize;
        let first = f(core::ptr::null_mut(), 0, &mut len);
        if first != DL_ERR_BUFFER_TOO_SMALL {
            assert_eq!(len, 0, "empty result expected with status {first}");
            return (first, String::new());
        }
        let mut buf = Vec::from_iter(core::iter::repeat_n(0u8, len));
        let status = f(buf.as_mut_ptr(), buf.len(), &mut len);
        assert_eq!(len, buf.len());
        (status, String::from_utf8(buf).unwrap())
    }

    fn words(amount: i64, unit: &str, flags: u32) -> String {
        let (s, t) = call(|o, c, l| unsafe {
            dl_vn_amount_words(amount, unit.as_ptr(), unit.len(), flags, o, c, l)
        });
        assert_eq!(s, DL_OK);
        t
    }

    #[test]
    fn amount_words() {
        assert_eq!(
            words(1_050_000, "đồng", 0),
            "Một triệu không trăm năm mươi nghìn đồng"
        );
        assert_eq!(words(-500_000, "đồng", 0), "Âm năm trăm nghìn đồng");
        assert_eq!(
            words(-500_000, "đồng", DL_WORDS_UNSIGNED),
            "Năm trăm nghìn đồng"
        );
        assert_eq!(words(105, "đồng", DL_WORDS_LE), "Một trăm lẻ năm đồng");
        assert_eq!(
            words(500_000, "đồng", DL_WORDS_CHAN),
            "Năm trăm nghìn đồng chẵn"
        );
        assert_eq!(words(5, "", 0), "Năm");
        assert!(words(i64::MIN, "đồng", 0).starts_with("Âm chín tỷ"));
        assert!(words(i64::MIN, "đồng", DL_WORDS_UNSIGNED).starts_with("Chín tỷ"));
        let (s, t) = call(|o, c, l| unsafe { dl_vn_number_words(24, DL_WORDS_TU, o, c, l) });
        assert_eq!((s, t.as_str()), (DL_OK, "hai mươi tư"));
    }

    #[test]
    fn buffer_rules() {
        let unit = "đồng";
        let mut len = 0;
        let mut small = [0u8; 4];
        let s = unsafe {
            dl_vn_amount_words(
                1,
                unit.as_ptr(),
                unit.len(),
                0,
                small.as_mut_ptr(),
                small.len(),
                &mut len,
            )
        };
        assert_eq!((s, len), (DL_ERR_BUFFER_TOO_SMALL, "Một đồng".len()));
        assert_eq!(small, [0; 4], "nothing written on BUFFER_TOO_SMALL");
        let s = unsafe {
            dl_vn_amount_words(1, core::ptr::null(), 3, 0, small.as_mut_ptr(), 4, &mut len)
        };
        assert_eq!(s, DL_ERR_NULL_POINTER);
        let bad = [0xffu8, 0xfe];
        let s =
            unsafe { dl_vn_amount_words(1, bad.as_ptr(), 2, 0, small.as_mut_ptr(), 4, &mut len) };
        assert_eq!(s, DL_ERR_INVALID_UTF8);
        let s = unsafe { dl_vn_number_words(1, 0, small.as_mut_ptr(), 4, core::ptr::null_mut()) };
        assert_eq!(s, DL_ERR_NULL_POINTER);
        assert_eq!(crate::dl_ffi_abi_version(), crate::DL_FFI_ABI_VERSION);
    }

    fn payload(
        bin: &str,
        acc: &str,
        amount: u64,
        flags: u32,
        bill: &str,
        purpose: &str,
    ) -> (i32, String) {
        call(|o, c, l| unsafe {
            dl_vietqr_payload(
                bin.as_ptr(),
                bin.len(),
                acc.as_ptr(),
                acc.len(),
                amount,
                flags,
                bill.as_ptr(),
                bill.len(),
                purpose.as_ptr(),
                purpose.len(),
                o,
                c,
                l,
            )
        })
    }

    fn verify(p: &str) -> i32 {
        call(|o, c, l| unsafe { dl_vietqr_verify(p.as_ptr(), p.len(), o, c, l) }).0
    }

    #[test]
    fn vietqr() {
        // NAPAS example 6.1.3, through the C ABI.
        let (s, p) = payload(
            "970403",
            "0011012345678",
            180_000,
            DL_VIETQR_DYNAMIC,
            "NPS6869",
            "thanh toan don hang",
        );
        assert_eq!(s, DL_OK);
        assert_eq!(
            p,
            "00020101021238570010A00000072701270006970403011300110123456780208QRIBFTTA\
             530370454061800005802VN62340107NPS68690819thanh toan don hang63042E2E"
        );
        assert_eq!(verify(&p), DL_OK);

        // Vietnamese text is rejected unless folding is asked for.
        let (s, msg) = payload(
            "970436",
            "1031933430",
            50_000,
            DL_VIETQR_DYNAMIC,
            "",
            "Bàn 5",
        );
        assert_eq!(s, DL_ERR_INVALID_INPUT);
        assert!(msg.contains("62.08"), "{msg}");
        let (s, p) = payload(
            "970436",
            "1031933430",
            50_000,
            DL_VIETQR_DYNAMIC | DL_VIETQR_FOLD_TEXT,
            "",
            "Bàn 5",
        );
        assert_eq!(s, DL_OK);
        assert!(p.contains("0805Ban 5"), "{p}");
        assert_eq!(verify(&p), DL_OK);

        // Folding: tabs/emoji become spaces, spaces collapse, cut to 25 characters.
        let (s, p) = payload(
            "970436",
            "1031933430",
            0,
            DL_VIETQR_FOLD_TEXT,
            "",
            "  Tiền\tphòng  101 🎉 khách sạn Đà Nẵng xyz ",
        );
        assert_eq!(s, DL_OK);
        assert!(p.contains("0824Tien phong 101 khach san63"), "{p}");
        assert_eq!(verify(&p), DL_OK);
        // A word ending exactly at character 25 followed by more text (regression of 0.2.0,
        // which produced 27 characters and an invalid payload).
        let (s, p) = payload(
            "970436",
            "1031933430",
            0,
            DL_VIETQR_FOLD_TEXT,
            "",
            "Thanh toán hoá đơn bàn số 12",
        );
        assert_eq!(s, DL_OK, "{p}");
        assert!(p.contains("0825Thanh toan hoa don ban so63"), "{p}");
        // Every cut length from 1 to 60 characters stays valid and at most 25 long.
        let long = "ab cde f ghij klm nopqrs tuv wx yz Đà Nẵng Hà Nội Sài Gòn Huế";
        for end in 1..=long.chars().count() {
            let part: String = long.chars().take(end).collect();
            let folded = fold_field(&part);
            if let Some(f) = &folded {
                assert!(
                    f.len() <= MAX_ADDITIONAL && !f.ends_with(' ') && !f.starts_with(' '),
                    "{part:?} -> {f:?}"
                );
            }
            let (s, p) = payload("970436", "1031933430", 0, DL_VIETQR_FOLD_TEXT, &part, &part);
            assert_eq!(s, DL_OK, "{part:?}: {p}");
            assert_eq!(verify(&p), DL_OK, "{part:?}");
        }
        // Text that folds to nothing is omitted.
        let (s, p) = payload("970436", "1031933430", 0, DL_VIETQR_FOLD_TEXT, "", "🎉");
        assert_eq!(s, DL_OK);
        assert!(p.contains("5802VN6304"), "{p}");

        // Amount bounds and BIN.
        assert_eq!(
            payload("970436", "1031933430", 10_000_000_000_000, 0, "", "").0,
            DL_ERR_INVALID_INPUT
        );
        assert_eq!(
            payload("97043", "1031933430", 1, 0, "", "").0,
            DL_ERR_INVALID_INPUT
        );

        // A corrupted payload fails verification.
        let mut bad = p.clone();
        bad.replace_range(bad.len() - 1.., "0");
        assert_eq!(verify(&bad), DL_ERR_INVALID_INPUT);
    }

    #[test]
    fn diacritics() {
        let name = "Đặng Thị Hương";
        let (s, t) =
            call(|o, c, l| unsafe { dl_vn_remove_diacritics(name.as_ptr(), name.len(), o, c, l) });
        assert_eq!((s, t.as_str()), (DL_OK, "Dang Thi Huong"));
    }
}
