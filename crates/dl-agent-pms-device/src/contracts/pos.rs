//! POS payloads (`Commands/PosRequests.cs`). As for the lock payloads, members System.Text.Json
//! can leave `null` are `Option`; value types keep the C# defaults.

use dl_json::{Map, ToJson, Value, json};

use super::json::{Payload, PayloadError, Reader, opt_json as option_to_json, opt_str};

const POS_TARGET: &str = "SmilePms.DeviceAgent.Commands.PosTarget";
const POS_PRINT_REQUEST: &str = "SmilePms.DeviceAgent.Commands.PosPrintRequest";
const POS_KICK_REQUEST: &str = "SmilePms.DeviceAgent.Commands.PosKickRequest";
const POS_DISPLAY_REQUEST: &str = "SmilePms.DeviceAgent.Commands.PosDisplayRequest";
const POS_READ_REQUEST: &str = "SmilePms.DeviceAgent.Commands.PosReadRequest";
const POS_SPEAK_REQUEST: &str = "SmilePms.DeviceAgent.Commands.PosSpeakRequest";

/// `PosTransports`: the `target.transport` values.
pub mod transports {
    /// Raw TCP (port 9100 by default).
    pub const TCP: &str = "tcp";
    /// The Windows spooler (RAW).
    pub const SPOOLER: &str = "spooler";
    /// A serial port.
    pub const SERIAL: &str = "serial";
    /// Bluetooth (browser-side; the agent refuses).
    pub const BLUETOOTH: &str = "bluetooth";
    /// An OPOS logical device.
    pub const OPOS: &str = "opos";
    /// A PC/SC smart-card reader (ACR122U and alike); endpoint = reader name or part of it.
    pub const PCSC: &str = "pcsc";
    /// Windows speech (SAPI SpVoice); endpoint = voice name part or `default`.
    pub const SAPI: &str = "sapi";
}

/// `PosReadDevices`: what pos.read reads, and its timeout bounds.
pub mod read_devices {
    /// The printer's real-time status (DLE EOT), the default.
    pub const PRINTER: &str = "printer";
    /// A card on a COM or NFC reader (masked + SHA-256 only).
    pub const CARD: &str = "card";
    /// Default `timeoutMs`.
    pub const DEFAULT_TIMEOUT_MS: i32 = 10_000;
    /// Smallest `timeoutMs` the router accepts.
    pub const MIN_TIMEOUT_MS: i32 = 500;
    /// Largest `timeoutMs`: under the 30 s command timeout.
    pub const MAX_TIMEOUT_MS: i32 = 25_000;
}

/// Where a POS device is reached (resolved by the API from MM_PosDeviceProfile).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PosTarget {
    /// `transport`: one of [`transports`].
    pub transport: Option<String>,
    /// `endpoint`: `host[:port]`, printer name, port name or OPOS logical name.
    pub endpoint: Option<String>,
    /// `baudRate` (serial).
    pub baud_rate: Option<i32>,
    /// `oposProfile`.
    pub opos_profile: Option<String>,
}

/// pos.print: raw ESC/POS bytes (base64); `cut` appends GS V 66 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PosPrintRequest {
    /// `target`.
    pub target: Option<PosTarget>,
    /// `data` (base64).
    pub data: Option<String>,
    /// `cut`, default false.
    pub cut: bool,
    /// `copies`, default 1.
    pub copies: i32,
}

impl Default for PosPrintRequest {
    fn default() -> Self {
        PosPrintRequest {
            target: None,
            data: None,
            cut: false,
            copies: 1,
        }
    }
}

/// pos.kick: open the cash drawer (ESC p m t1 t2) or an OPOS drawer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PosKickRequest {
    /// `target`.
    pub target: Option<PosTarget>,
    /// `pin`: connector pin 2 or 5, default 2.
    pub pin: i32,
    /// `onMs`, default 50.
    pub on_ms: i32,
    /// `offMs`, default 500.
    pub off_ms: i32,
}

impl Default for PosKickRequest {
    fn default() -> Self {
        PosKickRequest {
            target: None,
            pin: 2,
            on_ms: 50,
            off_ms: 500,
        }
    }
}

/// pos.display: the lines shown on a customer pole display, `columns` wide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PosDisplayRequest {
    /// `target`.
    pub target: Option<PosTarget>,
    /// `lines` (`null` list and `null` elements are kept, like C#).
    pub lines: Option<Vec<Option<String>>>,
    /// `columns`, default 20.
    pub columns: i32,
}

impl Default for PosDisplayRequest {
    fn default() -> Self {
        PosDisplayRequest {
            target: None,
            lines: None,
            columns: 20,
        }
    }
}

/// pos.read: printer status (default) or a card read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PosReadRequest {
    /// `target`.
    pub target: Option<PosTarget>,
    /// `device`: [`read_devices::PRINTER`] when missing; `None` only for an explicit `null`.
    pub device: Option<String>,
    /// `timeoutMs`, default 10 000.
    pub timeout_ms: i32,
}

impl Default for PosReadRequest {
    fn default() -> Self {
        PosReadRequest {
            target: None,
            device: Some(read_devices::PRINTER.to_owned()),
            timeout_ms: read_devices::DEFAULT_TIMEOUT_MS,
        }
    }
}

/// pos.speak: the payment speaker says `text` (SAPI volume 0-100, rate -10..10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PosSpeakRequest {
    /// `target`.
    pub target: Option<PosTarget>,
    /// `text`.
    pub text: Option<String>,
    /// `volume`, default 100.
    pub volume: i32,
    /// `rate`, default 0.
    pub rate: i32,
}

