//! `pos.stock_balance` (RT-1024, ADR 0165; `PosStockBalanceCommandHandler.cs`; ported from the
//! PDAOrder agent by RT-1427): a SELECT-only read of SMILE_BO stock. Like `pos.bill_list` it
//! opens no write transaction and writes no `Pda_SyncCommandLog` row (a redelivery reads
//! again). A store that is not mapped, a bad BO database name or a missing ledger is a
//! rejection with the same reason `txn.stock_deplete` uses; a cancelled read propagates, other
//! SQL failures propagate to the channel's failure count. The log carries the store, the BO
//! store and the counts only, never a quantity. The verb itself runs in
//! `executor/pos_reads.rs`.

use std::sync::Arc;
use std::time::Duration;

use dl_tds::Param;

use super::error::Result;
use super::sql;
use super::sql::COMMAND_TIMEOUT;
use super::sql_stock::{bo, varchar};
use super::sql_store::{SqlSiteCommandStore, SqlSiteCommandUnit};
use super::stock_unit::TxnStockUnit;
use crate::contracts::wire::{PosStockBalance, txn_stock_reasons as reasons};
use crate::util::ctx::Ctx;

/// The ledger and balance statements run at this command timeout (`CommandTimeout = 25`).
const READ_TIMEOUT: Duration = Duration::from_secs(25);

/// `SqlPosStockSource.LedgerSql`: whether the BO ledger table exists.
pub const LEDGER_SQL: &str =
    "SELECT CASE WHEN OBJECT_ID(N'{0}.dbo.INVTrn', N'U') IS NULL THEN 0 ELSE 1 END;";

/// `SqlPosStockSource.Sql`: the posted balance per item over every ledger line of the BO
/// store; `{1}` is the generated `@I0, @I1...` list.
pub const BALANCE_SQL: &str =
    "SELECT LTRIM(RTRIM(t.ICode)) AS ICode, SUM(ISNULL(t.TrnQty, 0)) AS Qty
  FROM {0}.dbo.INVTrn t
 WHERE t.RVC = @Rvc AND t.ICode IN ({1})
 GROUP BY LTRIM(RTRIM(t.ICode))
 ORDER BY 1;";

/// What a [`PosStockSource`] read: the SMILE_BO database and store the cloud store maps to
/// and the balances found, or the `txn_stock_reasons` value that refused the read (nothing was
/// read then).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PosStockRead {
    /// The refusing reason.
    pub refusal: Option<&'static str>,
    /// The BO database read.
    pub database: String,
    /// The BO store.
    pub store_rvc: i32,
    /// The balances of the asked items that have ledger lines.
    pub items: Vec<PosStockBalance>,
}

impl PosStockRead {
    /// A refused read.
    pub fn refused(reason: &'static str) -> Self {
        PosStockRead {
            refusal: Some(reason),
            ..PosStockRead::default()
        }
    }
}

/// Read seam of `pos.stock_balance`: SMILE_BO balances of some items in one cloud stock store.
pub trait PosStockSource: Send + Sync {
    /// The posted balance of each of `item_codes` that has ledger lines in the BO store of
    /// `store_code`.
    fn read(&self, store_code: i32, item_codes: &[String], ctx: &Ctx) -> Result<PosStockRead>;
}

fn read_balances(
    unit: &mut SqlSiteCommandUnit<'_>,
    store_code: i32,
    item_codes: &[String],
    ctx: &Ctx,
) -> Result<PosStockRead> {
    let Some(map) = unit.map(store_code, ctx)? else {
        return Ok(PosStockRead::refused(reasons::STORE_UNMAPPED));
    };
    let Some(database) = unit.resolve_database(map.database.as_deref(), ctx)? else {
        return Ok(PosStockRead::refused(reasons::DATABASE_INVALID));
    };
    unit.c.set_timeout(Some(READ_TIMEOUT))?;
    let ledger = sql::query(unit.c, &bo(LEDGER_SQL, &database), &[], ctx)?;
    let ready = ledger
        .rows()
        .first()
        .map(|r| sql::int32(r, 0))
        .transpose()?
        == Some(1);
    if !ready {
        return Ok(PosStockRead::refused(reasons::LEDGER_MISSING));
    }
    if !unit.stores_exist(&database, &[map.store_rvc], ctx)? {
        return Ok(PosStockRead::refused(reasons::STORE_UNMAPPED));
    }

    let names: Vec<String> = (0..item_codes.len()).map(|i| format!("@I{i}")).collect();
    let text = bo(BALANCE_SQL, &database).replace("{1}", &names.join(", "));
    let mut params = vec![Param::new("Rvc", map.store_rvc)];
    for (i, code) in item_codes.iter().enumerate() {
        params.push(varchar(&format!("I{i}"), code));
    }
    let rows = sql::query(unit.c, &text, &params, ctx)?;
    let mut items = Vec::with_capacity(rows.rows().len());
    for row in rows.rows() {
        items.push(PosStockBalance {
            item_code: sql::text(row, 0).unwrap_or_default(),
            qty: sql::number(row, 1)?,
        });
    }
    Ok(PosStockRead {
        refusal: None,
        database,
        store_rvc: map.store_rvc,
        items,
    })
}

impl PosStockSource for SqlSiteCommandStore {
    fn read(&self, store_code: i32, item_codes: &[String], ctx: &Ctx) -> Result<PosStockRead> {
        let mut c = self.open(ctx)?;
        let r = {
            let mut unit = SqlSiteCommandUnit {
                c: &mut c,
                long_timeout: false,
            };
            read_balances(&mut unit, store_code, item_codes, ctx)
        };
        // Back to the pooled command timeout; a read leaves no transaction behind.
        if !c.is_broken()
            && let Err(e) = c.set_timeout(Some(COMMAND_TIMEOUT))
        {
            r?;
            return Err(e.into());
        }
        self.release(c);
        r
    }
}

/// Convenience for the host: the store as a stock source.
pub fn stock_source_of(store: &Arc<SqlSiteCommandStore>) -> Arc<dyn PosStockSource> {
    store.clone()
}
