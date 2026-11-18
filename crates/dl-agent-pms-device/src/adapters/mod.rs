//! Device adapters (`Adapters/`): the door-lock adapter selected by `Agent:Adapter`
//! ([`DeviceAdapter`], C# IDeviceAdapter) and the POS device router ([`PosDevice`], C#
//! IPosDevice). Every verb gets a decoded payload and a call context [`Cx`] (deadline =
//! `Agent:CommandTimeout`, plus the service stop) and returns the JSON result for the API or
//! an [`AdapterError`] refusal (C# DeviceAdapterException).
//!
//! Verbs are blocking and run on the worker thread, one command at a time (like the C#
//! agent, whose vendor calls are synchronous). Long waits must poll [`Cx::is_cancelled`] or
//! use [`Cx::sleep`] and return [`Cx::cancelled`] once cancelled; the processor reports that
//! as DEVICE_TIMEOUT, or posts nothing when the service is stopping. Any other unexpected
//! failure is [`adapter_failed`] (C# "any other exception"). A panic in a verb is caught and
//! reported as DEVICE_ADAPTER_FAILED too.

mod cx;
pub mod lock;
mod not_connected;
pub mod pos;
mod simulated;

use dl_json::Value;

use crate::config::{DotnetError, LockOptions};
pub use crate::contracts::AdapterError;
use crate::contracts::{
    CancelAllRequest, EraseRequest, KeyCardRequest, PosDisplayRequest, PosKickRequest,
    PosPrintRequest, PosReadRequest, PosSpeakRequest, ReadRequest, codes,
};
pub use cx::{CANCELLED, Cx};
pub use not_connected::NotConnectedAdapter;
pub use simulated::SimulatedLockAdapter;

/// `NotConnectedAdapter.Name`: the production default, every verb answers DEVICE_NOT_CONNECTED.
pub const NOT_CONNECTED: &str = "none";

/// `SimulatedLockAdapter.Name`: the in-memory lock encoder for development.
pub const SIMULATED_LOCK: &str = "simulated-lock";

/// `SmileLockRouter.Name`: the SMILE_FO door-lock vendors.
pub const SMILE_LOCK: &str = "smile-lock";

/// `DeviceAdapters.Names`: the values `Agent:Adapter` accepts, in the C# order (the
/// validator message lists them in this order). Compared case-sensitively, like C#.
pub const NAMES: [&str; 3] = [NOT_CONNECTED, SIMULATED_LOCK, SMILE_LOCK];

/// One door-lock device family (IDeviceAdapter, spec §6.7 verbs). Results are JSON for the
/// API; refusals are [`AdapterError`] with a [`codes`] value.
pub trait DeviceAdapter: Send + Sync {
    /// Short family name used in logs and error messages (`Family`).
    fn family(&self) -> &str;

    /// lock.issue: encode new key cards for a stay.
    fn issue(&self, request: &KeyCardRequest, cx: &Cx) -> Result<Value, AdapterError>;

    /// lock.duplicate: encode extra cards without invalidating existing ones.
    fn duplicate(&self, request: &KeyCardRequest, cx: &Cx) -> Result<Value, AdapterError>;

    /// lock.read: read the card on the encoder.
    fn read(&self, request: &ReadRequest, cx: &Cx) -> Result<Value, AdapterError>;

    /// lock.erase: wipe the card on the encoder (VB6 tmpLockLink CO); it must be the named card.
    fn erase(&self, request: &EraseRequest, cx: &Cx) -> Result<Value, AdapterError>;

    /// lock.extend: change the end time of a stay's cards.
    fn extend(&self, request: &KeyCardRequest, cx: &Cx) -> Result<Value, AdapterError>;

    /// lock.cancelAll: invalidate every card of a room.
    fn cancel_all(&self, request: &CancelAllRequest, cx: &Cx) -> Result<Value, AdapterError>;
}

/// The POS hardware verbs (IPosDevice; POS-PRT-03, HW-01..06). The device is named by the
/// payload's `target`, so one agent drives every POS device of its workstation.
pub trait PosDevice: Send + Sync {
    /// pos.print.
    fn print(&self, request: &PosPrintRequest, cx: &Cx) -> Result<Value, AdapterError>;

    /// pos.kick.
    fn kick(&self, request: &PosKickRequest, cx: &Cx) -> Result<Value, AdapterError>;

    /// pos.display.
    fn display(&self, request: &PosDisplayRequest, cx: &Cx) -> Result<Value, AdapterError>;

    /// pos.read.
    fn read(&self, request: &PosReadRequest, cx: &Cx) -> Result<Value, AdapterError>;

    /// pos.speak.
    fn speak(&self, request: &PosSpeakRequest, cx: &Cx) -> Result<Value, AdapterError>;
}

/// The C# catch-all for an unexpected exception in a verb: DEVICE_ADAPTER_FAILED with
/// `"{exception_type}: {message}"` (for example `"IOException: The port is closed."`). The
/// processor logs it at error level.
pub fn adapter_failed(exception_type: &str, message: &str) -> AdapterError {
    AdapterError::new(
        codes::ADAPTER_FAILED,
        format!("{exception_type}: {message}"),
    )
}

/// `DeviceAdapters.Create(name, lockOptions)`: the single composition point for the
/// door-lock device. An unknown name is the C# `ArgumentOutOfRangeException`.
pub fn create(name: &str, lock: &LockOptions) -> Result<Box<dyn DeviceAdapter>, DotnetError> {
    match name {
        NOT_CONNECTED => Ok(Box::new(NotConnectedAdapter)),
        SIMULATED_LOCK => Ok(Box::new(SimulatedLockAdapter::new())),
        SMILE_LOCK => Ok(lock::smile_lock(lock)),
        _ => {
            let newline = if cfg!(windows) { "\r\n" } else { "\n" };
            Err(DotnetError::new(
                "System.ArgumentOutOfRangeException",
                format!(
                    "Unknown device adapter; expected one of {} (Parameter 'name'){newline}Actual value was {name}.",
                    NAMES.join(", ")
                ),
            ))
        }
    }
}
