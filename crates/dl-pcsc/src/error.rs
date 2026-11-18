//! The crate error type and the PC/SC result codes.

use core::fmt;

/// PC/SC result codes (`SCARD_S_*`, `SCARD_E_*`, `SCARD_F_*`, `SCARD_W_*`), as unsigned
/// 32-bit values. They are the same on Windows, pcsc-lite and macOS except
/// [`SCARD_E_UNSUPPORTED_FEATURE`](codes::SCARD_E_UNSUPPORTED_FEATURE), which pcsc-lite and
/// macOS define as `0x8010001F` (the value of `SCARD_E_UNEXPECTED` on Windows).
pub mod codes {
    /// No error.
    pub const SCARD_S_SUCCESS: u32 = 0x0000_0000;
    /// An internal consistency check failed.
    pub const SCARD_F_INTERNAL_ERROR: u32 = 0x8010_0001;
    /// The action was cancelled by `SCardCancel`.
    pub const SCARD_E_CANCELLED: u32 = 0x8010_0002;
    /// The supplied handle was invalid.
    pub const SCARD_E_INVALID_HANDLE: u32 = 0x8010_0003;
    /// One or more of the supplied parameters could not be properly interpreted.
    pub const SCARD_E_INVALID_PARAMETER: u32 = 0x8010_0004;
    /// Registry startup information is missing or invalid.
    pub const SCARD_E_INVALID_TARGET: u32 = 0x8010_0005;
    /// Not enough memory available to complete this command.
    pub const SCARD_E_NO_MEMORY: u32 = 0x8010_0006;
    /// An internal consistency timer has expired.
    pub const SCARD_F_WAITED_TOO_LONG: u32 = 0x8010_0007;
    /// The data buffer to receive returned data is too small.
    pub const SCARD_E_INSUFFICIENT_BUFFER: u32 = 0x8010_0008;
    /// The specified reader name is not recognized.
    pub const SCARD_E_UNKNOWN_READER: u32 = 0x8010_0009;
    /// The user-specified timeout value has expired.
    pub const SCARD_E_TIMEOUT: u32 = 0x8010_000A;
    /// The smart card cannot be accessed because of other connections outstanding.
    pub const SCARD_E_SHARING_VIOLATION: u32 = 0x8010_000B;
    /// The operation requires a smart card, but no smart card is in the device.
    pub const SCARD_E_NO_SMARTCARD: u32 = 0x8010_000C;
    /// The specified smart card name is not recognized.
    pub const SCARD_E_UNKNOWN_CARD: u32 = 0x8010_000D;
    /// The system could not dispose of the media in the requested manner.
    pub const SCARD_E_CANT_DISPOSE: u32 = 0x8010_000E;
    /// The requested protocols are incompatible with the protocol in use by the card.
    pub const SCARD_E_PROTO_MISMATCH: u32 = 0x8010_000F;
    /// The reader or smart card is not ready to accept commands.
    pub const SCARD_E_NOT_READY: u32 = 0x8010_0010;
    /// One or more of the supplied parameter values could not be properly interpreted.
    pub const SCARD_E_INVALID_VALUE: u32 = 0x8010_0011;
    /// The action was cancelled by the system, presumably to log off or shut down.
    pub const SCARD_E_SYSTEM_CANCELLED: u32 = 0x8010_0012;
    /// An internal communications error has been detected.
    pub const SCARD_F_COMM_ERROR: u32 = 0x8010_0013;
    /// An internal error has been detected, but the source is unknown.
    pub const SCARD_F_UNKNOWN_ERROR: u32 = 0x8010_0014;
    /// An ATR obtained from the registry is not a valid ATR string.
    pub const SCARD_E_INVALID_ATR: u32 = 0x8010_0015;
    /// An attempt was made to end a non-existent transaction.
    pub const SCARD_E_NOT_TRANSACTED: u32 = 0x8010_0016;
    /// The specified reader is not currently available for use.
    pub const SCARD_E_READER_UNAVAILABLE: u32 = 0x8010_0017;
    /// The operation has been aborted to allow the server application to exit.
    pub const SCARD_P_SHUTDOWN: u32 = 0x8010_0018;
    /// The PCI receive buffer was too small.
    pub const SCARD_E_PCI_TOO_SMALL: u32 = 0x8010_0019;
    /// The reader driver does not meet minimal requirements for support.
    pub const SCARD_E_READER_UNSUPPORTED: u32 = 0x8010_001A;
    /// The reader driver did not produce a unique reader name.
    pub const SCARD_E_DUPLICATE_READER: u32 = 0x8010_001B;
    /// The smart card does not meet minimal requirements for support.
    pub const SCARD_E_CARD_UNSUPPORTED: u32 = 0x8010_001C;
    /// The smart card resource manager is not running.
    pub const SCARD_E_NO_SERVICE: u32 = 0x8010_001D;
    /// The smart card resource manager has shut down.
    pub const SCARD_E_SERVICE_STOPPED: u32 = 0x8010_001E;
    /// An unexpected card error has occurred.
    pub const SCARD_E_UNEXPECTED: u32 = 0x8010_001F;
    /// No primary provider can be found for the smart card.
    pub const SCARD_E_ICC_INSTALLATION: u32 = 0x8010_0020;
    /// The requested order of object creation is not supported.
    pub const SCARD_E_ICC_CREATEORDER: u32 = 0x8010_0021;
    /// This smart card does not support the requested feature (`0x80100022` on Windows,
    /// `0x8010001F` on pcsc-lite and macOS).
    #[cfg(windows)]
    pub const SCARD_E_UNSUPPORTED_FEATURE: u32 = 0x8010_0022;
    /// This smart card does not support the requested feature (`0x80100022` on Windows,
    /// `0x8010001F` on pcsc-lite and macOS).
    #[cfg(not(windows))]
    pub const SCARD_E_UNSUPPORTED_FEATURE: u32 = 0x8010_001F;
    /// The identified directory does not exist in the smart card.
    pub const SCARD_E_DIR_NOT_FOUND: u32 = 0x8010_0023;
    /// The identified file does not exist in the smart card.
    pub const SCARD_E_FILE_NOT_FOUND: u32 = 0x8010_0024;
    /// The supplied path does not represent a smart card directory.
    pub const SCARD_E_NO_DIR: u32 = 0x8010_0025;
    /// The supplied path does not represent a smart card file.
    pub const SCARD_E_NO_FILE: u32 = 0x8010_0026;
    /// Access is denied to this file.
    pub const SCARD_E_NO_ACCESS: u32 = 0x8010_0027;
    /// The smart card does not have enough memory to store the information.
    pub const SCARD_E_WRITE_TOO_MANY: u32 = 0x8010_0028;
    /// There was an error trying to set the smart card file object pointer.
    pub const SCARD_E_BAD_SEEK: u32 = 0x8010_0029;
    /// The supplied PIN is incorrect.
    pub const SCARD_E_INVALID_CHV: u32 = 0x8010_002A;
    /// An unrecognized error code was returned from a layered component.
    pub const SCARD_E_UNKNOWN_RES_MNG: u32 = 0x8010_002B;
    /// The requested certificate does not exist.
    pub const SCARD_E_NO_SUCH_CERTIFICATE: u32 = 0x8010_002C;
    /// The requested certificate could not be obtained.
    pub const SCARD_E_CERTIFICATE_UNAVAILABLE: u32 = 0x8010_002D;
    /// Cannot find a smart card reader.
    pub const SCARD_E_NO_READERS_AVAILABLE: u32 = 0x8010_002E;
    /// A communications error with the smart card has been detected.
    pub const SCARD_E_COMM_DATA_LOST: u32 = 0x8010_002F;
    /// The requested key container does not exist on the smart card.
    pub const SCARD_E_NO_KEY_CONTAINER: u32 = 0x8010_0030;
    /// The smart card resource manager is too busy to complete this operation.
    pub const SCARD_E_SERVER_TOO_BUSY: u32 = 0x8010_0031;
    /// The reader cannot communicate with the card, due to ATR string configuration
    /// conflicts.
    pub const SCARD_W_UNSUPPORTED_CARD: u32 = 0x8010_0065;
    /// The smart card is not responding to a reset.
    pub const SCARD_W_UNRESPONSIVE_CARD: u32 = 0x8010_0066;
    /// Power has been removed from the smart card.
    pub const SCARD_W_UNPOWERED_CARD: u32 = 0x8010_0067;
    /// The smart card has been reset, so any shared state information is invalid.
    pub const SCARD_W_RESET_CARD: u32 = 0x8010_0068;
    /// The smart card has been removed, so further communication is not possible.
    pub const SCARD_W_REMOVED_CARD: u32 = 0x8010_0069;
    /// Access was denied because of a security violation.
    pub const SCARD_W_SECURITY_VIOLATION: u32 = 0x8010_006A;
    /// The card cannot be accessed because the wrong PIN was presented.
    pub const SCARD_W_WRONG_CHV: u32 = 0x8010_006B;
    /// The maximum number of PIN entry attempts has been reached.
    pub const SCARD_W_CHV_BLOCKED: u32 = 0x8010_006C;
    /// The end of the smart card file has been reached.
    pub const SCARD_W_EOF: u32 = 0x8010_006D;
    /// The user pressed "Cancel" on a smart card selection dialog.
    pub const SCARD_W_CANCELLED_BY_USER: u32 = 0x8010_006E;
    /// No PIN was presented to the smart card.
    pub const SCARD_W_CARD_NOT_AUTHENTICATED: u32 = 0x8010_006F;

