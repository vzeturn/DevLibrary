//! Port of ContractTests.cs and the contract parts of Pos/PosContractTests.cs, reading the
//! golden files in place (apps/device-agent/contract), plus the System.Text.Json behaviour
//! measured on .NET 10 with `DeviceJson.Options` (expected values copied from that run).

use dl_agent_pms_device::contracts::{
    AdapterError, CancelAllRequest, ClaimResponse, CommandOutcome, EraseRequest, KeyCardRequest,
    Payload, PendingResult, PosDisplayRequest, PosKickRequest, PosPrintRequest, PosReadRequest,
    PosSpeakRequest, ReadRequest, ResultRequest, codes, decode_payload, format_date_time_offset,
    format_wall_clock, kinds, parse_json, read_devices, transports,
};
use dl_json::{ToJson, Value};
use dl_time::OffsetDateTime;
use dl_uuid::Uuid;

const CLAIM_RESPONSE: &str = include_str!("contract/claim.response.json");
const LOCK_ERASE_PAYLOAD: &str = include_str!("contract/lock.erase.payload.json");
const LOCK_ISSUE_PAYLOAD: &str = include_str!("contract/lock.issue.payload.json");
const LOCK_ISSUE_RESULT: &str = include_str!("contract/lock.issue.result.json");
const POS_DISPLAY_PAYLOAD: &str = include_str!("contract/pos.display.payload.json");
const POS_KICK_PAYLOAD: &str = include_str!("contract/pos.kick.payload.json");
const POS_PRINT_PAYLOAD: &str = include_str!("contract/pos.print.payload.json");
const POS_READ_CARD_PAYLOAD: &str = include_str!("contract/pos.read-card.payload.json");
const POS_READ_PAYLOAD: &str = include_str!("contract/pos.read.payload.json");
const POS_SPEAK_PAYLOAD: &str = include_str!("contract/pos.speak.payload.json");
const RESULT_REQUEST: &str = include_str!("contract/result.request.json");

fn json(text: &str) -> Value {
    parse_json(text.as_bytes()).expect("fixture is JSON")
}

fn decode<T: Payload>(text: &str) -> Result<T, String> {
    decode_payload::<T>(&json(text)).map_err(|e| e.message().to_owned())
}

fn s(v: &Option<String>) -> &str {
    v.as_deref().unwrap_or("<null>")
}

#[test]
fn contract_lock_issue_payload_parses() {
    let r: KeyCardRequest = decode(LOCK_ISSUE_PAYLOAD).unwrap();
    assert_eq!(r.folio_num, Some(123_456));
    assert_eq!((s(&r.room), r.cards), ("101", 2));
    assert_eq!(
        (format_wall_clock(r.from), format_wall_clock(r.to)),
        ("2026-10-14T14:00:00".into(), "2026-10-16T12:00:00".into())
    );
}

#[test]
fn contract_lock_issue_payload_runs_on_the_simulated_adapter_and_matches_the_result_fixture() {
    use dl_agent_pms_device::adapters::{Cx, DeviceAdapter, SimulatedLockAdapter};
    let r: KeyCardRequest = decode(LOCK_ISSUE_PAYLOAD).unwrap();
    assert_eq!(r.folio_num, Some(123_456));
    let result = SimulatedLockAdapter::new()
        .issue(&r, &Cx::unbounded())
        .unwrap();
    assert_eq!(
        result,
        json(LOCK_ISSUE_RESULT),
        "{}",
        dl_json::to_string(&result)
    );
}

#[test]
fn contract_lock_erase_payload_parses() {
    let r: EraseRequest = decode(LOCK_ERASE_PAYLOAD).unwrap();
    let address = r.lock_address.as_ref().unwrap();
    let vendor = r.vendor.as_ref().unwrap();
    assert_eq!(
        (
            s(&r.room),
            s(&r.card_number),
            s(&address.address),
            s(&vendor.mf_lock_name)
        ),
        ("101", "A1B2C3D4", "0101", "hls")
    );
    assert_eq!(r.to_json(), json(LOCK_ERASE_PAYLOAD));
}

