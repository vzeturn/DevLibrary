//! Pure conversions between the portable API values and the platform's PC/SC values.
//! Both platform variants are compiled everywhere so they can be tested on any host.

use crate::{CardState, Protocol, Protocols};

/// `SCARD_PROTOCOL_RAW` in winscard.h. (Unused on other platforms outside tests.)
#[allow(dead_code)]
pub(crate) const PROTOCOL_RAW_WINDOWS: u32 = 0x0001_0000;
/// `SCARD_PROTOCOL_RAW` in pcsc-lite and macOS PCSC.framework. (Unused on Windows
/// outside tests.)
#[allow(dead_code)]
pub(crate) const PROTOCOL_RAW_PCSCLITE: u32 = 0x0000_0004;
/// `SCARD_PROTOCOL_RAW` of this platform.
#[cfg(windows)]
pub(crate) const PROTOCOL_RAW: u32 = PROTOCOL_RAW_WINDOWS;
/// `SCARD_PROTOCOL_RAW` of this platform.
#[cfg(not(windows))]
pub(crate) const PROTOCOL_RAW: u32 = PROTOCOL_RAW_PCSCLITE;

const PROTOCOL_T0: u32 = 1;
const PROTOCOL_T1: u32 = 2;

/// The `dwPreferredProtocols` bit mask for `protocols`, given the platform's RAW bit.
pub(crate) fn protocols_to_raw(protocols: Protocols, raw_bit: u32) -> u32 {
    let mut r = 0;
    if protocols.contains(Protocols::T0) {
        r |= PROTOCOL_T0;
    }
    if protocols.contains(Protocols::T1) {
        r |= PROTOCOL_T1;
    }
    if protocols.contains(Protocols::RAW) {
        r |= raw_bit;
    }
    r
}

/// The protocol for an active-protocol value (`SCARD_PROTOCOL_UNDEFINED` gives `None`).
pub(crate) fn protocol_from_raw(value: u32, raw_bit: u32) -> Option<Protocol> {
    match value {
        PROTOCOL_T0 => Some(Protocol::T0),
        PROTOCOL_T1 => Some(Protocol::T1),
        v if v == raw_bit => Some(Protocol::Raw),
        _ => None,
    }
}

/// `SCardStatus` `dwState` on Windows: an ordinal (`SCARD_UNKNOWN` = 0 ...
/// `SCARD_SPECIFIC` = 6).
pub(crate) fn card_state_windows(value: u32) -> CardState {
    match value {
        1 => CardState::Absent,
        2 => CardState::Present,
        3 => CardState::Swallowed,
        4 => CardState::Powered,
        5 => CardState::Negotiable,
        6 => CardState::Specific,
        _ => CardState::Unknown,
    }
}

/// `SCardStatus` `dwState` on pcsc-lite and macOS: a bit mask (`SCARD_UNKNOWN` = 0x01 ...
/// `SCARD_SPECIFIC` = 0x40) where several bits are set at once (e.g. PRESENT | POWERED |
/// NEGOTIABLE); the most advanced state wins.
pub(crate) fn card_state_pcsclite(value: u32) -> CardState {
    if value & 0x40 != 0 {
        CardState::Specific
    } else if value & 0x20 != 0 {
        CardState::Negotiable
    } else if value & 0x10 != 0 {
        CardState::Powered
    } else if value & 0x08 != 0 {
        CardState::Swallowed
    } else if value & 0x04 != 0 {
        CardState::Present
    } else if value & 0x02 != 0 {
        CardState::Absent
    } else {
        CardState::Unknown
    }
}

/// `SCardStatus` `dwState` of this platform.
pub(crate) fn card_state(value: u32) -> CardState {
    if cfg!(windows) {
        card_state_windows(value)
    } else {
        card_state_pcsclite(value)
    }
}

/// Splits a PC/SC multi-string (NUL-separated names ending with an empty name) of UTF-16
/// code units. Stops at the first empty name; invalid UTF-16 is replaced.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn split_multi_u16(buf: &[u16]) -> Vec<String> {
    buf.split(|c| *c == 0)
        .take_while(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// Splits a PC/SC multi-string of UTF-8 bytes. Stops at the first empty name; invalid
/// UTF-8 is replaced.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn split_multi_u8(buf: &[u8]) -> Vec<String> {
    buf.split(|c| *c == 0)
        .take_while(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocols() {
        assert_eq!(protocols_to_raw(Protocols::ANY, PROTOCOL_RAW_WINDOWS), 3);
        assert_eq!(protocols_to_raw(Protocols::T1, PROTOCOL_RAW_WINDOWS), 2);
        assert_eq!(
            protocols_to_raw(Protocols::RAW, PROTOCOL_RAW_WINDOWS),
            0x10000
        );
        assert_eq!(
            protocols_to_raw(Protocols::RAW | Protocols::T0, PROTOCOL_RAW_PCSCLITE),
            5
        );
        assert_eq!(protocols_to_raw(Protocols::NONE, PROTOCOL_RAW_PCSCLITE), 0);
        assert_eq!(protocol_from_raw(1, PROTOCOL_RAW), Some(Protocol::T0));
        assert_eq!(protocol_from_raw(2, PROTOCOL_RAW), Some(Protocol::T1));
        assert_eq!(
            protocol_from_raw(0x10000, PROTOCOL_RAW_WINDOWS),
            Some(Protocol::Raw)
        );
        assert_eq!(
            protocol_from_raw(4, PROTOCOL_RAW_PCSCLITE),
            Some(Protocol::Raw)
        );
        assert_eq!(protocol_from_raw(4, PROTOCOL_RAW_WINDOWS), None);
        assert_eq!(protocol_from_raw(0, PROTOCOL_RAW), None);
    }

    #[test]
    fn card_states() {
        assert_eq!(card_state_windows(0), CardState::Unknown);
        assert_eq!(card_state_windows(2), CardState::Present);
        assert_eq!(card_state_windows(6), CardState::Specific);
        assert_eq!(card_state_windows(99), CardState::Unknown);
        assert_eq!(card_state_pcsclite(0x01), CardState::Unknown);
        assert_eq!(card_state_pcsclite(0x02), CardState::Absent);
        assert_eq!(
            card_state_pcsclite(0x04 | 0x10 | 0x20),
            CardState::Negotiable
        );
        assert_eq!(card_state_pcsclite(0x04 | 0x10 | 0x40), CardState::Specific);
        assert_eq!(card_state_pcsclite(0), CardState::Unknown);
    }

    #[test]
    fn multi_strings() {
        let s: Vec<u16> = "ACS ACR122 0\0Other Reader 1\0\0".encode_utf16().collect();
        assert_eq!(split_multi_u16(&s), ["ACS ACR122 0", "Other Reader 1"]);
        assert!(split_multi_u16(&[0, 0]).is_empty());
        assert!(split_multi_u16(&[]).is_empty());
        // Missing final terminator: the last name is still returned.
        let s: Vec<u16> = "A\0B".encode_utf16().collect();
        assert_eq!(split_multi_u16(&s), ["A", "B"]);
        assert_eq!(
            split_multi_u8(b"ACS ACR122U PICC Interface 00 00\0\0garbage"),
            ["ACS ACR122U PICC Interface 00 00"]
        );
        assert_eq!(split_multi_u8(b"\xFFx\0\0"), ["\u{FFFD}x"]);
        assert!(split_multi_u8(b"\0").is_empty());
    }
}
