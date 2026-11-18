//! Port of the adapter runs and result fixtures of Pos/PosContractTests.cs (the payload
//! parsing half is in tests/contract.rs): each fixture runs end to end, queue ->
//! CommandProcessor -> router -> fake transport.

mod support;

#[path = "pos_fakes/mod.rs"]
mod fakes;

use std::sync::Arc;

use dl_agent_pms_device::adapters::pos::{AcrNfcAdapter, EscPos, PosDeviceRouter, SpeakerAdapter};
use dl_agent_pms_device::adapters::{Cx, PosDevice};
use dl_agent_pms_device::agent::log::NullLogger;
use dl_agent_pms_device::agent::{CommandProcessor, StopToken, SystemClock};
use dl_agent_pms_device::config::AgentOptions;
use dl_agent_pms_device::contracts::{PosReadRequest, codes, decode_payload, kinds, parse_json};
use dl_json::Value;
use fakes::{PosDeviceFake, PosPcscFake, PosSpeechFake, PosTransportFake, lines};
use support::{FakeAdapter, FakeQueue, Row, TempOutbox};

const POS_PRINT_PAYLOAD: &str = include_str!("contract/pos.print.payload.json");
const POS_KICK_PAYLOAD: &str = include_str!("contract/pos.kick.payload.json");
const POS_DISPLAY_PAYLOAD: &str = include_str!("contract/pos.display.payload.json");
const POS_READ_PAYLOAD: &str = include_str!("contract/pos.read.payload.json");
const POS_READ_RESULT: &str = include_str!("contract/pos.read.result.json");
const POS_READ_CARD_PAYLOAD: &str = include_str!("contract/pos.read-card.payload.json");
const POS_READ_CARD_RESULT: &str = include_str!("contract/pos.read-card.result.json");
const POS_SPEAK_PAYLOAD: &str = include_str!("contract/pos.speak.payload.json");

fn json(s: &str) -> Value {
    parse_json(s.as_bytes()).unwrap()
}

struct T {
    queue: Arc<FakeQueue>,
    outbox: TempOutbox,
    tcp: Arc<PosTransportFake>,
    serial: Arc<PosTransportFake>,
    spooler: Arc<PosTransportFake>,
}

impl T {
    fn new() -> T {
        T {
            queue: FakeQueue::new(Arc::new(SystemClock)),
            outbox: TempOutbox::new(),
            tcp: PosTransportFake::new("tcp"),
            serial: PosTransportFake::new("serial"),
            spooler: PosTransportFake::new("spooler"),
        }
    }

    fn router(&self) -> PosDeviceRouter {
        PosDeviceRouter::new(self.tcp.clone(), self.serial.clone(), self.spooler.clone())
    }

    fn run(&self, kind: &str, payload: &str, pos: Option<Arc<dyn PosDevice>>) -> Row {
        let id = self.queue.enqueue(kind, payload);
        let reporter = self.outbox.reporter(
            self.queue.clone(),
            Arc::new(SystemClock),
            Arc::new(NullLogger),
        );
        let processor = CommandProcessor::new(
            self.queue.clone(),
            FakeAdapter::new(),
            reporter,
            &AgentOptions::default(),
            Arc::new(NullLogger),
            pos,
        );
        assert!(processor.run_once(&StopToken::new()).unwrap());
        self.queue.read(id)
    }
}

#[test]
fn contract_pos_print_payload_prints_on_the_tcp_printer() {
    let t = T::new();
    let row = t.run(
        kinds::POS_PRINT,
        POS_PRINT_PAYLOAD,
        Some(Arc::new(t.router())),
    );

    assert_eq!(row.status, "done");
    let sent = t.tcp.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0.endpoint.as_deref(), Some("192.168.1.16:9100"));
    let mut expected = vec![0x1B, 0x40];
    expected.extend_from_slice(b"Hello\n");
    expected.extend_from_slice(&EscPos::CUT);
    assert_eq!(sent[0].1, expected);
    assert_eq!(
        row.result.unwrap(),
        json(r#"{"transport":"tcp","bytes":12,"copies":1}"#)
    );
}

