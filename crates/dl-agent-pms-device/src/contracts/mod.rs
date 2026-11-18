//! The wire contract (`Commands/*.cs`, `Queue/ApiContracts.cs`): command kinds, device error
//! codes, outcomes, the lock and POS payloads, the API DTOs, and [`AdapterError`] (the C#
//! `DeviceAdapterException`). Golden files: `apps/device-agent/contract/*.json`.
//!
//! Payloads decode with [`decode_payload`], which refuses what System.Text.Json with
//! `DeviceJson.Options` refuses and returns its message ([`PayloadError`]).

mod api;
mod json;
mod lock;
mod pos;

use std::fmt;

use dl_json::Value;
use dl_time::OffsetDateTime;
use dl_uuid::Uuid;

pub use api::{ClaimResponse, PendingResult, ResultRequest, format_date_time_offset};
pub use json::{
    MIN_WALL_CLOCK, Payload, PayloadError, Reader, decode_payload, format_wall_clock, parse_json,
    parse_wall_clock, to_dotnet_json,
};
pub use lock::{
    CancelAllRequest, EraseRequest, KeyCardRequest, LockAddress, LockVendor, ReadRequest,
};
pub use pos::{
    PosDisplayRequest, PosKickRequest, PosPrintRequest, PosReadRequest, PosSpeakRequest, PosTarget,
    read_devices, transports,
};

/// `DeviceCommandKinds`.
pub mod kinds {
    /// Encode new key cards for a stay.
    pub const LOCK_ISSUE: &str = "lock.issue";
    /// Encode extra cards without invalidating existing ones.
    pub const LOCK_DUPLICATE: &str = "lock.duplicate";
    /// Read the card on the encoder.
    pub const LOCK_READ: &str = "lock.read";
    /// Change the end time of a stay's cards.
    pub const LOCK_EXTEND: &str = "lock.extend";
    /// Invalidate every card of a room.
    pub const LOCK_CANCEL_ALL: &str = "lock.cancelAll";
    /// Wipe the named card on the encoder.
    pub const LOCK_ERASE: &str = "lock.erase";
    /// Print raw ESC/POS bytes.
    pub const POS_PRINT: &str = "pos.print";
    /// Open the cash drawer.
    pub const POS_KICK: &str = "pos.kick";
    /// Show lines on the pole display.
    pub const POS_DISPLAY: &str = "pos.display";
    /// Read printer status or a card.
    pub const POS_READ: &str = "pos.read";
    /// Say a text on the payment speaker.
    pub const POS_SPEAK: &str = "pos.speak";
}

/// `DeviceErrorCodes`.
pub mod codes {
    /// The command kind is unknown to this agent.
    pub const UNSUPPORTED_KIND: &str = "DEVICE_UNSUPPORTED_KIND";
    /// The payload does not decode, or a value in it is refused.
    pub const BAD_PAYLOAD: &str = "DEVICE_BAD_PAYLOAD";
    /// The adapter failed unexpectedly.
    pub const ADAPTER_FAILED: &str = "DEVICE_ADAPTER_FAILED";
    /// The adapter did not answer within `CommandTimeout`.
    pub const TIMEOUT: &str = "DEVICE_TIMEOUT";
    /// No card on the encoder / reader.
    pub const NO_CARD: &str = "DEVICE_NO_CARD";
    /// No device is configured for this verb.
    pub const NOT_CONNECTED: &str = "DEVICE_NOT_CONNECTED";
    /// The encoder is absent, its port is closed or busy, or the link failed.
    pub const UNAVAILABLE: &str = "DEVICE_UNAVAILABLE";
    /// The vendor SDK answered with an error code; the message names it.
    pub const ERROR: &str = "DEVICE_ERROR";
    /// Lock settings or the room's RoomLockDef row are missing or name an unknown lock.
    pub const NOT_CONFIGURED: &str = "DEVICE_NOT_CONFIGURED";
    /// A known lock vendor whose native SDK is not part of this build or not installed.
    pub const SDK_NOT_INSTALLED: &str = "DEVICE_SDK_NOT_INSTALLED";
    /// An OPOS device whose vendor service object is not in this build.
    pub const VENDOR_SDK_MISSING: &str = "DEVICE_VENDOR_SDK_MISSING";
    /// The transport cannot run this verb.
    pub const TRANSPORT_UNSUPPORTED: &str = "DEVICE_TRANSPORT_UNSUPPORTED";
    /// The transport needs another operating system.
    pub const PLATFORM_UNSUPPORTED: &str = "DEVICE_PLATFORM_UNSUPPORTED";
}

