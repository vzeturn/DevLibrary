//! `Adapters/Lock/LockFailures.cs`: typed refusals and small VB6 / .NET helpers shared by
//! the door-lock adapters.

use dl_json::Value;
use dl_time::DateTime;

use crate::adapters::adapter_failed;
use crate::contracts::{AdapterError, LockAddress, codes, format_wall_clock};

/// `LockFailures.NotConfigured`: DEVICE_NOT_CONFIGURED with `message`.
pub fn not_configured(message: impl Into<String>) -> AdapterError {
    AdapterError::new(codes::NOT_CONFIGURED, message)
}

/// `LockFailures.SdkNotInstalled`: a known vendor whose native SDK is not in this build
/// (DEVICE_SDK_NOT_INSTALLED `lock SDK not installed for {vendor}`), never a silent success.
pub fn sdk_not_installed(vendor: &str) -> AdapterError {
    AdapterError::new(
        codes::SDK_NOT_INSTALLED,
        format!("lock SDK not installed for {vendor}"),
    )
}

/// `LockFailures.Unsupported`: DEVICE_UNSUPPORTED_KIND `{verb} is not supported by the {vendor} lock`.
pub fn unsupported(vendor: &str, verb: &str) -> AdapterError {
    AdapterError::new(
        codes::UNSUPPORTED_KIND,
        format!("{verb} is not supported by the {vendor} lock"),
    )
}

/// `LockFailures.RequireAddress`: the room's RoomLockDef row with a non-blank RAddress
/// (returned as `(row, address)`), or DEVICE_NOT_CONFIGURED with `refusal`.
pub fn require_address<'a>(
    address: Option<&'a LockAddress>,
    refusal: &str,
) -> Result<(&'a LockAddress, &'a str), AdapterError> {
    match address {
        Some(a) => match a.address.as_deref() {
            Some(raddress) if !is_blank(raddress) => Ok((a, raddress)),
            _ => Err(not_configured(refusal)),
        },
        None => Err(not_configured(refusal)),
    }
}

/// `LockFailures.NoSetupForRoom` (VB6 mdlLockDotNET wording).
pub fn no_setup_for_room(room: &str) -> String {
    format!("No setup for room {room}. (RoomLockDef/RAddress)")
}

/// `LockFailures.MissingRoomLockDef` (VB6 Orbita / HSmart wording).
pub fn missing_room_lock_def(room: &str) -> String {
    format!("{room}. (RoomLockDef/RAddress)")
}

/// `LockFailures.Val` (VB6 `Val`): the leading decimal digits after white space and an
/// optional sign, 0 when there are none. More digits than an `Int64` holds fail like C#
/// `long.Parse` (`OverflowException`, reported as DEVICE_ADAPTER_FAILED).
pub fn val(text: Option<&str>) -> Result<i64, AdapterError> {
    let s = text.unwrap_or("").trim_start_matches(char::is_whitespace);
    let b = s.as_bytes();
    let start = usize::from(matches!(b.first(), Some(b'-' | b'+')));
    let end = start + b[start..].iter().take_while(|c| c.is_ascii_digit()).count();
    if end == start {
        return Ok(0);
    }
    s[..end].parse::<i64>().map_err(|_| {
        adapter_failed(
            "OverflowException",
            "Value was either too large or too small for an Int64.",
        )
    })
}

// ---- .NET string rules used by the lock adapters -------------------------------------

/// The C# `NullReferenceException` of a member call on a `null` payload string, which the
/// processor reports as DEVICE_ADAPTER_FAILED.
pub(crate) fn null_reference() -> AdapterError {
    adapter_failed(
        "NullReferenceException",
        "Object reference not set to an instance of an object.",
    )
}

/// The payload string, or the C# `NullReferenceException` when it is `null`.
pub(crate) fn required(s: &Option<String>) -> Result<&str, AdapterError> {
    s.as_deref().ok_or_else(null_reference)
}

/// `$"{s}"`: `null` interpolates as the empty string.
pub(crate) fn text(s: &Option<String>) -> &str {
    s.as_deref().unwrap_or("")
}

/// `string.IsNullOrWhiteSpace` of a present string.
pub(crate) fn is_blank(s: &str) -> bool {
    s.chars().all(char::is_whitespace)
}

/// `string.IsNullOrWhiteSpace`.
pub(crate) fn is_null_or_blank(s: Option<&str>) -> bool {
    s.is_none_or(is_blank)
}

/// `string.Trim()`.
pub(crate) fn trim(s: &str) -> &str {
    s.trim_matches(char::is_whitespace)
}

