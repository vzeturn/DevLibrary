use core::fmt;
use std::io;

/// Errors from opening, configuring and listing serial ports.
///
/// Reads and writes go through [`std::io::Read`] / [`std::io::Write`] and return
/// [`std::io::Error`]; a timeout there is [`std::io::ErrorKind::TimedOut`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Serial ports are not available on this platform (for example WebAssembly).
    Unsupported,
    /// The port name cannot be a serial device name (empty, contains NUL, bad characters).
    InvalidName(String),
    /// No port with this name exists (Windows `ERROR_FILE_NOT_FOUND`, Unix `ENOENT`).
    NotFound(String),
    /// The port is open in another handle or process (Windows `ERROR_ACCESS_DENIED` /
    /// `ERROR_SHARING_VIOLATION`, Unix `EBUSY` or a held exclusive lock).
    Busy(String),
    /// The process may not open the device (Unix `EACCES` / `EPERM`; on Linux the user
    /// is usually missing from the `dialout` group).
    PermissionDenied(String),
    /// The device exists but is not a serial port (no terminal or comm state).
    NotASerialPort(String),
    /// A combination of settings the platform or driver cannot apply.
    InvalidSettings(&'static str),
    /// The baud rate is not supported by the platform or the driver.
    UnsupportedBaudRate(u32),
    /// An operating system call failed.
    Os {
        /// The function that failed.
        op: &'static str,
        /// The OS error code (`GetLastError` on Windows, `errno` on Unix).
        code: i32,
    },
}

impl Error {
    /// The OS error code, when the error came from a failed system call.
    pub fn os_code(&self) -> Option<i32> {
        match self {
            Error::Os { code, .. } => Some(*code),
            _ => None,
        }
    }

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
    pub(crate) fn os(op: &'static str, e: &io::Error) -> Error {
        Error::Os {
            op,
            code: e.raw_os_error().unwrap_or(0),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported => f.write_str("serial ports are not available on this platform"),
            Error::InvalidName(n) => write!(f, "invalid serial port name {n:?}"),
            Error::NotFound(n) => write!(f, "serial port {n} not found"),
            Error::Busy(n) => write!(f, "serial port {n} is in use"),
            Error::PermissionDenied(n) => write!(f, "permission denied opening serial port {n}"),
            Error::NotASerialPort(n) => write!(f, "{n} is not a serial port"),
            Error::InvalidSettings(what) => write!(f, "invalid serial settings: {what}"),
            Error::UnsupportedBaudRate(b) => write!(f, "unsupported baud rate {b}"),
            Error::Os { op, code } => {
                let os = io::Error::from_raw_os_error(*code);
                write!(f, "{op} failed: {os}")
            }
        }
    }
}

impl core::error::Error for Error {}

impl From<Error> for io::Error {
    fn from(e: Error) -> io::Error {
        let kind = match &e {
            Error::Unsupported => io::ErrorKind::Unsupported,
            Error::InvalidName(_) | Error::InvalidSettings(_) | Error::UnsupportedBaudRate(_) => {
                io::ErrorKind::InvalidInput
            }
            Error::NotFound(_) => io::ErrorKind::NotFound,
            Error::Busy(_) => io::ErrorKind::ResourceBusy,
            Error::PermissionDenied(_) => io::ErrorKind::PermissionDenied,
            Error::NotASerialPort(_) => io::ErrorKind::InvalidInput,
            Error::Os { code, .. } => io::Error::from_raw_os_error(*code).kind(),
        };
        io::Error::new(kind, e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_conversion() {
        assert_eq!(
            Error::NotFound("COM250".into()).to_string(),
            "serial port COM250 not found"
        );
        assert_eq!(
            Error::InvalidName(String::new()).to_string(),
            "invalid serial port name \"\""
        );
        let os = Error::Os {
            op: "SetCommState",
            code: 87,
        };
        assert_eq!(os.os_code(), Some(87));
        assert!(os.to_string().starts_with("SetCommState failed: "), "{os}");
        assert_eq!(Error::Unsupported.os_code(), None);
        let io: io::Error = Error::Busy("COM3".into()).into();
        assert_eq!(io.kind(), io::ErrorKind::ResourceBusy);
        let io: io::Error = Error::NotFound("x".into()).into();
        assert_eq!(io.kind(), io::ErrorKind::NotFound);
        let io: io::Error = Error::UnsupportedBaudRate(1).into();
        assert_eq!(io.kind(), io::ErrorKind::InvalidInput);
    }
}