#[test]
fn contract_lock_payloads_with_address_and_vendor_parse_strictly() {
    let issue: KeyCardRequest = decode(
        r#"{"room":"101","from":"2026-10-14T14:00:00","to":"2026-10-16T12:00:00","cards":1,"folioNum":1,"lockAddress":null,"vendor":{"keyType":"MFCARD","mfLockName":"hls"},"addGuest":true}"#,
    )
    .unwrap();
    assert_eq!(
        (issue.add_guest, s(&issue.vendor.as_ref().unwrap().key_type)),
        (Some(true), "MFCARD")
    );
    assert_eq!(issue.lock_address, None);
    assert_eq!(decode::<ReadRequest>("{}").unwrap().vendor, None);
    assert!(
        decode::<CancelAllRequest>(
            r#"{"room":"101","vendor":{"keyType":"MFCARD","mfLockName":"hls","extra":1}}"#
        )
        .is_err()
    );
}

#[test]
fn contract_claim_response_deserializes() {
    let claim = ClaimResponse::from_json(&json(CLAIM_RESPONSE))
        .unwrap()
        .unwrap();
    assert_eq!(claim.lease_seconds, Some(60));
    assert_eq!(
        claim.id,
        Uuid::parse("6f1c2b0e-8d1a-4c55-9a51-3f7d2b9e0a11").unwrap()
    );
    assert_eq!(claim.kind.as_deref(), Some(kinds::LOCK_ISSUE));
    assert_eq!(claim.attempt, 1);
    assert_eq!(claim.payload.as_ref().unwrap()["room"], "101");

    let claimed_at = OffsetDateTime::parse_rfc3339("2026-10-14T07:00:00Z").unwrap();
    let cmd = claim.to_command(claimed_at).unwrap();
    assert_eq!(
        format_date_time_offset(cmd.lease_until),
        "2026-10-14T07:00:55+00:00"
    );
    assert_eq!(
        (cmd.kind.as_str(), cmd.claim_token.to_string().as_str()),
        ("lock.issue", "0b8f6c1e-2c3d-4e5f-8a9b-1c2d3e4f5a6b")
    );
}

#[test]
fn contract_failed_result_request_serializes_to_the_fixture() {
    let outcome = CommandOutcome::failed(codes::TIMEOUT, "Adapter did not answer within 30 s");
    let body = ResultRequest::from_outcome(
        Uuid::parse("0b8f6c1e-2c3d-4e5f-8a9b-1c2d3e4f5a6b").unwrap(),
        &outcome,
    );
    assert_eq!(body.to_json(), json(RESULT_REQUEST));
}

#[test]
fn contract_payload_with_unknown_member_is_refused() {
    let err = decode::<KeyCardRequest>(
        r#"{"room":"101","rooom":1,"from":"2026-10-14T14:00:00","to":"2026-10-16T12:00:00"}"#,
    )
    .unwrap_err();
    assert_eq!(
        err,
        "The JSON property 'rooom' could not be mapped to any .NET member contained in type 'SmilePms.DeviceAgent.Commands.KeyCardRequest'."
    );
}

#[test]
fn contract_pos_print_payload_parses() {
    let r: PosPrintRequest = decode(POS_PRINT_PAYLOAD).unwrap();
    let t = r.target.as_ref().unwrap();
    assert_eq!(
        (s(&t.transport), s(&t.endpoint)),
        (transports::TCP, "192.168.1.16:9100")
    );
    assert_eq!((s(&r.data), r.cut, r.copies), ("G0BIZWxsbwo=", true, 1));
}

#[test]
fn contract_pos_kick_payload_parses() {
    let r: PosKickRequest = decode(POS_KICK_PAYLOAD).unwrap();
    let t = r.target.as_ref().unwrap();
    assert_eq!(
        (s(&t.transport), s(&t.endpoint), t.baud_rate),
        (transports::SERIAL, "COM3", Some(9600))
    );
    assert_eq!((r.pin, r.on_ms, r.off_ms), (2, 50, 500));
}

#[test]
fn contract_pos_display_payload_parses() {
    let r: PosDisplayRequest = decode(POS_DISPLAY_PAYLOAD).unwrap();
    assert_eq!(
        r.lines,
        Some(vec![
            Some("Cà phê sữa đá".to_owned()),
            Some("Tổng: 45,000".to_owned())
        ])
    );
    assert_eq!(
        (s(&r.target.as_ref().unwrap().endpoint), r.columns),
        ("COM4", 20)
    );
}

