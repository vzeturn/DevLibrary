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