    /// Names and values; the first match wins. `SCARD_E_UNSUPPORTED_FEATURE` comes before
    /// `SCARD_E_UNEXPECTED` so that on pcsc-lite, where both are `0x8010001F`, the code gets
    /// that platform's name.
    const NAMES: &[(u32, &str)] = &[
        (SCARD_S_SUCCESS, "SCARD_S_SUCCESS"),
        (SCARD_F_INTERNAL_ERROR, "SCARD_F_INTERNAL_ERROR"),
        (SCARD_E_CANCELLED, "SCARD_E_CANCELLED"),
        (SCARD_E_INVALID_HANDLE, "SCARD_E_INVALID_HANDLE"),
        (SCARD_E_INVALID_PARAMETER, "SCARD_E_INVALID_PARAMETER"),
        (SCARD_E_INVALID_TARGET, "SCARD_E_INVALID_TARGET"),
        (SCARD_E_NO_MEMORY, "SCARD_E_NO_MEMORY"),
        (SCARD_F_WAITED_TOO_LONG, "SCARD_F_WAITED_TOO_LONG"),
        (SCARD_E_INSUFFICIENT_BUFFER, "SCARD_E_INSUFFICIENT_BUFFER"),
        (SCARD_E_UNKNOWN_READER, "SCARD_E_UNKNOWN_READER"),
        (SCARD_E_TIMEOUT, "SCARD_E_TIMEOUT"),
        (SCARD_E_SHARING_VIOLATION, "SCARD_E_SHARING_VIOLATION"),
        (SCARD_E_NO_SMARTCARD, "SCARD_E_NO_SMARTCARD"),
        (SCARD_E_UNKNOWN_CARD, "SCARD_E_UNKNOWN_CARD"),
        (SCARD_E_CANT_DISPOSE, "SCARD_E_CANT_DISPOSE"),
        (SCARD_E_PROTO_MISMATCH, "SCARD_E_PROTO_MISMATCH"),
        (SCARD_E_NOT_READY, "SCARD_E_NOT_READY"),
        (SCARD_E_INVALID_VALUE, "SCARD_E_INVALID_VALUE"),
        (SCARD_E_SYSTEM_CANCELLED, "SCARD_E_SYSTEM_CANCELLED"),
        (SCARD_F_COMM_ERROR, "SCARD_F_COMM_ERROR"),
        (SCARD_F_UNKNOWN_ERROR, "SCARD_F_UNKNOWN_ERROR"),
        (SCARD_E_INVALID_ATR, "SCARD_E_INVALID_ATR"),
        (SCARD_E_NOT_TRANSACTED, "SCARD_E_NOT_TRANSACTED"),
        (SCARD_E_READER_UNAVAILABLE, "SCARD_E_READER_UNAVAILABLE"),
        (SCARD_P_SHUTDOWN, "SCARD_P_SHUTDOWN"),
        (SCARD_E_PCI_TOO_SMALL, "SCARD_E_PCI_TOO_SMALL"),
        (SCARD_E_READER_UNSUPPORTED, "SCARD_E_READER_UNSUPPORTED"),
        (SCARD_E_DUPLICATE_READER, "SCARD_E_DUPLICATE_READER"),
        (SCARD_E_CARD_UNSUPPORTED, "SCARD_E_CARD_UNSUPPORTED"),
        (SCARD_E_NO_SERVICE, "SCARD_E_NO_SERVICE"),
        (SCARD_E_SERVICE_STOPPED, "SCARD_E_SERVICE_STOPPED"),
        (SCARD_E_UNSUPPORTED_FEATURE, "SCARD_E_UNSUPPORTED_FEATURE"),
        (SCARD_E_UNEXPECTED, "SCARD_E_UNEXPECTED"),
        (SCARD_E_ICC_INSTALLATION, "SCARD_E_ICC_INSTALLATION"),
        (SCARD_E_ICC_CREATEORDER, "SCARD_E_ICC_CREATEORDER"),
        (SCARD_E_DIR_NOT_FOUND, "SCARD_E_DIR_NOT_FOUND"),
        (SCARD_E_FILE_NOT_FOUND, "SCARD_E_FILE_NOT_FOUND"),
        (SCARD_E_NO_DIR, "SCARD_E_NO_DIR"),
        (SCARD_E_NO_FILE, "SCARD_E_NO_FILE"),
        (SCARD_E_NO_ACCESS, "SCARD_E_NO_ACCESS"),
        (SCARD_E_WRITE_TOO_MANY, "SCARD_E_WRITE_TOO_MANY"),
        (SCARD_E_BAD_SEEK, "SCARD_E_BAD_SEEK"),
        (SCARD_E_INVALID_CHV, "SCARD_E_INVALID_CHV"),
        (SCARD_E_UNKNOWN_RES_MNG, "SCARD_E_UNKNOWN_RES_MNG"),
        (SCARD_E_NO_SUCH_CERTIFICATE, "SCARD_E_NO_SUCH_CERTIFICATE"),
        (
            SCARD_E_CERTIFICATE_UNAVAILABLE,
            "SCARD_E_CERTIFICATE_UNAVAILABLE",
        ),
        (SCARD_E_NO_READERS_AVAILABLE, "SCARD_E_NO_READERS_AVAILABLE"),
        (SCARD_E_COMM_DATA_LOST, "SCARD_E_COMM_DATA_LOST"),
        (SCARD_E_NO_KEY_CONTAINER, "SCARD_E_NO_KEY_CONTAINER"),
        (SCARD_E_SERVER_TOO_BUSY, "SCARD_E_SERVER_TOO_BUSY"),
        (SCARD_W_UNSUPPORTED_CARD, "SCARD_W_UNSUPPORTED_CARD"),
        (SCARD_W_UNRESPONSIVE_CARD, "SCARD_W_UNRESPONSIVE_CARD"),
        (SCARD_W_UNPOWERED_CARD, "SCARD_W_UNPOWERED_CARD"),
        (SCARD_W_RESET_CARD, "SCARD_W_RESET_CARD"),
        (SCARD_W_REMOVED_CARD, "SCARD_W_REMOVED_CARD"),
        (SCARD_W_SECURITY_VIOLATION, "SCARD_W_SECURITY_VIOLATION"),
        (SCARD_W_WRONG_CHV, "SCARD_W_WRONG_CHV"),
        (SCARD_W_CHV_BLOCKED, "SCARD_W_CHV_BLOCKED"),
        (SCARD_W_EOF, "SCARD_W_EOF"),
        (SCARD_W_CANCELLED_BY_USER, "SCARD_W_CANCELLED_BY_USER"),
        (
            SCARD_W_CARD_NOT_AUTHENTICATED,
            "SCARD_W_CARD_NOT_AUTHENTICATED",
        ),
        // Plain Win32 codes that winscard.dll returns for some calls.
        #[cfg(windows)]
        (6, "ERROR_INVALID_HANDLE"),
    ];

