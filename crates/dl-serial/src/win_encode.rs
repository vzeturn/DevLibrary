//! Pure encodings for the Win32 serial API: DCB flag words, COMMTIMEOUTS values, device
//! paths, error mapping and registry name parsing. Compiled and tested on every platform;
//! only `sys_windows` uses it at run time.
#![cfg_attr(not(windows), allow(dead_code))]

use core::time::Duration;

use crate::settings::millis_ceil;
use crate::{Error, Flow, Parity, Settings, StopBits};

// DCB bit field positions (winbase.h, in declaration order).
pub(crate) const F_BINARY: u32 = 1 << 0;
pub(crate) const F_PARITY: u32 = 1 << 1;
pub(crate) const F_OUTX_CTS_FLOW: u32 = 1 << 2;
pub(crate) const DTR_CONTROL_SHIFT: u32 = 4;
pub(crate) const F_TX_CONTINUE_ON_XOFF: u32 = 1 << 7;
pub(crate) const F_OUTX: u32 = 1 << 8;
pub(crate) const F_INX: u32 = 1 << 9;
pub(crate) const RTS_CONTROL_SHIFT: u32 = 12;
// Always left 0: no DSR handshake, no error-char replacement, keep NUL bytes, and errors
// do not abort I/O until ClearCommError.
#[cfg(test)]
pub(crate) const F_OUTX_DSR_FLOW: u32 = 1 << 3;
#[cfg(test)]
pub(crate) const F_DSR_SENSITIVITY: u32 = 1 << 6;
#[cfg(test)]
pub(crate) const F_ERROR_CHAR: u32 = 1 << 10;
#[cfg(test)]
pub(crate) const F_NULL: u32 = 1 << 11;
#[cfg(test)]
pub(crate) const F_ABORT_ON_ERROR: u32 = 1 << 14;
/// Bits 0..=14 are defined; `fDummy2` (15..=31) is kept as the driver returned it.
pub(crate) const DEFINED_FLAGS: u32 = 0x7FFF;

pub(crate) const DTR_CONTROL_DISABLE: u32 = 0;
pub(crate) const DTR_CONTROL_ENABLE: u32 = 1;
pub(crate) const RTS_CONTROL_DISABLE: u32 = 0;
pub(crate) const RTS_CONTROL_ENABLE: u32 = 1;
pub(crate) const RTS_CONTROL_HANDSHAKE: u32 = 2;

pub(crate) const XON: u8 = 0x11;
pub(crate) const XOFF: u8 = 0x13;

const MAXDWORD: u32 = u32::MAX;

/// The DCB fields this crate sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DcbConfig {
    pub baud_rate: u32,
    pub flags: u32,
    pub byte_size: u8,
    pub parity: u8,
    pub stop_bits: u8,
    pub xon_char: u8,
    pub xoff_char: u8,
    pub xon_lim: u16,
    pub xoff_lim: u16,
}

/// Builds the DCB fields for `s`, with the DTR and RTS lines raised or not.
///
/// `old_flags` is the flag word from `GetCommState`; only its reserved bits are kept.
pub(crate) fn dcb_config(s: &Settings, old_flags: u32, dtr: bool, rts: bool) -> DcbConfig {
    let mut flags = (old_flags & !DEFINED_FLAGS) | F_BINARY | F_TX_CONTINUE_ON_XOFF;
    if s.parity != Parity::None {
        flags |= F_PARITY;
    }
    let dtr_control = if dtr {
        DTR_CONTROL_ENABLE
    } else {
        DTR_CONTROL_DISABLE
    };
    flags |= dtr_control << DTR_CONTROL_SHIFT;
    let rts_control = match s.flow {
        Flow::RtsCts => {
            flags |= F_OUTX_CTS_FLOW;
            RTS_CONTROL_HANDSHAKE
        }
        Flow::XonXoff => {
            flags |= F_OUTX | F_INX;
            if rts {
                RTS_CONTROL_ENABLE
            } else {
                RTS_CONTROL_DISABLE
            }
        }
        Flow::None if rts => RTS_CONTROL_ENABLE,
        Flow::None => RTS_CONTROL_DISABLE,
    };
    flags |= rts_control << RTS_CONTROL_SHIFT;
    DcbConfig {
        baud_rate: s.baud_rate,
        flags,
        byte_size: s.data_bits.bits(),
        parity: match s.parity {
            Parity::None => 0,
            Parity::Odd => 1,
            Parity::Even => 2,
            Parity::Mark => 3,
            Parity::Space => 4,
        },
        stop_bits: match s.stop_bits {
            StopBits::One => 0,
            StopBits::OnePointFive => 1,
            StopBits::Two => 2,
        },
        xon_char: XON,
        xoff_char: XOFF,
        // Same limits as .NET's SerialPort for its 4096-byte input buffer.
        xon_lim: 1024,
        xoff_lim: 1024,
    }
}

