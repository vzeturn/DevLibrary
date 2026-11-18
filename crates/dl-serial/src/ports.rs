use core::cmp::Ordering;

/// A serial port found by [`available_ports`](crate::available_ports).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PortInfo {
    /// The name to pass to [`SerialPort::open`](crate::SerialPort::open): `COM3` on
    /// Windows, `/dev/ttyUSB0` on Linux, `/dev/cu.usbserial-1410` on macOS.
    pub name: String,
    /// What kind of device provides the port, as far as it can be told.
    pub kind: PortKind,
}

/// The device behind a serial port.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PortKind {
    /// A USB serial adapter or a USB CDC-ACM device.
    Usb {
        /// USB vendor id.
        vid: u16,
        /// USB product id.
        pid: u16,
        /// The device serial number, when it has one.
        serial: Option<String>,
        /// The product name (USB descriptor on Linux, device friendly name on Windows).
        product: Option<String>,
    },
    /// A Bluetooth serial (SPP / RFCOMM) port.
    Bluetooth,
    /// A PCI or PCI Express serial card.
    Pci,
    /// A built-in port, a virtual port, or a device that could not be identified.
    Unknown,
}

/// Orders port names so that `COM2 < COM10` and `ttyUSB2 < ttyUSB10`.
#[cfg_attr(
    not(any(
        windows,
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        target_os = "macos"
    )),
    allow(dead_code)
)]
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn split(s: &str) -> (&str, Option<u64>) {
        let digits = s.bytes().rev().take_while(u8::is_ascii_digit).count();
        let (head, tail) = s.split_at(s.len() - digits);
        (head, tail.parse().ok())
    }
    let (ha, na) = split(a);
    let (hb, nb) = split(b);
    ha.cmp(hb).then(na.cmp(&nb)).then(a.cmp(b))
}

/// Parses 1 to 4 hexadecimal digits (with surrounding whitespace, as sysfs writes them).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_hex_u16(s: &str) -> Option<u16> {
    let s = s.trim();
    if s.is_empty() || s.len() > 4 {
        return None;
    }
    u16::from_str_radix(s, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec![
            "COM10",
            "COM3",
            "COM1",
            "/dev/ttyUSB10",
            "/dev/ttyUSB2",
            "COM",
        ];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            v,
            [
                "/dev/ttyUSB2",
                "/dev/ttyUSB10",
                "COM",
                "COM1",
                "COM3",
                "COM10"
            ]
        );
    }

    #[test]
    fn hex() {
        assert_eq!(parse_hex_u16("1a86\n"), Some(0x1a86));
        assert_eq!(parse_hex_u16("0403"), Some(0x0403));
        assert_eq!(parse_hex_u16(""), None);
        assert_eq!(parse_hex_u16("12345"), None);
        assert_eq!(parse_hex_u16("zz"), None);
    }
}
