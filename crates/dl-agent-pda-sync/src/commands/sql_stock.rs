//! [`TxnStockUnit`] reads and bookkeeping on the command's SQL connection and transaction
//! (`SqlTxnStockUnit.cs`, ADR 0164): the store map, the ledger probes, the held lines and the
//! applied state. The only interpolated text is the database name, validated by
//! [`resolve_database`](TxnStockUnit::resolve_database) (`^[A-Za-z0-9_]+$` and
//! `HAS_DBACCESS`) and bracketed; every value is a parameter. The posting half is
//! [`super::sql_stock_post`].

use std::time::Duration;

use dl_tds::{Param, ParamValue, Results, Row};
use dl_time::Date;
use dl_uuid::Uuid;

use super::error::{CommandError, Result};
use super::sql;
use super::sql_stock_text as q;
use super::sql_store::SqlSiteCommandUnit;
use super::stock_unit::{
    StockApplied, StockCodeCount, StockHeldLine, StockLedgerProfile, StockSettings, StockStoreMap,
    StockVoucher, TxnStockUnit,
};
use crate::util::ctx::Ctx;

/// The default stock database (`DefaultDatabase`).
pub const DEFAULT_DATABASE: &str = "SMILE_BO";

/// SqlClient's default `CommandTimeout`, which the .NET stock unit keeps (the other command
/// units run at 15 s).
pub(super) const STOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// `SELECT Version, StoreKey ... WITH (UPDLOCK, HOLDLOCK)` of `txn.stock_*`.
pub const LOCK_APPLIED_SQL: &str = "SELECT Version, StoreKey FROM dbo.Pda_TxnApplied WITH (UPDLOCK, HOLDLOCK) WHERE AggregateKey = @Key;";

/// The upsert of `dbo.Pda_TxnApplied` with the BO key (`SaveSql`).
pub const SAVE_APPLIED_SQL: &str = "UPDATE dbo.Pda_TxnApplied SET Version = @Version, RowHash = @Hash, CommandId = @CommandId, StoreKey = @StoreKey, AtUtc = SYSUTCDATETIME() \
WHERE AggregateKey = @Key; \
IF @@ROWCOUNT = 0 INSERT INTO dbo.Pda_TxnApplied (AggregateKey, Version, RowHash, CommandId, StoreKey) \
VALUES (@Key, @Version, @Hash, @CommandId, @StoreKey);";

/// `Bo`: `{0}` becomes the bracketed database name.
pub(super) fn bo(sql: &str, database: &str) -> String {
    sql.replace("{0}", &format!("[{database}]"))
}

