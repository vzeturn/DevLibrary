//! `Adapters/Lock/HuneHSmartV8Adapter.cs`.

use std::sync::Arc;

use dl_json::{Value, json};

use super::failures::{
    card_count, format_short, missing_room_lock_def, opt, require_address, sdk_not_installed, text,
    trim, unsupported, wall,
};
use crate::adapters::{Cx, DeviceAdapter};
use crate::config::LockOptions;
use crate::contracts::{
    AdapterError, CancelAllRequest, EraseRequest, KeyCardRequest, ReadRequest, codes,
};

/// One HSmart V8 KeyCard request (frmLockHSmartV8.MakeCard). Times are `yy-MM-dd HH:mm:ss`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HuneCard {
    /// WSParameter HUNEComPort.
    pub com_port: i32,
    /// GlobalParameter HUNECardCode (secret).
    pub card_pass: String,
    /// GlobalParameter HUNESystemCode (secret).
    pub sys_code: String,
    /// GlobalParameter HUNEHotelCode.
    pub hotel_code: String,
    /// RoomLockDef RAddress.
    pub address: String,
    /// Start of the stay.
    pub start: String,
    /// End of the stay.
    pub end: String,
}

/// A card read back by HSmart ReadMessage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HuneCardInfo {
    /// The card number.
    pub card_number: String,
    /// The lock address.
    pub address: String,
    /// Start of the stay.
    pub start: String,
    /// End of the stay.
    pub end: String,
}

/// `IHuneNative`: HNAccessG.DLL (Hune 2013 / HSmart V8) KeyCard and ReadMessage. No DLL or
/// header is in hand, so there is no binding; the adapter runs on fakes in tests and
/// refuses in production. `Err` is a failure of the native call itself.
pub trait HuneNative: Send + Sync {
    /// KeyCard: 0 on success, with the card number and the per-room Rmm the encoder returns.
    fn key_card(&self, card: &HuneCard) -> Result<(i32, String, String), AdapterError>;

    /// ReadMessage: 0 on success, with the card on the encoder.
    fn read_message(
        &self,
        com_port: i32,
        card_pass: &str,
        sys_code: &str,
    ) -> Result<(i32, HuneCardInfo), AdapterError>;
}

/// I-03 Hune HSmart V8 (LockKeyType HUNE2013). VB6: read RoomLockDef.RAddress, format the
/// stay as `yy-mm-dd hh:nn:ss` (VB6 `hh` is 24-hour), call KeyCard with CardPass / SysCode /
/// HotelCode, then write the returned Rmm back to RoomLockDef (the API does that from `rmm`
/// in the result).
pub struct HuneHSmartV8Adapter {
    options: LockOptions,
    native: Option<Arc<dyn HuneNative>>,
}

impl HuneHSmartV8Adapter {
    /// `HuneHSmartV8Adapter.Name`.
    pub const NAME: &'static str = "hune-hsmart-v8";

    /// `HuneHSmartV8Adapter.TimeFormat`.
    pub const TIME_FORMAT: &'static str = "yy-MM-dd HH:mm:ss";

    /// An adapter over the HSmart native; `None` refuses with DEVICE_SDK_NOT_INSTALLED.
    pub fn new(options: LockOptions, native: Option<Arc<dyn HuneNative>>) -> HuneHSmartV8Adapter {
        HuneHSmartV8Adapter { options, native }
    }

    fn sdk(&self) -> Result<&dyn HuneNative, AdapterError> {
        self.native
            .as_deref()
            .ok_or_else(|| sdk_not_installed("hune"))
    }

    fn write(&self, r: &KeyCardRequest) -> Result<Value, AdapterError> {
        let sdk = self.sdk()?;
        let (_, address) = require_address(
            r.lock_address.as_ref(),
            &missing_room_lock_def(text(&r.room)),
        )?;
        let o = &self.options;
        let card = HuneCard {
            com_port: o.hune_com_port,
            card_pass: o.hune_card_code.clone(),
            sys_code: o.hune_system_code.clone(),
            hotel_code: o.hune_hotel_code.clone(),
            address: address.to_owned(),
            start: format_short(r.from),
            end: format_short(r.to),
        };
        let count = card_count(r.cards)?;
        let mut cards = Vec::new();
        let mut rmm = String::new();
        for n in 0..count {
            let (rc, card_number, card_rmm) = sdk.key_card(&card)?;
            if rc != 0 {
                return Err(AdapterError::new(
                    codes::ERROR,
                    format!("Make Card Error. Code={rc}"),
                ));
            }
            if n == 0 {
                rmm = trim(&card_rmm).to_owned();
            }
            cards.push(json!({
                "cardNumber": trim(&card_number),
                "room": opt(&r.room),
                "from": wall(r.from),
                "to": wall(r.to),
            }));
        }
        Ok(json!({"cards": Value::Array(cards), "rmm": rmm}))
    }
}

impl DeviceAdapter for HuneHSmartV8Adapter {
    fn family(&self) -> &str {
        Self::NAME
    }

    fn issue(&self, request: &KeyCardRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.write(request)
    }

    fn duplicate(&self, request: &KeyCardRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.write(request)
    }

    fn extend(&self, request: &KeyCardRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.write(request)
    }

    /// Deviation kept from C#: VB6 shows CardPass and SysCode in the read error; they are
    /// lock secrets, so the message keeps only the COM port and the code.
    fn read(&self, _: &ReadRequest, _: &Cx) -> Result<Value, AdapterError> {
        let sdk = self.sdk()?;
        let o = &self.options;
        let (rc, info) =
            sdk.read_message(o.hune_com_port, &o.hune_card_code, &o.hune_system_code)?;
        if rc != 0 {
            return Err(AdapterError::new(
                codes::ERROR,
                format!("Reading Error. COM Port={}, Err Code={rc}", o.hune_com_port),
            ));
        }
        Ok(json!({
            "cardNumber": trim(&info.card_number),
            "address": info.address,
            "from": info.start,
            "to": info.end,
        }))
    }

    /// HSmart V8 in SMILE_FO has no erase call.
    fn erase(&self, _: &EraseRequest, _: &Cx) -> Result<Value, AdapterError> {
        Err(unsupported("Hune HSmart V8", "erase"))
    }

    fn cancel_all(&self, request: &CancelAllRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.sdk()?;
        Ok(json!({
            "room": opt(&request.room),
            "cancelled": false,
            "reason": "HSmart V8 has no room-wide cancel",
        }))
    }
}
