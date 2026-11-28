//! `txn.counters` (ADR 0161, RT-0848; `SiteCommandExecutor.Counters.cs`): raises each listed
//! `dbo.Para` counter of the RVC to the cloud's floor when it is lower, never lowering it, in
//! one transaction with the command log row (a replay returns the recorded result and writes
//! nothing). Each counter row is update-locked before it is read, so `GetRVCSeqNumber` cannot
//! take a number between the read and the raise and continues above the raised value. The
//! result also carries the highest counter in the backing tables, so the cloud's handover
//! seeds above every number the store holds. A counter kept in a per-RVC object (RT-0654) is
//! refused whole.

use super::SiteCommandExecutor;
use super::txn::REFUSED;
use crate::commands::error::{CommandError, Result};
use crate::commands::units::SiteCommandUnit;
use crate::contracts::WireJson;
use crate::contracts::wire::{
    SyncCommand, SyncCommandResult, TxnCounterState, TxnCountersPayload, TxnCountersResult,
    txn_counter_names, txn_reasons,
};
use crate::reporting::Value;
use crate::util::ctx::Ctx;

impl SiteCommandExecutor {
    /// `txn.counters` (`TxnCountersAsync`).
    pub(super) fn txn_counters(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let Some(p) = TxnCountersPayload::from_element(&c.payload) else {
            return Ok(self.invalid(c, "payload is not a txn.counters object"));
        };
        if let Some(problem) = p.problem() {
            return Ok(self.invalid(c, &problem));
        }
        if !c.aggregate_key.is_empty()
            && c.aggregate_key != txn_counter_names::aggregate_key(p.rvc_no)
        {
            return Ok(self.invalid(c, "AggregateKey does not match RvcNo"));
        }
        let mut refused: Option<&'static str> = None;
        let r = self.store.run_once(
            c.command_id,
            &mut |unit: &mut dyn SiteCommandUnit, t: &Ctx| {
                refused = None;
                let counters = unit.counters();
                let mut states = Vec::with_capacity(p.counters.len());
                for name in txn_counter_names::ALL {
                    let Some(floor) = p.counters.iter().find(|f| f.name == name) else {
                        continue;
                    };
                    if counters.has_own_counter_object(p.rvc_no, name, t)? {
                        refused = Some(txn_reasons::COUNTER_OBJECT_UNSUPPORTED);
                        return Err(CommandError::invalid(REFUSED));
                    }
                    let before = counters.lock(p.rvc_no, name, t)?;
                    let mut stored = before;
                    if floor.floor > before {
                        counters.raise(p.rvc_no, name, floor.floor, t)?;
                        stored = floor.floor;
                    }
                    states.push(TxnCounterState {
                        name: name.to_owned(),
                        before,
                        stored,
                        in_use: counters.in_use(p.rvc_no, name, t)?,
                    });
                }
                let summary: Vec<String> = states
                    .iter()
                    .map(|s| {
                        let in_use = s.in_use.map_or_else(|| "-".to_owned(), |n| n.to_string());
                        format!("{} {}->{} (in use {in_use})", s.name, s.before, s.stored)
                    })
                    .collect();
                self.log.info(
                    "Site command {Id} (txn.counters) RVC {Rvc}: {Counters}",
                    &[
                        Value::from(c.command_id),
                        Value::from(p.rvc_no),
                        Value::from(summary.join(", ").as_str()),
                    ],
                );
                let result = TxnCountersResult {
                    rvc_no: p.rvc_no,
                    counters: states,
                };
                Ok(self.applied(c, Some(result.to_element())))
            },
            ctx,
        );
        match refused {
            Some(reason) => {
                self.log.warn(
                    "Site command {Id} (txn.counters) refused: RVC {Rvc} {Reason}",
                    &[
                        Value::from(c.command_id),
                        Value::from(p.rvc_no),
                        Value::from(reason),
                    ],
                );
                Ok(self.reject(c, reason))
            }
            None => r,
        }
    }
}