/// A command claimed from the queue. `claim_token` proves this agent still holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCommand {
    /// Command id.
    pub id: Uuid,
    /// One of [`kinds`] (or an unknown kind, answered DEVICE_UNSUPPORTED_KIND); empty when
    /// the API sent `null`.
    pub kind: String,
    /// The payload, decoded per kind with [`decode_payload`].
    pub payload: Value,
    /// The claim token sent back with the result.
    pub claim_token: Uuid,
    /// Delivery attempt, from 1.
    pub attempt: i32,
    /// When the claim lapses, on the agent's clock (server lease less a safety margin).
    pub lease_until: OffsetDateTime,
}

/// Structured failure reported to the API (`DeviceError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceError {
    /// One of [`codes`].
    pub code: String,
    /// Human-readable reason, at most [`CommandOutcome::MAX_MESSAGE_LENGTH`] UTF-16 units.
    pub message: String,
}

/// What a command produced (`CommandOutcome`): a result or an error.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandOutcome {
    /// The adapter's JSON result when done.
    pub result: Option<Value>,
    /// The error when failed.
    pub error: Option<DeviceError>,
}

impl CommandOutcome {
    /// `CommandOutcome.MaxMessageLength`, in UTF-16 code units like C# `string.Length`.
    pub const MAX_MESSAGE_LENGTH: usize = 400;

    /// True when there is no error.
    pub fn ok(&self) -> bool {
        self.error.is_none()
    }

    /// `CommandOutcome.Done(result)`.
    pub fn done(result: Value) -> CommandOutcome {
        CommandOutcome {
            result: Some(result),
            error: None,
        }
    }

    /// `CommandOutcome.Failed(code, message)`: the message is cut to 400 UTF-16 units. A
    /// surrogate pair cut in half is dropped whole (C# would keep a lone surrogate).
    pub fn failed(code: &str, message: &str) -> CommandOutcome {
        CommandOutcome {
            result: None,
            error: Some(DeviceError {
                code: code.to_owned(),
                message: truncate_utf16(message, Self::MAX_MESSAGE_LENGTH).to_owned(),
            }),
        }
    }
}

/// The longest prefix of `s` that is at most `max` UTF-16 code units.
fn truncate_utf16(s: &str, max: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > max {
            return &s[..i];
        }
    }
    s
}

/// An adapter refusal with a device error code (the C# `DeviceAdapterException`); the
/// processor reports it as the command's error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterError {
    /// One of [`codes`].
    pub code: String,
    /// The exception message shown to the clerk.
    pub message: String,
}

impl AdapterError {
    /// A refusal with `code` and `message`.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> AdapterError {
        AdapterError {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AdapterError {}

impl From<PayloadError> for AdapterError {
    /// A payload System.Text.Json refuses is DEVICE_BAD_PAYLOAD with its message.
    fn from(e: PayloadError) -> AdapterError {
        AdapterError::new(codes::BAD_PAYLOAD, e.message())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_message_is_capped_at_400_utf16_units() {
        let long = "é".repeat(401);
        assert_eq!(
            CommandOutcome::failed("X", &long)
                .error
                .unwrap()
                .message
                .chars()
                .count(),
            400
        );
        let emoji = format!("{}😀", "a".repeat(399));
        assert_eq!(
            CommandOutcome::failed("X", &emoji).error.unwrap().message,
            "a".repeat(399)
        );
        assert_eq!(
            CommandOutcome::failed("X", "short").error.unwrap().message,
            "short"
        );
        assert!(CommandOutcome::done(Value::Null).ok());
    }

    #[test]
    fn payload_errors_become_bad_payload() {
        let e: AdapterError = PayloadError::new("payload is null").into();
        assert_eq!(
            (e.code.as_str(), e.message.as_str()),
            (codes::BAD_PAYLOAD, "payload is null")
        );
    }
}
