//! `PosDeviceRouter.cs`: the single composition point for POS hardware.

use std::sync::Arc;

use dl_json::{ToJson, Value, json};

use super::base64::decode_dotnet;
use super::escpos::EscPos;
use super::nfc::AcrNfcAdapter;
use super::opos::{BluetoothRefusingAdapter, OposAdapter};
use super::serial::{
    EscPosSerialAdapter, SerialCardReaderAdapter, SerialPortOpener, SystemSerialPortOpener,
};
use super::speaker::SpeakerAdapter;
use super::spooler::WindowsSpoolerAdapter;
use super::tcp::EscPosTcpAdapter;
use super::{EscPosTransport, failures, target_of, text};
use crate::adapters::{Cx, PosDevice, adapter_failed};
use crate::contracts::{
    AdapterError, PosDisplayRequest, PosKickRequest, PosPrintRequest, PosReadRequest,
    PosSpeakRequest, PosTarget, read_devices, transports,
};

/// Routes each pos.* command to the adapter named by `target.transport`: tcp / serial /
/// spooler are ESC/POS byte links; opos goes to [`OposAdapter`], bluetooth to
/// [`BluetoothRefusingAdapter`]; a card read goes to the COM reader (serial) or the NFC
/// reader (pcsc); pos.speak goes to the SAPI speaker.
pub struct PosDeviceRouter {
    printers: Vec<Arc<dyn EscPosTransport>>,
    serial: Arc<dyn EscPosTransport>,
    opos: OposAdapter,
    bluetooth: BluetoothRefusingAdapter,
    serial_reader: Option<SerialCardReaderAdapter>,
    nfc: Option<AcrNfcAdapter>,
    speaker: Option<SpeakerAdapter>,
}

impl PosDeviceRouter {
    /// Most copies one pos.print may ask for.
    pub const MAX_COPIES: i32 = 5;

    /// A router over three byte links (looked up by their [`EscPosTransport::transport`]
    /// name), without card readers or speaker, and with the refusing OPOS adapter (the C#
    /// constructor with its optional arguments left out).
    pub fn new(
        tcp: Arc<dyn EscPosTransport>,
        serial: Arc<dyn EscPosTransport>,
        spooler: Arc<dyn EscPosTransport>,
    ) -> PosDeviceRouter {
        PosDeviceRouter {
            printers: vec![tcp, serial.clone(), spooler],
            serial,
            opos: OposAdapter::refusing(),
            bluetooth: BluetoothRefusingAdapter,
            serial_reader: None,
            nfc: None,
            speaker: None,
        }
    }

    /// With the COM card reader.
    pub fn with_serial_reader(mut self, reader: SerialCardReaderAdapter) -> PosDeviceRouter {
        self.serial_reader = Some(reader);
        self
    }

    /// With the PC/SC NFC reader.
    pub fn with_nfc(mut self, nfc: AcrNfcAdapter) -> PosDeviceRouter {
        self.nfc = Some(nfc);
        self
    }

    /// With the payment speaker.
    pub fn with_speaker(mut self, speaker: SpeakerAdapter) -> PosDeviceRouter {
        self.speaker = Some(speaker);
        self
    }

    /// With another OPOS adapter (the default refuses every verb, like C#).
    pub fn with_opos(mut self, opos: OposAdapter) -> PosDeviceRouter {
        self.opos = opos;
        self
    }

    /// `Create()`: the real transports of this host (the spooler only on Windows), the COM
    /// card reader, the NFC reader, the SAPI speaker and the OPOS control objects.
    pub fn create() -> PosDeviceRouter {
        let ports: Arc<dyn SerialPortOpener> = Arc::new(SystemSerialPortOpener);
        PosDeviceRouter::new(
            Arc::new(EscPosTcpAdapter),
            Arc::new(EscPosSerialAdapter::new(ports.clone())),
            Arc::new(WindowsSpoolerAdapter::create()),
        )
        .with_serial_reader(SerialCardReaderAdapter::new(ports))
        .with_nfc(AcrNfcAdapter::create())
        .with_speaker(SpeakerAdapter::create())
        .with_opos(OposAdapter::create())
    }

