//! Port of Pos/PosDeviceRouterTests.cs, plus the C# outcomes for `null` payload members.

#[path = "pos_fakes/mod.rs"]
mod fakes;

use std::sync::Arc;

use dl_agent_pms_device::adapters::pos::{EscPos, EscPosTransport, PosDeviceRouter, failures};
use dl_agent_pms_device::adapters::{Cx, PosDevice};
use dl_agent_pms_device::contracts::{
    PosDisplayRequest, PosKickRequest, PosPrintRequest, PosReadRequest, PosSpeakRequest, PosTarget,
    codes,
};
use fakes::{PosTransportFake, b64, lines, target};

struct T {
    tcp: Arc<PosTransportFake>,
    serial: Arc<PosTransportFake>,
    spooler: Arc<PosTransportFake>,
}

impl T {
    fn new() -> T {
        T {
            tcp: PosTransportFake::new("tcp"),
            serial: PosTransportFake::new("serial"),
            spooler: PosTransportFake::new("spooler"),
        }
    }

    fn router(&self) -> PosDeviceRouter {
        PosDeviceRouter::new(self.tcp.clone(), self.serial.clone(), self.spooler.clone())
    }

    fn nothing_sent(&self) -> bool {
        self.tcp.sent().is_empty()
            && self.serial.sent().is_empty()
            && self.spooler.sent().is_empty()
    }
}

fn cx() -> Cx {
    Cx::unbounded()
}

fn print(target: PosTarget, data: &str) -> PosPrintRequest {
    PosPrintRequest {
        target: Some(target),
        data: Some(data.to_owned()),
        ..PosPrintRequest::default()
    }
}

fn opos(profile: &str) -> PosTarget {
    PosTarget {
        opos_profile: Some(profile.to_owned()),
        ..target("opos", "CashDrawer1")
    }
}

#[test]
fn prt03_print_goes_to_the_transport_named_by_the_target() {
    for transport in ["tcp", "serial", "spooler"] {
        let t = T::new();
        t.router()
            .print(&print(target(transport, "P1"), &b64(&[0x41])), &cx())
            .unwrap();
        let all = [&t.tcp, &t.serial, &t.spooler];
        let used: Vec<&str> = all
            .iter()
            .filter(|f| !f.sent().is_empty())
            .map(|f| f.transport())
            .collect();
        assert_eq!(used, [transport]);
        let fake = all.iter().find(|f| f.transport() == transport).unwrap();
        assert_eq!(fake.sent_bytes(), [vec![0x41]]);
    }
}

#[test]
fn prt03_print_repeats_data_per_copy_each_with_its_cut() {
    let t = T::new();
    let request = PosPrintRequest {
        cut: true,
        copies: 2,
        ..print(target("tcp", "P1"), &b64(&[0x41, 0x42]))
    };
    let result = t.router().print(&request, &cx()).unwrap();
    let mut one = vec![0x41, 0x42];
    one.extend_from_slice(&EscPos::CUT);
    assert_eq!(t.tcp.sent_bytes(), [[one.clone(), one].concat()]);
    assert_eq!(result["bytes"], 12);
    assert_eq!(result["copies"], 2);
    assert_eq!(
        dl_json::to_string(&result),
        r#"{"transport":"tcp","bytes":12,"copies":2}"#
    );
}

#[test]
fn prt03_bad_print_payloads_are_refused_before_any_byte_is_sent() {
    for (data, copies, transport) in [
        ("not base64!", 1, "tcp"),
        ("", 1, "tcp"),
        ("QQ==", 0, "tcp"),
        ("QQ==", 6, "tcp"),
        ("QQ==", 1, "lpt"),
    ] {
        let t = T::new();
        let request = PosPrintRequest {
            copies,
            ..print(target(transport, "P1"), data)
        };
        let e = t.router().print(&request, &cx()).unwrap_err();
        assert_eq!(e.code, codes::BAD_PAYLOAD, "{data:?} {copies} {transport}");
        assert!(t.tcp.sent().is_empty());
    }
}

