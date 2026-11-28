//! [`TxnCounterUnit`] on the command transaction (`SqlTxnCounterUnit.cs`, ADR 0161). The
//! counter row is read with `UPDLOCK, HOLDLOCK` so `GetRVCSeqNumber`'s single
//! `UPDATE ... OUTPUT` waits for the commit and then continues above the raised value. The
//! backing-table reads mirror the procedure's self-heal floor (same tables, same
//! `RVC * 100,000,000` prefix range) and run only on tables that exist; table names come from
//! the fixed list below, never from the payload.

use dl_tds::{Param, ParamValue, Value};

use super::error::{CommandError, Result};
use super::sql;
use super::sql_store::SqlSiteCommandUnit;
use super::txn_counters::TxnCounterUnit;
use crate::capture::sql_builder;
use crate::contracts::wire::txn_counter_names;
use crate::util::ctx::Ctx;

/// `OwnObjectSql`.
pub const OWN_OBJECT_SQL: &str = "SELECT COUNT(*) FROM sys.objects WHERE schema_id = SCHEMA_ID(N'dbo') AND type IN ('SO', 'U') AND name = @Object;";

/// `LockSql`.
pub const LOCK_SQL: &str = "IF NOT EXISTS (SELECT 1 FROM dbo.Para WITH (UPDLOCK, HOLDLOCK) WHERE RVC = @Rvc AND ParaName = @Name) \
INSERT INTO dbo.Para (RVC, ParaName, ParaStr) VALUES (@Rvc, @Name, '0'); \
SELECT MAX(ISNULL(TRY_CAST(NULLIF(RTRIM(ParaStr), '') AS bigint), 0)) FROM dbo.Para WITH (UPDLOCK, HOLDLOCK) \
WHERE RVC = @Rvc AND ParaName = @Name;";

/// `RaiseSql`.
pub const RAISE_SQL: &str = "UPDATE dbo.Para SET ParaStr = CAST(@Value AS varchar(20)) \
WHERE RVC = @Rvc AND ParaName = @Name AND ISNULL(TRY_CAST(NULLIF(RTRIM(ParaStr), '') AS bigint), 0) < @Value;";

/// One backing table of a counter (`Backing`): table, column, filtered by RVC, raw counter
/// allowed.
pub type Backing = (&'static str, &'static str, bool, bool);

/// What `GetRVCSeqNumber` floors each counter against (`Backing`).
pub fn backing(name: &str) -> &'static [Backing] {
    match name {
        txn_counter_names::LAST_CHECK => &[
            ("Invoice", "CheckNo", true, false),
            ("TmpInvoice", "CheckNo", true, false),
            ("IOrgInvoice", "CheckNo", false, false),
        ],
        txn_counter_names::LAST_TRN => &[
            ("Trn", "TrnSeq", true, false),
            ("TmpTrn", "TrnSeq", true, false),
        ],
        txn_counter_names::LAST_PAYMENT => &[
            ("Payment", "PaymentNo", true, true),
            ("TmpPayment", "PaymentNo", true, true),
        ],
        txn_counter_names::CLOSE_NUMBER => &[
            ("TaxTable", "Sub2", true, false),
            ("TmpTaxTable", "Sub2", true, false),
        ],
        _ => &[],
    }
}

/// `InUseSql`: the highest counter in one backing table, the composed range of the RVC or
/// (payments) the raw counter.
pub fn in_use_sql(table: &str, column: &str, by_rvc: bool, raw: bool) -> Result<String> {
    let c = quote(column)?;
    let counter = if raw {
        format!(
            "CASE WHEN {c} >= @Prefix AND {c} < @Prefix + 100000000 THEN {c} - @Prefix WHEN {c} >= 1 AND {c} < 100000000 THEN {c} END"
        )
    } else {
        format!("CASE WHEN {c} >= @Prefix AND {c} < @Prefix + 100000000 THEN {c} - @Prefix END")
    };
    let filter = if by_rvc {
        "WHERE RVC = @Rvc".to_owned()
    } else {
        format!("WHERE {c} >= @Prefix AND {c} < @Prefix + 100000000")
    };
    Ok(format!(
        "IF OBJECT_ID(N'dbo.{table}', N'U') IS NOT NULL SELECT CAST(MAX({counter}) AS bigint) FROM dbo.{} {filter}; ELSE SELECT CAST(NULL AS bigint);",
        quote(table)?
    ))
}

/// `Quote`: a bracketed identifier.
fn quote(identifier: &str) -> Result<String> {
    sql_builder::quote(identifier).map_err(|e| CommandError::invalid(e.to_string()))
}

/// `AddKey`: `@Rvc` int, `@Name` varchar(50).
fn key_params(rvc_no: i32, name: &str) -> Vec<Param> {
    vec![
        Param::new("Rvc", ParamValue::I32(Some(rvc_no))),
        Param::new("Name", ParamValue::VarChar(Some(name.to_owned()))),
    ]
}

impl TxnCounterUnit for SqlSiteCommandUnit<'_> {
    fn has_own_counter_object(&mut self, rvc_no: i32, name: &str, ctx: &Ctx) -> Result<bool> {
        if name != txn_counter_names::LAST_CHECK && name != txn_counter_names::LAST_TRN {
            return Ok(false);
        }
        let object = format!("{name}{rvc_no}");
        let r = sql::query(
            self.c,
            OWN_OBJECT_SQL,
            &[Param::new("Object", ParamValue::String(Some(object)))],
            ctx,
        )?;
        Ok(sql::scalar(&r).map(sql::to_i64).transpose()?.unwrap_or(0) > 0)
    }

    fn lock(&mut self, rvc_no: i32, name: &str, ctx: &Ctx) -> Result<i64> {
        let r = sql::query(self.c, LOCK_SQL, &key_params(rvc_no, name), ctx)?;
        match sql::scalar(&r) {
            None | Some(Value::Null) => Ok(0),
            Some(v) => sql::to_i64(v),
        }
    }

    fn raise(&mut self, rvc_no: i32, name: &str, value: i64, ctx: &Ctx) -> Result<()> {
        let mut params = key_params(rvc_no, name);
        params.push(Param::new("Value", ParamValue::I64(Some(value))));
        sql::execute(self.c, RAISE_SQL, &params, ctx)?;
        Ok(())
    }

    fn in_use(&mut self, rvc_no: i32, name: &str, ctx: &Ctx) -> Result<Option<i64>> {
        let mut max: Option<i64> = None;
        for (table, column, by_rvc, raw) in backing(name) {
            let params = [
                Param::new("Rvc", ParamValue::I32(Some(rvc_no))),
                Param::new(
                    "Prefix",
                    ParamValue::I64(Some(i64::from(rvc_no) * 100_000_000)),
                ),
            ];
            let r = sql::query(
                self.c,
                &in_use_sql(table, column, *by_rvc, *raw)?,
                &params,
                ctx,
            )?;
            if let Some(v) = sql::scalar(&r).filter(|v| !matches!(v, Value::Null)) {
                let n = sql::to_i64(v)?;
                if max.is_none_or(|m| n > m) {
                    max = Some(n);
                }
            }
        }
        Ok(max)
    }
}
