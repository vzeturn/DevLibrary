//! POS test doubles (port of SmilePms.DeviceAgent.Tests/Fakes/Pos*Fake.cs, plus an OPOS
//! control fake). Included by the tests/pos_*.rs files with `#[path]` (a folder module, so it is no test
//! target of its own).

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::io::{self, ErrorKind, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dl_agent_pms_device::adapters::pos::{
    ByteLink, ComFailure, EscPosTransport, OposArg, OposControl, OposHost, OposOpen, OposSession,
    PcscCardSource, RawSpooler, SerialPortOpener, SpeechEngine, Win32Error,
};
use dl_agent_pms_device::adapters::{AdapterError, Cx, PosDevice, adapter_failed};
use dl_agent_pms_device::contracts::{
    PosDisplayRequest, PosKickRequest, PosPrintRequest, PosReadRequest, PosSpeakRequest, PosTarget,
};
use dl_json::{Value, json};

/// A target with a transport and an endpoint.
pub fn target(transport: &str, endpoint: &str) -> PosTarget {
    PosTarget {
        transport: Some(transport.to_owned()),
        endpoint: Some(endpoint.to_owned()),
        ..PosTarget::default()
    }
}

/// `["a", "b"]` as payload lines.
pub fn lines(l: &[&str]) -> Vec<Option<String>> {
    l.iter().map(|s| Some((*s).to_owned())).collect()
}

/// Base64 of `bytes`.
pub fn b64(bytes: &[u8]) -> String {
    dl_encoding::base64::STANDARD.encode(bytes)
}

/// `PosTransportFake`: records what was sent and answers status queries from a queue.
pub struct PosTransportFake {
    transport: String,
    pub sent: Mutex<Vec<(PosTarget, Vec<u8>)>>,
    pub queries: Mutex<Vec<Vec<u8>>>,
    pub replies: Mutex<VecDeque<Vec<u8>>>,
    pub fail: Mutex<Option<AdapterError>>,
}

impl PosTransportFake {
    pub fn new(transport: &str) -> Arc<PosTransportFake> {
        Arc::new(PosTransportFake {
            transport: transport.to_owned(),
            sent: Mutex::default(),
            queries: Mutex::default(),
            replies: Mutex::default(),
            fail: Mutex::default(),
        })
    }

    pub fn sent(&self) -> Vec<(PosTarget, Vec<u8>)> {
        self.sent.lock().unwrap().clone()
    }

    pub fn sent_bytes(&self) -> Vec<Vec<u8>> {
        self.sent().into_iter().map(|(_, b)| b).collect()
    }

    pub fn queries(&self) -> Vec<Vec<u8>> {
        self.queries.lock().unwrap().clone()
    }

    pub fn reply(&self, bytes: &[u8]) {
        self.replies.lock().unwrap().push_back(bytes.to_vec());
    }

    pub fn fail_with(&self, e: AdapterError) {
        *self.fail.lock().unwrap() = Some(e);
    }
}

impl EscPosTransport for PosTransportFake {
    fn transport(&self) -> &str {
        &self.transport
    }

    fn send(&self, target: &PosTarget, bytes: &[u8], _cx: &Cx) -> Result<(), AdapterError> {
        if let Some(e) = self.fail.lock().unwrap().clone() {
            return Err(e);
        }
        self.sent
            .lock()
            .unwrap()
            .push((target.clone(), bytes.to_vec()));
        Ok(())
    }

    fn query(
        &self,
        _target: &PosTarget,
        request: &[u8],
        _reply_length: usize,
        _cx: &Cx,
    ) -> Result<Vec<u8>, AdapterError> {
        if let Some(e) = self.fail.lock().unwrap().clone() {
            return Err(e);
        }
        self.queries.lock().unwrap().push(request.to_vec());
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| adapter_failed("InvalidOperationException", "Queue empty."))
    }
}

/// What a drained fake port answers (a real port with ReadTimeout).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DrainedRead {
    #[default]
    Zero,
    Timeout,
}

#[derive(Default)]
pub struct SerialState {
    pub opened: Vec<(String, i32)>,
    pub written: Vec<u8>,
    /// Bytes the device sends back.
    pub reply: Vec<u8>,
    /// When set, open fails with it (port absent or busy).
    pub open_fails: Option<(ErrorKind, String)>,
    pub closed: bool,
    /// A read after the reply waits its read timeout, then times out (a reader with no card).
    pub hang_when_drained: bool,
    pub drained: DrainedRead,
    /// Reads made after the reply ran out.
    pub drained_reads: usize,
}

