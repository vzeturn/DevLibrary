//! `Adapters/Lock/OrbitaCLockAdapter.cs`.

use std::sync::Arc;

use dl_json::{Value, json};

use super::failures::{
    card_count, format_long, is_null_or_blank, missing_room_lock_def, opt, pad_left,
    require_address, required, sdk_not_installed, text, trim, wall,
};
use crate::adapters::{Cx, DeviceAdapter};
use crate::config::{LockOptions, eq_ignore_case};
use crate::contracts::{
    AdapterError, CancelAllRequest, EraseRequest, KeyCardRequest, ReadRequest, codes,
};

/// One `dv_write_card` request (8 ANSI strings in VB6; the trailing ones are vendor
/// constants).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrbitaCard {
    /// RoomLockDef RBuilding (`00`-padded for V56).
    pub building: String,
    /// RoomLockDef RFloor (`0000`-padded for V56).
    pub floor: String,
    /// RoomLockDef RRoom, or RAddress when RRoom is blank.
    pub room: String,
    /// Start of the stay, `yyyy-MM-dd HH:mm:ss`.
    pub start: String,
    /// End of the stay, `yyyy-MM-dd HH:mm:ss`.
    pub end: String,
}

/// A card read back by `dv_read_card`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrbitaCardInfo {
    /// The card number.
    pub card_number: String,
    /// Building.
    pub building: String,
    /// Floor.
    pub floor: String,
    /// Room (the lock address).
    pub room: String,
    /// Start of the stay.
    pub start: String,
    /// End of the stay.
    pub end: String,
}

/// `IOrbitaNative`: CLock.dll (Orbita CLock) `dv_connect`, `dv_write_card`, `dv_read_card`,
/// `dv_delete_card`, `dv_disconnect`. No DLL or header is in hand, so there is no binding;
/// tests use fakes. `Err` is a failure of the native call itself.
pub trait OrbitaNative: Send + Sync {
    /// `dv_connect(port)`: 0 on success.
    fn connect(&self, port: i32) -> Result<i32, AdapterError>;
    /// `dv_write_card`: the code and the card number.
    fn write_card(&self, card: &OrbitaCard) -> Result<(i32, String), AdapterError>;
    /// `dv_read_card`: the code and the card on the encoder.
    fn read_card(&self) -> Result<(i32, OrbitaCardInfo), AdapterError>;
    /// `dv_delete_card`: the code and the deleted card number.
    fn delete_card(&self) -> Result<(i32, String), AdapterError>;
    /// `dv_disconnect`.
    fn disconnect(&self);
}

/// I-03 Orbita CLock (LockKeyType ORBITA). GlobalParameter OrbitaCLock picks the VB6
/// routine: 1, 2 and 3 (V56) use CLock.dll, and version 3 pads building to `00` and floor
/// to `0000`; any other value is the old OCX path, which this agent does not carry. Every
/// call is bracketed by `dv_connect(1)` / `dv_disconnect` like VB6.
pub struct OrbitaCLockAdapter {
    options: LockOptions,
    native: Option<Arc<dyn OrbitaNative>>,
}

impl OrbitaCLockAdapter {
    /// `OrbitaCLockAdapter.Name`.
    pub const NAME: &'static str = "orbita-clock";

    /// `OrbitaCLockAdapter.TimeFormat`.
    pub const TIME_FORMAT: &'static str = "yyyy-MM-dd HH:mm:ss";

    /// An adapter over the CLock native; `None` refuses with DEVICE_SDK_NOT_INSTALLED.
    pub fn new(options: LockOptions, native: Option<Arc<dyn OrbitaNative>>) -> OrbitaCLockAdapter {
        OrbitaCLockAdapter { options, native }
    }

    fn sdk(&self) -> Result<&dyn OrbitaNative, AdapterError> {
        let v = self.options.orbita_version;
        if !(1..=3).contains(&v) {
            return Err(sdk_not_installed(&format!("orbita OCX (OrbitaCLock={v})")));
        }
        self.native
            .as_deref()
            .ok_or_else(|| sdk_not_installed("orbita"))
    }

