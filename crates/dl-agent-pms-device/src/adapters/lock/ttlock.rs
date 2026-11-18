//! `Adapters/Lock/TtLockAdapter.cs`.

use std::time::Duration;

use dl_json::Value;
use dl_time::{DateTime, Offset, Time};

use super::failures::{
    is_blank, missing_room_lock_def, not_configured, require_address, sdk_not_installed, val,
};
use crate::adapters::{Cx, DeviceAdapter};
use crate::config::LockOptions;
use crate::contracts::{
    AdapterError, CancelAllRequest, EraseRequest, KeyCardRequest, LockAddress, ReadRequest, codes,
};

/// H-10 TTLock (LockKeyType TTLOCK). SMILE_FO fetches `hotelInfo` from the TTLock open
/// platform with GlobalParameter ttLockClientID / ttLockClientSec, then runs
/// `ttLockInterface.exe` (a VB6 wrapper of the vendor `cardencoder.dll`) with
/// [`tt_lock_args`]; the exit code is the card id. Neither binary is in hand, so after the
/// credential check every verb refuses with DEVICE_SDK_NOT_INSTALLED. The client secret is
/// never logged or put in a message.
pub struct TtLockAdapter {
    options: LockOptions,
}

impl TtLockAdapter {
    /// `TtLockAdapter.Name`.
    pub const NAME: &'static str = "ttlock";

    /// An adapter with the agent's TTLock credentials.
    pub fn new(options: LockOptions) -> TtLockAdapter {
        TtLockAdapter { options }
    }

    fn refuse(&self) -> Result<Value, AdapterError> {
        if is_blank(&self.options.tt_lock_client_id)
            || is_blank(&self.options.tt_lock_client_secret)
        {
            return Err(not_configured(tt_lock_args::INVALID_CREDENTIALS));
        }
        Err(sdk_not_installed(Self::NAME))
    }
}

impl DeviceAdapter for TtLockAdapter {
    fn family(&self) -> &str {
        Self::NAME
    }

    fn issue(&self, _: &KeyCardRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.refuse()
    }

    fn duplicate(&self, _: &KeyCardRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.refuse()
    }

    fn read(&self, _: &ReadRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.refuse()
    }

    fn erase(&self, _: &EraseRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.refuse()
    }

    fn extend(&self, _: &KeyCardRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.refuse()
    }

    fn cancel_all(&self, _: &CancelAllRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.refuse()
    }
}

/// `TtLockArgs`: the `ttLockInterface.exe` command line of SMILE_FO 6.091 FUN_015450a0
/// (pure, for the on-site build).
pub mod tt_lock_args {
    use super::*;

    /// Empty client id or secret.
    pub const INVALID_CREDENTIALS: &str = "Invalid Client ID/Secret configuration!";
    /// No hotelInfo.
    pub const INVALID_HOTEL_INFO: &str = "Invalid HotelInfo!";
    /// `ttLockInterface.exe` returned no card id.
    pub const WRITE_FAILED: &str = "Error Writing Card!";

    /// VB6 refetches hotelInfo when the cached value is older than 570 (0x23a) seconds.
    pub const HOTEL_INFO_MAX_AGE: Duration = Duration::from_secs(570);

    /// `"2 <cardType> <Val(RBuilding)> <Val(RFloor)> <RAddress without ':'> <endEpoch> <hotelInfo>"`.
    /// The end is the checkout truncated to the minute (VB6 formats `MM/dd/yyyy HH:mm`) as
    /// Unix seconds in the hotel's zone; `utc_offset` is that zone's offset at the checkout
    /// (C# `zone.GetUtcOffset(end)`).
    pub fn build(
        card_type: i32,
        address: Option<&LockAddress>,
        room: &str,
        checkout: DateTime,
        utc_offset: Offset,
        hotel_info: &str,
    ) -> Result<String, AdapterError> {
        if is_blank(hotel_info) {
            return Err(not_configured(INVALID_HOTEL_INFO));
        }
        let (a, raddress) = require_address(address, &missing_room_lock_def(room))?;
        // Hour and minute come from a valid time, so the truncated time is valid too.
        let minute =
            Time::from_hms(checkout.hour(), checkout.minute(), 0).unwrap_or(Time::MIDNIGHT);
        let epoch = DateTime::new(checkout.date(), minute)
            .assume_offset(utc_offset)
            .unix_seconds();
        Ok([
            "2".to_owned(),
            card_type.to_string(),
            val(a.building.as_deref())?.to_string(),
            val(a.floor.as_deref())?.to_string(),
            raddress.replace(':', ""),
            epoch.to_string(),
            hotel_info.to_owned(),
        ]
        .join(" "))
    }

    /// The exit code of `ttLockInterface.exe` is the card id; 0 or less is VB6
    /// "Error Writing Card!" (DEVICE_ERROR).
    pub fn card_id_of(exit_code: i32) -> Result<String, AdapterError> {
        if exit_code > 0 {
            Ok(exit_code.to_string())
        } else {
            Err(AdapterError::new(codes::ERROR, WRITE_FAILED))
        }
    }
}
