//! POS device adapters (`Adapters/Pos/`): the [`PosDeviceRouter`] and what it routes to.
//!
//! Each pos.* command goes to the adapter named by its payload's `target.transport`: tcp,
//! serial and spooler are ESC/POS byte links ([`EscPosTransport`]); a card read goes to the
//! COM reader ([`SerialCardReaderAdapter`]) or the PC/SC NFC reader ([`AcrNfcAdapter`]);
//! pos.speak goes to the SAPI speaker ([`SpeakerAdapter`]); opos goes to [`OposAdapter`]
//! and bluetooth to [`BluetoothRefusingAdapter`]. Every device link sits behind a small
//! trait so the tests drive the same code with fakes (the C# `Pos*Fake` classes).
//!
//! The C# records are non-nullable but System.Text.Json leaves missing members `null`; where
//! C# then dereferences them the verb fails like .NET would ([`failures::null_reference`],
//! DEVICE_ADAPTER_FAILED), so the API sees the same outcome.

mod base64;
mod card;
mod escpos;
mod nfc;
mod opos;
mod router;
mod serial;
mod speaker;
mod spooler;
#[cfg(windows)]
mod sta;
mod stream;
mod tcp;
mod unicode;

pub use card::CardIdentity;
pub use escpos::{EscPos, PosPrinterStatus};
pub use nfc::{AcrNfcAdapter, PcscCardSource, WinScardCardSource};
pub use opos::{
    BluetoothRefusingAdapter, ComFailure, OposAdapter, OposArg, OposControl, OposHost, OposOpen,
    OposSession, PROFILES as OPOS_PROFILES, opos_code_name, result_codes,
};
pub use router::PosDeviceRouter;
pub use serial::{
    EscPosSerialAdapter, SerialCardReaderAdapter, SerialLink, SerialPortOpener,
    SystemSerialPortOpener,
};
pub use speaker::{SapiSpeechEngine, SpeakerAdapter, SpeechEngine};
pub use spooler::{RawSpooler, Win32Error, WinSpoolRaw, WindowsSpoolerAdapter};
pub use stream::{ByteLink, EscPosStream};
pub use tcp::EscPosTcpAdapter;

use super::{Cx, PosDevice};
use crate::contracts::{AdapterError, PosTarget};

/// `PosDeviceRouter.Create()`: the POS router over this host's transports.
pub fn router() -> Box<dyn PosDevice> {
    Box::new(PosDeviceRouter::create())
}

/// `IEscPosTransport`: a byte link to an ESC/POS device (a receipt printer, or a pole display
/// speaking the same set).
pub trait EscPosTransport: Send + Sync {
    /// The [`transports`](crate::contracts::transports) name this link serves.
    fn transport(&self) -> &str;

    /// Sends `bytes` to the device named by `target`.
    fn send(&self, target: &PosTarget, bytes: &[u8], cx: &Cx) -> Result<(), AdapterError>;

    /// Sends `request` and reads exactly `reply_length` bytes (DLE EOT status).
    fn query(
        &self,
        target: &PosTarget,
        request: &[u8],
        reply_length: usize,
        cx: &Cx,
    ) -> Result<Vec<u8>, AdapterError>;
}

/// `PosFailures`: the typed refusals shared by the POS adapters, plus the .NET exceptions
/// the C# code raises on `null` payload members.
pub mod failures {
    use crate::adapters::adapter_failed;
    use crate::contracts::{AdapterError, PosTarget, codes};

    /// DEVICE_BAD_PAYLOAD with `message`.
    pub fn bad_payload(message: impl Into<String>) -> AdapterError {
        AdapterError::new(codes::BAD_PAYLOAD, message)
    }

    /// DEVICE_UNAVAILABLE: `"{transport} device {endpoint} unavailable: {reason}"`.
    pub fn unavailable(target: &PosTarget, reason: &str) -> AdapterError {
        AdapterError::new(
            codes::UNAVAILABLE,
            format!(
                "{} device {} unavailable: {reason}",
                super::text(&target.transport),
                super::text(&target.endpoint)
            ),
        )
    }

    /// DEVICE_TRANSPORT_UNSUPPORTED: `"{verb} is not supported over {transport}: {hint}"`.
    pub fn unsupported(transport: &str, verb: &str, hint: &str) -> AdapterError {
        AdapterError::new(
            codes::TRANSPORT_UNSUPPORTED,
            format!("{verb} is not supported over {transport}: {hint}"),
        )
    }

