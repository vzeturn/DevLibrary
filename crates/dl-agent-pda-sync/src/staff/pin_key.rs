//! Port of `Staff/PinKeyProvider.cs`: the tenant's PIN key `K_t` on the store machine
//! (ADR 0101), fetched once over the signed channel and kept in the secret store next to
//! the agent key.

use super::StaffError;
use super::cloud::{StaffCloud, StaffCloudResult, StaffCloudStatus, StaffPinKey};
use crate::contracts::staff;
use crate::security::{SharedSecretStore, names};
use crate::util::ctx::Ctx;
use std::sync::Arc;

/// The PIN key from the secret store, or from the cloud when not stored
/// (`PinKeyProvider`). The agent never holds the cloud's pepper, only this key of its
/// own tenant.
pub struct PinKeyProvider {
    secrets: SharedSecretStore,
    cloud: Arc<dyn StaffCloud>,
}

impl std::fmt::Debug for PinKeyProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinKeyProvider").finish_non_exhaustive()
    }
}

impl PinKeyProvider {
    /// A provider over `secrets` and `cloud`.
    pub fn new(secrets: SharedSecretStore, cloud: Arc<dyn StaffCloud>) -> PinKeyProvider {
        PinKeyProvider { secrets, cloud }
    }

    /// The key, or the failed cloud answer when it had to be fetched and could not be. A
    /// stored value that is not 32 bytes is fetched again.
    pub fn get(&self, ctx: &Ctx) -> Result<StaffCloudResult, StaffError> {
        if let Some(stored) = self.secrets.load(names::PIN_KEY)?
            && stored.len() == staff::KEY_BYTES
        {
            let key_id = staff::key_id(&stored);
            return Ok(StaffCloudResult {
                status: StaffCloudStatus::Ok,
                http_status: 0,
                pin_key: Some(StaffPinKey {
                    key: stored,
                    key_id,
                }),
            });
        }
        let fetched = self.cloud.get_pin_key(ctx)?;
        if fetched.status == StaffCloudStatus::Ok
            && let Some(key) = &fetched.pin_key
        {
            self.secrets.save(names::PIN_KEY, &key.key)?;
        }
        Ok(fetched)
    }

    /// Drops the stored key (the cloud answered that it changed).
    pub fn invalidate(&self) -> Result<(), StaffError> {
        self.secrets.delete(names::PIN_KEY)?;
        Ok(())
    }
}