    /// The symbolic name of a result code on this platform, such as
    /// `"SCARD_E_NO_SERVICE"` for `0x8010001D`. `None` for unknown codes.
    ///
    /// On pcsc-lite and macOS `0x8010001F` is named `SCARD_E_UNSUPPORTED_FEATURE` (that
    /// platform's name for it); on Windows it is `SCARD_E_UNEXPECTED`.
    pub fn code_name(code: u32) -> Option<&'static str> {
        NAMES.iter().find(|(c, _)| *c == code).map(|(_, n)| *n)
    }
}

/// Errors of this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A PC/SC call returned a result code other than `SCARD_S_SUCCESS`.
    Pcsc {
        /// The PC/SC function, e.g. `"SCardEstablishContext"`.
        op: &'static str,
        /// The result code as an unsigned 32-bit value, see [`codes`].
        code: u32,
    },
    /// No PC/SC library on this platform or it could not be loaded; the reason says why.
    Unsupported(String),
    /// An argument cannot be passed to PC/SC (e.g. a reader name with a NUL character).
    InvalidArgument(String),
    /// A card response is too short: fewer than 2 bytes (no status word), or no data
    /// where data is required (e.g. an empty UID).
    ShortResponse {
        /// Length of the response in bytes, status word included.
        len: usize,
    },
    /// The card answered with a status word other than `90 00`.
    Status {
        /// The status word, `SW1 << 8 | SW2`.
        sw: u16,
    },
}