impl Default for PosSpeakRequest {
    fn default() -> Self {
        PosSpeakRequest {
            target: None,
            text: None,
            volume: 100,
            rate: 0,
        }
    }
}

fn pos_target(r: &mut Reader<'_>, v: &Value) -> Result<Option<PosTarget>, PayloadError> {
    r.nested(v, POS_TARGET, |r, map: &Map| {
        let mut t = PosTarget::default();
        r.members(
            map,
            POS_TARGET,
            &["transport", "endpoint", "baudRate", "oposProfile"],
            |r, i, v| {
                match i {
                    0 => t.transport = r.string(v, POS_TARGET)?,
                    1 => t.endpoint = r.string(v, POS_TARGET)?,
                    2 => t.baud_rate = r.int32_opt(v, POS_TARGET)?,
                    _ => t.opos_profile = r.string(v, POS_TARGET)?,
                }
                Ok(())
            },
        )?;
        Ok(t)
    })
}

impl Payload for PosPrintRequest {
    fn decode(r: &mut Reader<'_>, value: &Value) -> Result<Self, PayloadError> {
        const T: &str = POS_PRINT_REQUEST;
        let map = r.object(value, T)?;
        let mut o = PosPrintRequest::default();
        r.members(map, T, &["target", "data", "cut", "copies"], |r, i, v| {
            match i {
                0 => o.target = pos_target(r, v)?,
                1 => o.data = r.string(v, T)?,
                2 => o.cut = r.boolean(v, T)?,
                _ => o.copies = r.int32(v, T)?,
            }
            Ok(())
        })?;
        Ok(o)
    }
}

impl Payload for PosKickRequest {
    fn decode(r: &mut Reader<'_>, value: &Value) -> Result<Self, PayloadError> {
        const T: &str = POS_KICK_REQUEST;
        let map = r.object(value, T)?;
        let mut o = PosKickRequest::default();
        r.members(map, T, &["target", "pin", "onMs", "offMs"], |r, i, v| {
            match i {
                0 => o.target = pos_target(r, v)?,
                1 => o.pin = r.int32(v, T)?,
                2 => o.on_ms = r.int32(v, T)?,
                _ => o.off_ms = r.int32(v, T)?,
            }
            Ok(())
        })?;
        Ok(o)
    }
}

impl Payload for PosDisplayRequest {
    fn decode(r: &mut Reader<'_>, value: &Value) -> Result<Self, PayloadError> {
        const T: &str = POS_DISPLAY_REQUEST;
        let map = r.object(value, T)?;
        let mut o = PosDisplayRequest::default();
        r.members(map, T, &["target", "lines", "columns"], |r, i, v| {
            match i {
                0 => o.target = pos_target(r, v)?,
                1 => o.lines = r.string_list(v)?,
                _ => o.columns = r.int32(v, T)?,
            }
            Ok(())
        })?;
        Ok(o)
    }
}

impl Payload for PosReadRequest {
    fn decode(r: &mut Reader<'_>, value: &Value) -> Result<Self, PayloadError> {
        const T: &str = POS_READ_REQUEST;
        let map = r.object(value, T)?;
        let mut o = PosReadRequest::default();
        r.members(map, T, &["target", "device", "timeoutMs"], |r, i, v| {
            match i {
                0 => o.target = pos_target(r, v)?,
                1 => o.device = r.string(v, T)?,
                _ => o.timeout_ms = r.int32(v, T)?,
            }
            Ok(())
        })?;
        Ok(o)
    }
}

impl Payload for PosSpeakRequest {
    fn decode(r: &mut Reader<'_>, value: &Value) -> Result<Self, PayloadError> {
        const T: &str = POS_SPEAK_REQUEST;
        let map = r.object(value, T)?;
        let mut o = PosSpeakRequest::default();
        r.members(map, T, &["target", "text", "volume", "rate"], |r, i, v| {
            match i {
                0 => o.target = pos_target(r, v)?,
                1 => o.text = r.string(v, T)?,
                2 => o.volume = r.int32(v, T)?,
                _ => o.rate = r.int32(v, T)?,
            }
            Ok(())
        })?;
        Ok(o)
    }
}

impl ToJson for PosTarget {
    fn to_json(&self) -> Value {
        json!({
            "transport": opt_str(&self.transport),
            "endpoint": opt_str(&self.endpoint),
            "baudRate": self.baud_rate,
            "oposProfile": opt_str(&self.opos_profile),
        })
    }
}

impl ToJson for PosPrintRequest {
    fn to_json(&self) -> Value {
        json!({"target": option_to_json(&self.target), "data": opt_str(&self.data), "cut": self.cut, "copies": self.copies})
    }
}

impl ToJson for PosKickRequest {
    fn to_json(&self) -> Value {
        json!({"target": option_to_json(&self.target), "pin": self.pin, "onMs": self.on_ms, "offMs": self.off_ms})
    }
}

impl ToJson for PosDisplayRequest {
    fn to_json(&self) -> Value {
        let lines = self.lines.as_ref().map_or(Value::Null, |l| {
            Value::Array(l.iter().map(opt_str).collect())
        });
        json!({"target": option_to_json(&self.target), "lines": lines, "columns": self.columns})
    }
}

impl ToJson for PosReadRequest {
    fn to_json(&self) -> Value {
        json!({"target": option_to_json(&self.target), "device": opt_str(&self.device), "timeoutMs": self.timeout_ms})
    }
}

impl ToJson for PosSpeakRequest {
    fn to_json(&self) -> Value {
        json!({"target": option_to_json(&self.target), "text": opt_str(&self.text), "volume": self.volume, "rate": self.rate})
    }
}
