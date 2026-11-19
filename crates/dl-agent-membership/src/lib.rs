//! Azolo Membership module for `dl-agent`: a port of the .NET `MM.Membership.Agent`
//! (read side M1, write side M2).
//!
//! The module heartbeats the Membership hub, reads `sync-config`, and runs the read-only
//! SMILE scanners that report to the hub: availability (occupancy per room type and
//! night, extra-bed usage), room inventory and room-type catalogue, and folio
//! observations behind a cursor. Every JSON body matches what the .NET agent sends for
//! the same SMILE data byte for byte ([`stj`]).
//!
//! Hub jobs (bookings written into SMILE, folio links, profile updates) run only when
//! `read_only` is `false` ([`jobs`], [`writer`]), behind the SMILE write gate. Read-only
//! (the default), pulled jobs are counted and left, so the hub's lease expires.
//!
//! The agent identity lives where the .NET installer put it:
//! `%ProgramData%\MM.Membership.Agent\config.dpapi`, sealed with DPAPI (LocalMachine,
//! entropy `MM.Membership.Agent.v1`), so an enrolled machine needs no new enrolment.

pub mod availability;
pub mod checkout;
pub mod config;
pub mod earn;
pub mod enroll;
pub mod folio;
pub mod hub;
pub mod identity;
pub mod jobs;
pub mod log;
pub mod module;
pub mod rooms;
pub mod settings;
pub mod smile;
pub mod stj;
pub mod sync;
pub mod wire;
pub mod writer;

pub use checkout::tool::membership_backfill_tool;
pub use enroll::tool::membership_enroll_tool;
pub use module::selftest::membership_selftest;
pub use module::{MembershipModule, membership_module};
