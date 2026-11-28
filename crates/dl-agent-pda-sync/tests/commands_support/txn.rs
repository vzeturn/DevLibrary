//! The in-memory `txn.snapshot` unit (port of the C# `FakeSiteCommandStore` txn part): the
//! live columns per table, the rows per table and the applied versions. A TCVN3 value is
//! kept as its ASCII bytes, as capture reads the column; every other value as the
//! `RawValue` SQL Server would return for it.

use std::collections::HashMap;

use dl_agent_pda_sync::commands::{
    CommandError, Result, TxnAppliedState, TxnCommandUnit, TxnInsertValue, TxnLiveColumn,
    TxnTableWrite, TxnValue,
};
use dl_agent_pda_sync::contracts::mapping::{RawValue, VendorRow};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_uuid::Uuid;

use super::txn_fo::{CounterBook, FolioBook};
use super::{Unit, lock};

/// The txn rows of the fake store.
#[derive(Debug, Clone, Default)]
pub struct TxnState {
    /// Live columns per table (configuration, copied into every draft).
    pub columns: HashMap<String, Vec<TxnLiveColumn>>,
    /// Rows per table with the scope key they were written under.
    pub rows: HashMap<String, Vec<(i64, VendorRow)>>,
    /// `dbo.Pda_TxnApplied`: aggregate to (version, hash, command id).
    pub applied: HashMap<String, (i64, String, Uuid)>,
    /// `Pda_TxnApplied.StoreKey` per keyed aggregate (ADR 0160).
    pub store_keys: HashMap<String, i64>,
    /// Last identity handed out per table (a rollback restores the clone).
    pub identities: HashMap<String, i64>,
    /// `txn.counters`: the `dbo.Para` counters and their configuration.
    pub counters: CounterBook,
    /// The room-charge verbs' Front Office.
    pub fo: FolioBook,
}

impl TxnState {
    /// The rows of `table`, in insert order.
    pub fn of(&self, table: &str) -> Vec<VendorRow> {
        self.rows
            .get(table)
            .map(|r| r.iter().map(|(_, row)| row.clone()).collect())
            .unwrap_or_default()
    }
}

/// The value SQL Server reads back for an inserted value.
fn stored(v: &TxnInsertValue) -> RawValue {
    use dl_tds::Value as V;
    let Some(value) = &v.value else {
        return RawValue::Null;
    };
    if let (true, TxnValue::Text(s)) = (v.tcvn3, value) {
        return RawValue::Bytes(s.as_bytes().to_vec());
    }
    let tds = match value {
        TxnValue::Bool(b) => V::Bool(*b),
        TxnValue::U8(n) => V::I8(*n),
        TxnValue::I16(n) => V::I16(*n),
        TxnValue::I32(n) => V::I32(*n),
        TxnValue::I64(n) => V::I64(*n),
        TxnValue::Decimal(d) => V::Decimal(*d),
        TxnValue::F64(f) => V::F64(*f),
        TxnValue::F32(f) => V::F32(*f),
        TxnValue::Text(s) => V::String(s.clone()),
        TxnValue::DateTime(d) => V::DateTime(*d),
        TxnValue::Bytes(b) => V::Bytes(b.clone()),
        TxnValue::Uuid(u) => V::Uuid(*u),
    };
    RawValue::from_tds(&tds)
}

/// The integer a key column holds, as the shift scope compares it.
fn key_of(v: &RawValue) -> Option<i64> {
    match v {
        RawValue::U8(n) => Some(i64::from(*n)),
        RawValue::I8(n) => Some(i64::from(*n)),
        RawValue::I16(n) => Some(i64::from(*n)),
        RawValue::I32(n) => Some(i64::from(*n)),
        RawValue::I64(n) => Some(*n),
        RawValue::Decimal(d) => d.to_i64(),
        _ => None,
    }
}

/// The stored form of written values, keyed by column name.
fn to_row(values: &[TxnInsertValue]) -> VendorRow {
    let mut row = VendorRow::new();
    for v in values {
        row.insert(v.column.name.clone(), stored(v));
    }
    row
}

/// Whether `row` belongs to `scope_key`: by its scope column when it has one (rows of
/// `insert`), else by the key it was replaced under.
fn in_scope(k: i64, row: &VendorRow, scope_column: &str, scope_key: i64) -> bool {
    match row.get(scope_column).and_then(key_of) {
        Some(v) => v == scope_key,
        None => k == scope_key,
    }
}

