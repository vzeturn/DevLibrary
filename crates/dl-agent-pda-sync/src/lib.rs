#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod util;

/// Wire DTOs, sync catalog, row hashing, legacy bundle (port of `Sync.Contracts`).
pub mod contracts;

/// Capture engine (port of `Sync.Capture`) and the agent's capture runner.
pub mod capture;

/// SQL bootstrap, enrollment workflow and probes (`Bootstrap/`).
pub mod bootstrap;
/// Cloud command channel and SQL command units (`Commands/`).
pub mod commands;
/// Agent options, paths, identity, health and error codes (`Config/`).
pub mod config;
/// Site event queue statistics (`Diagnostics/`; the bundle is dl-agent's).
pub mod diagnostics;
/// Composition root, background loops, `enroll` and `grants` (`Hosting/`); the Windows
/// service, self-update and diagnostics bundle are dl-agent's.
pub mod hosting;

/// The cloud's `legacy.export` command: the migrate script's export and upload, run by the agent.
pub mod legacy_export;
/// The old SyncAgent's SQL login, for the installer (RT-0303).
pub mod legacy_sql;
/// The dl-agent module, its tools, self-test and hub commands.
pub mod module;
/// SQLite outboxes and uploaders: batches, blobs, heartbeat (`Outbox/`).
pub mod outbox;
/// Print relay: hub jobs to spooler or raw TCP printers (`PrintRelay/`).
pub mod printrelay;
/// Logging, site events, health and tool runs (`Reporting/`).
pub mod reporting;
/// Secret store and agent signing key (`Security/`).
pub mod security;
/// Staff sync and PIN hashing (`Staff/`).
pub mod staff;
/// Cloud HTTP client, request signing, token and enrollment (`Transport/`).
pub mod transport;

pub use module::selftest::pda_sync_selftest;
pub use module::tool::{pda_enroll_tool, pda_grants_tool};
pub use module::{PdaSyncModule, pda_sync_module};
