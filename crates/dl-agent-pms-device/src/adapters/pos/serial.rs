//! `EscPosSerialAdapter.cs` and `SerialCardReaderAdapter.cs`: ESC/POS printers and pole
//! displays on a COM port, and card / RFID / magstripe readers on a COM port.

use std::io::{self, ErrorKind};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_json::Value;

use super::card::CardIdentity;
use super::stream::{ByteLink, EscPosStream, LinkError};
use super::{EscPosTransport, failures, os_message, seconds_0_1, target_of, trimmed_endpoint};
use crate::adapters::Cx;
use crate::contracts::{AdapterError, PosReadRequest, PosTarget, codes, transports};

/// Opens a serial port (COM3 on Windows, /dev/tty* elsewhere). Faked in tests.
pub trait SerialPortOpener: Send + Sync {
    /// The open line; dropping it closes the port. The error message is what the C# agent
    /// reports (for example `Access to the port 'COM3' is denied.`).
    fn open(&self, port_name: &str, baud_rate: i32) -> io::Result<Box<dyn ByteLink>>;
}

/// The open serial line of [`SystemSerialPortOpener`].
pub type SerialLink = Box<dyn ByteLink>;

/// dl-serial on the agent host: 8N1, no handshake, read timeout 3 s, write timeout 5 s (the
/// C# System.IO.Ports settings).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemSerialPortOpener;

impl SerialPortOpener for SystemSerialPortOpener {
    fn open(&self, port_name: &str, baud_rate: i32) -> io::Result<Box<dyn ByteLink>> {
        let baud = u32::try_from(baud_rate)
            .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "invalid baud rate"))?;
        let settings = dl_serial::Settings {
            read_timeout: Some(EscPosSerialAdapter::READ_TIMEOUT),
            write_timeout: Some(EscPosSerialAdapter::WRITE_TIMEOUT),
            ..dl_serial::Settings::new(baud)
        };
        match dl_serial::SerialPort::open(port_name, &settings) {
            Ok(port) => Ok(Box::new(port)),
            // System.IO.Ports words these two (FileNotFoundException, UnauthorizedAccessException).
            Err(dl_serial::Error::NotFound(_)) => Err(io::Error::new(
                ErrorKind::NotFound,
                format!("Could not find file '{port_name}'."),
            )),
            Err(dl_serial::Error::Busy(_) | dl_serial::Error::PermissionDenied(_)) => {
                Err(io::Error::new(
                    ErrorKind::PermissionDenied,
                    format!("Access to the port '{port_name}' is denied."),
                ))
            }
            Err(e) => Err(e.into()),
        }
    }
}

impl ByteLink for dl_serial::SerialPort {
    fn set_read_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        let write = self.write_timeout();
        self.set_timeouts(Some(timeout.max(Duration::from_millis(1))), write)
            .map_err(io::Error::from)
    }
}

/// The message of an I/O failure on an open port, as System.IO.Ports words it.
fn port_message(e: &io::Error) -> String {
    match e.kind() {
        ErrorKind::TimedOut | ErrorKind::WouldBlock => "The write timed out.".to_owned(),
        _ => os_message(e),
    }
}

/// The trimmed port name and baud rate of `target` (C# `WithLink` and the card reader).
fn port_and_baud(target: &PosTarget) -> Result<(&str, i32), AdapterError> {
    let port = trimmed_endpoint(target)?;
    if port.is_empty() {
        return Err(failures::bad_payload(
            "serial endpoint (port name) is empty",
        ));
    }
    let baud = target
        .baud_rate
        .unwrap_or(EscPosSerialAdapter::DEFAULT_BAUD_RATE);
    if !(300..=921_600).contains(&baud) {
        return Err(failures::bad_payload(format!(
            "baud rate {baud} is out of range"
        )));
    }
    Ok((port, baud))
}

/// ESC/POS printer on a COM port (POSWin clsPrinter / frmPrintCom), also the virtual COM
/// port of a Bluetooth printer paired with Windows. 8N1, 9600 baud by default.
pub struct EscPosSerialAdapter {
    opener: Arc<dyn SerialPortOpener>,
}

impl EscPosSerialAdapter {
    /// Baud rate when the target has none.
    pub const DEFAULT_BAUD_RATE: i32 = 9600;
    /// The port read timeout (System.IO.Ports `ReadTimeout = 3000`).
    pub const READ_TIMEOUT: Duration = Duration::from_secs(3);
    /// The port write timeout (System.IO.Ports `WriteTimeout = 5000`).
    pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

    /// A transport opening ports with `opener`.
    pub fn new(opener: Arc<dyn SerialPortOpener>) -> EscPosSerialAdapter {
        EscPosSerialAdapter { opener }
    }

