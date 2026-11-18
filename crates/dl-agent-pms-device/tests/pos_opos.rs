//! OPOS beyond C#: the call sequence on a registered UnifiedPOS control object (through a
//! fake dispatcher), the ResultCode -> device code mapping, and the exact C# refusal when
//! the control object is not registered.

#[path = "pos_fakes/mod.rs"]
mod fakes;

use dl_agent_pms_device::adapters::pos::{
    ComFailure, OposAdapter, PosDeviceRouter, opos_code_name, result_codes,
};
use dl_agent_pms_device::adapters::{Cx, PosDevice};
use dl_agent_pms_device::contracts::{
    PosDisplayRequest, PosKickRequest, PosPrintRequest, PosReadRequest, PosSpeakRequest, PosTarget,
    codes, read_devices,
};
use fakes::{OposFake, PosTransportFake, b64, lines, target};

fn opos(endpoint: &str, profile: &str) -> PosTarget {
    PosTarget {
        opos_profile: Some(profile.to_owned()),
        ..target("opos", endpoint)
    }
}

fn kick(endpoint: &str) -> PosKickRequest {
    PosKickRequest {
        target: Some(opos(endpoint, "1.8")),
        ..PosKickRequest::default()
    }
}

fn cx() -> Cx {
    Cx::unbounded()
}

const DRAWER: &str = "OPOS.CashDrawer";

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

