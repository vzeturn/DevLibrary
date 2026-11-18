//! Port of Pos/PosCardSpeakerTests.cs: POS-HW-04 card readers (COM, PC/SC NFC) and POS-HW-06
//! payment speaker, through the router.

#[path = "pos_fakes/mod.rs"]
mod fakes;

use std::io::ErrorKind;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_agent_pms_device::adapters::pos::{
    AcrNfcAdapter, CardIdentity, OposAdapter, PcscCardSource, PosDeviceRouter,
    SerialCardReaderAdapter, SpeakerAdapter, SpeechEngine,
};
use dl_agent_pms_device::adapters::{AdapterError, Cx, PosDevice};
use dl_agent_pms_device::contracts::{
    PosReadRequest, PosSpeakRequest, PosTarget, codes, read_devices,
};
use fakes::{DrainedRead, PosPcscFake, PosSerialPortFake, PosSpeechFake, PosTransportFake, target};

struct T {
    serial: Arc<PosTransportFake>,
    port: PosSerialPortFake,
    pcsc: Arc<PosPcscFake>,
    speech: Arc<PosSpeechFake>,
}

impl T {
    fn new() -> T {
        T {
            serial: PosTransportFake::new("serial"),
            port: PosSerialPortFake::new(),
            pcsc: Arc::new(PosPcscFake::default()),
            speech: Arc::new(PosSpeechFake::default()),
        }
    }

    fn router(&self, no_platform: bool) -> PosDeviceRouter {
        let pcsc: Option<Arc<dyn PcscCardSource>> =
            (!no_platform).then(|| self.pcsc.clone() as Arc<dyn PcscCardSource>);
        let speech: Option<Arc<dyn SpeechEngine>> =
            (!no_platform).then(|| self.speech.clone() as Arc<dyn SpeechEngine>);
        PosDeviceRouter::new(
            PosTransportFake::new("tcp"),
            self.serial.clone(),
            PosTransportFake::new("spooler"),
        )
        .with_serial_reader(SerialCardReaderAdapter::new(Arc::new(self.port.clone())))
        .with_nfc(AcrNfcAdapter::new(pcsc))
        .with_speaker(SpeakerAdapter::new(speech))
    }

    fn read(
        &self,
        transport: &str,
        endpoint: &str,
        timeout_ms: i32,
    ) -> Result<dl_json::Value, AdapterError> {
        self.router(false)
            .read(&card(transport, endpoint, timeout_ms), &Cx::unbounded())
    }
}

fn card(transport: &str, endpoint: &str, timeout_ms: i32) -> PosReadRequest {
    PosReadRequest {
        target: Some(target(transport, endpoint)),
        device: Some(read_devices::CARD.to_owned()),
        timeout_ms,
    }
}

fn speak(transport: &str, text: &str, volume: i32) -> PosSpeakRequest {
    PosSpeakRequest {
        target: Some(target(transport, "default")),
        text: Some(text.to_owned()),
        volume,
        ..PosSpeakRequest::default()
    }
}

#[test]
fn hw04_serial_reader_frame_is_answered_masked_and_hashed_never_raw() {
    let t = T::new();
    let mut reply = vec![0x02];
    reply.extend_from_slice(b"6280001234567890");
    reply.extend_from_slice(&[0x03, 0x0D, 0x0A]);
    t.port.set(|s| s.reply = reply);
    let result = t.read("serial", "COM5", 2000).unwrap();

    assert_eq!(result["masked"], "************7890");
    assert_eq!(
        result["hash"],
        CardIdentity::hash("6280001234567890").as_str()
    );
    assert_eq!(result["length"], 16);
    assert_eq!(result["source"], "serial");
    assert!(!dl_json::to_string(&result).contains("6280001234567890"));
    assert_eq!(t.port.opened(), [("COM5".to_owned(), 9600)]);
    assert!(t.port.closed());
    assert!(t.serial.sent().is_empty());
}