#[test]
fn hw01_printer_kick_pulses_the_drawer_through_the_printer_transport() {
    let t = T::new();
    let request = PosKickRequest {
        target: Some(target("serial", "COM3")),
        pin: 5,
        ..PosKickRequest::default()
    };
    let result = t.router().kick(&request, &cx()).unwrap();
    assert_eq!(t.serial.sent_bytes(), [vec![0x1B, 0x70, 0x01, 25, 250]]);
    assert_eq!(result["kicked"], true);
    assert_eq!(result["pin"], 5);
}

#[test]
fn hw01_unreachable_printer_drawer_fails_with_the_transport_refusal() {
    let t = T::new();
    t.tcp.fail_with(failures::unavailable(
        &target("tcp", "P1"),
        "connection refused",
    ));
    let request = PosKickRequest {
        target: Some(target("tcp", "P1")),
        ..PosKickRequest::default()
    };
    let e = t.router().kick(&request, &cx()).unwrap_err();
    assert_eq!(e.code, codes::UNAVAILABLE);
    assert_eq!(e.message, "tcp device P1 unavailable: connection refused");
}

#[test]
fn hw02_pole_display_runs_on_serial_and_answers_the_lines_shown() {
    let t = T::new();
    let l = lines(&["Cà phê", "45,000"]);
    let request = PosDisplayRequest {
        target: Some(target("serial", "COM4")),
        lines: Some(l.clone()),
        ..PosDisplayRequest::default()
    };
    let result = t.router().display(&request, &cx()).unwrap();
    assert_eq!(
        t.serial.sent_bytes(),
        [EscPos::display(Some(&l), 20).unwrap()]
    );
    assert_eq!(
        dl_json::to_string(&result["lines"]),
        r#"["Ca phe","45,000"]"#
    );
}

#[test]
fn hw02_pole_display_over_tcp_is_refused() {
    let t = T::new();
    let request = PosDisplayRequest {
        target: Some(target("tcp", "P1")),
        lines: Some(lines(&["x"])),
        ..PosDisplayRequest::default()
    };
    let e = t.router().display(&request, &cx()).unwrap_err();
    assert_eq!(e.code, codes::TRANSPORT_UNSUPPORTED);
    assert_eq!(
        e.message,
        "pos.display is not supported over tcp: a pole display is reached over serial or OPOS"
    );
    assert!(t.tcp.sent().is_empty());
}

#[test]
fn prt03_read_queries_printer_and_paper_status() {
    let t = T::new();
    t.tcp.reply(&[0x16]);
    t.tcp.reply(&[0x1E]);
    let request = PosReadRequest {
        target: Some(target("tcp", "P1")),
        ..PosReadRequest::default()
    };
    let result = t.router().read(&request, &cx()).unwrap();
    assert_eq!(
        t.tcp.queries(),
        [
            EscPos::PRINTER_STATUS_QUERY.to_vec(),
            EscPos::PAPER_STATUS_QUERY.to_vec()
        ]
    );
    assert_eq!(
        dl_json::to_string(&result),
        r#"{"online":true,"drawerPin3High":true,"paperOut":false,"paperNearEnd":true}"#
    );
}

#[test]
fn hw03_opos_profiles_refuse_every_verb_with_vendor_sdk_missing() {
    for profile in ["1.8", "wincor", "WINCOR"] {
        let t = T::new();
        let r = t.router();
        let target = opos(profile);
        let results = [
            r.print(&print(target.clone(), &b64(&[0x41])), &cx()),
            r.kick(
                &PosKickRequest {
                    target: Some(target.clone()),
                    ..PosKickRequest::default()
                },
                &cx(),
            ),
            r.display(
                &PosDisplayRequest {
                    target: Some(target.clone()),
                    lines: Some(lines(&["x"])),
                    ..PosDisplayRequest::default()
                },
                &cx(),
            ),
            r.read(
                &PosReadRequest {
                    target: Some(target.clone()),
                    ..PosReadRequest::default()
                },
                &cx(),
            ),
        ];
        for result in results {
            let e = result.unwrap_err();
            assert_eq!(e.code, codes::VENDOR_SDK_MISSING);
            assert!(
                e.message.starts_with("vendor-sdk-missing:"),
                "{}",
                e.message
            );
        }
        assert!(t.nothing_sent());
    }
    let e = T::new()
        .router()
        .kick(
            &PosKickRequest {
                target: Some(opos("wincor")),
                ..PosKickRequest::default()
            },
            &cx(),
        )
        .unwrap_err();
    assert_eq!(
        e.message,
        "vendor-sdk-missing: OPOS CashDrawer 'CashDrawer1' needs the SMILE_OPOS_WINCOR service object, which is not installed in this agent"
    );
}

