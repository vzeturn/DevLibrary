//! Which hub event means what, and what plays for it (the `OnEvent` handlers and
//! `announce` of the Go `main.go`, port `agent.rs`).
//!
//! The cloud broadcasts to the RVC group (`RealtimeNotifier.cs`):
//! - `GuestOrderRequested(rvcNo, tableNo, requestId)`: kind `order`;
//! - `GuestServiceRequested(rvcNo, tableNo, requestId, type)`: `REQUEST_BILL` is
//!   `service_bill`, `CHARGE_TO_ROOM` is `service_charge`, anything else `service_waiter`.
//!
//! What plays is decided per RVC by that RVC's [`Mode`]: the tone unless it is off, then
//! the spoken phrase when the voice is on, in the mode's language.

use dl_json::Value;

use crate::mode::Mode;
use crate::phrase::{build_phrase, normalize_lang};

/// The hub method of a guest order.
pub const GUEST_ORDER: &str = "GuestOrderRequested";
/// The hub method of a guest service call.
pub const GUEST_SERVICE: &str = "GuestServiceRequested";

/// A guest event the speaker announces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestEvent {
    /// The announcement kind (`order`, `service_bill`, `service_charge`,
    /// `service_waiter`).
    pub kind: &'static str,
    /// The RVC (outlet) the event belongs to; `0` when the argument was unreadable.
    pub rvc: i64,
    /// The table as the guest app sent it (empty when absent).
    pub table: String,
}

/// What to do for one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announcement {
    /// The RVC the decision was made for.
    pub rvc: i64,
    /// The tone to play, `None` when the RVC's tone is off.
    pub tone: Option<String>,
    /// What to say, `None` when the RVC's voice is off.
    pub speech: Option<Speech>,
}

/// A spoken phrase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Speech {
    /// The text.
    pub text: String,
    /// `vi` or `en`.
    pub lang: &'static str,
}

/// The announcement kind for a `GuestServiceRequested` request type.
pub fn service_kind(request_type: &str) -> &'static str {
    match request_type {
        "REQUEST_BILL" => "service_bill",
        "CHARGE_TO_ROOM" => "service_charge",
        _ => "service_waiter",
    }
}

/// Reads argument `i` as an integer; a wrong type is logged and read as `0` (Go keeps
/// alerting on a garbled event but says so).
fn arg_int(target: &str, args: &[Value], i: usize, name: &str) -> i64 {
    match args.get(i) {
        None | Some(Value::Null) => 0,
        Some(v) => v.as_i64().unwrap_or_else(|| {
            dl_log::warn!("speaker: event argument not an integer"; event = target, arg = name, kind = v.type_name());
            0
        }),
    }
}

/// Reads argument `i` as a string; a wrong type is logged and read as empty.
fn arg_str(target: &str, args: &[Value], i: usize, name: &str) -> String {
    match args.get(i) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => {
            dl_log::warn!("speaker: event argument not a string"; event = target, arg = name, kind = v.type_name());
            String::new()
        }
    }
}

/// Decodes a hub invocation into a [`GuestEvent`]; `None` for any other method.
pub fn parse_event(target: &str, args: &[Value]) -> Option<GuestEvent> {
    let kind = if target.eq_ignore_ascii_case(GUEST_ORDER) {
        "order"
    } else if target.eq_ignore_ascii_case(GUEST_SERVICE) {
        service_kind(&arg_str(target, args, 3, "requestType"))
    } else {
        return None;
    };
    Some(GuestEvent {
        kind,
        rvc: arg_int(target, args, 0, "rvcNo"),
        table: arg_str(target, args, 1, "tableNo"),
    })
}