/// `PosSerialPortFake`: a port opener whose line records writes and plays back the reply.
#[derive(Clone, Default)]
pub struct PosSerialPortFake {
    pub state: Arc<Mutex<SerialState>>,
}

impl PosSerialPortFake {
    pub fn new() -> PosSerialPortFake {
        PosSerialPortFake::default()
    }

    pub fn with(f: impl FnOnce(&mut SerialState)) -> PosSerialPortFake {
        let fake = PosSerialPortFake::default();
        f(&mut fake.state.lock().unwrap());
        fake
    }

    pub fn set(&self, f: impl FnOnce(&mut SerialState)) {
        f(&mut self.state.lock().unwrap());
    }

    pub fn opened(&self) -> Vec<(String, i32)> {
        self.state.lock().unwrap().opened.clone()
    }

    pub fn written(&self) -> Vec<u8> {
        self.state.lock().unwrap().written.clone()
    }

    pub fn closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }

    pub fn drained_reads(&self) -> usize {
        self.state.lock().unwrap().drained_reads
    }
}

impl SerialPortOpener for PosSerialPortFake {
    fn open(&self, port_name: &str, baud_rate: i32) -> io::Result<Box<dyn ByteLink>> {
        let mut s = self.state.lock().unwrap();
        if let Some((kind, message)) = &s.open_fails {
            return Err(io::Error::new(*kind, message.clone()));
        }
        s.opened.push((port_name.to_owned(), baud_rate));
        s.closed = false;
        Ok(Box::new(FakeLink {
            state: self.state.clone(),
            at: 0,
            read_timeout: Duration::from_secs(3),
        }))
    }
}

struct FakeLink {
    state: Arc<Mutex<SerialState>>,
    at: usize,
    read_timeout: Duration,
}

impl Read for FakeLink {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let (hang, drained) = {
            let mut s = self.state.lock().unwrap();
            if self.at < s.reply.len() {
                let n = buf.len().min(s.reply.len() - self.at);
                buf[..n].copy_from_slice(&s.reply[self.at..self.at + n]);
                self.at += n;
                return Ok(n);
            }
            s.drained_reads += 1;
            (s.hang_when_drained, s.drained)
        };
        if hang {
            std::thread::sleep(self.read_timeout);
            return Err(ErrorKind::TimedOut.into());
        }
        match drained {
            DrainedRead::Zero => Ok(0),
            DrainedRead::Timeout => Err(io::Error::new(
                ErrorKind::TimedOut,
                "The operation has timed out.",
            )),
        }
    }
}

impl Write for FakeLink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.state.lock().unwrap().written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl ByteLink for FakeLink {
    fn set_read_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.read_timeout = timeout;
        Ok(())
    }
}

impl Drop for FakeLink {
    fn drop(&mut self) {
        self.state.lock().unwrap().closed = true;
    }
}

/// `PosSpoolerFake`: records RAW jobs; `fail` makes the queue refuse them.
#[derive(Default)]
pub struct PosSpoolerFake {
    pub jobs: Mutex<Vec<(String, String, Vec<u8>)>>,
    pub fail: Mutex<Option<Win32Error>>,
}

impl RawSpooler for PosSpoolerFake {
    fn write_raw(
        &self,
        printer_name: &str,
        document_name: &str,
        bytes: &[u8],
    ) -> Result<(), Win32Error> {
        if let Some(e) = self.fail.lock().unwrap().clone() {
            return Err(e);
        }
        self.jobs.lock().unwrap().push((
            printer_name.to_owned(),
            document_name.to_owned(),
            bytes.to_vec(),
        ));
        Ok(())
    }
}

/// `PosPcscFake`: a reader that answers a preset UID, or fails with a preset refusal.
pub struct PosPcscFake {
    pub uid: Mutex<Vec<u8>>,
    pub fail: Mutex<Option<AdapterError>>,
    pub reads: Mutex<Vec<(String, Duration)>>,
}

impl Default for PosPcscFake {
    fn default() -> Self {
        PosPcscFake {
            uid: Mutex::new(vec![0x04, 0xA2, 0x3B, 0x1C]),
            fail: Mutex::default(),
            reads: Mutex::default(),
        }
    }
}

impl PcscCardSource for PosPcscFake {
    fn read_uid(
        &self,
        reader_name: &str,
        wait: Duration,
        _cx: &Cx,
    ) -> Result<Vec<u8>, AdapterError> {
        self.reads
            .lock()
            .unwrap()
            .push((reader_name.to_owned(), wait));
        if let Some(e) = self.fail.lock().unwrap().clone() {
            return Err(e);
        }
        Ok(self.uid.lock().unwrap().clone())
    }
}

