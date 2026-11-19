//! Port of `PrintRelay/SpoolerPrinting.cs` and `WindowsSpoolerPrinter.cs`: RAW printing
//! through the local Windows spooler (ADR 0105).

use super::raw_tcp::{PrintError, PrintSendError};
use crate::contracts::wire::{PrintQueueInfo, print_queue_kinds, print_spooler_address};
use crate::util::ctx::Ctx;
use crate::util::stj::eq_ignore_case;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

/// Sends one ESC/POS payload RAW to a local Windows print queue (`ISpoolerPrinter`), with
/// the raw TCP rule: `not_sent` only when the spooler cannot have printed anything.
pub trait SpoolerPrinter: Send + Sync {
    /// Prints `payload` to `queue` as the document `document_name`.
    fn print(
        &self,
        queue: &str,
        document_name: &str,
        payload: &[u8],
        ctx: &Ctx,
    ) -> Result<(), PrintError>;
}

/// Win32 errors the spooler code names (`SpoolerPrintException`).
pub mod win32 {
    /// `ERROR_ACCESS_DENIED`.
    pub const ACCESS_DENIED: i32 = 5;
    /// `ERROR_INVALID_PRINTER_NAME`.
    pub const INVALID_PRINTER_NAME: i32 = 1801;
    /// `ERROR_INVALID_DATATYPE`.
    pub const INVALID_DATATYPE: i32 = 1804;
    /// `RPC_S_SERVER_UNAVAILABLE`.
    pub const SPOOLER_UNAVAILABLE: i32 = 1722;
    /// `ERROR_SERVICE_NOT_ACTIVE`.
    pub const SPOOLER_NOT_RUNNING: i32 = 1062;
}

/// The site event of a spooler problem a person must fix on the server
/// (`SpoolerPrintException.EventCode`); `None` for a plain job failure.
pub fn event_code(win32_error: i32) -> Option<&'static str> {
    match win32_error {
        win32::ACCESS_DENIED => Some("PRINT_SPOOLER_ACCESS_DENIED"),
        win32::SPOOLER_UNAVAILABLE | win32::SPOOLER_NOT_RUNNING => {
            Some("PRINT_SPOOLER_UNAVAILABLE")
        }
        win32::INVALID_DATATYPE => Some("PRINT_SPOOLER_RAW_REFUSED"),
        _ => None,
    }
}

/// A Win32 error the next attempt cannot fix by itself (`IsPermanent`).
pub fn is_permanent(win32_error: i32) -> bool {
    matches!(
        win32_error,
        win32::ACCESS_DENIED | win32::INVALID_PRINTER_NAME | win32::INVALID_DATATYPE
    )
}

/// A spooler failure (`new SpoolerPrintException(...)`).
pub fn spooler_error(
    message: impl Into<String>,
    not_sent: bool,
    win32_error: i32,
    permanent: bool,
) -> PrintSendError {
    PrintSendError {
        message: message.into(),
        not_sent,
        permanent,
        win32_error: Some(win32_error),
    }
}

/// The Windows queues the relay may print to (`SpoolerQueues`): exactly the USB and WSD
/// queues of this agent's last successful discovery. Empty until discovery has read the
/// queues once.
#[derive(Debug, Default)]
pub struct SpoolerQueues {
    /// (trimmed name, exact name), first of case-insensitive duplicates kept.
    queues: Mutex<Arc<Vec<(String, String)>>>,
}

impl SpoolerQueues {
    /// An empty allow-list.
    pub fn new() -> SpoolerQueues {
        SpoolerQueues::default()
    }

    fn current(&self) -> Arc<Vec<(String, String)>> {
        self.queues
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// How many queues are allowed.
    pub fn count(&self) -> usize {
        self.current().len()
    }

    /// Replaces the allowed set with the USB/WSD queues of a fresh discovery.
    pub fn replace(&self, queues: &[PrintQueueInfo]) {
        let mut allowed: Vec<(String, String)> = Vec::new();
        for q in queues
            .iter()
            .filter(|q| print_queue_kinds::is_spooler(&q.kind) && !q.name.trim().is_empty())
        {
            let key = q.name.trim().to_owned();
            if !allowed.iter().any(|(k, _)| eq_ignore_case(k, &key)) {
                allowed.push((key, q.name.clone()));
            }
        }
        *self.queues.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(allowed);
    }

    /// The exact Windows name of the queue a `spooler:` address names, when that queue
    /// was reported (`TryResolve`).
    pub fn try_resolve(&self, address: &str) -> Option<String> {
        let requested = print_spooler_address::try_parse(Some(address))?;
        self.current()
            .iter()
            .find(|(k, _)| eq_ignore_case(k, &requested))
            .map(|(_, exact)| exact.clone())
    }
}

/// Decides, exactly once, whether a spooler job may still write (`SpoolGate`): the print
/// thread claims the write, the caller's timeout claims the cancel.
#[derive(Debug, Default)]
pub struct SpoolGate(AtomicU8);

const OPEN: u8 = 0;
const WRITING: u8 = 1;
const CANCELLED: u8 = 2;

impl SpoolGate {
    /// A gate that is still open.
    pub fn new() -> SpoolGate {
        SpoolGate(AtomicU8::new(OPEN))
    }