/// `COMMTIMEOUTS` in declaration order: ReadIntervalTimeout, ReadTotalTimeoutMultiplier,
/// ReadTotalTimeoutConstant, WriteTotalTimeoutMultiplier, WriteTotalTimeoutConstant.
///
/// Reads use MAXDWORD / MAXDWORD / timeout: ReadFile returns as soon as one byte arrives
/// (with everything already buffered), or with 0 bytes when the timeout expires. `None`
/// uses the largest constant and the caller retries; `Some(ZERO)` returns at once.
pub(crate) fn comm_timeouts(read: Option<Duration>, write: Option<Duration>) -> [u32; 5] {
    const LONGEST: u64 = (MAXDWORD - 1) as u64;
    let (interval, multiplier, constant) = match read {
        Some(d) if d.is_zero() => (MAXDWORD, 0, 0),
        Some(d) => (MAXDWORD, MAXDWORD, millis_ceil(d, LONGEST) as u32),
        None => (MAXDWORD, MAXDWORD, LONGEST as u32),
    };
    // A zero write constant means "no timeout" to Windows, so the shortest one is 1 ms.
    let write_constant = match write {
        Some(d) => millis_ceil(d, LONGEST).max(1) as u32,
        None => 0,
    };
    [interval, multiplier, constant, 0, write_constant]
}

