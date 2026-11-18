//! APDU helpers for contactless readers (ACR122U and other PC/SC 2.01 part 3 readers).

use crate::Error;

/// Status word of a successful command (`90 00`).
pub const SW_OK: u16 = 0x9000;

/// The PC/SC part 3 pseudo-APDU `GET DATA` for the card UID: `FF CA 00 00 00`.
///
/// Readers such as the ACR122U answer with the UID (4, 7 or 10 bytes for MIFARE / ISO
/// 14443-A cards) followed by `90 00`; `63 00` means the operation failed and `6A 81` that
/// the function is not supported for this card.
pub const fn get_uid() -> [u8; 5] {
    [0xFF, 0xCA, 0x00, 0x00, 0x00]
}

/// Splits a response into its data and status word (`SW1 << 8 | SW2`).
///
/// Errors with [`Error::ShortResponse`] when the response has fewer than 2 bytes.
pub fn split_status(response: &[u8]) -> Result<(&[u8], u16), Error> {
    match response {
        [data @ .., sw1, sw2] => Ok((data, u16::from_be_bytes([*sw1, *sw2]))),
        _ => Err(Error::ShortResponse {
            len: response.len(),
        }),
    }
}

/// The UID in a response to [`get_uid`] as uppercase hex without separators (e.g.
/// `"04A224B2C15E80"`).
///
/// Errors with [`Error::Status`] when the status word is not `90 00` and with
/// [`Error::ShortResponse`] when the response has no status word or no UID bytes.
pub fn uid_hex(response: &[u8]) -> Result<String, Error> {
    let (data, sw) = split_status(response)?;
    if sw != SW_OK {
        return Err(Error::Status { sw });
    }
    if data.is_empty() {
        return Err(Error::ShortResponse {
            len: response.len(),
        });
    }
    Ok(to_hex_upper(data))
}

/// Uppercase hex of `bytes`.
pub(crate) fn to_hex_upper(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 0x0F)]));
    }
    s
}

/// Whether a command APDU uses the extended length encoding (ISO 7816-4 5.1): a zero byte
/// after the 4-byte header followed by at least two more bytes. Extended responses can be
/// up to 65536 data bytes plus the status word.
pub(crate) fn is_extended(apdu: &[u8]) -> bool {
    apdu.len() >= 7 && apdu[4] == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_uid_bytes() {
        assert_eq!(get_uid(), [0xFF, 0xCA, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn split() {
        let (d, sw) = split_status(&[0x01, 0x02, 0x90, 0x00]).unwrap();
        assert_eq!((d, sw), (&[1u8, 2][..], 0x9000));
        let (d, sw) = split_status(&[0x63, 0x00]).unwrap();
        assert_eq!((d, sw), (&[][..], 0x6300));
        assert_eq!(split_status(&[0x90]), Err(Error::ShortResponse { len: 1 }));
        assert_eq!(split_status(&[]), Err(Error::ShortResponse { len: 0 }));
    }

    #[test]
    fn uid() {
        // 4-byte MIFARE Classic UID and 7-byte NTAG UID.
        assert_eq!(
            uid_hex(&[0xDE, 0xAD, 0xbe, 0x0f, 0x90, 0x00]).unwrap(),
            "DEADBE0F"
        );
        assert_eq!(
            uid_hex(&[0x04, 0xA2, 0x24, 0xB2, 0xC1, 0x5E, 0x80, 0x90, 0x00]).unwrap(),
            "04A224B2C15E80"
        );
        assert_eq!(uid_hex(&[0x63, 0x00]), Err(Error::Status { sw: 0x6300 }));
        assert_eq!(
            uid_hex(&[0x01, 0x6A, 0x81]),
            Err(Error::Status { sw: 0x6A81 })
        );
        assert_eq!(uid_hex(&[0x90, 0x00]), Err(Error::ShortResponse { len: 2 }));
        assert_eq!(uid_hex(&[0x00]), Err(Error::ShortResponse { len: 1 }));
    }

    #[test]
    fn extended_detection() {
        assert!(!is_extended(&get_uid()));
        assert!(!is_extended(&[0x00, 0xA4, 0x04, 0x00]));
        assert!(!is_extended(&[0x00, 0xA4, 0x04, 0x00, 0x02, 0x3F, 0x00]));
        // Case 2E: Le = 00 00 00 (65536 bytes).
        assert!(is_extended(&[0x00, 0xB0, 0x00, 0x00, 0x00, 0x00, 0x00]));
        // Case 3E with Lc = 00 00 01.
        assert!(is_extended(&[
            0x00, 0xD6, 0x00, 0x00, 0x00, 0x00, 0x01, 0xAA
        ]));
    }
}
