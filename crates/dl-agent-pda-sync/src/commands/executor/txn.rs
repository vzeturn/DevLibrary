//! `txn.snapshot` of the executor (`SiteCommandExecutor.Txn.cs`, ADR 0159, RT-0849): the
//! store-state refusals (unknown column, bad value, key conflict) leave `run_once` with an
//! error so the transaction rolls back with no log row; see [`crate::commands::txn`] for
//! the unit seam and the aggregate hash.

use std::sync::Arc;

use super::{SiteCommandExecutor, reasons};
use crate::commands::error::{CommandError, Result};
use crate::commands::txn::{TxnCommandUnit, convert_rows, read_aggregate, read_plan};
use crate::commands::txn_value::TxnTableWrite;
use crate::commands::units::SiteCommandUnit;
use crate::contracts::WireJson;
use crate::contracts::mapping::CatalogRowMapper;
use crate::contracts::wire::{
    SyncCommand, SyncCommandResult, TxnSnapshotPayload, TxnSnapshotResult, txn_outcomes,
    txn_reasons, txn_scopes,
};
use crate::reporting::Value;
use crate::util::ctx::Ctx;

/// The message of the error that leaves `run_once` on a store-state refusal.
pub(super) const REFUSED: &str = "txn.snapshot refused by the store";

impl SiteCommandExecutor {
    /// Runs `txn.snapshot` and `master.upsert` with `mapper` (the catalog mapper capture
    /// hashes with; without one the type is unknown).
    pub fn with_mapper(mut self, mapper: Arc<CatalogRowMapper>) -> Self {
        self.mapper = Some(mapper);
        self
    }

    pub(super) fn txn_snapshot(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let Some(p) = TxnSnapshotPayload::from_element(&c.payload) else {
            return Ok(self.invalid(c, "payload is not a txn.snapshot object"));
        };
        if let Some(problem) = p.problem() {
            return Ok(self.invalid(c, &problem));
        }
        if !c.aggregate_key.is_empty()
            && c.aggregate_key != txn_scopes::aggregate_key(&p.scope, p.scope_key)
        {
            return Ok(self.invalid(c, "AggregateKey does not match Scope and ScopeKey"));
        }
        let Some(mapper) = self.mapper.clone() else {
            return Ok(self.reject(c, reasons::UNKNOWN_TYPE));
        };
        let Some((scope_column, tables)) = txn_scopes::find(&p.scope) else {
            return Ok(self.invalid(c, "Scope is not allowed."));
        };
        let aggregate = p.aggregate();
        let mut plans = Vec::with_capacity(tables.len());
        for table in tables {
            plans.push(read_plan(&mapper, table)?);
        }
        // A refusal that depends on the store's state (unknown column, bad value, key
        // conflict) leaves run_once with an error so the transaction rolls back with no
        // log row: a redrive of the same command id runs again once the store is fixed.
        let mut refused: Option<&'static str> = None;
        let r = self.store.run_once(
            c.command_id,
            &mut |unit: &mut dyn SiteCommandUnit, t: &Ctx| {
                let unit = unit.txn();
                let mut writes = Vec::with_capacity(tables.len());
                let mut live = Vec::with_capacity(tables.len());
                for (table, (_, tcvn3)) in tables.iter().zip(&plans) {
                    let columns = unit.columns(table, t)?;
                    match convert_rows(&columns, p.rows_of(table), tcvn3) {
                        Ok(rows) => writes.push(TxnTableWrite {
                            table: (*table).to_owned(),
                            rows,
                        }),
                        Err(reason) => {
                            self.log.warn(
                                "Site command {Id} (txn.snapshot) refused: {Aggregate} table {Table} {Reason}",
                                &[
                                    Value::from(c.command_id),
                                    Value::from(aggregate.as_str()),
                                    Value::from(*table),
                                    Value::from(reason),
                                ],
                            );
                            refused = Some(reason);
                            return Err(CommandError::invalid(REFUSED));
                        }
                    }
                    live.push(columns);
                }

                let read = |unit: &mut dyn TxnCommandUnit| {
                    read_aggregate(unit, &mapper, scope_column, p.scope_key, tables, &live, &plans, t)
                };
                if let Some(stored) = unit.lock_applied_version(&aggregate, t)?
                    && stored >= p.version
                {
                    let (hash, row_counts) = read(unit)?;
                    let result = TxnSnapshotResult {
                        aggregate: aggregate.clone(),
                        version: p.version,
                        outcome: txn_outcomes::STALE.to_owned(),
                        applied_version: stored,
                        hash,
                        row_counts,
                    };
                    return Ok(self.applied(c, Some(result.to_element())));
                }

                if !unit.replace(scope_column, p.scope_key, &writes, t)? {
                    self.log.warn(
                        "Site command {Id} (txn.snapshot) refused: {Aggregate} v{Version} collides with another aggregate's key",
                        &[
                            Value::from(c.command_id),
                            Value::from(aggregate.as_str()),
                            Value::from(p.version),
                        ],
                    );
                    refused = Some(txn_reasons::KEY_CONFLICT);
                    return Err(CommandError::invalid(REFUSED));
                }

                let (hash, row_counts) = read(unit)?;
                unit.save_applied(&aggregate, p.version, &hash, c.command_id, t)?;
                let result = TxnSnapshotResult {
                    aggregate: aggregate.clone(),
                    version: p.version,
                    outcome: txn_outcomes::APPLIED.to_owned(),
                    applied_version: p.version,
                    hash,
                    row_counts,
                };
                Ok(self.applied(c, Some(result.to_element())))
            },
            ctx,
        );
        match refused {
            Some(reason) => Ok(self.reject(c, reason)),
            None => r,
        }
    }
}