    fn with_link<T>(
        &self,
        target: &PosTarget,
        io: impl FnOnce(&mut dyn ByteLink) -> Result<T, LinkError>,
    ) -> Result<T, AdapterError> {
        let (port, baud) = port_and_baud(target)?;
        let mut link = self
            .opener
            .open(port, baud)
            .map_err(|e| failures::unavailable(target, &os_message(&e)))?;
        io(link.as_mut()).map_err(|e| match e {
            LinkError::Refused(e) => e,
            LinkError::Write(e) | LinkError::Read(e) => {
                failures::unavailable(target, &port_message(&e))
            }
        })
    }
}

impl EscPosTransport for EscPosSerialAdapter {
    fn transport(&self) -> &str {
        transports::SERIAL
    }

    fn send(&self, target: &PosTarget, bytes: &[u8], _cx: &Cx) -> Result<(), AdapterError> {
        self.with_link(target, |link| {
            EscPosStream::send(link, bytes).map_err(LinkError::Write)
        })
    }

    fn query(
        &self,
        target: &PosTarget,
        request: &[u8],
        reply_length: usize,
        cx: &Cx,
    ) -> Result<Vec<u8>, AdapterError> {
        self.with_link(target, |link| {
            EscPosStream::query(link, request, reply_length, target, cx)
        })
    }
}

const STX: u8 = 0x02;
const ETX: u8 = 0x03;
const CR: u8 = 0x0D;
const LF: u8 = 0x0A;

/// Card / RFID / magstripe reader on a COM port (POS-HW-04): the reader sends one ASCII frame
/// per card, framed by STX/ETX and/or ended by CR or LF. The adapter waits up to the request
/// timeout for one frame and answers [`CardIdentity`] only.
pub struct SerialCardReaderAdapter {
    opener: Arc<dyn SerialPortOpener>,
}

impl SerialCardReaderAdapter {
    /// Pause before polling again after the port answered nothing (no card yet).
    pub const IDLE_POLL: Duration = Duration::from_millis(50);

    /// A reader opening ports with `opener`.
    pub fn new(opener: Arc<dyn SerialPortOpener>) -> SerialCardReaderAdapter {
        SerialCardReaderAdapter { opener }
    }

    /// pos.read with device `card` over serial.
    pub fn read(&self, request: &PosReadRequest, cx: &Cx) -> Result<Value, AdapterError> {
        let target = target_of(&request.target)?;
        let (port, baud) = port_and_baud(target)?;
        let mut link = self
            .opener
            .open(port, baud)
            .map_err(|e| failures::unavailable(target, &os_message(&e)))?;
        let wait = Duration::from_millis(u64::try_from(request.timeout_ms).unwrap_or(0));
        let frame = Self::read_frame(link.as_mut(), wait, cx).map_err(|e| match e {
            LinkError::Refused(e) => e,
            LinkError::Write(e) | LinkError::Read(e) => {
                failures::unavailable(target, &os_message(&e))
            }
        })?;
        drop(link);
        CardIdentity::of(&frame, transports::SERIAL)
    }

    /// One frame: bytes up to CR / LF / ETX (STX and leading line ends skipped), or what came
    /// before the line went quiet. A port read timeout or a 0-byte read only means "no card
    /// yet": with nothing read the reader keeps waiting until `wait` has passed
    /// (DEVICE_NO_CARD then).
    fn read_frame(link: &mut dyn ByteLink, wait: Duration, cx: &Cx) -> Result<String, LinkError> {
        let until = Instant::now() + wait;
        let mut frame: Vec<u8> = Vec::with_capacity(32);
        let mut one = [0u8; 1];
        let timed_out = loop {
            if frame.len() > CardIdentity::MAX_LENGTH {
                break false;
            }
            cx.check()?;
            let now = Instant::now();
            if now >= until {
                break true;
            }
            let left = until - now;
            link.set_read_timeout(left.min(EscPosSerialAdapter::READ_TIMEOUT))
                .map_err(LinkError::Read)?;
            let n = match link.read(&mut one) {
                Ok(n) => n,
                Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => 0,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(LinkError::Read(e)),
            };
            if n == 0 {
                if !frame.is_empty() {
                    break false;
                }
                let pause = Self::IDLE_POLL.min(until.saturating_duration_since(Instant::now()));
                cx.sleep(pause)?;
                continue;
            }
            match one[0] {
                STX => {}
                CR | LF | ETX if frame.is_empty() => {}
                CR | LF | ETX => break false,
                b => frame.push(b),
            }
        };
        if timed_out && frame.is_empty() {
            return Err(AdapterError::new(
                codes::NO_CARD,
                format!(
                    "no card within {} s",
                    seconds_0_1(i64::try_from(wait.as_millis()).unwrap_or(i64::MAX))
                ),
            )
            .into());
        }
        // Encoding.ASCII: bytes above 0x7F become '?'.
        Ok(frame
            .iter()
            .map(|&b| if b < 0x80 { char::from(b) } else { '?' })
            .collect())
    }
}
