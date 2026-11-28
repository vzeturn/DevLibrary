//! Port of `Staff/StaffSyncNudge.cs` (ADR 0150): the wake-up of the staff sync loop after
//! a cloud cashier write, and [`CashierSupport`], the command executor's view of the
//! staff sync (PIN key by key id, nudge).

use std::sync::Arc;
use std::time::Duration;

use super::StaffError;
use super::cloud::StaffPinKey;
use super::pin_key::PinKeyProvider;
use crate::capture::UploadWake;
use crate::commands::CashierCommandSupport;
use crate::util::ctx::Ctx;

/// Wakes the staff sync loop before its interval ends (`StaffSyncNudge`). Signals coalesce.
#[derive(Debug, Default)]
pub struct StaffSyncNudge {
    wake: UploadWake,
}

impl StaffSyncNudge {
    /// A nudge with no wake-up pending.
    pub fn new() -> StaffSyncNudge {
        StaffSyncNudge::default()
    }

    /// `Nudge`: one pending wake-up is enough.
    pub fn nudge(&self) {
        self.wake.pulse();
    }

    /// `WaitAsync`: waits `interval`, or less when nudged; false when `ctx` is done.
    pub fn wait(&self, interval: Duration, ctx: &Ctx) -> bool {
        self.wake.wait(interval, ctx);
        !ctx.is_done()
    }
}

/// [`CashierCommandSupport`] over the staff sync's [`PinKeyProvider`] and
/// [`StaffSyncNudge`] (`CashierCommandSupport`).
pub struct CashierSupport {
    keys: Arc<PinKeyProvider>,
    nudge: Arc<StaffSyncNudge>,
}

impl std::fmt::Debug for CashierSupport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CashierSupport").finish_non_exhaustive()
    }
}

impl CashierSupport {
    /// The support over `keys` and `nudge`.
    pub fn new(keys: Arc<PinKeyProvider>, nudge: Arc<StaffSyncNudge>) -> CashierSupport {
        CashierSupport { keys, nudge }
    }
}

impl CashierCommandSupport for CashierSupport {
    fn pin_key(&self, key_id: Option<&str>, ctx: &Ctx) -> Result<Option<StaffPinKey>, StaffError> {
        let got = self.keys.get(ctx)?.pin_key;
        match got {
            Some(key) if key_id.is_none_or(|id| key.key_id == id) => return Ok(Some(key)),
            None => return Ok(None),
            Some(_) => {}
        }
        let Some(key_id) = key_id else {
            return Ok(None);
        };
        // The stored key is older than the one the cloud sealed with: fetch the current one once.
        self.keys.invalidate()?;
        Ok(self
            .keys
            .get(ctx)?
            .pin_key
            .filter(|fresh| fresh.key_id == key_id))
    }

    fn clerks_changed(&self) {
        self.nudge.nudge();
    }
}