#[test]
fn hw04_serial_reader_without_a_card_in_time_is_device_no_card() {
    let t = T::new();
    t.port.set(|s| s.hang_when_drained = true);
    let e = t.read("serial", "COM5", 500).unwrap_err();
    assert_eq!(e.code, codes::NO_CARD);
    assert_eq!(e.message, "no card within 0.5 s");
}

#[test]
fn hw04_serial_reader_keeps_waiting_through_port_timeouts_until_the_request_deadline() {
    for drained in [DrainedRead::Zero, DrainedRead::Timeout] {
        let t = T::new();
        t.port.set(|s| s.drained = drained);
        let clock = Instant::now();
        let e = t.read("serial", "COM5", 600).unwrap_err();

        assert_eq!(e.code, codes::NO_CARD);
        let ms = clock.elapsed().as_millis();
        assert!(ms >= 550, "gave up after {ms} ms");
        assert!(
            t.port.drained_reads() > 1,
            "a quiet port read is retried, not taken as the end"
        );
    }
}

#[test]
fn hw04_serial_reader_answers_what_came_before_the_line_went_quiet() {
    let t = T::new();
    t.port.set(|s| s.reply = b"\r\n12345678".to_vec());
    let result = t.read("serial", "COM5", 2000).unwrap();
    assert_eq!(result["masked"], "****5678");
    assert_eq!(result["length"], 8);
}

#[test]
fn hw04_serial_reader_frame_longer_than_a_card_is_a_device_error() {
    let t = T::new();
    t.port.set(|s| s.reply = vec![b'1'; 80]);
    let e = t.read("serial", "COM5", 2000).unwrap_err();
    assert_eq!(e.code, codes::ERROR);
    assert_eq!(
        e.message,
        "the reader sent 65 characters, more than a card holds (64)"
    );
}

#[test]
fn hw04_busy_serial_reader_port_is_device_unavailable() {
    let t = T::new();
    t.port.set(|s| {
        s.open_fails = Some((
            ErrorKind::PermissionDenied,
            "Access to the port 'COM5' is denied.".into(),
        ));
    });
    let e = t.read("serial", "COM5", 2000).unwrap_err();
    assert_eq!(e.code, codes::UNAVAILABLE);
}

#[test]
fn hw04_nfc_reader_answers_the_uid_hex_masked_and_hashed() {
    let t = T::new();
    let result = t.read("pcsc", "ACR122", 2000).unwrap();

    assert_eq!(result["masked"], "****3B1C");
    assert_eq!(result["hash"], CardIdentity::hash("04A23B1C").as_str());
    assert_eq!(result["source"], "pcsc");
    assert_eq!(
        t.pcsc.reads.lock().unwrap().clone(),
        [("ACR122".to_owned(), Duration::from_secs(2))]
    );
}

#[test]
fn hw04_nfc_refusals_pass_through_and_the_message_carries_no_uid() {
    let t = T::new();
    *t.pcsc.fail.lock().unwrap() = Some(AdapterError::new(
        codes::NO_CARD,
        "no card on the reader within 2 s",
    ));
    let e = t.read("pcsc", "ACR122", 2000).unwrap_err();
    assert_eq!(e.code, codes::NO_CARD);
}

#[test]
fn hw04_nfc_empty_uid_is_device_no_card() {
    let t = T::new();
    t.pcsc.uid.lock().unwrap().clear();
    let e = t.read("pcsc", "ACR122", 2000).unwrap_err();
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        (codes::NO_CARD, "the reader answered an empty UID")
    );
}

#[test]
fn hw04_nfc_reader_off_windows_refuses_platform_unsupported() {
    let t = T::new();
    let e = t
        .router(true)
        .read(&card("pcsc", "ACR122", 2000), &Cx::unbounded())
        .unwrap_err();
    assert_eq!(e.code, codes::PLATFORM_UNSUPPORTED);
    assert_eq!(
        e.message,
        "the PC/SC (NFC) reader runs only on a Windows agent"
    );
}

