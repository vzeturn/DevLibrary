//! Port of `Staff/StaffPinHasher.cs`: the one place the agent turns a POS
//! `CashierDef.TouchID` into what leaves the store (ADR 0101).

use crate::contracts::staff;
use crate::contracts::wire::staff_pin_status;

/// The lookup v2 `HMAC-SHA256(K_t, pin)` (base64) of a clerk, or the status saying why
/// there is none. The PIN is trimmed on the right only, as the POS sign-in compared it
/// (`RTRIM(TouchID) = RTRIM(@TouchID)`). Nothing here logs.
///
/// A `pin_key` that is not 32 bytes cannot hash a usable PIN: the C# code throws
/// (`ArgumentException`), this returns `(None, "unusable")`. Callers always pass a key
/// checked to be 32 bytes.
pub fn hash(
    pin_key: &[u8],
    touch_id: Option<&str>,
    active: bool,
) -> (Option<String>, &'static str) {
    if !active {
        return (None, staff_pin_status::NONE);
    }
    // TrimEnd(): .NET white space (char.IsWhiteSpace), which Rust's trim_end matches.
    let pin = touch_id.map(str::trim_end);
    match pin {
        None | Some("") => (None, staff_pin_status::NONE),
        Some(p) if !staff::is_usable(Some(p)) => (None, staff_pin_status::UNUSABLE),
        Some(p) => match staff::lookup(pin_key, p) {
            Ok(mac) => (
                Some(dl_encoding::base64::STANDARD.encode(mac)),
                staff_pin_status::SET,
            ),
            Err(_) => (None, staff_pin_status::UNUSABLE),
        },
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The C# test holding the golden vector shared with the cloud's `Tokens` tests.
    const CS_TESTS: &str = include_str!("../../testdata/cs/Staff/StaffPinHasherTests.cs");

    /// The first string literal after `marker` in the C# test.
    pub fn golden(marker: &str) -> &'static str {
        let at = CS_TESTS.find(marker).expect("marker") + marker.len();
        let rest = &CS_TESTS[at..];
        let start = rest.find('"').expect("open quote") + 1;
        let len = rest[start..].find('"').expect("close quote");
        &rest[start..start + len]
    }

    pub fn golden_key() -> Vec<u8> {
        dl_encoding::hex::decode(golden("GoldenKey =")).unwrap()
    }

    pub fn golden_key_id() -> &'static str {
        golden("GoldenKeyId =")
    }

    pub fn golden_pin() -> &'static str {
        golden("GoldenPin =")
    }

    pub fn golden_lookup() -> &'static str {
        golden("GoldenLookup =")
    }

    fn set(lookup: &str) -> (Option<String>, &'static str) {
        (Some(lookup.to_owned()), staff_pin_status::SET)
    }

    #[test]
    fn a_usable_pin_hashes_to_the_golden_lookup_with_the_golden_key_id() {
        assert_eq!(golden_pin(), "482193");
        assert_eq!(
            hash(&golden_key(), Some(golden_pin()), true),
            set(golden_lookup())
        );
        assert_eq!(staff::key_id(&golden_key()), golden_key_id());
    }

    #[test]
    fn trailing_padding_is_trimmed_as_the_pos_sign_in_did_and_leading_padding_is_not() {
        let k = golden_key();
        let padded = format!("{}   ", golden_pin());
        assert_eq!(hash(&k, Some(&padded), true), set(golden_lookup()));
        let leading = format!(" {}", golden_pin());
        assert_eq!(
            hash(&k, Some(&leading), true),
            (None, staff_pin_status::UNUSABLE)
        );
    }

    #[test]
    fn a_disabled_clerk_carries_no_lookup_even_with_a_pin() {
        assert_eq!(
            hash(&golden_key(), Some(golden_pin()), false),
            (None, staff_pin_status::NONE)
        );
    }

    #[test]
    fn no_pin_on_the_pos_is_status_none() {
        for t in [None, Some(""), Some("    ")] {
            assert_eq!(hash(&golden_key(), t, true), (None, staff_pin_status::NONE));
        }
    }

    #[test]
    fn a_pin_the_cloud_keypad_cannot_type_is_unusable_and_never_hashed() {
        for t in ["1234567890123", "12ab", "12 34", "١٢٣٤"] {
            assert_eq!(
                hash(&golden_key(), Some(t), true),
                (None, staff_pin_status::UNUSABLE),
                "{t}"
            );
        }
    }

    #[test]
    fn one_to_twelve_digits_are_all_usable_since_pos_win_takes_short_touch_ids() {
        for t in ["7", "323", "0000", "123456789012"] {
            assert_eq!(hash(&golden_key(), Some(t), true).1, staff_pin_status::SET);
        }
    }
}
