//! Azolo Check-In as a dl-agent module (`modules.arrival`).
//!
//! A port of the .NET site agent `MM.Arrival.Agent` and its wire contract
//! `MM.Arrival.Sync.Contracts`: the agent long-polls the Check-In cloud over HTTPS, runs the
//! allow-listed SMILE PMS procedures the cloud asks for, and answers with AES-GCM sealed
//! envelopes. Envelopes, canonical hashes, deltas and projections are byte-identical to the
//! .NET code, checked against fixtures the .NET code wrote (`tests/golden_*.rs`).
//!
//! Register it with [`arrival_module`] and [`arrival_enroll_tool`]; it runs only where the
//! config sets `modules.arrival.enabled = true`.

pub mod agent;
pub mod contracts;
pub mod crypto;
pub mod legacy;
pub mod legacy_sql;
pub mod module;
pub mod netdec;
pub mod stj;
pub mod tools;

mod error;

pub use error::Error;
pub use module::arrival_module;
pub use module::selftest::arrival_selftest;
pub use tools::arrival_enroll_tool;
