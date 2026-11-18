//! `WindowsSpoolerAdapter.cs`: ESC/POS through the Windows spooler as a RAW job (POSWin
//! RawPrinterHelper / clsPrintSYS), for USB printers installed as a Windows queue.

use std::sync::Arc;

use super::{EscPosTransport, failures, trimmed_endpoint};
use crate::adapters::Cx;
use crate::contracts::{AdapterError, PosTarget, codes, transports};

/// A Win32 error (the C# `Win32Exception`): the code and its system message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Win32Error {
    /// `NativeErrorCode`.
    pub code: u32,
    /// The system message for the code (`FormatMessage`).
    pub message: String,
}

impl Win32Error {
    /// The error for `code` with the system's message text.
    pub fn from_code(code: u32) -> Win32Error {
        let message = i32::try_from(code).map_or_else(
            |_| format!("Unknown error (0x{code:x})"),
            |c| super::os_message(&std::io::Error::from_raw_os_error(c)),
        );
        Win32Error { code, message }
    }
}

/// Writes one RAW document to a named Windows printer queue. Faked in tests.
pub trait RawSpooler: Send + Sync {
    /// Spools `bytes` as one RAW job; the error is the queue's refusal.
    fn write_raw(
        &self,
        printer_name: &str,
        document_name: &str,
        bytes: &[u8],
    ) -> Result<(), Win32Error>;
}

/// winspool.drv RAW printing through `dl_winos::spooler::write_raw`.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinSpoolRaw;

impl RawSpooler for WinSpoolRaw {
    fn write_raw(
        &self,
        printer_name: &str,
        document_name: &str,
        bytes: &[u8],
    ) -> Result<(), Win32Error> {
        match dl_winos::spooler::write_raw(printer_name, document_name, bytes) {
            Ok(_job) => Ok(()),
            Err(e) => Err(match e.code() {
                Some(code) => Win32Error::from_code(code),
                None => Win32Error {
                    code: 0,
                    message: e.to_string(),
                },
            }),
        }
    }
}

/// The spooler transport: Windows only (elsewhere every send refuses with
/// DEVICE_PLATFORM_UNSUPPORTED); no status back-channel, so a status read refuses.
pub struct WindowsSpoolerAdapter {
    spooler: Option<Arc<dyn RawSpooler>>,
}

impl WindowsSpoolerAdapter {
    /// The document name of every job.
    pub const DOCUMENT_NAME: &'static str = "SMILE POS";

    /// A transport over `spooler`; `None` is a host without the spooler.
    pub fn new(spooler: Option<Arc<dyn RawSpooler>>) -> WindowsSpoolerAdapter {
        WindowsSpoolerAdapter { spooler }
    }

    /// The winspool shim on Windows; none elsewhere.
    pub fn create() -> WindowsSpoolerAdapter {
        let spooler: Option<Arc<dyn RawSpooler>> = if cfg!(windows) {
            Some(Arc::new(WinSpoolRaw))
        } else {
            None
        };
        WindowsSpoolerAdapter::new(spooler)
    }
}

impl EscPosTransport for WindowsSpoolerAdapter {
    fn transport(&self) -> &str {
        transports::SPOOLER
    }

    fn send(&self, target: &PosTarget, bytes: &[u8], cx: &Cx) -> Result<(), AdapterError> {
        let Some(spooler) = &self.spooler else {
            return Err(AdapterError::new(
                codes::PLATFORM_UNSUPPORTED,
                "the Windows spooler transport runs only on a Windows agent",
            ));
        };
        let printer = trimmed_endpoint(target)?;
        if printer.is_empty() {
            return Err(failures::bad_payload(
                "spooler endpoint (printer name) is empty",
            ));
        }
        // Task.Run(..., ct): a cancelled call does not start the job.
        cx.check()?;
        spooler
            .write_raw(printer, Self::DOCUMENT_NAME, bytes)
            .map_err(|e| {
                failures::unavailable(target, &format!("spooler error {}: {}", e.code, e.message))
            })
    }

    fn query(
        &self,
        _target: &PosTarget,
        _request: &[u8],
        _reply_length: usize,
        _cx: &Cx,
    ) -> Result<Vec<u8>, AdapterError> {
        Err(failures::unsupported(
            transports::SPOOLER,
            "a status read",
            "the Windows spooler has no status back-channel; use the tcp or serial profile of the printer",
        ))
    }
}