#[test]
fn contract_pos_read_payloads_parse() {
    let r: PosReadRequest = decode(POS_READ_PAYLOAD).unwrap();
    assert_eq!(
        (s(&r.device), r.timeout_ms),
        (read_devices::PRINTER, read_devices::DEFAULT_TIMEOUT_MS)
    );
    assert_eq!(s(&r.target.as_ref().unwrap().endpoint), "192.168.1.16");
    let card: PosReadRequest = decode(POS_READ_CARD_PAYLOAD).unwrap();
    let t = card.target.as_ref().unwrap();
    assert_eq!(
        (
            s(&t.transport),
            s(&t.endpoint),
            s(&card.device),
            card.timeout_ms
        ),
        (transports::PCSC, "ACR122", read_devices::CARD, 10_000)
    );
}

#[test]
fn contract_pos_speak_payload_parses() {
    let r: PosSpeakRequest = decode(POS_SPEAK_PAYLOAD).unwrap();
    let t = r.target.as_ref().unwrap();
    assert_eq!(
        (
            s(&t.transport),
            s(&t.endpoint),
            s(&r.text),
            r.volume,
            r.rate
        ),
        (transports::SAPI, "default", "Đã nhận 45.000 đồng", 80, 0)
    );
}

#[test]
fn contract_pos_payload_with_unknown_member_is_device_bad_payload() {
    let err = decode_payload::<PosKickRequest>(&json(
        r#"{"target":{"transport":"tcp","endpoint":"P1","pinn":2}}"#,
    ))
    .unwrap_err();
    let refusal = AdapterError::from(err);
    assert_eq!(refusal.code, codes::BAD_PAYLOAD);
    assert_eq!(
        refusal.message,
        "The JSON property 'pinn' could not be mapped to any .NET member contained in type 'SmilePms.DeviceAgent.Commands.PosTarget'."
    );
}

const F: &str = r#""from":"2026-10-14T14:00:00","to":"2026-10-16T12:00:00""#;

/// Decodes `payload` as `T` and returns the System.Text.Json-shaped output or the error.
fn run<T: Payload + ToJson>(payload: &str) -> Result<Value, String> {
    decode::<T>(payload).map(|t| t.to_json())
}

fn check<T: Payload + ToJson>(payload: &str, expected: Result<&str, &str>) {
    let got = run::<T>(payload);
    match expected {
        Ok(out) => assert_eq!(got, Ok(json(out)), "{payload}"),
        Err(msg) => assert_eq!(got, Err(msg.to_owned()), "{payload}"),
    }
}

const KCR_BASE: &str = r#"{"room":"101","from":"2026-10-14T14:00:00","to":"2026-10-16T12:00:00","cards":1,"folioNum":null,"lockAddress":null,"vendor":null,"addGuest":null}"#;

fn kcr(room: &str, extra: &str) -> String {
    format!(
        r#"{{"room":{room},"from":"2026-10-14T14:00:00","to":"2026-10-16T12:00:00"{extra},"lockAddress":null,"vendor":null}}"#
    )
}