/// Turns `COM3` into `\\.\COM3`, which also works for COM10 and above.
pub(crate) fn device_path(name: &str) -> Result<String, Error> {
    let invalid = || Error::InvalidName(name.to_string());
    if name.is_empty() || name.contains('\0') {
        return Err(invalid());
    }
    if let Some(rest) = name
        .strip_prefix(r"\\.\")
        .or_else(|| name.strip_prefix(r"\\?\"))
    {
        if rest.is_empty() {
            return Err(invalid());
        }
        return Ok(name.to_string());
    }
    let bad = |c: char| {
        c.is_whitespace()
            || c.is_control()
            || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    };
    if name.chars().any(bad) {
        return Err(invalid());
    }
    Ok(format!(r"\\.\{name}"))
}

/// Maps a `CreateFileW` error code for a port.
pub(crate) fn open_error(name: &str, code: u32) -> Error {
    match code {
        2 | 3 => Error::NotFound(name.to_string()),
        5 | 32 => Error::Busy(name.to_string()),
        123 | 161 => Error::InvalidName(name.to_string()),
        _ => Error::Os {
            op: "CreateFileW",
            code: code as i32,
        },
    }
}

/// Reads `VID_xxxx` and `PID_xxxx` from a USB or FTDIBUS device id such as
/// `VID_1A86&PID_7523` or `VID_0403+PID_6001+A50285BIA`.
pub(crate) fn parse_usb_ids(device_id: &str) -> Option<(u16, u16)> {
    let up = device_id.to_ascii_uppercase();
    let field = |key: &str| -> Option<u16> {
        let at = up.find(key)? + key.len();
        let hex = up.get(at..at + 4)?;
        u16::from_str_radix(hex, 16).ok()
    };
    Some((field("VID_")?, field("PID_")?))
}

/// The device serial number, when Windows used it as the instance id.
///
/// USB: the instance id is the serial unless it was generated (contains `&`) or the node
/// is one interface (`&MI_xx`) of a composite device. FTDIBUS: the third `+` part of the
/// device id, which is the serial followed by the channel letter.
pub(crate) fn usb_serial(bus: &str, device_id: &str, instance_id: &str) -> Option<String> {
    let s = if bus.eq_ignore_ascii_case("FTDIBUS") {
        let part = device_id.split('+').nth(2)?;
        let mut chars = part.chars();
        chars.next_back()?;
        chars.as_str()
    } else {
        if device_id.to_ascii_uppercase().contains("&MI_") || instance_id.contains('&') {
            return None;
        }
        instance_id
    };
    (!s.is_empty()).then(|| s.to_string())
}

/// Removes the ` (COM3)` suffix Windows appends to friendly names.
pub(crate) fn strip_com_suffix(friendly: &str) -> &str {
    let t = friendly.trim();
    if let Some(open) = t.rfind(" (")
        && t.ends_with(')')
        && t[open + 2..].to_ascii_uppercase().starts_with("COM")
    {
        return t[..open].trim_end();
    }
    t
}

/// The display text of a `DeviceDesc` value such as `@oem12.inf,%ch340%;USB-SERIAL CH340`.
pub(crate) fn device_desc_text(desc: &str) -> &str {
    desc.rsplit(';').next().unwrap_or(desc).trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DataBits;

    fn dtr_control(flags: u32) -> u32 {
        (flags >> 4) & 3
    }
    fn rts_control(flags: u32) -> u32 {
        (flags >> 12) & 3
    }

    #[test]
    fn dcb_default_8n1() {
        let c = dcb_config(&Settings::default(), 0, true, true);
        assert_eq!(c.baud_rate, 9600);
        assert_eq!((c.byte_size, c.parity, c.stop_bits), (8, 0, 0));
        // fBinary, fDtrControl = ENABLE, fTXContinueOnXoff, fRtsControl = ENABLE.
        assert_eq!(c.flags, 0x0000_1091);
        assert_eq!(c.flags & 1, 1, "fBinary must be set");
        assert_eq!(dtr_control(c.flags), 1);
        assert_eq!(rts_control(c.flags), 1);
        for bit in [
            F_PARITY,
            F_OUTX_CTS_FLOW,
            F_OUTX_DSR_FLOW,
            F_DSR_SENSITIVITY,
            F_OUTX,
            F_INX,
            F_ERROR_CHAR,
            F_NULL,
            F_ABORT_ON_ERROR,
        ] {
            assert_eq!(c.flags & bit, 0, "bit {bit:#x}");
        }
        assert_eq!((c.xon_char, c.xoff_char), (0x11, 0x13));
    }

    #[test]
    fn dcb_parity_stop_and_flow() {
        let s = Settings {
            baud_rate: 115_200,
            data_bits: DataBits::Seven,
            parity: Parity::Even,
            stop_bits: StopBits::Two,
            flow: Flow::RtsCts,
            ..Settings::default()
        };
        let c = dcb_config(&s, 0, false, true);
        assert_eq!((c.byte_size, c.parity, c.stop_bits), (7, 2, 2));
        // fBinary | fParity | fOutxCtsFlow | fTXContinueOnXoff | RTS_CONTROL_HANDSHAKE.
        assert_eq!(c.flags, 0x0000_2087);
        assert_eq!(dtr_control(c.flags), 0);
        assert_eq!(rts_control(c.flags), 2);

        let s = Settings {
            parity: Parity::Mark,
            data_bits: DataBits::Five,
            stop_bits: StopBits::OnePointFive,
            flow: Flow::XonXoff,
            ..Settings::default()
        };
        let c = dcb_config(&s, 0, true, false);
        assert_eq!((c.byte_size, c.parity, c.stop_bits), (5, 3, 1));
        assert_eq!(c.flags, 0x0000_0393);
        assert_eq!(rts_control(c.flags), 0);
        let odd = Settings {
            parity: Parity::Odd,
            ..Settings::default()
        };
        assert_eq!(dcb_config(&odd, 0, true, true).parity, 1);
        let space = Settings {
            parity: Parity::Space,
            ..Settings::default()
        };
        assert_eq!(dcb_config(&space, 0, true, true).parity, 4);
    }

    #[test]
    fn dcb_keeps_reserved_bits_only() {
        let c = dcb_config(&Settings::default(), 0xFFFF_FFFF, true, true);
        assert_eq!(c.flags & !DEFINED_FLAGS, 0xFFFF_8000);
        assert_eq!(c.flags & DEFINED_FLAGS, 0x1091);
    }

    #[test]
    fn timeouts() {
        let ms = Duration::from_millis;
        assert_eq!(
            comm_timeouts(Some(ms(3000)), Some(ms(5000))),
            [u32::MAX, u32::MAX, 3000, 0, 5000]
        );
        assert_eq!(
            comm_timeouts(Some(Duration::ZERO), Some(Duration::ZERO)),
            [u32::MAX, 0, 0, 0, 1]
        );
        assert_eq!(
            comm_timeouts(None, None),
            [u32::MAX, u32::MAX, u32::MAX - 1, 0, 0]
        );
        assert_eq!(
            comm_timeouts(Some(Duration::from_micros(1)), None)[2],
            1,
            "rounded up"
        );
        assert_eq!(
            comm_timeouts(Some(Duration::MAX), Some(Duration::MAX)),
            [u32::MAX, u32::MAX, u32::MAX - 1, 0, u32::MAX - 1]
        );
    }

    #[test]
    fn device_paths() {
        assert_eq!(device_path("COM3").unwrap(), r"\\.\COM3");
        assert_eq!(device_path("COM10").unwrap(), r"\\.\COM10");
        assert_eq!(device_path("CNCA0").unwrap(), r"\\.\CNCA0");
        assert_eq!(device_path(r"\\.\COM12").unwrap(), r"\\.\COM12");
        for bad in [
            "", "COM3\0", "COM 3", "COM3:", "a/b", r"a\b", r"\\.\", "COM?",
        ] {
            assert_eq!(
                device_path(bad),
                Err(Error::InvalidName(bad.to_string())),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn open_errors() {
        assert_eq!(open_error("COM250", 2), Error::NotFound("COM250".into()));
        assert_eq!(open_error("COM3", 5), Error::Busy("COM3".into()));
        assert_eq!(open_error("COM3", 32), Error::Busy("COM3".into()));
        assert_eq!(open_error("x", 123), Error::InvalidName("x".into()));
        assert_eq!(
            open_error("x", 1450),
            Error::Os {
                op: "CreateFileW",
                code: 1450
            }
        );
    }

    #[test]
    fn usb_ids_and_serials() {
        assert_eq!(parse_usb_ids("VID_1A86&PID_7523"), Some((0x1a86, 0x7523)));
        assert_eq!(
            parse_usb_ids("VID_0403+PID_6001+A50285BIA"),
            Some((0x0403, 0x6001))
        );
        assert_eq!(
            parse_usb_ids("vid_2341&pid_0043&MI_00"),
            Some((0x2341, 0x0043))
        );
        assert_eq!(parse_usb_ids("ROOT_HUB30"), None);
        assert_eq!(parse_usb_ids("VID_12"), None);
        assert_eq!(
            usb_serial("USB", "VID_2341&PID_0043", "85735313233351F0E1A1"),
            Some("85735313233351F0E1A1".into())
        );
        assert_eq!(usb_serial("USB", "VID_1A86&PID_7523", "5&2f1a3b&0&2"), None);
        assert_eq!(usb_serial("USB", "VID_2341&PID_8036&MI_00", "ABC"), None);
        assert_eq!(
            usb_serial("FTDIBUS", "VID_0403+PID_6001+A50285BIA", "0000"),
            Some("A50285BI".into())
        );
        assert_eq!(usb_serial("FTDIBUS", "VID_0403+PID_6001", "0000"), None);
    }

    #[test]
    fn friendly_names() {
        assert_eq!(
            strip_com_suffix("USB-SERIAL CH340 (COM3)"),
            "USB-SERIAL CH340"
        );
        assert_eq!(strip_com_suffix("Printer (USB)"), "Printer (USB)");
        assert_eq!(strip_com_suffix("Plain"), "Plain");
        assert_eq!(
            device_desc_text("@oem12.inf,%ch340.devicedesc%;USB-SERIAL CH340"),
            "USB-SERIAL CH340"
        );
        assert_eq!(
            device_desc_text("Communications Port"),
            "Communications Port"
        );
    }
}
