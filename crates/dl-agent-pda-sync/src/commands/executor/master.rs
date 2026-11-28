//! `master.upsert` (ADR 0147, 0151, 0152) on the executor: the read (update-locked), the
//! hash check, the write and the re-read in the command transaction; entity `cashier` goes
//! to [`SiteCommandExecutor::cashier_upsert`]. Field values are never logged.

use super::{SiteCommandExecutor, reasons};
use crate::capture::plan;
use crate::commands::error::{CommandError, Result};
use crate::commands::master::{find_entity, master_reasons};
use crate::commands::units::SiteCommandUnit;
use crate::contracts::WireJson;
use crate::contracts::mapping::{RawValue, VendorRow};
use crate::contracts::wire::{
    MasterUpsertPayload, MasterUpsertResult, SyncCommand, SyncCommandResult,
};
use crate::reporting::Value;
use crate::util::ctx::Ctx;

impl SiteCommandExecutor {
    /// ADR 0147: the read (update-locked), the hash check, the update and the re-read all
    /// run in the command transaction. A create (no expected hash, ADR 0152) checks the key
    /// absent under the per-table create lock and inserts in the same transaction.
    pub(super) fn master_upsert(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "a master.upsert", MasterUpsertPayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(*r),
        };
        if p.entity == crate::contracts::cashier::ENTITY {
            return self.cashier_upsert(c, &p, ctx);
        }
        let Some(mapper) = self.mapper.clone() else {
            return Ok(self.reject(c, reasons::UNKNOWN_TYPE));
        };
        let Some(entity) = find_entity(&p.entity) else {
            return Ok(self.reject(c, master_reasons::UNSUPPORTED_ENTITY));
        };
        if p.fields
            .iter()
            .any(|(k, _)| !entity.writable.iter().any(|w| w.name == k))
        {
            return Ok(self.reject(c, master_reasons::FIELD_NOT_ALLOWED));
        }
        let expected = p.expected_hash.as_deref();
        if expected.is_none() && entity.insert_sql.is_none() {
            return Ok(self.reject(c, master_reasons::CREATE_UNSUPPORTED));
        }
        let mut values: Vec<(&str, &str)> = Vec::new();
        for column in entity.writable {
            let Some(value) = p.fields.get(column.name).and_then(Option::as_deref) else {
                if expected.is_some() && entity.keeps_missing {
                    continue;
                }
                return Ok(self.invalid(c, &format!("{} is required", column.name)));
            };
            if let Some(why) = column.problem(value) {
                return Ok(self.invalid(c, &why));
            }
            values.push((column.name, value));
        }
        let Some(key) = entity.parse_key(&p.key) else {
            return Ok(self.invalid(c, &format!("Key must be {}", entity.key_shape())));
        };
        if entity.protects(&key) {
            return Ok(self.reject(c, master_reasons::ROW_PROTECTED));
        }
        let Some(table) = mapper.catalog().find(entity.table) else {
            return Err(CommandError::invalid(format!(
                "Table {} is not in the sync catalog.",
                entity.table
            )));
        };
        let exclude = plan::exclude_of(mapper.catalog(), table);
        let bytes: Vec<String> = table
            .transforms
            .as_ref()
            .and_then(|x| x.tcvn3_decode.clone())
            .unwrap_or_default();
        let hash_of = |row: &VendorRow| {
            mapper
                .canonicalize(entity.table, row)
                .map(|canon| canon.hash)
                .map_err(|e| {
                    CommandError::invalid(format!(
                        "Master row of {} cannot be hashed: {e}",
                        entity.wire
                    ))
                })
        };
        let key_value = || Value::from(key.text.as_str());
        // Re-reads the written row inside the transaction and answers its hash, the hash
        // capture will mirror.
        let applied = |unit: &mut dyn SiteCommandUnit, t: &Ctx| {
            let Some(after) = unit.read_master_row(entity, &key, false, &exclude, &bytes, t)?
            else {
                return Err(CommandError::invalid(format!(
                    "{} {key} vanished inside its own write transaction.",
                    entity.wire
                )));
            };
            let result = MasterUpsertResult {
                entity: entity.wire.to_owned(),
                key: p.key.clone(),
                row_hash: hash_of(&after)?,
            };
            Ok(self.applied(c, Some(result.to_element())))
        };
        let Some(expected) = expected else {
            return self.store.run_once(
                c.command_id,
                &mut |unit: &mut dyn SiteCommandUnit, t: &Ctx| {
                    if unit
                        .read_master_row(entity, &key, true, &exclude, &bytes, t)?
                        .is_some()
                    {
                        self.log.warn(
                            "Site command {Id} (master.upsert) refused: {Entity} {Key} already exists on the site",
                            &[
                                Value::from(c.command_id),
                                Value::from(entity.wire),
                                key_value(),
                            ],
                        );
                        return Ok(self.reject(c, master_reasons::ROW_EXISTS));
                    }
                    if !unit.insert_master_row(entity, &key, &values, t)? {
                        return Ok(self.reject(c, master_reasons::NO_TEMPLATE));
                    }
                    applied(unit, t)
                },
                ctx,
            );
        };
        self.store.run_once(
            c.command_id,
            &mut |unit: &mut dyn SiteCommandUnit, t: &Ctx| {
                let Some(row) = unit.read_master_row(entity, &key, false, &exclude, &bytes, t)?
                else {
                    return Ok(self.reject(c, master_reasons::ROW_NOT_FOUND));
                };
                if hash_of(&row)? != expected {
                    self.log.warn(
                        "Site command {Id} (master.upsert) refused: {Entity} {Key} changed on the site since the editor read it",
                        &[
                            Value::from(c.command_id),
                            Value::from(entity.wire),
                            key_value(),
                        ],
                    );
                    return Ok(self.reject(c, master_reasons::CHANGED));
                }
                if let Some(guard) = entity.guard_column
                    && row.get(guard).is_some_and(flag_is_set)
                {
                    return Ok(self.reject(c, master_reasons::ROW_PROTECTED));
                }
                if !unit.update_master_row(entity, &key, &values, t)? {
                    return Ok(self.reject(c, master_reasons::ROW_NOT_FOUND));
                }
                applied(unit, t)
            },
            ctx,
        )
    }
}

/// A bit/tinyint flag column read as set: true, or any non-zero number.
fn flag_is_set(v: &RawValue) -> bool {
    match v {
        RawValue::Bool(b) => *b,
        RawValue::U8(n) => *n != 0,
        RawValue::I8(n) => *n != 0,
        RawValue::I16(n) => *n != 0,
        RawValue::U16(n) => *n != 0,
        RawValue::I32(n) => *n != 0,
        RawValue::U32(n) => *n != 0,
        RawValue::I64(n) => *n != 0,
        RawValue::U64(n) => *n != 0,
        _ => false,
    }
}