    /// `dv_connect(1)`, `work`, then always `dv_disconnect`.
    fn session(
        &self,
        work: impl FnOnce(&dyn OrbitaNative) -> Result<Value, AdapterError>,
    ) -> Result<Value, AdapterError> {
        let sdk = self.sdk()?;
        let rc = sdk.connect(1)?;
        if rc != 0 {
            return Err(AdapterError::new(
                codes::UNAVAILABLE,
                format!("Orbita encoder did not connect. Code={rc}"),
            ));
        }
        let result = work(sdk);
        sdk.disconnect();
        result
    }

    fn write(&self, r: &KeyCardRequest) -> Result<Value, AdapterError> {
        self.sdk()?;
        let (a, address) = require_address(
            r.lock_address.as_ref(),
            &missing_room_lock_def(text(&r.room)),
        )?;
        let v56 = self.options.orbita_version == 3;
        let building = trim(required(&a.building)?);
        let floor = trim(required(&a.floor)?);
        let card = OrbitaCard {
            building: if v56 {
                pad_left(building, 2, '0')
            } else {
                building.to_owned()
            },
            floor: if v56 {
                pad_left(floor, 4, '0')
            } else {
                floor.to_owned()
            },
            room: match a.room.as_deref() {
                room if is_null_or_blank(room) => trim(address).to_owned(),
                room => trim(room.unwrap_or_default()).to_owned(),
            },
            start: format_long(r.from),
            end: format_long(r.to),
        };
        self.session(|sdk| {
            let count = card_count(r.cards)?;
            let mut cards = Vec::new();
            for _ in 0..count {
                let (rc, card_number) = sdk.write_card(&card)?;
                if rc != 0 {
                    return Err(AdapterError::new(
                        codes::ERROR,
                        format!("Make Card Error. Code={rc}"),
                    ));
                }
                cards.push(json!({
                    "cardNumber": trim(&card_number),
                    "room": opt(&r.room),
                    "from": wall(r.from),
                    "to": wall(r.to),
                }));
            }
            Ok(json!({"cards": Value::Array(cards)}))
        })
    }
}

fn read_error(rc: i32) -> AdapterError {
    AdapterError::new(codes::ERROR, format!("Read Card Error. Code={rc}"))
}

impl DeviceAdapter for OrbitaCLockAdapter {
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

    fn read(&self, _: &ReadRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.session(|sdk| {
            let (rc, info) = sdk.read_card()?;
            if rc != 0 {
                return Err(read_error(rc));
            }
            Ok(json!({
                "cardNumber": trim(&info.card_number),
                "building": info.building,
                "floor": info.floor,
                "address": info.room,
                "from": info.start,
                "to": info.end,
            }))
        })
    }

    /// VB6 FUN_01543930: `dv_delete_card` on the card that is on the encoder; it must be
    /// the named one.
    fn erase(&self, request: &EraseRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.session(|sdk| {
            let (read, info) = sdk.read_card()?;
            if read != 0 {
                return Err(read_error(read));
            }
            let on_encoder = trim(&info.card_number);
            let wanted = required(&request.card_number)?;
            if !eq_ignore_case(on_encoder, trim(wanted)) {
                return Err(AdapterError::new(
                    codes::ERROR,
                    format!("The card on the encoder is {on_encoder}, not {wanted}"),
                ));
            }
            let (rc, _) = sdk.delete_card()?;
            if rc != 0 {
                return Err(AdapterError::new(
                    codes::ERROR,
                    format!("Delete Card Error. Code={rc}"),
                ));
            }
            Ok(json!({"cardNumber": wanted, "erased": true}))
        })
    }

    fn cancel_all(&self, request: &CancelAllRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.sdk()?;
        Ok(json!({
            "room": opt(&request.room),
            "cancelled": false,
            "reason": "CLock has no room-wide cancel; erase each card",
        }))
    }
}
