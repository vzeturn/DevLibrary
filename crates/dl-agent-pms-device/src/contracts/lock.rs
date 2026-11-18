//! Door-lock payloads (`Commands/KeyCardRequest.cs`). Members that System.Text.Json can
//! leave `null` (every C# reference type, nullable or not) are `Option`, so adapters can
//! reproduce the C# null handling; value types keep the C# defaults.

use dl_json::{Map, ToJson, Value, json};
use dl_time::DateTime;

use super::json::{
    MIN_WALL_CLOCK, Payload, PayloadError, Reader, opt_json, opt_str, wall_clock_value,
};

const LOCK_ADDRESS: &str = "SmilePms.DeviceAgent.Commands.LockAddress";
const LOCK_VENDOR: &str = "SmilePms.DeviceAgent.Commands.LockVendor";
const KEY_CARD_REQUEST: &str = "SmilePms.DeviceAgent.Commands.KeyCardRequest";
const CANCEL_ALL_REQUEST: &str = "SmilePms.DeviceAgent.Commands.CancelAllRequest";
const READ_REQUEST: &str = "SmilePms.DeviceAgent.Commands.ReadRequest";
const ERASE_REQUEST: &str = "SmilePms.DeviceAgent.Commands.EraseRequest";

/// SMILE_FO RoomLockDef addressing of one room, in the VB6 mdlLockDotNET P3 order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LockAddress {
    /// `building` (RBuilding).
    pub building: Option<String>,
    /// `floor` (RFloor).
    pub floor: Option<String>,
    /// `room` (RRoom).
    pub room: Option<String>,
    /// `door` (RDoor).
    pub door: Option<String>,
    /// `address` (RAddress).
    pub address: Option<String>,
    /// `rmm`.
    pub rmm: Option<String>,
    /// `fmm`.
    pub fmm: Option<String>,
    /// `lockType` (LockType; MULTI routes by it).
    pub lock_type: Option<String>,
}

/// The workstation's lock selection: WSParameter LockKeyType and MFLockName.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LockVendor {
    /// `keyType` (MFCARD, HUNE2013, ...).
    pub key_type: Option<String>,
    /// `mfLockName` (hls, goodum, ...).
    pub mf_lock_name: Option<String>,
}

/// Payload of lock.issue / lock.duplicate / lock.extend. Times are hotel-local wall time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyCardRequest {
    /// `room`.
    pub room: Option<String>,
    /// `from`; `0001-01-01T00:00:00` when missing (C# `default(DateTime)`).
    pub from: DateTime,
    /// `to`; `0001-01-01T00:00:00` when missing.
    pub to: DateTime,
    /// `cards`, default 1 (not range-checked here, like C#).
    pub cards: i32,
    /// `folioNum`.
    pub folio_num: Option<i64>,
    /// `lockAddress`: `None` when the room has no RoomLockDef row.
    pub lock_address: Option<LockAddress>,
    /// `vendor`.
    pub vendor: Option<LockVendor>,
    /// `addGuest`: true for a copy (Adel AddKey).
    pub add_guest: Option<bool>,
}

impl Default for KeyCardRequest {
    fn default() -> Self {
        KeyCardRequest {
            room: None,
            from: MIN_WALL_CLOCK,
            to: MIN_WALL_CLOCK,
            cards: 1,
            folio_num: None,
            lock_address: None,
            vendor: None,
            add_guest: None,
        }
    }
}

/// Payload of lock.cancelAll.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CancelAllRequest {
    /// `room`.
    pub room: Option<String>,
    /// `lockAddress`.
    pub lock_address: Option<LockAddress>,
    /// `vendor`.
    pub vendor: Option<LockVendor>,
}

/// Payload of lock.read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadRequest {
    /// `vendor`.
    pub vendor: Option<LockVendor>,
}

/// Payload of lock.erase (VB6 tmpLockLink CO): wipe this card on the encoder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EraseRequest {
    /// `room`.
    pub room: Option<String>,
    /// `cardNumber`.
    pub card_number: Option<String>,
    /// `lockAddress`.
    pub lock_address: Option<LockAddress>,
    /// `vendor`.
    pub vendor: Option<LockVendor>,
}

fn lock_address(r: &mut Reader<'_>, v: &Value) -> Result<Option<LockAddress>, PayloadError> {
    r.nested(v, LOCK_ADDRESS, |r, map: &Map| {
        let mut a = LockAddress::default();
        const NAMES: [&str; 8] = [
            "building", "floor", "room", "door", "address", "rmm", "fmm", "lockType",
        ];
        r.members(map, LOCK_ADDRESS, &NAMES, |r, i, v| {
            let s = r.string(v, LOCK_ADDRESS)?;
            match i {
                0 => a.building = s,
                1 => a.floor = s,
                2 => a.room = s,
                3 => a.door = s,
                4 => a.address = s,
                5 => a.rmm = s,
                6 => a.fmm = s,
                _ => a.lock_type = s,
            }
            Ok(())
        })?;
        Ok(a)
    })
}