/// `^[A-Za-z0-9_]+$` as .NET evaluates it (`$` also matches before a final newline).
pub(super) fn valid_name(name: &str) -> bool {
    let core = name.strip_suffix('\n').unwrap_or(name);
    !core.is_empty() && core.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `SqlDbType.VarChar` text parameter.
pub(super) fn varchar(name: &str, value: &str) -> Param {
    Param::new(name, ParamValue::VarChar(Some(value.to_owned())))
}

/// `SqlDbType.VarChar` text parameter, NULL for `None`.
pub(super) fn varchar_opt(name: &str, value: Option<&str>) -> Param {
    Param::new(name, ParamValue::VarChar(value.map(str::to_owned)))
}

/// `Text`: the trimmed value, `None` for NULL or empty.
pub(super) fn trimmed(row: &Row, i: usize) -> Option<String> {
    sql::text(row, i)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// A nullable integer column (`Convert.ToInt32`).
pub(super) fn opt_i32(row: &Row, i: usize) -> Result<Option<i32>> {
    sql::value(row, i).map(sql::to_i32).transpose()
}

/// A nullable bigint column (`Convert.ToInt64`).
pub(super) fn opt_i64(row: &Row, i: usize) -> Result<Option<i64>> {
    sql::value(row, i).map(sql::to_i64).transpose()
}

/// `rows.Select((_, i) => prefix + i)` joined: `@C0, @C1`.
pub(super) fn names(prefix: &str, count: usize, wrap: bool) -> Vec<String> {
    (0..count)
        .map(|i| {
            if wrap {
                format!("(@{prefix}{i})")
            } else {
                format!("@{prefix}{i}")
            }
        })
        .collect()
}

/// `Period`: a yyyyMM period, or `None` when the value is not one (an unset `INVPeriod`
/// reads as closed, as SMILE-PMS 444 does).
pub(super) fn period(value: Option<i32>) -> Option<i32> {
    value.filter(|v| (190_001..=299_912).contains(v) && (1..=12).contains(&(v % 100)))
}

fn to_date(row: &Row, i: usize) -> Result<Date> {
    sql::value(row, i)
        .ok_or_else(|| CommandError::invalid("Object cannot be cast from DBNull to other types."))?
        .convert::<Date>()
        .map_err(|e| CommandError::invalid(e.to_string()))
}

/// `ExecuteScalar() is int one && one == 1`.
fn is_one(r: &Results) -> bool {
    matches!(sql::scalar(r), Some(dl_tds::Value::I32(1)))
}

impl SqlSiteCommandUnit<'_> {
    /// Runs `text` at the stock unit's 30 s timeout; [`super::SqlSiteCommandStore`] puts the
    /// connection's own timeout back once the command is done.
    pub(super) fn stock_query(
        &mut self,
        text: &str,
        params: &[Param],
        ctx: &Ctx,
    ) -> Result<Results> {
        if !self.long_timeout {
            self.c.set_timeout(Some(STOCK_TIMEOUT))?;
            self.long_timeout = true;
        }
        sql::query(self.c, text, params, ctx)
    }

    pub(super) fn code_counts(r: &Results) -> Result<Vec<StockCodeCount>> {
        let mut codes = Vec::new();
        for row in r.rows() {
            codes.push(StockCodeCount {
                trn_code: sql::int32(row, 0)?,
                trn_sub: sql::int32(row, 1)?,
                lines: sql::int64(row, 2)?,
            });
        }
        Ok(codes)
    }

    /// `ExecuteScalar` of a text query as a trimmed string (`None` for no row or NULL).
    pub(super) fn scalar_i32(r: &Results) -> Result<Option<i32>> {
        sql::scalar(r).map(sql::to_i32).transpose()
    }

    /// The parameter list of the vendor posting procedure: `(name, type)`, a NULL type as "".
    pub(super) fn parameter_list(r: &Results) -> Vec<(String, String)> {
        r.rows()
            .iter()
            .map(|row| {
                (
                    sql::text(row, 0).unwrap_or_default(),
                    sql::text(row, 1).unwrap_or_default(),
                )
            })
            .collect()
    }
}

impl TxnStockUnit for SqlSiteCommandUnit<'_> {
    fn lock_applied(&mut self, aggregate: &str, ctx: &Ctx) -> Result<Option<StockApplied>> {
        let r = self.stock_query(
            LOCK_APPLIED_SQL,
            &[Param::new("Key", aggregate.to_owned())],
            ctx,
        )?;
        let Some(row) = r.rows().first() else {
            return Ok(None);
        };
        Ok(Some(StockApplied {
            version: sql::int64(row, 0)?,
            store_key: opt_i64(row, 1)?,
        }))
    }

    fn save_applied(
        &mut self,
        aggregate: &str,
        version: i64,
        hash: &str,
        command_id: Uuid,
        store_key: Option<i64>,
        ctx: &Ctx,
    ) -> Result<()> {
        self.stock_query(
            SAVE_APPLIED_SQL,
            &[
                Param::new("Key", aggregate.to_owned()),
                Param::new("Version", version),
                varchar("Hash", hash),
                Param::new("CommandId", command_id),
                Param::new("StoreKey", store_key),
            ],
            ctx,
        )?;
        Ok(())
    }

    fn map(&mut self, store_code: i32, ctx: &Ctx) -> Result<Option<StockStoreMap>> {
        let r = self.stock_query(q::MAP_SQL, &[Param::new("StoreCode", store_code)], ctx)?;
        let Some(row) = r.rows().first() else {
            return Ok(None);
        };
        Ok(Some(StockStoreMap {
            store_code: sql::int32(row, 0)?,
            database: trimmed(row, 1),
            store_rvc: sql::int32(row, 2)?,
            with_rvc: sql::int32(row, 3)?,
            dept_code: trimmed(row, 4),
            j_type: trimmed(row, 5),
        }))
    }

    fn resolve_database(&mut self, configured: Option<&str>, ctx: &Ctx) -> Result<Option<String>> {
        let name = match configured.map(str::trim) {
            Some(n) if !n.is_empty() => n,
            _ => DEFAULT_DATABASE,
        };
        if !valid_name(name) {
            return Ok(None);
        }
        let r = self.stock_query(q::DATABASE_ACCESS_SQL, &[Param::new("Name", name)], ctx)?;
        Ok(is_one(&r).then(|| name.to_owned()))
    }

    fn stores_exist(&mut self, database: &str, rvcs: &[i32], ctx: &Ctx) -> Result<bool> {
        let mut list: Vec<i32> = Vec::with_capacity(rvcs.len());
        for r in rvcs {
            if !list.contains(r) {
                list.push(*r);
            }
        }
        let text =
            bo(q::STORES_SQL, database).replace("{1}", &names("R", list.len(), false).join(", "));
        let params: Vec<Param> = list
            .iter()
            .enumerate()
            .map(|(i, r)| Param::new(&format!("R{i}"), *r))
            .collect();
        let r = self.stock_query(&text, &params, ctx)?;
        Ok(Self::scalar_i32(&r)?.map(i64::from) == i64::try_from(list.len()).ok())
    }

    fn ledger_profile(&mut self, database: &str, ctx: &Ctx) -> Result<StockLedgerProfile> {
        let r = self.stock_query(&bo(q::READY_SQL, database), &[], ctx)?;
        let row = r.rows().first().ok_or_else(|| {
            CommandError::invalid("Invalid attempt to read when no data is present.")
        })?;
        let ready = sql::int32(row, 0)? == 1;
        let has_voucher_types = sql::int32(row, 1)? == 1;
        if !ready {
            return Ok(StockLedgerProfile {
                ready: false,
                voucher_types: 0,
                sx_codes: Vec::new(),
            });
        }
        let mut voucher_types = 0;
        if has_voucher_types {
            let r = self.stock_query(&bo(q::VOUCHER_TYPES_SQL, database), &[], ctx)?;
            voucher_types = Self::scalar_i32(&r)?.unwrap_or(0);
        }
        let r = self.stock_query(&bo(q::SX_PROFILE_SQL, database), &[], ctx)?;
        Ok(StockLedgerProfile {
            ready: true,
            voucher_types,
            sx_codes: Self::code_counts(&r)?,
        })
    }

    fn settings(&mut self, database: &str, ctx: &Ctx) -> Result<StockSettings> {
        let r = self.stock_query(&bo(q::SETTINGS_SQL, database), &[], ctx)?;
        let row = r.rows().first().ok_or_else(|| {
            CommandError::invalid("Invalid attempt to read when no data is present.")
        })?;
        Ok(StockSettings {
            cost_method: opt_i32(row, 0)?,
            inv_period: period(opt_i32(row, 1)?),
            gl_period: period(opt_i32(row, 2)?),
        })
    }

    fn unknown_items(
        &mut self,
        database: &str,
        item_codes: &[String],
        ctx: &Ctx,
    ) -> Result<Vec<String>> {
        if item_codes.is_empty() {
            return Ok(Vec::new());
        }
        let text = bo(q::UNKNOWN_ITEMS_SQL, database)
            .replace("{1}", &names("I", item_codes.len(), true).join(", "));
        let params: Vec<Param> = item_codes
            .iter()
            .enumerate()
            .map(|(i, c)| varchar(&format!("I{i}"), c))
            .collect();
        let r = self.stock_query(&text, &params, ctx)?;
        Ok(r.rows()
            .iter()
            .map(|row| sql::text(row, 0).unwrap_or_default())
            .collect())
    }

    fn held(&mut self, aggregate: &str, ctx: &Ctx) -> Result<Vec<StockHeldLine>> {
        let r = self.stock_query(q::HELD_SQL, &[Param::new("Key", aggregate.to_owned())], ctx)?;
        let mut lines = Vec::new();
        for row in r.rows() {
            lines.push(StockHeldLine {
                item_code: sql::text(row, 0).unwrap_or_default(),
                store_code: sql::int32(row, 1)?,
                qty: sql::number(row, 2)?,
                day: to_date(row, 3)?,
            });
        }
        Ok(lines)
    }

    fn save_held(&mut self, aggregate: &str, lines: &[StockHeldLine], ctx: &Ctx) -> Result<()> {
        let key = Param::new("Key", aggregate.to_owned());
        self.stock_query(q::SAVE_HELD_SQL, std::slice::from_ref(&key), ctx)?;
        for l in lines {
            self.stock_query(
                q::INSERT_HELD_SQL,
                &[
                    key.clone(),
                    varchar("Item", &l.item_code),
                    Param::new("Store", l.store_code),
                    sql::money_param("Qty", l.qty),
                    Param::new("Day", ParamValue::Date(Some(l.day))),
                ],
                ctx,
            )?;
        }
        Ok(())
    }

    fn post_voucher(
        &mut self,
        voucher: &StockVoucher,
        ctx: &Ctx,
    ) -> super::stock_unit::StockResult<i64> {
        self.post_voucher_impl(voucher, ctx)
    }

    fn count_ledger_ready(&mut self, database: &str, ctx: &Ctx) -> Result<bool> {
        let r = self.stock_query(&bo(q::COUNT_READY_SQL, database), &[], ctx)?;
        Ok(is_one(&r))
    }

    fn count_target_valid(
        &mut self,
        database: &str,
        store_rvc: i32,
        post_loc: i32,
        dept: &str,
        ctx: &Ctx,
    ) -> Result<bool> {
        let r = self.stock_query(
            &bo(q::COUNT_TARGET_SQL, database),
            &[
                Param::new("Store", store_rvc),
                Param::new("Loc", post_loc),
                varchar("Dept", dept),
            ],
            ctx,
        )?;
        Ok(is_one(&r))
    }

    fn post_count(
        &mut self,
        session: &super::stock_unit::StockCountSession,
        ctx: &Ctx,
    ) -> super::stock_unit::StockResult<super::stock_unit::StockCountPosted> {
        self.post_count_impl(session, ctx)
    }

    fn read_count(
        &mut self,
        database: &str,
        session_seq: i64,
        ctx: &Ctx,
    ) -> Result<Option<super::stock_unit::StockCountPosted>> {
        self.read_count_impl(database, session_seq, ctx)
    }

    fn receipt_ledger(
        &mut self,
        database: &str,
        ctx: &Ctx,
    ) -> Result<super::stock_unit::StockReceiptLedger> {
        self.receipt_ledger_impl(database, ctx)
    }

    fn receipt_target_valid(
        &mut self,
        database: &str,
        store_rvc: i32,
        vendor_code: i64,
        item_codes: &[String],
        ctx: &Ctx,
    ) -> Result<bool> {
        if item_codes.is_empty() {
            return Ok(false);
        }
        let text = bo(q::RECEIPT_TARGET_SQL, database)
            .replace("{1}", &names("I", item_codes.len(), true).join(", "));
        let mut params = vec![
            Param::new("Store", store_rvc),
            Param::new("Vendor", vendor_code),
        ];
        params.extend(
            item_codes
                .iter()
                .enumerate()
                .map(|(i, c)| varchar(&format!("I{i}"), c)),
        );
        let r = self.stock_query(&text, &params, ctx)?;
        Ok(is_one(&r))
    }

    fn post_receipt(
        &mut self,
        receipt: &super::stock_unit::StockReceipt,
        ctx: &Ctx,
    ) -> super::stock_unit::StockResult<super::stock_unit::StockReceiptPosted> {
        self.post_receipt_impl(receipt, ctx)
    }

    fn read_receipt(
        &mut self,
        database: &str,
        receipt_seq: i64,
        ctx: &Ctx,
    ) -> Result<Option<super::stock_unit::StockReceiptPosted>> {
        self.read_receipt_impl(database, receipt_seq, ctx)
    }

    fn read_issue(
        &mut self,
        database: &str,
        vseq: i64,
        ctx: &Ctx,
    ) -> Result<Option<super::stock_unit::StockIssuePosted>> {
        let r = self.stock_query(
            &bo(q::ISSUE_READ_SQL, database),
            &[Param::new("VSeq", vseq)],
            ctx,
        )?;
        let Some(first) = r.rows().first() else {
            return Ok(None);
        };
        let mut lines = Vec::new();
        for row in r.rows() {
            if sql::value(row, 2).is_none() {
                continue;
            }
            lines.push(super::stock_unit::StockIssueLine {
                item_code: trimmed(row, 2).unwrap_or_default(),
                trn_qty: sql::number(row, 3)?,
                rvc: sql::int32(row, 4)?,
                with_rvc: sql::int32(row, 5)?,
            });
        }
        Ok(Some(super::stock_unit::StockIssuePosted {
            vseq,
            voucher_no: trimmed(first, 0).unwrap_or_default(),
            reference: trimmed(first, 1),
            lines,
        }))
    }

    fn store_type(&mut self, database: &str, rvc: i32, ctx: &Ctx) -> Result<Option<i32>> {
        let r = self.stock_query(
            &bo(q::STORE_TYPE_SQL, database),
            &[Param::new("Rvc", rvc)],
            ctx,
        )?;
        Self::scalar_i32(&r)
    }
}
