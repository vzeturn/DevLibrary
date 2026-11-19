//! What a fresh dl-agent enrol keeps from the .NET agent it replaces.
//!
//! The pilot rollout removes every old agent (the hub bench's `remove_legacy`: stop, delete
//! the service, rename its folder, never delete) and enrolls dl-agent afresh; there is no
//! in-place takeover of the old identity. Two things still come from the old install:
//!
//! * [`tenant_key`]: the tenant master key, so the legal vault files and the cloud's wrapped
//!   data keys stay readable (legal continuity, a data-loss guard);
//! * [`sqlauth`]: the PMS connection string and its sign-in mode (Windows or SQL login),
//!   preselected for the operator.
//!
//! [`installs`] finds the old folders, in place or renamed by the bench.

pub mod installs;
pub mod sqlauth;
pub mod tenant_key;