/// `ToUpperInvariant` (simple one-to-one case mapping, like .NET).
pub(crate) fn upper_invariant(s: &str) -> String {
    s.chars().map(|c| one_to_one(c, c.to_uppercase())).collect()
}

/// `ToLowerInvariant` (simple one-to-one case mapping, like .NET).
pub(crate) fn lower_invariant(s: &str) -> String {
    s.chars().map(|c| one_to_one(c, c.to_lowercase())).collect()
}

fn one_to_one(c: char, mut mapped: impl Iterator<Item = char>) -> char {
    match (mapped.next(), mapped.next()) {
        (Some(m), None) => m,
        _ => c,
    }
}

/// `string.Equals(a, b, StringComparison.OrdinalIgnoreCase)`; two `null`s are equal.
pub(crate) fn eq_opt_ignore_case(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => crate::config::eq_ignore_case(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// `s.PadLeft(width, c)`; the width counts UTF-16 units like .NET.
pub(crate) fn pad_left(s: &str, width: usize, c: char) -> String {
    let len: usize = s.chars().map(char::len_utf16).sum();
    let mut out = String::with_capacity(s.len() + width.saturating_sub(len));
    out.extend(std::iter::repeat_n(c, width.saturating_sub(len)));
    out.push_str(s);
    out
}

/// `new List<object>(cards)`: a negative capacity is the C# `ArgumentOutOfRangeException`.
pub(crate) fn card_count(cards: i32) -> Result<usize, AdapterError> {
    usize::try_from(cards).map_err(|_| {
        adapter_failed(
            "ArgumentOutOfRangeException",
            "Non-negative number required. (Parameter 'capacity')",
        )
    })
}

/// `t.ToString("yyyy-MM-dd HH:mm:ss", InvariantCulture)` (LockSDK and CLock times).
pub(crate) fn format_long(t: DateTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.year(),
        t.month().number(),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// `t.ToString("yy-MM-dd HH:mm:ss", InvariantCulture)` (HSmart V8 times).
pub(crate) fn format_short(t: DateTime) -> String {
    format!(
        "{:02}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.year().rem_euclid(100),
        t.month().number(),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// A `DateTime` member of a result (`WallClockDateTimeConverter`, `yyyy-MM-ddTHH:mm:ss`).
pub(crate) fn wall(t: DateTime) -> Value {
    Value::from(format_wall_clock(t))
}

/// A `string` member of a result: `null` stays `null`.
pub(crate) fn opt(s: &Option<String>) -> Value {
    s.as_deref().map_or(Value::Null, Value::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotnet_string_rules() {
        assert_eq!(trim(" \u{3000}a b\t"), "a b");
        assert!(is_blank("") && is_blank(" \u{a0}") && !is_blank(" x"));
        assert!(is_null_or_blank(None) && is_null_or_blank(Some(" ")));
        assert_eq!(upper_invariant("hune2013ß"), "HUNE2013ß");
        assert_eq!(lower_invariant("HLS"), "hls");
        assert!(eq_opt_ignore_case(None, None));
        assert!(eq_opt_ignore_case(Some("a1b2"), Some("A1B2")));
        assert!(!eq_opt_ignore_case(Some("101"), None));
        assert_eq!(pad_left("1", 2, '0'), "01");
        assert_eq!(pad_left("12345", 4, '0'), "12345");
        assert_eq!(card_count(2), Ok(2));
        assert_eq!(
            card_count(-1).unwrap_err().message,
            "ArgumentOutOfRangeException: Non-negative number required. (Parameter 'capacity')"
        );
        assert_eq!(
            required(&None).unwrap_err().message,
            "NullReferenceException: Object reference not set to an instance of an object."
        );
    }

    #[test]
    fn val_reads_like_vb6() {
        assert_eq!(val(Some("+7x")), Ok(7));
        assert_eq!(val(Some("-")), Ok(0));
        assert_eq!(val(None), Ok(0));
        assert_eq!(
            val(Some("99999999999999999999")).unwrap_err().code,
            codes::ADAPTER_FAILED
        );
    }

    #[test]
    fn time_formats() {
        let t = crate::contracts::parse_wall_clock("2026-10-14T14:05:09").unwrap();
        assert_eq!(format_long(t), "2026-10-14 14:05:09");
        assert_eq!(format_short(t), "26-10-14 14:05:09");
        let early = crate::contracts::parse_wall_clock("0005-01-02T03:04:05").unwrap();
        assert_eq!(format_long(early), "0005-01-02 03:04:05");
        assert_eq!(format_short(early), "05-01-02 03:04:05");
    }
}
