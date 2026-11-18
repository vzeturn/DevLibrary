use core::time::Duration;

use crate::Error;

/// Number of data bits per character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DataBits {
    /// 5 data bits.
    Five,
    /// 6 data bits.
    Six,
    /// 7 data bits.
    Seven,
    /// 8 data bits.
    #[default]
    Eight,
}

impl DataBits {
    /// The number of bits, 5 to 8.
    pub fn bits(self) -> u8 {
        match self {
            DataBits::Five => 5,
            DataBits::Six => 6,
            DataBits::Seven => 7,
            DataBits::Eight => 8,
        }
    }
}

impl TryFrom<u8> for DataBits {
    type Error = Error;

    fn try_from(bits: u8) -> Result<DataBits, Error> {
        match bits {
            5 => Ok(DataBits::Five),
            6 => Ok(DataBits::Six),
            7 => Ok(DataBits::Seven),
            8 => Ok(DataBits::Eight),
            _ => Err(Error::InvalidSettings("data bits must be 5, 6, 7 or 8")),
        }
    }
}

/// Parity bit mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Parity {
    /// No parity bit.
    #[default]
    None,
    /// Odd parity.
    Odd,
    /// Even parity.
    Even,
    /// Parity bit always 1.
    Mark,
    /// Parity bit always 0.
    Space,
}

/// Number of stop bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StopBits {
    /// 1 stop bit.
    #[default]
    One,
    /// 1.5 stop bits; only valid with 5 data bits.
    OnePointFive,
    /// 2 stop bits; not valid with 5 data bits.
    Two,
}

/// Flow control (handshake).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Flow {
    /// No flow control.
    #[default]
    None,
    /// Hardware flow control with the RTS and CTS lines.
    RtsCts,
    /// Software flow control with XON (0x11) and XOFF (0x13) characters.
    XonXoff,
}

/// Which driver buffers [`SerialPort::clear`](crate::SerialPort::clear) discards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Buffers {
    /// Bytes received but not read yet.
    Input,
    /// Bytes written but not transmitted yet.
    Output,
    /// Both.
    Both,
}

/// Line settings and timeouts for [`SerialPort::open`](crate::SerialPort::open).
///
/// The default is 9600 baud, 8 data bits, no parity, 1 stop bit (8N1), no flow control,
/// a 3 s read timeout and a 5 s write timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Settings {
    /// Bits per second, for example 9600 or 115200.
    pub baud_rate: u32,
    /// Data bits per character.
    pub data_bits: DataBits,
    /// Parity mode.
    pub parity: Parity,
    /// Stop bits.
    pub stop_bits: StopBits,
    /// Flow control.
    pub flow: Flow,
    /// How long a read waits for the first byte. `None` waits forever; `Some(ZERO)`
    /// returns at once. A read that gets no byte in time fails with
    /// [`std::io::ErrorKind::TimedOut`].
    pub read_timeout: Option<Duration>,
    /// How long a write waits for the port to accept data. `None` waits forever.
    pub write_timeout: Option<Duration>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            baud_rate: 9600,
            data_bits: DataBits::Eight,
            parity: Parity::None,
            stop_bits: StopBits::One,
            flow: Flow::None,
            read_timeout: Some(Duration::from_secs(3)),
            write_timeout: Some(Duration::from_secs(5)),
        }
    }
}

impl Settings {
    /// The default settings (8N1, no flow control, 3 s / 5 s timeouts) at `baud_rate`.
    pub fn new(baud_rate: u32) -> Settings {
        Settings {
            baud_rate,
            ..Settings::default()
        }
    }

    /// Checks the combinations every platform refuses.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.baud_rate == 0 {
            return Err(Error::InvalidSettings(
                "baud rate must be greater than zero",
            ));
        }
        match (self.data_bits, self.stop_bits) {
            (DataBits::Five, StopBits::Two) => Err(Error::InvalidSettings(
                "2 stop bits cannot be used with 5 data bits",
            )),
            (DataBits::Five, _) | (_, StopBits::One | StopBits::Two) => Ok(()),
            (_, StopBits::OnePointFive) => {
                Err(Error::InvalidSettings("1.5 stop bits need 5 data bits"))
            }
        }
    }
}

/// Rounds a duration up to whole milliseconds, saturating at `max`.
pub(crate) fn millis_ceil(d: Duration, max: u64) -> u64 {
    let ms = d.as_nanos().div_ceil(1_000_000);
    u64::try_from(ms).unwrap_or(u64::MAX).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_9600_8n1() {
        let s = Settings::default();
        assert_eq!(s.baud_rate, 9600);
        assert_eq!(s.data_bits.bits(), 8);
        assert_eq!(s.parity, Parity::None);
        assert_eq!(s.stop_bits, StopBits::One);
        assert_eq!(s.flow, Flow::None);
        assert_eq!(s.read_timeout, Some(Duration::from_secs(3)));
        assert_eq!(s.write_timeout, Some(Duration::from_secs(5)));
        assert_eq!(Settings::new(115_200).baud_rate, 115_200);
        assert!(s.validate().is_ok());
    }

    #[test]
    fn data_bits_conversion() {
        for b in 5..=8u8 {
            assert_eq!(DataBits::try_from(b).unwrap().bits(), b);
        }
        assert!(DataBits::try_from(4).is_err());
        assert!(DataBits::try_from(9).is_err());
    }

    #[test]
    fn validation() {
        let mut s = Settings::new(0);
        assert!(matches!(s.validate(), Err(Error::InvalidSettings(_))));
        s.baud_rate = 9600;
        s.stop_bits = StopBits::OnePointFive;
        assert!(s.validate().is_err());
        s.data_bits = DataBits::Five;
        assert!(s.validate().is_ok());
        s.stop_bits = StopBits::Two;
        assert!(s.validate().is_err());
        s.data_bits = DataBits::Seven;
        assert!(s.validate().is_ok());
    }

    #[test]
    fn millis_rounding() {
        assert_eq!(millis_ceil(Duration::ZERO, 100), 0);
        assert_eq!(millis_ceil(Duration::from_nanos(1), 100), 1);
        assert_eq!(millis_ceil(Duration::from_micros(1500), 100), 2);
        assert_eq!(millis_ceil(Duration::from_secs(3), u64::MAX), 3000);
        assert_eq!(millis_ceil(Duration::from_secs(1), 10), 10);
        assert_eq!(millis_ceil(Duration::MAX, u64::MAX), u64::MAX);
    }
}