    /// The bytes sent for a print: the data once per copy, each followed by a cut when asked.
    pub fn print_job(request: &PosPrintRequest) -> Result<Vec<u8>, AdapterError> {
        let copies = request.copies;
        if !(1..=Self::MAX_COPIES).contains(&copies) {
            return Err(failures::bad_payload(format!(
                "copies must be 1-{}, got {copies}",
                Self::MAX_COPIES
            )));
        }
        let data = request
            .data
            .as_deref()
            .ok_or_else(|| failures::argument_null("s"))?;
        let data =
            decode_dotnet(data).ok_or_else(|| failures::bad_payload("print data is not base64"))?;
        if data.is_empty() {
            return Err(failures::bad_payload("print data is empty"));
        }
        let mut one = data;
        if request.cut {
            one.extend_from_slice(&EscPos::CUT);
        }
        // 1..=5 copies, checked above.
        Ok(one.repeat(copies as usize))
    }

    fn printer(&self, transport: Option<&str>) -> Option<&Arc<dyn EscPosTransport>> {
        let transport = transport?;
        self.printers.iter().find(|p| p.transport() == transport)
    }

    /// `Route`: opos and bluetooth go to their adapters; a byte-link transport runs
    /// `byte_link` with it; anything else is a bad payload. A `null` target or transport
    /// fails like C# (NullReferenceException, ArgumentNullException).
    fn route(
        &self,
        target: &Option<PosTarget>,
        device: impl FnOnce(&dyn PosDevice) -> Result<Value, AdapterError>,
        byte_link: impl FnOnce(&PosTarget, &Arc<dyn EscPosTransport>) -> Result<Value, AdapterError>,
    ) -> Result<Value, AdapterError> {
        let target = target_of(target)?;
        match target.transport.as_deref() {
            Some(transports::OPOS) => device(&self.opos),
            Some(transports::BLUETOOTH) => device(&self.bluetooth),
            None => Err(failures::argument_null("key")),
            Some(t) => match self.printer(Some(t)) {
                Some(link) => byte_link(target, link),
                None => Err(unknown_transport(target)),
            },
        }
    }

    /// HW-04: a card on the COM reader or the NFC reader; masked + hash only.
    fn read_card(&self, request: &PosReadRequest, cx: &Cx) -> Result<Value, AdapterError> {
        let timeout = request.timeout_ms;
        if !(read_devices::MIN_TIMEOUT_MS..=read_devices::MAX_TIMEOUT_MS).contains(&timeout) {
            return Err(failures::bad_payload(format!(
                "timeoutMs must be {}-{}, got {timeout}",
                read_devices::MIN_TIMEOUT_MS,
                read_devices::MAX_TIMEOUT_MS
            )));
        }
        let target = target_of(&request.target)?;
        match target.transport.as_deref() {
            Some(transports::SERIAL) => match &self.serial_reader {
                Some(reader) => reader.read(request, cx),
                None => Err(failures::not_set_up("the serial card reader")),
            },
            Some(transports::PCSC) => match &self.nfc {
                Some(nfc) => nfc.read(request, cx),
                None => Err(failures::not_set_up("the NFC reader")),
            },
            Some(transports::OPOS) => self.opos.read(request, cx),
            other => Err(failures::unsupported(
                other.unwrap_or(""),
                "a card read",
                "a card reader is reached over serial (COM) or pcsc (NFC); a keyboard-wedge reader is read by the browser",
            )),
        }
    }

