//! Print relay (port of `PrintRelay/`, ADR 0098/0105): the cloud's print jobs to LAN
//! printers over raw TCP or to local USB/WSD queues through the Windows spooler.
//!
//! - [`policy`]: `PrinterTargetPolicy.cs` (the subnet and port gate).
//! - [`journal`]: `PrintJournal.cs` (one durable file per job).
//! - [`raw_tcp`]: `RawTcpPrinter.cs` and `PrintSendException`.
//! - [`spooler`]: `SpoolerPrinting.cs`, `WindowsSpoolerPrinter.cs`, `SpoolGate`.
//! - [`discovery`]: `PrintQueueDiscovery.cs`, `PrintQueueReporter.cs`.
//! - [`engine`]: `PrintRelayEngine*.cs` (journaled job flow, printer lanes, acks).
//! - [`service`]: `PrintRelayService.cs` (the order hub connection) and `AddPrintRelay`.

pub mod discovery;
pub mod engine;
pub mod journal;
pub mod policy;
pub mod raw_tcp;
pub mod service;
pub mod spooler;

pub use crate::config::PrintRelayOptions;
pub use discovery::{
    CloudPrintQueueReporter, PrintQueueCloud, PrintQueueDiscovery, PrintQueueSource,
    RawPrinterEntry, WindowsPrintQueueSource,
};
pub use engine::{PrintRelayEngine, PrintRelayEngineParts, PrintRelayJob, PrintRelayLink};
pub use journal::{PrintJournal, PrintJournalEntry, print_job_states};
pub use policy::{IpNetwork, PrinterTargetPolicy, TargetRefusal};
pub use raw_tcp::{PrintError, PrintSendError, RawPrinter, RawTcpPrinter};
pub use spooler::{SpoolGate, SpoolerPrinter, SpoolerQueues, WindowsSpoolerPrinter};

/// The agent is stopping (the C# `OperationCanceledException` out of a relay call).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The operation was canceled.")
    }
}

impl std::error::Error for Cancelled {}

/// An I/O error's message without Rust's ` (os error N)` suffix, closer to the .NET
/// `SocketException.Message`.
pub(crate) fn os_message(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(i) if s.ends_with(')') => s[..i].to_owned(),
        _ => s,
    }
}
