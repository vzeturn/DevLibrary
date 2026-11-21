//! SMILE PMS device agent as the dl-agent module `pms-device`: claims device commands
//! (door-lock encoders, POS hardware) from the PMS API, runs them on the workstation's
//! devices and reports the results. Port of `SmilePmsDeviceAgent` (.NET, then its Rust
//! port in the SMILE-PMS repo), built on DevLibrary only; it reads the same configuration
//! and data files and speaks the same wire contract.
//!
//! Register [`pms_device_module`] with `dl_agent::cli::App::module`; the module is opt-in
//! (`modules.pms-device.enabled = true`). See the crate README for the settings and the
//! switch-over from the Windows service.

// `deny`, not `forbid`: `native` (vendor DLL calls) is the one module allowed `unsafe`.
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod adapters;
pub mod agent;
pub mod config;
pub mod contracts;
pub mod module;
pub mod native;
pub mod queue;
pub mod tools;

pub use module::selftest::pms_device_selftest;
pub use module::{Hooks, NAME, PmsDeviceModule, pms_device_module, pms_device_module_with};
pub use tools::pms_device_enroll_tool;