#[test]
fn contract_pos_kick_payload_kicks_through_the_serial_printer() {
    let t = T::new();
    let row = t.run(
        kinds::POS_KICK,
        POS_KICK_PAYLOAD,
        Some(Arc::new(t.router())),
    );

    assert_eq!(row.status, "done");
    let sent = t.serial.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        (sent[0].0.endpoint.as_deref(), sent[0].0.baud_rate),
        (Some("COM3"), Some(9600))
    );
    assert_eq!(sent[0].1, [0x1B, 0x70, 0x00, 25, 250]);
    assert_eq!(row.result.unwrap(), json(r#"{"kicked":true,"pin":2}"#));
}

#[test]
fn contract_pos_display_payload_shows_on_the_pole() {
    let t = T::new();
    let row = t.run(
        kinds::POS_DISPLAY,
        POS_DISPLAY_PAYLOAD,
        Some(Arc::new(t.router())),
    );

    assert_eq!(row.status, "done");
    let l = lines(&["Cà phê sữa đá", "Tổng: 45,000"]);
    assert_eq!(
        t.serial.sent_bytes(),
        [EscPos::display(Some(&l), 20).unwrap()]
    );
    assert_eq!(
        row.result.unwrap(),
        json(r#"{"lines":["Ca phe sua da","Tong: 45,000"]}"#)
    );
}

#[test]
fn contract_pos_read_payload_answers_the_result_fixture() {
    let t = T::new();
    t.tcp.reply(&[0x12]);
    t.tcp.reply(&[0x12]);
    let request: PosReadRequest = decode_payload(&json(POS_READ_PAYLOAD)).unwrap();
    let result = t.router().read(&request, &Cx::unbounded()).unwrap();

    assert_eq!(result, json(POS_READ_RESULT));
    assert_eq!(dl_json::to_string(&result), POS_READ_RESULT.trim());
}

#[test]
fn contract_pos_read_card_payload_answers_the_masked_result_fixture() {
    let t = T::new();
    let router = t
        .router()
        .with_nfc(AcrNfcAdapter::new(Some(Arc::new(PosPcscFake::default()))));
    let row = t.run(
        kinds::POS_READ,
        POS_READ_CARD_PAYLOAD,
        Some(Arc::new(router)),
    );

    assert_eq!(row.status, "done");
    let result = row.result.unwrap();
    assert_eq!(result, json(POS_READ_CARD_RESULT));
    assert_eq!(dl_json::to_string(&result), POS_READ_CARD_RESULT.trim());
}

#[test]
fn contract_pos_speak_payload_is_said_by_the_speaker() {
    let t = T::new();
    let speech = Arc::new(PosSpeechFake::default());
    let router = t
        .router()
        .with_speaker(SpeakerAdapter::new(Some(speech.clone())));
    let row = t.run(kinds::POS_SPEAK, POS_SPEAK_PAYLOAD, Some(Arc::new(router)));

    assert_eq!(row.status, "done");
    assert_eq!(
        speech.spoken(),
        [(
            "default".to_owned(),
            "Đã nhận 45.000 đồng".to_owned(),
            80,
            0
        )]
    );
    assert_eq!(
        row.result.unwrap(),
        json(r#"{"spoken":true,"characters":19}"#)
    );
}

#[test]
fn contract_pos_payload_with_unknown_member_is_device_bad_payload() {
    let t = T::new();
    let row = t.run(
        kinds::POS_KICK,
        r#"{"target":{"transport":"tcp","endpoint":"P1","pinn":2}}"#,
        Some(Arc::new(t.router())),
    );

    assert_eq!(row.error.unwrap().code, codes::BAD_PAYLOAD);
    assert!(t.tcp.sent().is_empty());
}

#[test]
fn processor_dispatches_each_pos_kind_to_its_verb() {
    for (kind, fixture, request) in [
        (kinds::POS_PRINT, POS_PRINT_PAYLOAD, "PosPrintRequest"),
        (kinds::POS_KICK, POS_KICK_PAYLOAD, "PosKickRequest"),
        (kinds::POS_DISPLAY, POS_DISPLAY_PAYLOAD, "PosDisplayRequest"),
        (kinds::POS_READ, POS_READ_PAYLOAD, "PosReadRequest"),
        (kinds::POS_READ, POS_READ_CARD_PAYLOAD, "PosReadRequest"),
        (kinds::POS_SPEAK, POS_SPEAK_PAYLOAD, "PosSpeakRequest"),
    ] {
        let t = T::new();
        let pos = Arc::new(PosDeviceFake::default());
        let row = t.run(kind, fixture, Some(pos.clone()));

        assert_eq!(row.status, "done", "{kind}");
        assert_eq!(pos.calls(), [request]);
    }
}

#[test]
fn processor_without_a_pos_device_answers_device_not_connected() {
    let t = T::new();
    let row = t.run(kinds::POS_KICK, POS_KICK_PAYLOAD, None);

    assert_eq!(row.error.unwrap().code, codes::NOT_CONNECTED);
}

#[test]
fn processor_reports_the_opos_refusal_as_the_command_error() {
    let t = T::new();
    let row = t.run(
        kinds::POS_KICK,
        r#"{"target":{"transport":"opos","endpoint":"CashDrawer1","oposProfile":"wincor"}}"#,
        Some(Arc::new(t.router())),
    );

    assert_eq!(row.status, "failed");
    assert_eq!(row.error.unwrap().code, codes::VENDOR_SDK_MISSING);
}

#[test]
fn processor_reports_a_missing_target_like_the_csharp_agent() {
    let t = T::new();
    let row = t.run(kinds::POS_KICK, "{}", Some(Arc::new(t.router())));

    assert_eq!(row.status, "failed");
    let e = row.error.unwrap();
    assert_eq!(e.code, codes::ADAPTER_FAILED);
    assert_eq!(
        e.message,
        "NullReferenceException: Object reference not set to an instance of an object."
    );
}