#[test]
fn hw03_unknown_opos_profile_is_a_bad_payload() {
    let e = T::new()
        .router()
        .kick(
            &PosKickRequest {
                target: Some(opos("epson")),
                ..PosKickRequest::default()
            },
            &cx(),
        )
        .unwrap_err();
    assert_eq!(e.code, codes::BAD_PAYLOAD);
    assert_eq!(
        e.message,
        "OPOS profile must be one of 1.8, wincor, got 'epson'"
    );
}

#[test]
fn prt03_bluetooth_is_refused_on_the_agent_with_the_browser_hint() {
    let e = T::new()
        .router()
        .print(&print(target("bluetooth", "TM-P20"), &b64(&[0x41])), &cx())
        .unwrap_err();
    assert_eq!(e.code, codes::TRANSPORT_UNSUPPORTED);
    assert!(e.message.contains("Web Bluetooth"));
}

// ---- What C# does with `null` members (System.Text.Json leaves them null) ----

const NRE: &str = "NullReferenceException: Object reference not set to an instance of an object.";

#[test]
fn null_members_fail_like_the_csharp_router() {
    let t = T::new();
    let r = t.router();
    let e = r.kick(&PosKickRequest::default(), &cx()).unwrap_err();
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        (codes::ADAPTER_FAILED, NRE)
    );

    let no_transport = PosTarget {
        endpoint: Some("P1".into()),
        ..PosTarget::default()
    };
    let e = r
        .print(&print(no_transport, &b64(&[0x41])), &cx())
        .unwrap_err();
    assert_eq!(
        e.message,
        "ArgumentNullException: Value cannot be null. (Parameter 'key')"
    );

    let no_data = PosPrintRequest {
        target: Some(target("tcp", "P1")),
        ..PosPrintRequest::default()
    };
    let e = r.print(&no_data, &cx()).unwrap_err();
    assert_eq!(
        e.message,
        "ArgumentNullException: Value cannot be null. (Parameter 's')"
    );

    let e = r
        .display(
            &PosDisplayRequest {
                target: Some(target("serial", "COM4")),
                ..PosDisplayRequest::default()
            },
            &cx(),
        )
        .unwrap_err();
    assert_eq!(e.message, NRE);

    let e = r
        .speak(
            &PosSpeakRequest {
                target: Some(target("sapi", "default")),
                ..PosSpeakRequest::default()
            },
            &cx(),
        )
        .unwrap_err();
    assert_eq!(
        e.code,
        codes::NOT_CONNECTED,
        "no speaker: checked before the text"
    );

    let e = r
        .read(
            &PosReadRequest {
                device: None,
                ..PosReadRequest::default()
            },
            &cx(),
        )
        .unwrap_err();
    assert_eq!(e.message, "read device must be printer or card, got ''");

    let e = r
        .print(&print(target("lpt", "P1"), "QQ=="), &cx())
        .unwrap_err();
    assert_eq!(
        e.message,
        "unknown POS transport 'lpt' for this verb; expected tcp, serial, spooler, bluetooth or opos"
    );
    assert!(t.nothing_sent());
}

#[test]
fn empty_status_reply_is_the_csharp_index_error() {
    let t = T::new();
    t.tcp.reply(&[]);
    t.tcp.reply(&[0x12]);
    let e = t
        .router()
        .read(
            &PosReadRequest {
                target: Some(target("tcp", "P1")),
                ..PosReadRequest::default()
            },
            &cx(),
        )
        .unwrap_err();
    assert_eq!(
        e.message,
        "IndexOutOfRangeException: Index was outside the bounds of the array."
    );
}
