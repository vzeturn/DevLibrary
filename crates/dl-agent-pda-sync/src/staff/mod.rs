//! Staff sync and PIN hashing (port of `Staff/`, ADR 0101): the POS clerk list, hashed
//! with the tenant's PIN key, becomes the cloud's staff accounts.
//!
//! - [`pin_hasher`]: `StaffPinHasher.cs`.
//! - [`cloud`]: `StaffCloudClient.cs` (`/sync/v1/staff`, `/sync/v1/staff/pin-key`).
//! - [`pin_key`]: `PinKeyProvider.cs`.
//! - [`source`]: `SqlStaffSource.cs`.
//! - [`runner`]: `StaffSyncRunner.cs`.
//! - [`hosting`]: `StaffSyncHosting.cs` (the loop and its `after_capture` factory).

pub mod cloud;
pub mod hosting;
pub mod pin_hasher;
pub mod pin_key;
pub mod runner;
pub mod source;

pub use cloud::{StaffCloud, StaffCloudClient, StaffCloudResult, StaffCloudStatus, StaffPinKey};
pub use hosting::{STATE_FILE_NAME, StaffSyncService};
pub use pin_key::PinKeyProvider;
pub use runner::{StaffSyncOutcome, StaffSyncParts, StaffSyncRunner, StaffSyncState};
pub use source::{SqlStaffSource, StaffSource};

/// What a staff cycle lets escape (a SQL or secret store failure, cancellation): the loop
/// restarts with backoff, as the C# `ResilientService` does.
pub type StaffError = Box<dyn std::error::Error + Send + Sync>;
