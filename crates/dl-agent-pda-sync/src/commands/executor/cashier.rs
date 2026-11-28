//! `master.upsert` entity `cashier` (ADR 0150) on the executor
//! (`SiteCommandExecutor.Cashier.cs`): the clerk is read update-locked and hashed as the
//! staff sync projects it, the write goes ahead only on the hash the editor saw, and the
//! sealed PIN is opened only here and never logged.

use super::SiteCommandExecutor;
use crate::commands::cashier::cashier_reasons;
use crate::commands::error::{CommandError, Result};
use crate::commands::master::master_reasons;
use crate::commands::units::{CashierApply, SiteCommandUnit};
use crate::contracts::WireJson;
use crate::contracts::cashier::{self, CashierWrite};
use crate::contracts::staff;
use crate::contracts::wire::{
    CommandStatus, MasterUpsertPayload, MasterUpsertResult, SyncCommand, SyncCommandResult,
};
use crate::reporting::Value;
use crate::util::ctx::Ctx;

impl SiteCommandExecutor {
    /// `CashierUpsertAsync`.
    pub(super) fn cashier_upsert(
        &self,
        c: &SyncCommand,
        p: &MasterUpsertPayload,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        let Some(support) = self.cashiers.clone() else {
            return Ok(self.reject(c, master_reasons::UNSUPPORTED_ENTITY));
        };
        let write: CashierWrite = match CashierWrite::parse(&p.fields) {
            Ok(w) => w,
            Err(problem) => return Ok(self.invalid(c, &problem)),
        };
        let key = p.key.as_str();
        if !cashier::is_writable_key(key) {
            return Ok(self.invalid(
                c,
                &format!(
                    "Key must be 1 to {} printable characters and not SML",
                    cashier::MAX_KEY_LENGTH
                ),
            ));
        }
        if p.expected_hash.is_none() && write.pin_sealed.is_none() {
            return Ok(self.invalid(c, "a new cashier needs a PIN"));
        }

        let pin_key = support
            .pin_key(write.pin_key_id.as_deref(), ctx)
            .map_err(|e| {
                CommandError::invalid(format!("The staff PIN key could not be read: {e}"))
            })?;
        let Some(pin_key) = pin_key else {
            return Ok(self.reject(c, cashier_reasons::PIN_KEY_UNAVAILABLE));
        };
        let pin = match write.pin_sealed.as_deref() {
            None => None,
            Some(sealed) => match cashier::open_pin(&pin_key.key, key, Some(sealed)) {
                Some(p) if staff::is_usable(Some(p.as_str())) => Some(p),
                _ => return Ok(self.invalid(c, "pin_sealed does not open for this clerk")),
            },
        };

        let result = self.store.run_once(
            c.command_id,
            &mut |unit: &mut dyn SiteCommandUnit, t: &Ctx| {
                let cashiers = unit.cashiers();
                let current = cashiers.read(key, &pin_key.key, t)?;
                if current.is_none() && p.expected_hash.is_some() {
                    return Ok(self.reject(c, master_reasons::ROW_NOT_FOUND));
                }
                if let Some(cur) = &current
                    && Some(cashier::row_hash(cur).as_str()) != p.expected_hash.as_deref()
                {
                    self.log.warn(
                        "Site command {Id} (master.upsert) refused: cashier {Key} changed on the site since the editor read it",
                        &[Value::from(c.command_id), Value::from(key)],
                    );
                    return Ok(self.reject(c, master_reasons::CHANGED));
                }
                if let Some(pin) = &pin
                    && cashiers.pin_taken(key, pin.as_str(), t)?
                {
                    return Ok(self.reject(c, cashier_reasons::PIN_TAKEN));
                }

                let store_key = current
                    .as_ref()
                    .map_or_else(|| key.to_owned(), |cur| cur.key.clone());
                let apply = CashierApply {
                    key: &store_key,
                    create: current.is_none(),
                    name: &write.name,
                    active: write.active,
                    security: write.security,
                    pin: pin.as_ref().map(|p| p.as_str()),
                    rvcs: &write.rvcs,
                    allowed: &write.functions,
                };
                if let Some(refusal) = cashiers.apply(&apply, t)? {
                    return Ok(self.reject(c, &refusal));
                }
                let Some(after) = cashiers.read(&store_key, &pin_key.key, t)? else {
                    return Err(CommandError::invalid(format!(
                        "Cashier {store_key} vanished inside its own write transaction."
                    )));
                };
                let result = MasterUpsertResult {
                    entity: cashier::ENTITY.to_owned(),
                    key: store_key.clone(),
                    row_hash: cashier::row_hash(&after),
                };
                Ok(self.applied(c, Some(result.to_element())))
            },
            ctx,
        )?;
        drop(pin);
        if result.status == CommandStatus::Applied {
            support.clerks_changed();
        }
        Ok(result)
    }
}
