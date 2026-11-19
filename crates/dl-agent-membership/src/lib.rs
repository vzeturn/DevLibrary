//! Azolo Membership module for `dl-agent`: the read side of the .NET
//! `MM.Membership.Agent` (milestone M1 of the port).
//!
//! The module heartbeats the Membership hub, reads `sync-config`, and runs the read-only
//! SMILE scanners that report to the hub: availability (occupancy per room type and
//! night, extra-bed usage), room inventory and room-type catalogue, and folio
//! observations behind a cursor. Every JSON body matches what the .NET agent sends for
//! the same SMILE data byte for byte ([`stj`]).
//!
//! Hub jobs (bookings written into SMILE, folio links, profile updates) belong to the
//! write side (M2). In M1 the module may pull jobs to count them, but never executes or
//! acknowledges one, so the hub's lease expires and the .NET agent (or M2) picks it up.
//!
//! The agent identity lives where the .NET installer put it:
//! `%ProgramData%\MM.Membership.Agent\config.dpapi`, sealed with DPAPI (LocalMachine,
//! entropy `MM.Membership.Agent.v1`), so an enrolled machine needs no new enrolment.

pub mod availability;
pub mod config;
pub mod enroll;
pub mod folio;
pub mod hub;
pub mod identity;
pub mod log;
pub mod module;
pub mod rooms;
pub mod settings;
pub mod smile;
pub mod stj;
pub mod sync;
pub mod wire;

pub use enroll::tool::membership_enroll_tool;
pub use module::{MembershipModule, membership_module};