/// What plays for `event` under the RVC's `mode`.
pub fn decide(event: &GuestEvent, mode: &Mode) -> Announcement {
    // An explicit "tone": null means silent; only the voice may still speak.
    let tone = (!mode.tone_off()).then(|| mode.tone_or_empty().to_owned());
    let speech = mode.voice.then(|| Speech {
        text: build_phrase(event.kind, &event.table, &mode.lang),
        lang: normalize_lang(&mode.lang),
    });
    Announcement {
        rvc: event.rvc,
        tone,
        speech,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<Value> {
        dl_json::parse(s).unwrap().as_array().unwrap().clone()
    }

    fn ev(kind: &'static str, rvc: i64, table: &str) -> GuestEvent {
        GuestEvent {
            kind,
            rvc,
            table: table.into(),
        }
    }

    #[test]
    fn hub_events_map_to_kinds_like_go() {
        assert_eq!(
            parse_event(GUEST_ORDER, &args(r#"[1,"T05",42]"#)),
            Some(ev("order", 1, "T05"))
        );
        assert_eq!(
            parse_event(GUEST_SERVICE, &args(r#"[3,"T1",7,"REQUEST_BILL"]"#)),
            Some(ev("service_bill", 3, "T1"))
        );
        assert_eq!(
            parse_event(GUEST_SERVICE, &args(r#"[3,"T1",7,"CHARGE_TO_ROOM"]"#)),
            Some(ev("service_charge", 3, "T1"))
        );
        assert_eq!(
            parse_event(GUEST_SERVICE, &args(r#"[3,"T1",7,"CALL_WAITER"]"#)),
            Some(ev("service_waiter", 3, "T1"))
        );
        assert_eq!(
            parse_event(GUEST_SERVICE, &args(r#"[3,"T1"]"#)),
            Some(ev("service_waiter", 3, "T1"))
        );
        // SignalR method names are case-insensitive.
        assert_eq!(
            parse_event("guestorderrequested", &args(r#"[2,"A"]"#)),
            Some(ev("order", 2, "A"))
        );
        assert_eq!(parse_event("PrintJob", &args("[]")), None);
        assert_eq!(parse_event("ForceUninstall", &args("[]")), None);
    }

    #[test]
    fn garbled_arguments_still_alert_with_zero_values() {
        assert_eq!(
            parse_event(GUEST_ORDER, &args("[]")),
            Some(ev("order", 0, ""))
        );
        assert_eq!(
            parse_event(GUEST_ORDER, &args(r#"["1",5]"#)),
            Some(ev("order", 0, ""))
        );
        assert_eq!(
            parse_event(GUEST_ORDER, &args("[null,null]")),
            Some(ev("order", 0, ""))
        );
    }

    #[test]
    fn default_mode_plays_the_guest_tone_and_speaks_vietnamese() {
        let a = decide(&ev("order", 1, "12"), &Mode::default());
        assert_eq!(a.tone.as_deref(), Some("guest"));
        assert_eq!(
            a.speech,
            Some(Speech {
                text: "Bàn 12 đặt món".into(),
                lang: "vi"
            })
        );
        assert_eq!(a.rvc, 1);
    }

    #[test]
    fn tone_off_keeps_the_voice() {
        let mode = crate::mode::parse_mode(r#"{"tone":null,"voice":true,"lang":"en"}"#);
        let a = decide(&ev("service_bill", 2, "7"), &mode);
        assert_eq!(a.tone, None);
        assert_eq!(
            a.speech,
            Some(Speech {
                text: "Table 7 requests the bill".into(),
                lang: "en"
            })
        );
        // A blank tone name is off as well.
        let blank = crate::mode::parse_mode(r#"{"tone":"","voice":false}"#);
        let a = decide(&ev("order", 2, "7"), &blank);
        assert_eq!((a.tone, a.speech), (None, None));
    }

    #[test]
    fn voice_off_plays_only_the_tone() {
        let a = decide(&ev("service_waiter", 4, "9"), &Mode::new("bell", false));
        assert_eq!(a.tone.as_deref(), Some("bell"));
        assert_eq!(a.speech, None);
    }

    #[test]
    fn each_rvc_uses_its_own_mode() {
        let cache = crate::mode::Cache::new();
        cache.set(1, Mode::new("gong", false));
        cache.set(2, Mode::new("chime", true).with_lang("en-US"));
        let order = |rvc| decide(&ev("order", rvc, "5"), &cache.get(rvc));
        let one = order(1);
        assert_eq!((one.tone.as_deref(), one.speech), (Some("gong"), None));
        let two = order(2);
        assert_eq!(two.tone.as_deref(), Some("chime"));
        assert_eq!(two.speech.unwrap().text, "Table 5 placed an order");
        // An RVC without a fetched mode falls back to the default.
        let three = order(3);
        assert_eq!(three.tone.as_deref(), Some("guest"));
        assert_eq!(three.speech.unwrap().lang, "vi");
    }

    #[test]
    fn service_kinds_and_auto_language() {
        let mode = Mode::new("service", true).with_lang("auto");
        for (kind, want) in [
            ("service_charge", "Bàn 3 tính vào phòng"),
            ("service_waiter", "Bàn 3 gọi phục vụ"),
            ("service_bill", "Bàn 3 yêu cầu thanh toán"),
        ] {
            let a = decide(&ev(kind, 1, "3"), &mode);
            assert_eq!(a.speech.unwrap().text, want);
        }
        assert_eq!(
            decide(&ev("order", 1, ""), &mode).speech.unwrap().text,
            "Bàn — đặt món"
        );
    }
}