    /// DEVICE_NOT_CONNECTED: `"{what} is not set up on this agent"`.
    pub fn not_set_up(what: &str) -> AdapterError {
        AdapterError::new(
            codes::NOT_CONNECTED,
            format!("{what} is not set up on this agent"),
        )
    }

    /// DEVICE_PLATFORM_UNSUPPORTED: `"{what} runs only on a Windows agent"`.
    pub fn platform_unsupported(what: &str) -> AdapterError {
        AdapterError::new(
            codes::PLATFORM_UNSUPPORTED,
            format!("{what} runs only on a Windows agent"),
        )
    }

    /// DEVICE_VENDOR_SDK_MISSING: `"vendor-sdk-missing: {what}"` (owner rule: a device without
    /// its vendor SDK refuses; it never reports a silent success).
    pub fn vendor_sdk_missing(what: &str) -> AdapterError {
        AdapterError::new(
            codes::VENDOR_SDK_MISSING,
            format!("vendor-sdk-missing: {what}"),
        )
    }

    /// What C# raises when it dereferences a `null` payload member.
    pub fn null_reference() -> AdapterError {
        adapter_failed(
            "NullReferenceException",
            "Object reference not set to an instance of an object.",
        )
    }

    /// `ArgumentNullException` for `parameter` (a `null` dictionary key or base64 string).
    pub fn argument_null(parameter: &str) -> AdapterError {
        adapter_failed(
            "ArgumentNullException",
            &format!("Value cannot be null. (Parameter '{parameter}')"),
        )
    }
}

/// A C# string member as interpolated: `null` is empty.
fn text(s: &Option<String>) -> &str {
    s.as_deref().unwrap_or("")
}

/// `request.Target`, or the NullReferenceException C# raises on it.
fn target_of(target: &Option<PosTarget>) -> Result<&PosTarget, AdapterError> {
    target.as_ref().ok_or_else(failures::null_reference)
}

/// `target.Endpoint.Trim()`, or the NullReferenceException C# raises on a `null` endpoint.
fn trimmed_endpoint(target: &PosTarget) -> Result<&str, AdapterError> {
    target
        .endpoint
        .as_deref()
        .map(str::trim)
        .ok_or_else(failures::null_reference)
}

/// Seconds formatted like .NET `TimeSpan.FromMilliseconds(ms).TotalSeconds.ToString("0.#")`
/// (checked against .NET 10 for 0..=30000 ms).
fn seconds_0_1(ms: i64) -> String {
    let tenths = (ms.max(0) + 50) / 100;
    if tenths % 10 == 0 {
        format!("{}", tenths / 10)
    } else {
        format!("{}.{}", tenths / 10, tenths % 10)
    }
}

/// .NET `string.Contains(value, StringComparison.OrdinalIgnoreCase)` (per-char simple upper
/// case mapping).
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    fn upper(c: char) -> char {
        let mut u = c.to_uppercase();
        match (u.next(), u.next()) {
            (Some(one), None) => one,
            _ => c,
        }
    }
    let h: Vec<char> = haystack.chars().map(upper).collect();
    let n: Vec<char> = needle.chars().map(upper).collect();
    n.is_empty() || h.windows(n.len()).any(|w| w == n.as_slice())
}

/// The text of an OS error without Rust's `" (os error N)"` suffix: the same
/// `FormatMessage` text .NET puts in its exception messages.
fn os_message(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(i) if s.ends_with(')') => s[..i].to_owned(),
        _ => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_match_dotnet() {
        assert_eq!(seconds_0_1(2000), "2");
        assert_eq!(seconds_0_1(600), "0.6");
        assert_eq!(seconds_0_1(1250), "1.3");
        assert_eq!(seconds_0_1(25000), "25");
        assert!(contains_ignore_case("Microsoft An - Vietnamese", "an"));
        assert!(contains_ignore_case("ÁN", "án"));
        assert!(!contains_ignore_case("STRASSE", "ß"));
        assert!(contains_ignore_case("x", ""));
        let e = std::io::Error::from_raw_os_error(2);
        assert!(!os_message(&e).contains("os error"));
    }
}
