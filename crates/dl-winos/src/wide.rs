//! UTF-16 conversions shared by the Windows code and the platform-independent decoders.

use crate::Error;

/// `s` as a NUL-terminated UTF-16 string. Rejects interior NUL characters, which would
/// silently truncate the value on the Windows side.
pub(crate) fn to_wide(s: &str) -> Result<Vec<u16>, Error> {
    if s.contains('\0') {
        return Err(Error::InvalidInput("string contains a NUL character"));
    }
    Ok(s.encode_utf16().chain(core::iter::once(0)).collect())
}

/// Little-endian UTF-16 code units of `bytes`. A trailing odd byte is dropped.
pub(crate) fn units_le(bytes: &[u8]) -> Vec<u16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u16::from_le_bytes(*p))
        .collect()
}

/// The string in `units` up to the first NUL, or all of `units` when there is none.
/// Unpaired surrogates become U+FFFD.
pub(crate) fn until_nul(units: &[u16]) -> String {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_rejects_nul() {
        assert_eq!(to_wide("ab").unwrap(), vec![97, 98, 0]);
        assert_eq!(to_wide("").unwrap(), vec![0]);
        assert_eq!(to_wide("đ").unwrap(), vec![0x111, 0]);
        assert!(matches!(to_wide("a\0b"), Err(Error::InvalidInput(_))));
    }

    #[test]
    fn units_and_nul() {
        assert_eq!(units_le(&[]), Vec::<u16>::new());
        assert_eq!(units_le(&[0x41]), Vec::<u16>::new());
        assert_eq!(units_le(&[0x41, 0, 0x11, 0x01, 0x7f]), vec![0x41, 0x111]);
        assert_eq!(until_nul(&[0x41, 0x42]), "AB");
        assert_eq!(until_nul(&[0x41, 0, 0x42]), "A");
        assert_eq!(until_nul(&[]), "");
        assert_eq!(until_nul(&[0xD800, 0x41]), "\u{FFFD}A");
    }
}