    fn read_status(&self, request: &PosReadRequest, cx: &Cx) -> Result<Value, AdapterError> {
        self.route(
            &request.target,
            |d| d.read(request, cx),
            |target, printer| {
                let status = printer.query(target, &EscPos::PRINTER_STATUS_QUERY, 1, cx)?;
                let paper = printer.query(target, &EscPos::PAPER_STATUS_QUERY, 1, cx)?;
                let (Some(&status), Some(&paper)) = (status.first(), paper.first()) else {
                    return Err(adapter_failed(
                        "IndexOutOfRangeException",
                        "Index was outside the bounds of the array.",
                    ));
                };
                Ok(EscPos::parse_status(status, paper)?.to_json())
            },
        )
    }
}

fn unknown_transport(target: &PosTarget) -> AdapterError {
    failures::bad_payload(format!(
        "unknown POS transport '{}' for this verb; expected tcp, serial, spooler, bluetooth or opos",
        text(&target.transport)
    ))
}

impl PosDevice for PosDeviceRouter {
    fn print(&self, request: &PosPrintRequest, cx: &Cx) -> Result<Value, AdapterError> {
        self.route(
            &request.target,
            |d| d.print(request, cx),
            |target, printer| {
                let job = Self::print_job(request)?;
                printer.send(target, &job, cx)?;
                Ok(json!({
                    "transport": target.transport,
                    "bytes": job.len(),
                    "copies": request.copies,
                }))
            },
        )
    }

    fn kick(&self, request: &PosKickRequest, cx: &Cx) -> Result<Value, AdapterError> {
        self.route(
            &request.target,
            |d| d.kick(request, cx),
            |target, printer| {
                // PrinterKickDrawerAdapter: ESC p m t1 t2 over the printer's own transport.
                let pulse = EscPos::kick(request.pin, request.on_ms, request.off_ms)?;
                printer.send(target, &pulse, cx)?;
                Ok(json!({"kicked": true, "pin": request.pin}))
            },
        )
    }

    fn display(&self, request: &PosDisplayRequest, cx: &Cx) -> Result<Value, AdapterError> {
        self.route(
            &request.target,
            |d| d.display(request, cx),
            |target, _| {
                if target.transport.as_deref() != Some(transports::SERIAL) {
                    return Err(failures::unsupported(
                        text(&target.transport),
                        "pos.display",
                        "a pole display is reached over serial or OPOS",
                    ));
                }
                // SerialPoleAdapter: the lines ASCII-folded and fitted to the display width.
                let bytes = EscPos::display(request.lines.as_deref(), request.columns)?;
                self.serial.send(target, &bytes, cx)?;
                Ok(json!({"lines": shown_lines(request)}))
            },
        )
    }

    fn read(&self, request: &PosReadRequest, cx: &Cx) -> Result<Value, AdapterError> {
        match request.device.as_deref() {
            Some(read_devices::CARD) => self.read_card(request, cx),
            Some(read_devices::PRINTER) => self.read_status(request, cx),
            other => Err(failures::bad_payload(format!(
                "read device must be printer or card, got '{}'",
                other.unwrap_or("")
            ))),
        }
    }

    fn speak(&self, request: &PosSpeakRequest, cx: &Cx) -> Result<Value, AdapterError> {
        let target = target_of(&request.target)?;
        if target.transport.as_deref() != Some(transports::SAPI) {
            return Err(failures::unsupported(
                text(&target.transport),
                "pos.speak",
                "the payment speaker is a sapi device",
            ));
        }
        match &self.speaker {
            Some(speaker) => speaker.speak(request, cx),
            None => Err(failures::not_set_up("the payment speaker")),
        }
    }
}

/// The lines a pole display shows (`FitLine(l, columns).TrimEnd()`), after
/// [`EscPos::display`] validated them.
pub(super) fn shown_lines(request: &PosDisplayRequest) -> Vec<String> {
    request
        .lines
        .iter()
        .flatten()
        .map(|l| {
            EscPos::fit_line(l.as_deref().unwrap_or(""), request.columns)
                .trim_end()
                .to_owned()
        })
        .collect()
}