/// KeyCardRequest cases from the .NET 10 run.
#[test]
fn key_card_request_matches_system_text_json() {
    let conv = |path: &str, pos: usize| {
        format!(
            "The JSON value could not be converted to SmilePms.DeviceAgent.Commands.KeyCardRequest. Path: {path} | LineNumber: 0 | BytePositionInLine: {pos}."
        )
    };
    let p = |body: &str| format!("{{{body}}}");
    check::<KeyCardRequest>(&p(&format!(r#""room":"101",{F}"#)), Ok(KCR_BASE));
    check::<KeyCardRequest>(
        &p(F),
        Ok(&kcr(
            "null",
            r#","cards":1,"folioNum":null,"addGuest":null"#,
        )),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":null,{F}"#)),
        Ok(&kcr(
            "null",
            r#","cards":1,"folioNum":null,"addGuest":null"#,
        )),
    );
    check::<KeyCardRequest>(&p(&format!(r#""room":5,{F}"#)), Err(&conv("$.room", 9)));
    check::<KeyCardRequest>(&p(&format!(r#""room":true,{F}"#)), Err(&conv("$.room", 12)));
    check::<KeyCardRequest>(&p(&format!(r#""room":{{}},{F}"#)), Err(&conv("$.room", 9)));
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","cards":"2",{F}"#)),
        Ok(&kcr(
            r#""1""#,
            r#","cards":2,"folioNum":null,"addGuest":null"#,
        )),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","cards":2.0,{F}"#)),
        Err(&conv("$.cards", 23)),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","cards":"2.0",{F}"#)),
        Err(&conv("$.cards", 25)),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","cards":" 2",{F}"#)),
        Err(&conv("$.cards", 24)),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","cards":null,{F}"#)),
        Err(&conv("$.cards", 24)),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","cards":1e2,{F}"#)),
        Err(&conv("$.cards", 23)),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","cards":2147483648,{F}"#)),
        Err(&conv("$.cards", 30)),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","cards":true,{F}"#)),
        Err(&conv("$.cards", 24)),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","cards":-3,{F}"#)),
        Ok(&kcr(
            r#""1""#,
            r#","cards":-3,"folioNum":null,"addGuest":null"#,
        )),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","folioNum":"123456",{F}"#)),
        Ok(&kcr(
            r#""1""#,
            r#","cards":1,"folioNum":123456,"addGuest":null"#,
        )),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","folioNum":1.5,{F}"#)),
        Err(&conv("$.folioNum", 26)),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","folioNum":null,{F}"#)),
        Ok(&kcr(
            r#""1""#,
            r#","cards":1,"folioNum":null,"addGuest":null"#,
        )),
    );
    let date = |got: &str| format!("Expected a yyyy-MM-dd'T'HH:mm:ss date, got '{got}'");
    check::<KeyCardRequest>(
        r#"{"room":"1","from":"2026-10-14 14:00:00","to":"2026-10-16T12:00:00"}"#,
        Err(&date("2026-10-14 14:00:00")),
    );
    check::<KeyCardRequest>(
        r#"{"room":"1","from":null,"to":"2026-10-16T12:00:00"}"#,
        Err(&date("")),
    );
    check::<KeyCardRequest>(
        r#"{"room":"1","from":5,"to":"2026-10-16T12:00:00"}"#,
        Err(&conv("$.from", 20)),
    );
    check::<KeyCardRequest>(
        r#"{"room":"1","from":"2026-02-30T00:00:00","to":"2026-10-16T12:00:00"}"#,
        Err(&date("2026-02-30T00:00:00")),
    );
    check::<KeyCardRequest>(
        r#"{"room":"1"}"#,
        Ok(
            r#"{"room":"1","from":"0001-01-01T00:00:00","to":"0001-01-01T00:00:00","cards":1,"folioNum":null,"lockAddress":null,"vendor":null,"addGuest":null}"#,
        ),
    );
    check::<KeyCardRequest>(&p(&format!(r#""Room":"101",{F}"#)), Ok(KCR_BASE));
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"a","ROOM":"b",{F}"#)),
        Ok(&kcr(
            r#""b""#,
            r#","cards":1,"folioNum":null,"addGuest":null"#,
        )),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"101","x":{{"a":[1,2]}},{F}"#)),
        Err(
            "The JSON property 'x' could not be mapped to any .NET member contained in type 'SmilePms.DeviceAgent.Commands.KeyCardRequest'.",
        ),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","addGuest":"true",{F}"#)),
        Err(&conv("$.addGuest", 29)),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","addGuest":1,{F}"#)),
        Err(&conv("$.addGuest", 24)),
    );
    let addr = |pos: usize| {
        format!(
            "The JSON value could not be converted to SmilePms.DeviceAgent.Commands.LockAddress. Path: $.lockAddress | LineNumber: 0 | BytePositionInLine: {pos}."
        )
    };
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","lockAddress":5,{F}"#)),
        Err(&addr(27)),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","lockAddress":[],{F}"#)),
        Err(&addr(27)),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","lockAddress":{{}},{F}"#)),
        Ok(
            r#"{"room":"1","from":"2026-10-14T14:00:00","to":"2026-10-16T12:00:00","cards":1,"folioNum":null,"lockAddress":{"building":null,"floor":null,"room":null,"door":null,"address":null,"rmm":null,"fmm":null,"lockType":null},"vendor":null,"addGuest":null}"#,
        ),
    );
    check::<KeyCardRequest>(
        &p(&format!(
            r#""room":"1","vendor":{{"keyType":"X","mfLockName":"y","extra":1}},{F}"#
        )),
        Err(
            "The JSON property 'extra' could not be mapped to any .NET member contained in type 'SmilePms.DeviceAgent.Commands.LockVendor'.",
        ),
    );
    check::<KeyCardRequest>(
        &p(&format!(r#""room":"1","vendor":{{"keyType":5}},{F}"#)),
        Err(
            "The JSON value could not be converted to SmilePms.DeviceAgent.Commands.LockVendor. Path: $.vendor.keyType | LineNumber: 0 | BytePositionInLine: 33.",
        ),
    );
    check::<KeyCardRequest>("null", Err("payload is null"));
    check::<KeyCardRequest>("[]", Err(&conv("$", 1)));
    check::<KeyCardRequest>(r#""x""#, Err(&conv("$", 3)));
    check::<KeyCardRequest>("5", Err(&conv("$", 1)));
    check::<KeyCardRequest>(
        &p(&format!(r#""r\u006fom":"\u0041",{F}"#)),
        Ok(&kcr(
            r#""A""#,
            r#","cards":1,"folioNum":null,"addGuest":null"#,
        )),
    );
}

#[test]
fn lock_requests_match_system_text_json() {
    check::<ReadRequest>("{}", Ok(r#"{"vendor":null}"#));
    check::<ReadRequest>(r#"{"vendor":null}"#, Ok(r#"{"vendor":null}"#));
    check::<ReadRequest>(
        r#"{"vendor":{"keyType":5}}"#,
        Err(
            "The JSON value could not be converted to SmilePms.DeviceAgent.Commands.LockVendor. Path: $.vendor.keyType | LineNumber: 0 | BytePositionInLine: 22.",
        ),
    );
    check::<ReadRequest>(
        r#"{"vendor":{"keyTypee":"x"}}"#,
        Err(
            "The JSON property 'keyTypee' could not be mapped to any .NET member contained in type 'SmilePms.DeviceAgent.Commands.LockVendor'.",
        ),
    );
    check::<CancelAllRequest>(
        r#"{"room":"101"}"#,
        Ok(r#"{"room":"101","lockAddress":null,"vendor":null}"#),
    );
    check::<EraseRequest>(LOCK_ERASE_PAYLOAD.trim(), Ok(LOCK_ERASE_PAYLOAD.trim()));
}

#[test]
fn pos_requests_match_system_text_json() {
    const T: &str = r#""target":{"transport":"serial","endpoint":"COM4"}"#;
    const TO: &str =
        r#""target":{"transport":"serial","endpoint":"COM4","baudRate":null,"oposProfile":null}"#;
    check::<PosDisplayRequest>(
        &format!(r#"{{{T},"lines":["a","b"]}}"#),
        Ok(&format!(r#"{{{TO},"lines":["a","b"],"columns":20}}"#)),
    );
    check::<PosDisplayRequest>(
        &format!(r#"{{{T},"lines":["a",null]}}"#),
        Ok(&format!(r#"{{{TO},"lines":["a",null],"columns":20}}"#)),
    );
    check::<PosDisplayRequest>(
        &format!(r#"{{{T},"lines":null}}"#),
        Ok(&format!(r#"{{{TO},"lines":null,"columns":20}}"#)),
    );
    check::<PosDisplayRequest>(
        &format!(r#"{{{T}}}"#),
        Ok(&format!(r#"{{{TO},"lines":null,"columns":20}}"#)),
    );
    check::<PosDisplayRequest>(
        &format!(r#"{{{T},"lines":"x"}}"#),
        Err(
            "The JSON value could not be converted to System.Collections.Generic.IReadOnlyList`1[System.String]. Path: $.lines | LineNumber: 0 | BytePositionInLine: 62.",
        ),
    );
    check::<PosDisplayRequest>(
        &format!(r#"{{{T},"lines":["a",5]}}"#),
        Err(
            "The JSON value could not be converted to System.String. Path: $.lines[1] | LineNumber: 0 | BytePositionInLine: 65.",
        ),
    );
    check::<PosDisplayRequest>(
        &format!(r#"{{{T},"lines":[],"columns":"20"}}"#),
        Ok(&format!(r#"{{{TO},"lines":[],"columns":20}}"#)),
    );
    check::<PosKickRequest>("{}", Ok(r#"{"target":null,"pin":2,"onMs":50,"offMs":500}"#));
    check::<PosKickRequest>(
        r#"{"target":null}"#,
        Ok(r#"{"target":null,"pin":2,"onMs":50,"offMs":500}"#),
    );
    check::<PosKickRequest>(
        r#"{"target":{"transport":"tcp"}}"#,
        Ok(
            r#"{"target":{"transport":"tcp","endpoint":null,"baudRate":null,"oposProfile":null},"pin":2,"onMs":50,"offMs":500}"#,
        ),
    );
    check::<PosKickRequest>(
        r#"{"target":{"transport":"tcp","endpoint":"x","baudRate":"9600"}}"#,
        Ok(
            r#"{"target":{"transport":"tcp","endpoint":"x","baudRate":9600,"oposProfile":null},"pin":2,"onMs":50,"offMs":500}"#,
        ),
    );
    check::<PosPrintRequest>(
        r#"{"target":{"transport":"tcp","endpoint":"x"},"data":"AA==","cut":"true"}"#,
        Err(
            "The JSON value could not be converted to SmilePms.DeviceAgent.Commands.PosPrintRequest. Path: $.cut | LineNumber: 0 | BytePositionInLine: 71.",
        ),
    );
    check::<PosPrintRequest>(
        r#"{"target":{"transport":"tcp","endpoint":"x"},"data":"AA==","cut":null}"#,
        Err(
            "The JSON value could not be converted to SmilePms.DeviceAgent.Commands.PosPrintRequest. Path: $.cut | LineNumber: 0 | BytePositionInLine: 69.",
        ),
    );
    check::<PosPrintRequest>(
        r#"{"target":{"transport":"tcp","endpoint":"x"}}"#,
        Ok(
            r#"{"target":{"transport":"tcp","endpoint":"x","baudRate":null,"oposProfile":null},"data":null,"cut":false,"copies":1}"#,
        ),
    );
    check::<PosReadRequest>(
        r#"{"target":{"transport":"tcp","endpoint":"x"}}"#,
        Ok(
            r#"{"target":{"transport":"tcp","endpoint":"x","baudRate":null,"oposProfile":null},"device":"printer","timeoutMs":10000}"#,
        ),
    );
    check::<PosReadRequest>(
        r#"{"target":{"transport":"tcp","endpoint":"x"},"device":null}"#,
        Ok(
            r#"{"target":{"transport":"tcp","endpoint":"x","baudRate":null,"oposProfile":null},"device":null,"timeoutMs":10000}"#,
        ),
    );
    check::<PosSpeakRequest>(
        POS_SPEAK_PAYLOAD.trim(),
        Ok(
            r#"{"target":{"transport":"sapi","endpoint":"default","baudRate":null,"oposProfile":null},"text":"Đã nhận 45.000 đồng","volume":80,"rate":0}"#,
        ),
    );
}

const ID: &str = "6f1c2b0e-8d1a-4c55-9a51-3f7d2b9e0a11";
const TOKEN: &str = "0b8f6c1e-2c3d-4e5f-8a9b-1c2d3e4f5a6b";

fn claim(body: &str) -> Result<Option<ClaimResponse>, String> {
    ClaimResponse::from_json(&json(body)).map_err(|e| e.message().to_owned())
}

#[test]
fn claim_envelope_matches_system_text_json() {
    let ok = |id: &str, rest: &str| {
        claim(&format!(
            r#"{{"id":"{id}","kind":"k","payload":{{}},"claimToken":"{TOKEN}"{rest}}}"#
        ))
        .unwrap()
        .unwrap()
    };
    assert_eq!(
        ok("6F1C2B0E-8D1A-4C55-9A51-3F7D2B9E0A11", r#","attempt":1"#)
            .id
            .to_string(),
        ID
    );
    assert_eq!(ok(ID, r#","attempt":"1""#).attempt, 1);
    assert_eq!(
        ok(ID, r#","attempt":1,"leaseSeconds":null"#).lease_seconds,
        None
    );
    assert_eq!(ok(ID, r#","attempt":1,"new":[1]"#).attempt, 1);
    let bad_guid = |pos: usize| {
        format!(
            "The JSON value could not be converted to SmilePms.DeviceAgent.Queue.ClaimResponse. Path: $.id | LineNumber: 0 | BytePositionInLine: {pos}."
        )
    };
    let with_id = |id: &str| {
        claim(&format!(
            r#"{{"id":{id},"kind":"k","payload":{{}},"claimToken":"{TOKEN}","attempt":1}}"#
        ))
    };
    assert_eq!(with_id(&format!(r#""{{{ID}}}""#)), Err(bad_guid(46)));
    assert_eq!(
        with_id(r#""6f1c2b0e8d1a4c559a513f7d2b9e0a11""#),
        Err(bad_guid(40))
    );
    assert_eq!(with_id("null"), Err(bad_guid(10)));
    assert_eq!(with_id(r#""zz""#), Err(bad_guid(10)));

    let no_payload = claim(&format!(
        r#"{{"id":"{ID}","kind":"k","claimToken":"{TOKEN}","attempt":1}}"#
    ))
    .unwrap()
    .unwrap();
    assert_eq!(no_payload.payload, None);
    assert!(
        no_payload.to_command(OffsetDateTime::now_utc()).is_err(),
        "default(JsonElement).Clone() throws"
    );
    let null_payload = claim(&format!(
        r#"{{"id":"{ID}","kind":"k","payload":null,"claimToken":"{TOKEN}","attempt":1}}"#
    ))
    .unwrap()
    .unwrap();
    assert_eq!(null_payload.payload, Some(Value::Null));
    let no_id = claim(&format!(
        r#"{{"kind":"k","payload":{{}},"claimToken":"{TOKEN}","attempt":1}}"#
    ))
    .unwrap()
    .unwrap();
    assert_eq!(no_id.id, Uuid::NIL);
    let no_kind = claim(&format!(
        r#"{{"id":"{ID}","payload":{{}},"claimToken":"{TOKEN}","attempt":1}}"#
    ))
    .unwrap()
    .unwrap();
    assert_eq!(no_kind.kind, None);
    assert_eq!(claim("null"), Ok(None));
    assert_eq!(
        claim("[]"),
        Err("The JSON value could not be converted to SmilePms.DeviceAgent.Queue.ClaimResponse. Path: $ | LineNumber: 0 | BytePositionInLine: 1.".to_owned())
    );
}

#[test]
fn result_and_pending_bodies_serialize_like_system_text_json() {
    let done = CommandOutcome::done(json(r#"{"a":1}"#));
    let token = Uuid::parse(TOKEN).unwrap();
    assert_eq!(
        ResultRequest::from_outcome(token, &done).to_json(),
        json(&format!(
            r#"{{"claimToken":"{TOKEN}","outcome":"done","result":{{"a":1}},"error":null}}"#
        ))
    );

    let claimed_at = OffsetDateTime::parse_rfc3339("2026-10-14T06:59:00.123Z").unwrap();
    let cmd = ClaimResponse::from_json(&json(CLAIM_RESPONSE))
        .unwrap()
        .unwrap()
        .to_command(claimed_at)
        .unwrap();
    let pending = PendingResult::from_command(&cmd, &CommandOutcome::failed("X", "m"));
    let text = dl_json::to_string(&pending.to_json());
    assert_eq!(
        text,
        format!(
            r#"{{"commandId":"{ID}","kind":"lock.issue","leaseUntil":"2026-10-14T06:59:55.123+00:00","body":{{"claimToken":"{TOKEN}","outcome":"failed","result":null,"error":{{"code":"X","message":"m"}}}}}}"#
        )
    );
    assert_eq!(PendingResult::from_json(&json(&text)).unwrap(), pending);

    let local = OffsetDateTime::parse_rfc3339("2026-10-14T14:00:55.1234567+07:00").unwrap();
    assert_eq!(
        format_date_time_offset(local),
        "2026-10-14T14:00:55.1234567+07:00"
    );
    let read = PendingResult::from_json(&json(&format!(
        r#"{{"commandId":"{ID}","kind":"k","leaseUntil":"2026-10-14T07:00:55+07:00","body":{{"claimToken":"{TOKEN}","outcome":"done"}}}}"#
    )))
    .unwrap();
    assert_eq!(
        (read.body.result.clone(), read.body.error.clone()),
        (None, None)
    );
    assert_eq!(
        dl_json::to_string(&read.body.to_json()),
        format!(r#"{{"claimToken":"{TOKEN}","outcome":"done","result":null,"error":null}}"#)
    );
}