fn lock_vendor(r: &mut Reader<'_>, v: &Value) -> Result<Option<LockVendor>, PayloadError> {
    r.nested(v, LOCK_VENDOR, |r, map: &Map| {
        let mut o = LockVendor::default();
        r.members(map, LOCK_VENDOR, &["keyType", "mfLockName"], |r, i, v| {
            let s = r.string(v, LOCK_VENDOR)?;
            if i == 0 {
                o.key_type = s
            } else {
                o.mf_lock_name = s
            }
            Ok(())
        })?;
        Ok(o)
    })
}

impl Payload for KeyCardRequest {
    fn decode(r: &mut Reader<'_>, value: &Value) -> Result<Self, PayloadError> {
        const T: &str = KEY_CARD_REQUEST;
        let map = r.object(value, T)?;
        let mut o = KeyCardRequest::default();
        const NAMES: [&str; 8] = [
            "room",
            "from",
            "to",
            "cards",
            "folioNum",
            "lockAddress",
            "vendor",
            "addGuest",
        ];
        r.members(map, T, &NAMES, |r, i, v| {
            match i {
                0 => o.room = r.string(v, T)?,
                1 => o.from = r.wall_clock(v, T)?,
                2 => o.to = r.wall_clock(v, T)?,
                3 => o.cards = r.int32(v, T)?,
                4 => o.folio_num = r.int64_opt(v, T)?,
                5 => o.lock_address = lock_address(r, v)?,
                6 => o.vendor = lock_vendor(r, v)?,
                _ => o.add_guest = r.boolean_opt(v, T)?,
            }
            Ok(())
        })?;
        Ok(o)
    }
}

impl Payload for CancelAllRequest {
    fn decode(r: &mut Reader<'_>, value: &Value) -> Result<Self, PayloadError> {
        const T: &str = CANCEL_ALL_REQUEST;
        let map = r.object(value, T)?;
        let mut o = CancelAllRequest::default();
        r.members(map, T, &["room", "lockAddress", "vendor"], |r, i, v| {
            match i {
                0 => o.room = r.string(v, T)?,
                1 => o.lock_address = lock_address(r, v)?,
                _ => o.vendor = lock_vendor(r, v)?,
            }
            Ok(())
        })?;
        Ok(o)
    }
}

impl Payload for ReadRequest {
    fn decode(r: &mut Reader<'_>, value: &Value) -> Result<Self, PayloadError> {
        let map = r.object(value, READ_REQUEST)?;
        let mut o = ReadRequest::default();
        r.members(map, READ_REQUEST, &["vendor"], |r, _, v| {
            o.vendor = lock_vendor(r, v)?;
            Ok(())
        })?;
        Ok(o)
    }
}

impl Payload for EraseRequest {
    fn decode(r: &mut Reader<'_>, value: &Value) -> Result<Self, PayloadError> {
        const T: &str = ERASE_REQUEST;
        let map = r.object(value, T)?;
        let mut o = EraseRequest::default();
        r.members(
            map,
            T,
            &["room", "cardNumber", "lockAddress", "vendor"],
            |r, i, v| {
                match i {
                    0 => o.room = r.string(v, T)?,
                    1 => o.card_number = r.string(v, T)?,
                    2 => o.lock_address = lock_address(r, v)?,
                    _ => o.vendor = lock_vendor(r, v)?,
                }
                Ok(())
            },
        )?;
        Ok(o)
    }
}

impl ToJson for LockAddress {
    fn to_json(&self) -> Value {
        json!({
            "building": opt_str(&self.building),
            "floor": opt_str(&self.floor),
            "room": opt_str(&self.room),
            "door": opt_str(&self.door),
            "address": opt_str(&self.address),
            "rmm": opt_str(&self.rmm),
            "fmm": opt_str(&self.fmm),
            "lockType": opt_str(&self.lock_type),
        })
    }
}

impl ToJson for LockVendor {
    fn to_json(&self) -> Value {
        json!({"keyType": opt_str(&self.key_type), "mfLockName": opt_str(&self.mf_lock_name)})
    }
}

/// `DeviceJson.ToElement(request)`: every member, `null` included.
impl ToJson for KeyCardRequest {
    fn to_json(&self) -> Value {
        json!({
            "room": opt_str(&self.room),
            "from": wall_clock_value(self.from),
            "to": wall_clock_value(self.to),
            "cards": self.cards,
            "folioNum": self.folio_num,
            "lockAddress": opt_json(&self.lock_address),
            "vendor": opt_json(&self.vendor),
            "addGuest": self.add_guest,
        })
    }
}

impl ToJson for CancelAllRequest {
    fn to_json(&self) -> Value {
        json!({"room": opt_str(&self.room), "lockAddress": opt_json(&self.lock_address), "vendor": opt_json(&self.vendor)})
    }
}

impl ToJson for ReadRequest {
    fn to_json(&self) -> Value {
        json!({"vendor": opt_json(&self.vendor)})
    }
}

impl ToJson for EraseRequest {
    fn to_json(&self) -> Value {
        json!({
            "room": opt_str(&self.room),
            "cardNumber": opt_str(&self.card_number),
            "lockAddress": opt_json(&self.lock_address),
            "vendor": opt_json(&self.vendor),
        })
    }
}
