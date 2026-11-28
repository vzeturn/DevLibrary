//! The fake's `master.upsert` entity `cashier` (ADR 0150): `dbo.CashierDef` + RVC +
//! mapped permissions by `Initial`, keyed case-insensitively like SQL Server.

use dl_agent_pda_sync::commands::{CashierApply, CashierCommandUnit, CommandError, Result};
use dl_agent_pda_sync::contracts::wire::StaffClerk;
use dl_agent_pda_sync::staff::source;
use dl_agent_pda_sync::util::ctx::Ctx;

use super::Unit;

/// One store clerk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeCashier {
    pub key: String,
    pub name: String,
    pub active: bool,
    pub security: Option<i32>,
    pub touch_id: Option<String>,
    pub rvcs: Vec<i32>,
    pub functions: Vec<String>,
}

impl FakeCashier {
    /// The clerk as the staff sync projects it.
    pub fn clerk(&self, pin_key: &[u8]) -> StaffClerk {
        source::to_clerk(
            pin_key,
            &self.key,
            &self.name,
            self.active,
            self.security,
            self.touch_id.as_deref(),
            &self.rvcs,
            &self.functions,
        )
    }
}

/// The map key of a clerk key (`StringComparer.OrdinalIgnoreCase`).
pub fn cashier_key(key: &str) -> String {
    key.to_uppercase()
}

impl CashierCommandUnit for Unit<'_> {
    fn read(&mut self, key: &str, pin_key: &[u8], _: &Ctx) -> Result<Option<StaffClerk>> {
        Ok(self
            .s
            .cashiers
            .get(&cashier_key(key))
            .map(|c| c.clerk(pin_key)))
    }

    fn pin_taken(&mut self, key: &str, pin: &str, _: &Ctx) -> Result<bool> {
        Ok(self.s.cashiers.values().any(|c| {
            c.active
                && !c.key.eq_ignore_ascii_case(key)
                && c.touch_id.as_deref().map(str::trim_end) == Some(pin)
        }))
    }

    fn apply(&mut self, apply: &CashierApply<'_>, _: &Ctx) -> Result<Option<String>> {
        if let Some(refusal) = self.owner.knobs().cashier_refusal.clone() {
            return Ok(Some(refusal));
        }
        let current = self.s.cashiers.get(&cashier_key(apply.key)).cloned();
        if apply.create == current.is_some() {
            return Err(CommandError::invalid("create/update mismatch"));
        }
        self.s.cashiers.insert(
            cashier_key(apply.key),
            FakeCashier {
                key: apply.key.to_owned(),
                name: apply.name.to_owned(),
                active: apply.active,
                security: Some(apply.security),
                touch_id: apply
                    .pin
                    .map(str::to_owned)
                    .or_else(|| current.and_then(|c| c.touch_id)),
                rvcs: apply.rvcs.to_vec(),
                functions: apply.allowed.to_vec(),
            },
        );
        Ok(None)
    }
}