#[test]
fn kick_drives_a_registered_cash_drawer() {
    let host = OposFake::registering(&[DRAWER]);
    let result = OposAdapter::new(host.clone())
        .kick(&kick("CashDrawer1"), &cx())
        .unwrap();
    assert_eq!(dl_json::to_string(&result), r#"{"kicked":true,"pin":2}"#);
    assert_eq!(
        host.calls(),
        s(&[
            "create OPOS.CashDrawer",
            r#"Open("CashDrawer1")"#,
            "ClaimDevice(1000)",
            "DeviceEnabled=true",
            "ResultCode?",
            "OpenDrawer()",
            "DeviceEnabled=false",
            "ReleaseDevice()",
            "Close()",
        ])
    );
}

#[test]
fn an_unregistered_control_object_keeps_the_csharp_refusal() {
    let host = OposFake::registering(&[]);
    let e = OposAdapter::new(host.clone())
        .kick(&kick("CashDrawer1"), &cx())
        .unwrap_err();
    assert_eq!(e.code, codes::VENDOR_SDK_MISSING);
    assert_eq!(
        e.message,
        "vendor-sdk-missing: OPOS CashDrawer 'CashDrawer1' needs the SMILE_OPOS_1_8 service object, which is not installed in this agent"
    );
    assert!(host.calls().is_empty());
}

#[test]
fn display_drives_a_registered_line_display() {
    let host = OposFake::registering(&["OPOS.LineDisplay"]);
    let request = PosDisplayRequest {
        target: Some(opos("LineDisplay1", "wincor")),
        lines: Some(lines(&["Cà phê", "45,000"])),
        ..PosDisplayRequest::default()
    };
    let result = OposAdapter::new(host.clone())
        .display(&request, &cx())
        .unwrap();
    assert_eq!(
        dl_json::to_string(&result),
        r#"{"lines":["Ca phe","45,000"]}"#
    );
    assert_eq!(
        host.calls(),
        s(&[
            "create OPOS.LineDisplay",
            r#"Open("LineDisplay1")"#,
            "ClaimDevice(1000)",
            "DeviceEnabled=true",
            "ResultCode?",
            "ClearText()",
            r#"DisplayTextAt(0, 0, "Ca phe              ", 0)"#,
            r#"DisplayTextAt(1, 0, "45,000              ", 0)"#,
            "DeviceEnabled=false",
            "ReleaseDevice()",
            "Close()",
        ])
    );
}

#[test]
fn display_lines_are_checked_before_the_device_is_opened() {
    let host = OposFake::registering(&["OPOS.LineDisplay"]);
    let request = PosDisplayRequest {
        target: Some(opos("LineDisplay1", "1.8")),
        lines: Some(lines(&["a", "b", "c", "d", "e"])),
        ..PosDisplayRequest::default()
    };
    let e = OposAdapter::new(host.clone())
        .display(&request, &cx())
        .unwrap_err();
    assert_eq!(e.code, codes::BAD_PAYLOAD);
    assert_eq!(host.calls(), s(&["create OPOS.LineDisplay"]));
}

#[test]
fn result_codes_map_to_device_codes() {
    let cases = [
        ("Open", result_codes::NO_EXIST, codes::NOT_CONFIGURED),
        ("Open", result_codes::NO_SERVICE, codes::VENDOR_SDK_MISSING),
        ("ClaimDevice", result_codes::CLAIMED, codes::UNAVAILABLE),
        ("ClaimDevice", result_codes::TIMEOUT, codes::UNAVAILABLE),
        ("ResultCode", result_codes::NO_HARDWARE, codes::UNAVAILABLE),
        ("ResultCode", result_codes::OFFLINE, codes::UNAVAILABLE),
        ("OpenDrawer", result_codes::BUSY, codes::UNAVAILABLE),
        ("OpenDrawer", result_codes::FAILURE, codes::ERROR),
        ("OpenDrawer", result_codes::ILLEGAL, codes::ERROR),
        ("OpenDrawer", 999, codes::ERROR),
    ];
    for (method, rc, code) in cases {
        let host = OposFake::registering(&[DRAWER]);
        host.code(method, rc);
        let e = OposAdapter::new(host.clone())
            .kick(&kick("CashDrawer1"), &cx())
            .unwrap_err();
        assert_eq!(e.code, code, "{method} {rc}");
        let calls = host.calls();
        // Open failed: nothing to close. Otherwise the device is always closed.
        assert_eq!(
            calls.last().unwrap() == "Close()",
            method != "Open",
            "{calls:?}"
        );
        assert_eq!(
            calls.contains(&"ReleaseDevice()".to_owned()),
            method != "Open" && method != "ClaimDevice",
            "{calls:?}"
        );
    }
}

#[test]
fn result_code_messages_name_the_step_and_code() {
    let host = OposFake::registering(&[DRAWER]);
    host.code("Open", result_codes::NO_SERVICE);
    let e = OposAdapter::new(host)
        .kick(&kick("CashDrawer1"), &cx())
        .unwrap_err();
    assert_eq!(
        e.message,
        "vendor-sdk-missing: OPOS CashDrawer 'CashDrawer1' Open returned OPOS_E_NOSERVICE (104)"
    );

    let host = OposFake::registering(&[DRAWER]);
    host.code("OpenDrawer", result_codes::EXTENDED);
    host.code("ResultCodeExtended", 7);
    let e = OposAdapter::new(host)
        .kick(&kick("CashDrawer1"), &cx())
        .unwrap_err();
    assert_eq!(e.code, codes::ERROR);
    assert_eq!(
        e.message,
        "OPOS CashDrawer 'CashDrawer1' OpenDrawer returned OPOS_E_EXTENDED (114), extended 7"
    );

    let host = OposFake::registering(&[DRAWER]);
    host.com_failures.lock().unwrap().insert(
        "OpenDrawer".into(),
        ComFailure {
            hresult: 0x8002_0009,
            detail: "boom".into(),
        },
    );
    let e = OposAdapter::new(host.clone())
        .kick(&kick("CashDrawer1"), &cx())
        .unwrap_err();
    assert_eq!(
        e.message,
        "OPOS CashDrawer 'CashDrawer1' OpenDrawer failed: 0x80020009 boom"
    );
    assert_eq!(host.calls().last().map(String::as_str), Some("Close()"));
}

#[test]
fn verbs_without_a_certain_opos_mapping_keep_the_csharp_refusal() {
    let host =
        OposFake::registering(&["OPOS.POSPrinter", "OPOS.MSR", "OPOS.ToneIndicator", DRAWER]);
    let a = OposAdapter::new(host.clone());
    let t = opos("Dev1", "1.8");
    let results = [
        (
            a.print(
                &PosPrintRequest {
                    target: Some(t.clone()),
                    data: Some(b64(&[0x41])),
                    ..PosPrintRequest::default()
                },
                &cx(),
            ),
            "POSPrinter",
        ),
        (
            a.read(
                &PosReadRequest {
                    target: Some(t.clone()),
                    ..PosReadRequest::default()
                },
                &cx(),
            ),
            "POSPrinter",
        ),
        (
            a.read(
                &PosReadRequest {
                    target: Some(t.clone()),
                    device: Some(read_devices::CARD.to_owned()),
                    ..PosReadRequest::default()
                },
                &cx(),
            ),
            "MSR",
        ),
        (
            a.speak(
                &PosSpeakRequest {
                    target: Some(t.clone()),
                    text: Some("x".into()),
                    ..PosSpeakRequest::default()
                },
                &cx(),
            ),
            "ToneIndicator",
        ),
    ];
    for (result, class) in results {
        let e = result.unwrap_err();
        assert_eq!(e.code, codes::VENDOR_SDK_MISSING);
        assert_eq!(
            e.message,
            format!(
                "vendor-sdk-missing: OPOS {class} 'Dev1' needs the SMILE_OPOS_1_8 service object, which is not installed in this agent"
            )
        );
    }
    assert!(host.calls().is_empty());
}

#[test]
fn the_profile_is_checked_first_and_the_logical_name_must_not_be_empty() {
    let host = OposFake::registering(&[DRAWER]);
    let a = OposAdapter::new(host.clone());
    let bad_profile = PosKickRequest {
        target: Some(opos("CashDrawer1", "epson")),
        ..PosKickRequest::default()
    };
    assert_eq!(
        a.kick(&bad_profile, &cx()).unwrap_err().code,
        codes::BAD_PAYLOAD
    );
    assert!(host.calls().is_empty());

    let e = a.kick(&kick("  "), &cx()).unwrap_err();
    assert_eq!(e.code, codes::BAD_PAYLOAD);
    assert_eq!(e.message, "opos endpoint (logical device name) is empty");
}

#[test]
fn the_router_sends_opos_targets_to_the_configured_adapter() {
    let host = OposFake::registering(&[DRAWER]);
    let tcp = PosTransportFake::new("tcp");
    let router = PosDeviceRouter::new(
        tcp.clone(),
        PosTransportFake::new("serial"),
        PosTransportFake::new("spooler"),
    )
    .with_opos(OposAdapter::new(host.clone()));
    let result = router.kick(&kick("CashDrawer1"), &cx()).unwrap();
    assert_eq!(result["kicked"], true);
    assert!(tcp.sent().is_empty());
    assert_eq!(host.calls()[0], "create OPOS.CashDrawer");
}

#[test]
fn opos_code_names() {
    assert_eq!(opos_code_name(0), Some("OPOS_SUCCESS"));
    assert_eq!(opos_code_name(104), Some("OPOS_E_NOSERVICE"));
    assert_eq!(opos_code_name(115), Some("OPOS_E_DEPRECATED"));
    assert_eq!(opos_code_name(100), None);
}
