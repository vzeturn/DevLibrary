//! Port of `Commands/SiteCommandExecutor.Cashier.cs` (ADR 0150): `master.upsert` entity
//! `cashier`, one cashier across `CashierDef`, `CashierRVC` and the mapped
//! `ClerkPermission` rows. The clerk is read update-locked and hashed exactly as the staff
//! sync projects it; the write goes ahead only when that hash is the one the editor saw
//! (or, for a create, when no such clerk exists), so a POS WIN edit is never overwritten.
//! The sealed PIN is opened only here, checked against the other active clerks and
//! written to `TouchID`; it is never logged. The write itself runs in
//! `executor/cashier.rs`.

use crate::staff::{StaffError, StaffPinKey};
use crate::util::ctx::Ctx;

/// What the cashier write needs from the staff sync (`ICashierCommandSupport`): the
/// tenant PIN key and a way to send the clerk list now.
pub trait CashierCommandSupport: Send + Sync {
    /// The tenant's PIN key `K_t`; with `key_id` only a key of that id (a stale stored key
    /// is dropped and fetched again once). `None` when the key cannot be had.
    fn pin_key(
        &self,
        key_id: Option<&str>,
        ctx: &Ctx,
    ) -> std::result::Result<Option<StaffPinKey>, StaffError>;

    /// Asks the staff sync to read and send the clerk list now, so the cloud mirror shows
    /// the write within seconds.
    fn clerks_changed(&self);
}

/// Rejection reasons of `master.upsert` entity `cashier` (`CashierReasons`), besides
/// [`master_reasons`](super::master::master_reasons).
pub mod cashier_reasons {
    /// The tenant PIN key (of the sealed PIN's key id) cannot be had.
    pub const PIN_KEY_UNAVAILABLE: &str = "cashier_pin_key_unavailable";
    /// Another active clerk signs in with the new PIN.
    pub const PIN_TAKEN: &str = "cashier_pin_taken";
    /// This store's `CashierDef` cannot hold the write.
    pub const SCHEMA_UNSUPPORTED: &str = super::super::sql_cashier::SCHEMA_UNSUPPORTED;
}
