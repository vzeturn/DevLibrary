#![doc = include_str!("../README.md")]

mod error;
mod raw;
mod runtime;
mod wide;

#[cfg(windows)]
#[path = "sys_windows.rs"]
mod sys;

#[cfg(not(windows))]
#[path = "sys_stub.rs"]
mod sys;

pub mod acl;
pub mod control;
pub mod dpapi;
pub mod process;
pub mod scm;

pub use error::{Error, codes};
pub use runtime::{StopSignal, is_service_process, run_service, run_service_with_exit_code};
pub use wide::{command_line, escape_arg};