impl TxnCommandUnit for Unit<'_> {
    fn columns(&mut self, table: &str, _ctx: &Ctx) -> Result<Vec<TxnLiveColumn>> {
        self.owner.call(format!("columns:{table}"));
        Ok(self.s.txn.columns.get(table).cloned().unwrap_or_default())
    }

    fn lock_applied_version(&mut self, aggregate: &str, _ctx: &Ctx) -> Result<Option<i64>> {
        self.owner.call(format!("lock:{aggregate}"));
        Ok(self.s.txn.applied.get(aggregate).map(|a| a.0))
    }

    fn replace(
        &mut self,
        _scope_column: &str,
        scope_key: i64,
        tables: &[TxnTableWrite],
        _ctx: &Ctx,
    ) -> Result<bool> {
        self.owner.call(format!("replace:{scope_key}"));
        if lock(&self.owner.knobs).txn_key_conflict {
            return Ok(false);
        }
        for t in tables {
            let rows = self.s.txn.rows.entry(t.table.clone()).or_default();
            rows.retain(|(k, _)| *k != scope_key);
            for values in &t.rows {
                let mut row = VendorRow::new();
                for v in values {
                    row.insert(v.column.name.clone(), stored(v));
                }
                rows.push((scope_key, row));
            }
        }
        Ok(true)
    }

    fn read_rows(
        &mut self,
        table: &str,
        scope_column: &str,
        scope_key: i64,
        columns: &[String],
        _bytes_columns: &[String],
        _ctx: &Ctx,
    ) -> Result<Vec<VendorRow>> {
        let rows = self.s.txn.rows.get(table).cloned().unwrap_or_default();
        Ok(rows
            .into_iter()
            .filter(|(k, row)| in_scope(*k, row, scope_column, scope_key))
            .map(|(_, row)| {
                let mut out = VendorRow::new();
                for c in columns {
                    out.insert(c.clone(), row.get(c).cloned().unwrap_or(RawValue::Null));
                }
                out
            })
            .collect())
    }

    fn save_applied(
        &mut self,
        aggregate: &str,
        version: i64,
        hash: &str,
        command_id: Uuid,
        _ctx: &Ctx,
    ) -> Result<()> {
        self.owner.call(format!("save:{aggregate}"));
        self.s
            .txn
            .applied
            .insert(aggregate.to_owned(), (version, hash.to_owned(), command_id));
        Ok(())
    }

    fn lock_applied(&mut self, aggregate: &str, _ctx: &Ctx) -> Result<Option<TxnAppliedState>> {
        self.owner.call(format!("lock:{aggregate}"));
        Ok(self.s.txn.applied.get(aggregate).map(|a| TxnAppliedState {
            version: a.0,
            store_key: self.s.txn.store_keys.get(aggregate).copied(),
        }))
    }

    fn insert(&mut self, table: &str, row: &[TxnInsertValue], _ctx: &Ctx) -> Result<i64> {
        self.owner.call(format!("insert:{table}"));
        let identity = self
            .s
            .txn
            .columns
            .get(table)
            .and_then(|c| c.iter().find(|c| c.generated))
            .map(|c| c.name.clone())
            .ok_or_else(|| {
                CommandError::invalid(format!(
                    "dbo.{table} gave no identity for the inserted row."
                ))
            })?;
        let next = self.s.txn.identities.get(table).copied().unwrap_or(0) + 1;
        self.s.txn.identities.insert(table.to_owned(), next);
        let mut values = to_row(row);
        values.insert(identity, RawValue::I64(next));
        self.s
            .txn
            .rows
            .entry(table.to_owned())
            .or_default()
            .push((0, values));
        Ok(next)
    }

    fn update(
        &mut self,
        table: &str,
        key_column: &str,
        key: i64,
        values: &[TxnInsertValue],
        _ctx: &Ctx,
    ) -> Result<()> {
        self.owner.call(format!("update:{table}"));
        let target = self
            .s
            .txn
            .rows
            .get_mut(table)
            .and_then(|rows| {
                rows.iter_mut()
                    .find(|(_, r)| r.get(key_column).and_then(key_of) == Some(key))
            })
            .ok_or_else(|| {
                CommandError::invalid(format!("dbo.{table} has no row for the shift key {key}."))
            })?;
        for (name, value) in to_row(values) {
            target.1.insert(name, value);
        }
        Ok(())
    }

    fn save_applied_keyed(
        &mut self,
        aggregate: &str,
        version: i64,
        hash: &str,
        command_id: Uuid,
        store_key: Option<i64>,
        ctx: &Ctx,
    ) -> Result<()> {
        self.save_applied(aggregate, version, hash, command_id, ctx)?;
        if let Some(key) = store_key {
            self.s.txn.store_keys.insert(aggregate.to_owned(), key);
        }
        Ok(())
    }
}