/// `PosSpeechFake`: records each sentence instead of saying it.
#[derive(Default)]
pub struct PosSpeechFake {
    pub spoken: Mutex<Vec<(String, String, i32, i32)>>,
}

impl PosSpeechFake {
    pub fn spoken(&self) -> Vec<(String, String, i32, i32)> {
        self.spoken.lock().unwrap().clone()
    }
}

impl SpeechEngine for PosSpeechFake {
    fn speak(
        &self,
        voice_hint: &str,
        text: &str,
        volume: i32,
        rate: i32,
    ) -> Result<(), AdapterError> {
        self.spoken
            .lock()
            .unwrap()
            .push((voice_hint.to_owned(), text.to_owned(), volume, rate));
        Ok(())
    }
}

/// `PosDeviceFake`: records each verb the processor dispatches and answers `{"ok":true}`.
#[derive(Default)]
pub struct PosDeviceFake {
    pub calls: Mutex<Vec<&'static str>>,
}

impl PosDeviceFake {
    fn record(&self, verb: &'static str) -> Result<Value, AdapterError> {
        self.calls.lock().unwrap().push(verb);
        Ok(json!({"ok": true}))
    }

    pub fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
}

impl PosDevice for PosDeviceFake {
    fn print(&self, _: &PosPrintRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.record("PosPrintRequest")
    }

    fn kick(&self, _: &PosKickRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.record("PosKickRequest")
    }

    fn display(&self, _: &PosDisplayRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.record("PosDisplayRequest")
    }

    fn read(&self, _: &PosReadRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.record("PosReadRequest")
    }

    fn speak(&self, _: &PosSpeakRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.record("PosSpeakRequest")
    }
}

/// An OPOS host whose control objects record every call and answer scripted result codes
/// (0 unless scripted).
#[derive(Default)]
pub struct OposFake {
    /// ProgIDs that are registered; any other is `NotRegistered`.
    pub registered: Mutex<Vec<String>>,
    /// Result code per method (or property `ResultCode`).
    pub codes: Mutex<HashMap<String, i32>>,
    /// A COM failure per method.
    pub com_failures: Mutex<HashMap<String, ComFailure>>,
    /// `ProgID` created, then each call as `Method(args)` / `Property=value` / `Property?`.
    pub calls: Arc<Mutex<Vec<String>>>,
}

impl OposFake {
    pub fn registering(prog_ids: &[&str]) -> Arc<OposFake> {
        let fake = OposFake::default();
        *fake.registered.lock().unwrap() = prog_ids.iter().map(|s| (*s).to_owned()).collect();
        Arc::new(fake)
    }

    pub fn code(&self, method: &str, rc: i32) {
        self.codes.lock().unwrap().insert(method.to_owned(), rc);
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

struct FakeControl {
    codes: HashMap<String, i32>,
    com_failures: HashMap<String, ComFailure>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl OposControl for FakeControl {
    fn call(&mut self, method: &str, args: &[OposArg]) -> Result<i32, ComFailure> {
        let args: Vec<String> = args
            .iter()
            .map(|a| match a {
                OposArg::I32(n) => n.to_string(),
                OposArg::Str(s) => format!("{s:?}"),
            })
            .collect();
        self.calls
            .lock()
            .unwrap()
            .push(format!("{method}({})", args.join(", ")));
        if let Some(f) = self.com_failures.get(method) {
            return Err(f.clone());
        }
        Ok(self.codes.get(method).copied().unwrap_or(0))
    }

    fn set_bool(&mut self, property: &str, value: bool) -> Result<(), ComFailure> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("{property}={value}"));
        Ok(())
    }

    fn get_i32(&mut self, property: &str) -> Result<i32, ComFailure> {
        self.calls.lock().unwrap().push(format!("{property}?"));
        Ok(self.codes.get(property).copied().unwrap_or(0))
    }
}

impl OposHost for OposFake {
    fn with_control(
        &self,
        prog_id: &str,
        session: OposSession,
    ) -> Result<Result<Value, AdapterError>, OposOpen> {
        if !self.registered.lock().unwrap().iter().any(|p| p == prog_id) {
            return Err(OposOpen::NotRegistered);
        }
        self.calls.lock().unwrap().push(format!("create {prog_id}"));
        let mut control = FakeControl {
            codes: self.codes.lock().unwrap().clone(),
            com_failures: self.com_failures.lock().unwrap().clone(),
            calls: self.calls.clone(),
        };
        Ok(session(&mut control))
    }
}
