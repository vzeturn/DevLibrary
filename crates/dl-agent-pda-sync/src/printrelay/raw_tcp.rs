//! Port of `PrintRelay/RawTcpPrinter.cs`: raw TCP (JetDirect, port 9100) printing.

use super::os_message;
use crate::util::ctx::Ctx;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// A print that failed (`PrintSendException`, `SpoolerPrintException`). `not_sent` is true
/// only when no byte can have reached the printer, so the job is safe to print again;
/// anything later is "outcome unknown" and must never be reprinted. `permanent` marks a
/// not-sent failure that retrying cannot fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrintSendError {
    /// The message.
    pub message: String,
    /// No byte can have reached the printer.
    pub not_sent: bool,
    /// Retrying cannot fix it.
    pub permanent: bool,
    /// The Win32 error of a spooler failure (`SpoolerPrintException.Win32Error`); `None`
    /// for raw TCP.
    pub win32_error: Option<i32>,
}

impl PrintSendError {
    /// A raw TCP failure.
    pub fn new(message: impl Into<String>, not_sent: bool) -> PrintSendError {
        PrintSendError {
            message: message.into(),
            not_sent,
            permanent: false,
            win32_error: None,
        }
    }

    /// Whether the cloud may push the job again: nothing was sent and another attempt can
    /// succeed.
    pub fn retryable(&self) -> bool {
        self.not_sent && !self.permanent
    }
}

impl std::fmt::Display for PrintSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PrintSendError {}

/// Why a print did not finish: a send failure, or the agent stopping (the C#
/// `OperationCanceledException`, which is not a print outcome).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrintError {
    /// The print failed.
    Send(PrintSendError),
    /// The context ended.
    Cancelled,
}

impl From<PrintSendError> for PrintError {
    fn from(e: PrintSendError) -> PrintError {
        PrintError::Send(e)
    }
}

/// Sends one ESC/POS payload to a printer at an already checked address (`IRawPrinter`).
pub trait RawPrinter: Send + Sync {
    /// Prints `payload` to `target`.
    fn print(&self, target: SocketAddr, payload: &[u8], ctx: &Ctx) -> Result<(), PrintError>;
}

/// Dial timeout.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(5);
/// Write timeout.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(15);
/// Drain timeout.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

/// `SocketException.SocketErrorCode` names for the connect failures.
fn socket_error_code(e: &std::io::Error) -> &'static str {
    match e.kind() {
        ErrorKind::ConnectionRefused => "ConnectionRefused",
        ErrorKind::ConnectionReset => "ConnectionReset",
        ErrorKind::ConnectionAborted => "ConnectionAborted",
        ErrorKind::TimedOut => "TimedOut",
        ErrorKind::HostUnreachable => "HostUnreachable",
        ErrorKind::NetworkUnreachable => "NetworkUnreachable",
        ErrorKind::NetworkDown => "NetworkDown",
        ErrorKind::AddrNotAvailable => "AddressNotAvailable",
        ErrorKind::AddrInUse => "AddressAlreadyInUse",
        ErrorKind::PermissionDenied => "AccessDenied",
        _ => "SocketError",
    }
}

/// Raw TCP printing as the speaker agent does it (`RawTcpPrinter`): dial within 5 s,
/// write within 15 s, half-close and drain the printer's answer for up to 1 s so the bytes
/// are flushed before the socket closes.
#[derive(Debug, Default, Clone, Copy)]
pub struct RawTcpPrinter;

impl RawPrinter for RawTcpPrinter {
    fn print(&self, target: SocketAddr, payload: &[u8], ctx: &Ctx) -> Result<(), PrintError> {
        if ctx.is_done() {
            return Err(PrintError::Cancelled);
        }
        let dial = ctx
            .remaining()
            .map_or(DIAL_TIMEOUT, |r| r.min(DIAL_TIMEOUT));
        let mut socket = match TcpStream::connect_timeout(&target, dial) {
            Ok(s) => s,
            Err(_) if ctx.is_done() => return Err(PrintError::Cancelled),
            Err(e) if e.kind() == ErrorKind::TimedOut => {
                return Err(PrintSendError::new(
                    format!(
                        "printer {target} did not answer within {} s",
                        DIAL_TIMEOUT.as_secs()
                    ),
                    true,
                )
                .into());
            }
            Err(e) => {
                return Err(PrintSendError::new(
                    format!(
                        "printer {target} refused the connection: {}",
                        socket_error_code(&e)
                    ),
                    true,
                )
                .into());
            }
        };
        let _ = socket.set_nodelay(true);

        let part_way = |m: String| {
            PrintSendError::new(
                format!("sending to printer {target} failed part way: {m}"),
                false,
            )
        };
        let deadline = Instant::now() + WRITE_TIMEOUT;
        let mut sent = 0;
        while sent < payload.len() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || ctx.is_done() {
                return Err(part_way("The operation was canceled.".into()).into());
            }
            let _ = socket.set_write_timeout(Some(left.min(Duration::from_millis(500))));
            match socket.write(&payload[sent..]) {
                Ok(0) => return Err(part_way("the connection was closed".into()).into()),
                Ok(n) => sent += n,
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(part_way(os_message(&e)).into()),
            }
        }
        if let Err(e) = socket.shutdown(Shutdown::Write) {
            return Err(part_way(os_message(&e)).into());
        }

        // The bytes are out; a printer that never answers is normal, so the drain only
        // bounds the close.
        let drain_end = Instant::now() + DRAIN_TIMEOUT;
        let mut buffer = [0u8; 256];
        loop {
            let left = drain_end.saturating_duration_since(Instant::now());
            if left.is_zero() || ctx.is_done() {
                break;
            }
            let _ = socket.set_read_timeout(Some(left));
            match socket.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn prints_the_bytes_and_half_closes() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut got = Vec::new();
            s.read_to_end(&mut got).unwrap();
            got
        });
        RawTcpPrinter
            .print(addr, b"\x1b@hello", &Ctx::background())
            .unwrap();
        assert_eq!(server.join().unwrap(), b"\x1b@hello");
    }

    #[test]
    fn a_refused_connection_is_not_sent() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        drop(l);
        match RawTcpPrinter.print(addr, b"x", &Ctx::background()) {
            Err(PrintError::Send(e)) => {
                assert!(e.not_sent && e.retryable(), "{e}");
                assert!(e.message.starts_with(&format!("printer {addr} ")), "{e}");
            }
            other => panic!("{other:?}"),
        }
    }
}
