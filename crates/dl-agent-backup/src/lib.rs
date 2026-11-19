//! SRBackup as a dl-agent module (`modules.backup`).
//!
//! A port of the SRBackup site agent (Go, `agent/internal/*`): it polls the SRBackup hub for
//! SQL Server servers, databases and schedules, runs `BACKUP DATABASE` / `BACKUP LOG` when a
//! schedule is due, uploads the file to the schedule's destination, applies retention,
//! restores on request and answers the hub's connection tests.

pub mod backup;
pub mod core;
pub mod crypto;
pub mod discovery;
pub mod enroll;
pub mod error;
pub mod executor;
pub mod hub;
pub mod log;
pub mod module;
pub mod report;
pub mod retention;
pub mod schedule;
pub mod seal;
pub mod sql;
pub mod state;
pub mod sync;
pub mod upload;

#[cfg(test)]
mod fakes;
#[cfg(test)]
mod testhub;
#[cfg(test)]
mod testutil;

pub use enroll::tool::backup_enroll_tool;
pub use error::Error;
pub use module::backup_module;
pub use module::selftest::backup_selftest;