    /// The print thread claims the write; `false` when the caller already cancelled.
    pub fn try_begin_write(&self) -> bool {
        self.0
            .compare_exchange(OPEN, WRITING, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// The caller claims the cancel; `false` when the thread already began writing.
    pub fn try_cancel(&self) -> bool {
        self.0
            .compare_exchange(OPEN, CANCELLED, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

/// How long a spooler print may take (`WindowsSpoolerPrinter.Timeout`).
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// The hint the C# code appends to a Win32 error.
fn hint(code: i32) -> &'static str {
    match code {
        win32::ACCESS_DENIED => " (the agent's service account may not print to this queue)",
        win32::SPOOLER_UNAVAILABLE | win32::SPOOLER_NOT_RUNNING => {
            " (the Print Spooler service is not running)"
        }
        win32::INVALID_DATATYPE => {
            " (the queue's driver does not take RAW data; use a v3 or Generic / Text Only driver)"
        }
        win32::INVALID_PRINTER_NAME => " (the queue no longer exists)",
        _ => "",
    }
}

/// RAW printing through the local Windows spooler (`WindowsSpoolerPrinter`) over
/// `dl_winos::spooler::write_raw`, on a thread of its own bounded by [`TIMEOUT`].
///
/// `write_raw` runs `OpenPrinter` .. `EndDocPrinter` as one call and aborts the document
/// on a failure after `StartDocPrinter`; the failing call comes back as the error's
/// operation. A failure of `OpenPrinterW`, `StartDocPrinterW` or `StartPagePrinter` is
/// "not sent"; anything later is "outcome unknown" (fail closed). The C# code claims the
/// [`SpoolGate`] between `StartPagePrinter` and the first `WritePrinter`; `write_raw`
/// cannot stop there, so the thread claims it before the call and a timeout after the
/// thread started is always "outcome unknown".
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsSpoolerPrinter;

impl SpoolerPrinter for WindowsSpoolerPrinter {
    fn print(
        &self,
        queue: &str,
        document_name: &str,
        payload: &[u8],
        ctx: &Ctx,
    ) -> Result<(), PrintError> {
        if !cfg!(windows) {
            return Err(spooler_error(
                "Windows spooler printing is only available on Windows",
                true,
                0,
                true,
            )
            .into());
        }
        let gate = Arc::new(SpoolGate::new());
        let (tx, rx) = mpsc::channel();
        let (q, d, p, g) = (
            queue.to_owned(),
            document_name.to_owned(),
            payload.to_vec(),
            gate.clone(),
        );
        let spawned = std::thread::Builder::new()
            .name("spooler".into())
            .spawn(move || {
                if !g.try_begin_write() {
                    return; // The caller already answered "not sent".
                }
                let _ = tx.send(dl_winos::spooler::write_raw(&q, &d, &p));
            });
        if spawned.is_err() {
            return Err(spooler_error(
                format!("the Windows spooler did not accept queue '{queue}' within {} s; nothing was written", TIMEOUT.as_secs()),
                true,
                0,
                false,
            )
            .into());
        }
        let wait = ctx.remaining().map_or(TIMEOUT, |r| r.min(TIMEOUT));
        match rx.recv_timeout(wait) {
            Ok(Ok(_job)) => Ok(()),
            Ok(Err(e)) => Err(failure(queue, &e).into()),
            Err(_) => {
                if gate.try_cancel() {
                    return Err(spooler_error(
                        format!("the Windows spooler did not accept queue '{queue}' within {} s; nothing was written", TIMEOUT.as_secs()),
                        true,
                        0,
                        false,
                    )
                    .into());
                }
                if ctx.is_done() {
                    return Err(PrintError::Cancelled);
                }
                Err(spooler_error(
                    format!(
                        "writing to Windows queue '{queue}' did not finish within {} s",
                        TIMEOUT.as_secs()
                    ),
                    false,
                    0,
                    false,
                )
                .into())
            }
        }
    }
}

/// A `write_raw` failure as the C# `Failure(...)` would word it.
fn failure(queue: &str, e: &dl_winos::Error) -> PrintSendError {
    let code = e.code().and_then(|c| i32::try_from(c).ok()).unwrap_or(0);
    let before_write = matches!(
        code,
        win32::INVALID_PRINTER_NAME
            | win32::ACCESS_DENIED
            | win32::INVALID_DATATYPE
            | win32::SPOOLER_UNAVAILABLE
            | win32::SPOOLER_NOT_RUNNING
    );
    let what = match (code, e.operation()) {
        (_, Some("OpenPrinterW")) | (win32::INVALID_PRINTER_NAME, _) => {
            format!("the Windows queue '{queue}' could not be opened")
        }
        (_, Some("StartDocPrinterW")) | (win32::INVALID_DATATYPE, _) => {
            format!("the Windows queue '{queue}' refused a RAW document")
        }
        (_, Some("StartPagePrinter")) => format!("the Windows queue '{queue}' refused a page"),
        (_, Some("EndPagePrinter")) => {
            format!("the Windows queue '{queue}' failed to end the page")
        }
        (_, Some("EndDocPrinter")) => {
            format!("the Windows queue '{queue}' failed to end the document")
        }
        _ => format!("writing to the Windows queue '{queue}' failed"),
    };
    // dl-winos aborts the document after a failed StartPagePrinter, as the C# code does
    // before answering "not sent".
    let not_sent = match e.operation() {
        Some(op) => matches!(op, "OpenPrinterW" | "StartDocPrinterW" | "StartPagePrinter"),
        None => before_write,
    };
    spooler_error(
        format!("{what}: Win32 error {code}{}", hint(code)),
        not_sent,
        code,
        not_sent && is_permanent(code),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(name: &str, kind: &str) -> PrintQueueInfo {
        PrintQueueInfo {
            name: name.into(),
            kind: kind.into(),
            port_name: "P".into(),
            host: None,
            port: None,
        }
    }

    #[test]
    fn the_allow_list_holds_only_usb_and_wsd_queues_case_insensitively() {
        let s = SpoolerQueues::new();
        assert_eq!(s.try_resolve("spooler:Cashier"), None);
        s.replace(&[
            q("Cashier", print_queue_kinds::USB),
            q("Bar ", print_queue_kinds::WSD),
            q("Kitchen", print_queue_kinds::TCP),
            q("  ", print_queue_kinds::USB),
        ]);
        assert_eq!(s.count(), 2);
        assert_eq!(s.try_resolve("spooler:cashier").as_deref(), Some("Cashier"));
        assert_eq!(s.try_resolve(" SPOOLER: bar").as_deref(), Some("Bar "));
        assert_eq!(s.try_resolve("spooler:Kitchen"), None);
        assert_eq!(s.try_resolve("Cashier"), None);
    }

    #[test]
    fn the_gate_decides_once() {
        let g = SpoolGate::new();
        assert!(g.try_cancel());
        assert!(!g.try_begin_write());
        let g = SpoolGate::new();
        assert!(g.try_begin_write());
        assert!(!g.try_cancel());
    }

    #[test]
    fn spooler_errors_name_their_site_event_and_permanence() {
        assert_eq!(event_code(5), Some("PRINT_SPOOLER_ACCESS_DENIED"));
        assert_eq!(event_code(1722), Some("PRINT_SPOOLER_UNAVAILABLE"));
        assert_eq!(event_code(1062), Some("PRINT_SPOOLER_UNAVAILABLE"));
        assert_eq!(event_code(1804), Some("PRINT_SPOOLER_RAW_REFUSED"));
        assert_eq!(event_code(1801), None);
        assert!(is_permanent(1801) && is_permanent(5) && is_permanent(1804));
        assert!(!is_permanent(1722));
    }

    #[cfg(windows)]
    #[test]
    fn an_unknown_queue_is_not_sent_and_permanent() {
        let e = WindowsSpoolerPrinter
            .print("pda-sync-no-such-queue", "doc", b"x", &Ctx::background())
            .unwrap_err();
        match e {
            PrintError::Send(e) => {
                assert!(e.not_sent && e.permanent && !e.retryable(), "{e:?}");
                assert_eq!(e.win32_error, Some(1801));
                assert!(
                    e.message
                        .contains("Win32 error 1801 (the queue no longer exists)"),
                    "{e}"
                );
            }
            other => panic!("{other:?}"),
        }
    }
}
