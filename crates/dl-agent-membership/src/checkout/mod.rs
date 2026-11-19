//! The checkout scanner (`Worker/CheckoutScanner.cs`): earn for bookings whose guests
//! checked out in SMILE, walk-in earn by the first guest's phone, the PMS-cancelled report
//! and the daily summary. It runs on the scan thread every `checkout_interval_seconds`
//! and only reads SMILE.
//!
//! Not ported: the extra-bed terms and special-service sync that rides on the .NET scan
//! cycle (`ExtraBedTermsSync`).

pub mod backfill;
pub mod scanner;
pub mod settings;
pub mod sql;
pub mod tally;
pub mod tool;
pub mod walk_in;

#[cfg(test)]
pub(crate) mod tests;

pub use scanner::{CheckoutDb, CheckoutError, CheckoutOutcome, CheckoutScanner};
pub use settings::CheckoutSettings;
pub use walk_in::WalkInEarnState;