#[test]
fn hw04_card_reads_over_a_printer_link_or_with_a_bad_timeout_are_refused() {
    for (transport, timeout_ms) in [
        ("tcp", 2000),
        ("bluetooth", 2000),
        ("serial", 100),
        ("serial", 30000),
    ] {
        let t = T::new();
        let e = t.read(transport, "X", timeout_ms).unwrap_err();
        assert!(
            [codes::TRANSPORT_UNSUPPORTED, codes::BAD_PAYLOAD].contains(&e.code.as_str()),
            "{transport} {timeout_ms}: {e}"
        );
        assert!(t.port.opened().is_empty());
    }
    let e = T::new().read("serial", "X", 100).unwrap_err();
    assert_eq!(e.message, "timeoutMs must be 500-25000, got 100");
}

#[test]
fn hw04_unknown_read_device_is_a_bad_payload() {
    let request = PosReadRequest {
        target: Some(target("tcp", "P1")),
        device: Some("scale".to_owned()),
        ..PosReadRequest::default()
    };
    let e = T::new()
        .router(false)
        .read(&request, &Cx::unbounded())
        .unwrap_err();
    assert_eq!(e.code, codes::BAD_PAYLOAD);
    assert_eq!(
        e.message,
        "read device must be printer or card, got 'scale'"
    );
}

#[test]
fn hw04_mask_keeps_only_the_last_four() {
    assert_eq!(CardIdentity::mask("1234"), "****");
    assert_eq!(CardIdentity::mask("12"), "**");
    assert_eq!(CardIdentity::mask("01234"), "*1234");
}

#[test]
fn hw06_speak_says_the_text_through_sapi() {
    let t = T::new();
    let result = t
        .router(false)
        .speak(&speak("sapi", "Đã nhận 45.000 đồng", 80), &Cx::unbounded())
        .unwrap();

    assert_eq!(
        t.speech.spoken(),
        [(
            "default".to_owned(),
            "Đã nhận 45.000 đồng".to_owned(),
            80,
            0
        )]
    );
    assert_eq!(result["spoken"], true);
    assert_eq!(result["characters"], 19);
}

#[test]
fn hw06_speak_without_sapi_is_device_unavailable() {
    let e = T::new()
        .router(true)
        .speak(&speak("sapi", "xin chao", 100), &Cx::unbounded())
        .unwrap_err();
    assert_eq!(e.code, codes::UNAVAILABLE);
    assert_eq!(
        e.message,
        "sapi device default unavailable: Windows speech (SAPI) is not available on this agent"
    );
}

#[test]
fn hw06_bad_speak_requests_are_refused_before_anything_is_said() {
    for (transport, text, volume) in [("tcp", "x", 100), ("sapi", "", 100), ("sapi", "x", 101)] {
        let t = T::new();
        assert!(
            t.router(false)
                .speak(&speak(transport, text, volume), &Cx::unbounded())
                .is_err()
        );
        assert!(t.speech.spoken().is_empty());
    }
    let long = "a".repeat(201);
    let e = T::new()
        .router(false)
        .speak(&speak("sapi", &long, 100), &Cx::unbounded())
        .unwrap_err();
    assert_eq!(e.message, "speak text must be 1-200 characters");
    let e = T::new()
        .router(false)
        .speak(
            &PosSpeakRequest {
                rate: 11,
                ..speak("sapi", "x", 100)
            },
            &Cx::unbounded(),
        )
        .unwrap_err();
    assert_eq!(e.message, "rate must be -10..10, got 11");
}

#[test]
fn hw06_opos_and_bluetooth_refuse_speak() {
    let request = PosSpeakRequest {
        target: Some(PosTarget {
            opos_profile: Some("1.8".to_owned()),
            ..target("opos", "Tone1")
        }),
        text: Some("x".to_owned()),
        ..PosSpeakRequest::default()
    };
    let e = OposAdapter::refusing()
        .speak(&request, &Cx::unbounded())
        .unwrap_err();
    assert_eq!(e.code, codes::VENDOR_SDK_MISSING);
}