impl Error {
    /// The PC/SC result code, for [`Error::Pcsc`].
    pub fn code(&self) -> Option<u32> {
        match self {
            Error::Pcsc { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// The smart card resource manager is not running (`SCARD_E_NO_SERVICE`) or stopped
    /// (`SCARD_E_SERVICE_STOPPED`). On Windows the service starts when a reader is plugged
    /// in and stops after the last one is removed, which invalidates every context:
    /// establish a new one later.
    pub fn is_no_service(&self) -> bool {
        matches!(
            self.code(),
            Some(codes::SCARD_E_NO_SERVICE | codes::SCARD_E_SERVICE_STOPPED)
        )
    }

    /// The timeout of a wait expired (`SCARD_E_TIMEOUT`).
    pub fn is_timeout(&self) -> bool {
        self.code() == Some(codes::SCARD_E_TIMEOUT)
    }

    /// The card was removed (`SCARD_W_REMOVED_CARD`) or there is no card in the reader
    /// (`SCARD_E_NO_SMARTCARD`).
    pub fn is_removed(&self) -> bool {
        matches!(
            self.code(),
            Some(codes::SCARD_W_REMOVED_CARD | codes::SCARD_E_NO_SMARTCARD)
        )
    }

    /// The wait was cancelled with [`Canceller::cancel`](crate::Canceller::cancel)
    /// (`SCARD_E_CANCELLED`).
    pub fn is_cancelled(&self) -> bool {
        self.code() == Some(codes::SCARD_E_CANCELLED)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Pcsc { op, code } => match codes::code_name(*code) {
                Some(name) => write!(f, "{op} failed: {name} (0x{code:08X})"),
                None => write!(f, "{op} failed: 0x{code:08X}"),
            },
            Error::Unsupported(reason) => write!(f, "PC/SC is not available: {reason}"),
            Error::InvalidArgument(m) => write!(f, "invalid argument: {m}"),
            Error::ShortResponse { len } => {
                write!(f, "card response of {len} bytes is too short")
            }
            Error::Status { sw } => write!(f, "card returned status word {sw:04X}"),
        }
    }
}

impl core::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_of_codes() {
        assert_eq!(codes::code_name(0x8010_001D), Some("SCARD_E_NO_SERVICE"));
        assert_eq!(
            codes::code_name(0x8010_002E),
            Some("SCARD_E_NO_READERS_AVAILABLE")
        );
        assert_eq!(codes::code_name(0x8010_000A), Some("SCARD_E_TIMEOUT"));
        assert_eq!(codes::code_name(0x8010_0069), Some("SCARD_W_REMOVED_CARD"));
        assert_eq!(
            codes::code_name(0x8010_001E),
            Some("SCARD_E_SERVICE_STOPPED")
        );
        assert_eq!(codes::code_name(0x8010_0002), Some("SCARD_E_CANCELLED"));
        assert_eq!(codes::code_name(0x1234_5678), None);
        #[cfg(windows)]
        {
            assert_eq!(codes::code_name(0x8010_001F), Some("SCARD_E_UNEXPECTED"));
            assert_eq!(
                codes::code_name(0x8010_0022),
                Some("SCARD_E_UNSUPPORTED_FEATURE")
            );
        }
        #[cfg(not(windows))]
        {
            assert_eq!(
                codes::code_name(0x8010_001F),
                Some("SCARD_E_UNSUPPORTED_FEATURE")
            );
            assert_eq!(codes::code_name(0x8010_0022), None);
        }
    }

    #[test]
    fn display_and_predicates() {
        let e = Error::Pcsc {
            op: "SCardEstablishContext",
            code: codes::SCARD_E_NO_SERVICE,
        };
        assert_eq!(
            e.to_string(),
            "SCardEstablishContext failed: SCARD_E_NO_SERVICE (0x8010001D)"
        );
        assert!(e.is_no_service() && !e.is_timeout() && !e.is_removed());
        let stopped = Error::Pcsc {
            op: "SCardGetStatusChange",
            code: codes::SCARD_E_SERVICE_STOPPED,
        };
        assert!(stopped.is_no_service());
        let t = Error::Pcsc {
            op: "SCardGetStatusChange",
            code: codes::SCARD_E_TIMEOUT,
        };
        assert!(t.is_timeout() && !t.is_no_service());
        let r = Error::Pcsc {
            op: "SCardTransmit",
            code: codes::SCARD_W_REMOVED_CARD,
        };
        assert!(r.is_removed());
        assert_eq!(
            r.to_string(),
            "SCardTransmit failed: SCARD_W_REMOVED_CARD (0x80100069)"
        );
        let c = Error::Pcsc {
            op: "SCardGetStatusChange",
            code: codes::SCARD_E_CANCELLED,
        };
        assert!(c.is_cancelled());
        let u = Error::Pcsc {
            op: "SCardConnect",
            code: 0x8010_00FF,
        };
        assert_eq!(u.to_string(), "SCardConnect failed: 0x801000FF");
        assert_eq!(u.code(), Some(0x8010_00FF));
        assert_eq!(
            Error::Status { sw: 0x6A81 }.to_string(),
            "card returned status word 6A81"
        );
        assert_eq!(Error::Status { sw: 0x6A81 }.code(), None);
        assert!(
            Error::Unsupported("x".into())
                .to_string()
                .contains("not available")
        );
    }
}
