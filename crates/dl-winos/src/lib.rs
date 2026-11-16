#![doc = include_str!("../README.md")]

mod error;
mod wide;

#[cfg(windows)]
#[path = "sys_windows.rs"]
mod sys;

#[cfg(not(windows))]
#[path = "sys_stub.rs"]
mod sys;

#[cfg(windows)]
#[path = "com_windows.rs"]
mod com_sys;

#[cfg(not(windows))]
#[path = "com_stub.rs"]
mod com_sys;

#[cfg(windows)]
#[path = "dll_windows.rs"]
mod dll_sys;

#[cfg(not(windows))]
#[path = "dll_stub.rs"]
mod dll_sys;

pub mod com;
pub mod dll;
pub mod registry;
pub mod spooler;

pub use error::{Error, codes};
