//! `Adapters/Lock/TpLock.cs`: the LockSDK.dll TP_* family (LockSDK.h, `__stdcall`, ANSI)
//! used by the MF bridge for hls, goodum and ezibee, and its return codes.

use crate::contracts::{AdapterError, codes};

/// A card read back by `TP_ReadGuestCardEx`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TpGuestCard {
    /// `card_snr`: the card serial.
    pub card_snr: String,
    /// `room_no`: the lock address (RAddress), not the PMS room.
    pub room_no: String,
    /// `checkin_time`.
    pub checkin_time: String,
    /// `checkout_time`.
    pub checkout_time: String,
}

/// `ITpLockNative`: the TP_* calls. Times are `yyyy-MM-dd HH:mm:ss`; the room is the
/// RoomLockDef RAddress. Every call returns the LockSDK return code (see [`tp_codes`]);
/// `Err` is what the C# P/Invoke would throw (DLL or export missing, a string the ANSI code
/// page cannot carry), reported as DEVICE_ADAPTER_FAILED. Implemented by the real LockSDK
/// binding (`crate::native`, 32-bit Windows only) and by fakes in tests.
pub trait TpLockNative: Send + Sync {
    /// `TP_Configuration(lock_type)`: 4 = RF57, 5 = RF50.
    fn configuration(&self, lock_type: i32) -> Result<i32, AdapterError>;

    /// `TP_MakeGuestCardEx`: encodes the card on the encoder; returns the code and the
    /// card serial.
    fn make_guest_card(
        &self,
        room_no: &str,
        checkin_time: &str,
        checkout_time: &str,
        flags: i32,
    ) -> Result<(i32, String), AdapterError>;

    /// `TP_ReadGuestCardEx`.
    fn read_guest_card(&self) -> Result<(i32, TpGuestCard), AdapterError>;

    /// `TP_CancelCard`: wipes the card on the encoder; returns the code and its serial.
    fn cancel_card(&self) -> Result<(i32, String), AdapterError>;

    /// `TP_GetCardSnr`: the code and the serial of the card on the encoder.
    fn get_card_snr(&self) -> Result<(i32, String), AdapterError>;
}

/// `TpCodes`: LockSDK.h return codes and their device error mapping.
pub mod tp_codes {
    use super::{AdapterError, codes};

    /// `OPR_OK`.
    pub const OK: i32 = 1;

    /// `NO_CARD`: no card on the encoder.
    pub const NO_CARD: i32 = -1;

    /// The LockSDK time format.
    pub const TIME_FORMAT: &str = "yyyy-MM-dd HH:mm:ss";

    /// The LockSDK.h name of a return code, `UNKNOWN` for any other value.
    pub fn name_of(rc: i32) -> &'static str {
        match rc {
            -1 => "NO_CARD",
            -2 => "NO_RW_MACHINE",
            -3 => "INVALID_CARD",
            -4 => "CARD_TYPE_ERROR",
            -5 => "RDWR_ERROR",
            -6 => "PORT_NOT_OPEN",
            -7 => "END_OF_DATA_CARD",
            -8 => "INVALID_PARAMETER",
            -9 => "INVALID_OPR",
            -10 => "OTHER_ERROR",
            -11 => "PORT_IN_USED",
            -12 => "COMM_ERROR",
            -20 => "ERR_CLIENT",
            -29 => "ERR_NOT_REGISTERED",
            -30 => "ERR_NO_CLIENT_DATA",
            -31 => "ERR_ROOMS_CNT_OVER",
            _ => "UNKNOWN",
        }
    }

    /// `TpCodes.Check`: -1 → DEVICE_NO_CARD; -2 / -6 / -11 / -12 → DEVICE_UNAVAILABLE; any
    /// other code but [`OK`] → DEVICE_ERROR `"{call} failed: {NAME} ({rc})"`.
    pub fn check(rc: i32, call: &str) -> Result<(), AdapterError> {
        if rc == OK {
            return Ok(());
        }
        let code = match rc {
            -1 => codes::NO_CARD,
            -2 | -6 | -11 | -12 => codes::UNAVAILABLE,
            _ => codes::ERROR,
        };
        Err(AdapterError::new(
            code,
            format!("{call} failed: {} ({rc})", name_of(rc)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::tp_codes::*;
    use crate::contracts::codes;

    #[test]
    fn codes_map() {
        assert!(check(OK, "X").is_ok());
        let e = check(-29, "TP_MakeGuestCardEx").unwrap_err();
        assert_eq!(
            (e.code.as_str(), e.message.as_str()),
            (
                codes::ERROR,
                "TP_MakeGuestCardEx failed: ERR_NOT_REGISTERED (-29)"
            )
        );
        assert_eq!(
            check(-99, "X").unwrap_err().message,
            "X failed: UNKNOWN (-99)"
        );
        assert_eq!(check(0, "X").unwrap_err().code, codes::ERROR);
    }
}
