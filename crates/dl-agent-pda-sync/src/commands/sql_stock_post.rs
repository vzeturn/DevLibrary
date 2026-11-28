//! The posting half of [`TxnStockUnit`] (`SqlTxnStockUnit.cs`, `.Issue.cs`, `.Receipt.cs`):
//! vouchers through the vendor `INVUpdateGLAvg`, count sessions and receipts through
//! `RCVUpdateGL`, all inside the command transaction. A post that leaves its voucher in the
//! tmp tables is a failure that rolls the command back, except an account the vendor cannot
//! post (a refusal).

use dl_tds::{Param, ParamValue, ResultSet};

use super::error::{CommandError, Result};
use super::sql;
use super::sql_stock::{bo, opt_i32, opt_i64, trimmed, varchar, varchar_opt};
use super::sql_stock_text as q;
use super::sql_store::SqlSiteCommandUnit;
use super::stock_unit::{
    StockCountDifference, StockCountPosted, StockCountSession, StockError, StockReceipt,
    StockReceiptLedger, StockReceiptPosted, StockReceiptPostedLine, StockResult, StockVoucher,
};
use crate::contracts::wire::txn_stock_reasons as reasons;
use crate::util::ctx::Ctx;

/// `ReceiptPrefix`: the voucher number prefix of a receipt.
pub const RECEIPT_PREFIX: &str = "PNK";

/// `ReceiptDigits`: the digits after the prefix.
pub const RECEIPT_DIGITS: i32 = 4;

const NUMERIC_TYPES: [&str; 5] = ["int", "bigint", "numeric", "decimal", "smallint"];
const FLAG_TYPES: [&str; 4] = ["bit", "int", "smallint", "tinyint"];
const TEXT_TYPES: [&str; 4] = ["varchar", "nvarchar", "char", "nchar"];

fn in_set(set: &[&str], value: &str) -> bool {
    set.iter().any(|t| t.eq_ignore_ascii_case(value))
}

/// `^@\w{1,127}$` as .NET evaluates it (`$` also matches before a final newline).
fn valid_parameter(name: &str) -> bool {
    let core = name.strip_suffix('\n').unwrap_or(name);
    let Some(rest) = core.strip_prefix('@') else {
        return false;
    };
    let n = rest.chars().count();
    (1..=127).contains(&n) && rest.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// `BindPost`: the argument list that calls the vendor posting procedure from its own
/// parameter list: the first (numeric) parameter is the voucher (`@VSeq`), a sale flag is 0
/// (the vendor "Exec INVUpdateGL vSeq, 0, user" shape), one text parameter is the user.
/// `None` when any parameter is something else.
pub fn bind_post(parameters: &[(String, String)]) -> Option<String> {
    if parameters.is_empty() || !in_set(&NUMERIC_TYPES, &parameters[0].1) {
        return None;
    }
    let mut args = Vec::with_capacity(parameters.len());
    let mut user = false;
    for (i, (name, kind)) in parameters.iter().enumerate() {
        if !valid_parameter(name) {
            return None;
        }
        if i == 0 {
            args.push(format!("{name} = @VSeq"));
        } else if name.to_lowercase().contains("sale") && in_set(&FLAG_TYPES, kind) {
            args.push(format!("{name} = 0"));
        } else if !user && in_set(&TEXT_TYPES, kind) {
            args.push(format!("{name} = @User"));
            user = true;
        } else {
            return None;
        }
    }
    Some(args.join(", "))
}

/// `"({0}, @C{0}, @Q{0})"` per line (`rows`), with `@U` when `unit_price`.
fn value_rows(count: usize, unit_price: bool) -> String {
    (0..count)
        .map(|i| {
            if unit_price {
                format!("({i}, @C{i}, @Q{i}, @U{i})")
            } else {
                format!("({i}, @C{i}, @Q{i})")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn lock_prefix(database: &str) -> Param {
    Param::new("LockPrefix", format!("Pda_TxnStockVNo:{database}:"))
}

fn day_param(name: &str, day: dl_time::Date) -> Param {
    Param::new(name, ParamValue::DateTime(Some(day.midnight())))
}

fn year_month(day: dl_time::Date) -> i32 {
    day.year() * 100 + i32::from(day.month().number())
}

/// The `PdaCheck` set of `r`: the first set with `columns` columns named `PdaCheck`.
fn check_set(set: &ResultSet, columns: usize) -> bool {
    set.columns().len() == columns
        && set
            .columns()
            .first()
            .is_some_and(|c| c.name() == "PdaCheck")
}

impl SqlSiteCommandUnit<'_> {
    fn post_parameters(&mut self, database: &str, ctx: &Ctx) -> Result<Vec<(String, String)>> {
        let r = self.stock_query(&bo(q::POST_PARAMETERS_SQL, database), &[], ctx)?;
        Ok(Self::parameter_list(&r))
    }

    /// `PostAsync`: the vendor post of voucher `vseq`; the voucher must have left the tmp
    /// tables afterwards, else the command is refused or fails (and rolls back).
    fn post(
        &mut self,
        database: &str,
        vseq: i64,
        user: &str,
        args: &str,
        ctx: &Ctx,
    ) -> StockResult<()> {
        let text = bo(q::POST_SQL_TEMPLATE, database).replace("{1}", args);
        let r = self.stock_query(
            &text,
            &[Param::new("VSeq", vseq), varchar("User", user)],
            ctx,
        )?;
        let mut rc: Option<i32> = None;
        let mut left = -1;
        // The vendor procedure may return result sets of its own: the check is the one named PdaCheck.
        for set in r.sets() {
            if check_set(set, 3)
                && let Some(row) = set.rows().first()
            {
                rc = opt_i32(row, 1)?;
                left = sql::int32(row, 2)?;
            }
        }
        if left == 0 {
            return Ok(());
        }
        let unpostable = self.stock_query(
            &bo(q::UNPOSTABLE_SQL, database),
            &[Param::new("VSeq", vseq)],
            ctx,
        )?;
        if Self::scalar_i32(&unpostable)?.unwrap_or(0) > 0 {
            return Err(StockError::Refused(reasons::ACCOUNT_INVALID));
        }
        Err(CommandError::invalid(format!(
            "INVUpdateGLAvg did not post stock voucher {vseq} in {database} (return code {}); the command is rolled back.",
            rc.map_or_else(|| "null".to_owned(), |n| n.to_string())
        ))
        .into())
    }

    pub(super) fn post_voucher_impl(
        &mut self,
        voucher: &StockVoucher,
        ctx: &Ctx,
    ) -> StockResult<i64> {
        let parameters = self.post_parameters(&voucher.database, ctx)?;
        let args = bind_post(&parameters).ok_or(StockError::Refused(reasons::POST_UNSUPPORTED))?;
        let text = bo(q::VOUCHER_NUMBERING_SQL, &voucher.database);
        let text = format!("{text}\n{}", bo(q::VOUCHER_INSERT_SQL, &voucher.database))
            .replace("{1}", &value_rows(voucher.lines.len(), false));
        let mut params = vec![
            varchar_opt("JType", voucher.j_type.as_deref()),
            lock_prefix(&voucher.database),
            Param::new("P", year_month(voucher.day)),
            day_param("VDate", voucher.day),
            varchar(
                "VDesc",
                voucher.description.as_deref().unwrap_or(&voucher.reference),
            ),
            Param::new("TrnSub", voucher.trn_sub),
            varchar("VRef", &voucher.reference),
            varchar("User", &voucher.user),
            Param::new("StoreRvc", voucher.store_rvc),
            Param::new("WithRvc", voucher.with_rvc),
            varchar_opt("Dept", voucher.dept_code.as_deref()),
        ];
        for (i, l) in voucher.lines.iter().enumerate() {
            params.push(varchar(&format!("C{i}"), &l.item_code));
            params.push(sql::money_param(&format!("Q{i}"), l.trn_qty));
        }
        let r = self.stock_query(&text, &params, ctx)?;
        let Some(seq) = sql::scalar(&r) else {
            return Err(StockError::Refused(reasons::VOUCHER_TYPE_UNKNOWN));
        };
        let vseq = sql::to_i64(seq)?;
        self.post(&voucher.database, vseq, &voucher.user, &args, ctx)?;
        Ok(vseq)
    }

    pub(super) fn post_count_impl(
        &mut self,
        session: &StockCountSession,
        ctx: &Ctx,
    ) -> StockResult<StockCountPosted> {
        let parameters = self.post_parameters(&session.database, ctx)?;
        let args = bind_post(&parameters).ok_or(StockError::Refused(reasons::POST_UNSUPPORTED))?;
        let text = bo(q::COUNT_SQL, &session.database)
            .replace("{1}", &value_rows(session.lines.len(), false))
            .replace("{2}", q::VOUCHER_NUMBERING_SQL)
            .replace("{0}", &format!("[{}]", session.database));
        let mut params = vec![
            varchar_opt("JType", session.j_type.as_deref()),
            lock_prefix(&session.database),
            Param::new("P", session.period),
            day_param("VDate", session.day),
            varchar("VDesc", &session.reference),
            varchar("User", &session.user),
            Param::new("StoreRvc", session.store_rvc),
            Param::new("WithRvc", session.with_rvc),
            varchar("Dept", &session.dept_code),
        ];
        for (i, l) in session.lines.iter().enumerate() {
            params.push(varchar(&format!("C{i}"), &l.item_code));
            params.push(sql::money_param(&format!("Q{i}"), l.counted_qty));
        }
        let r = self.stock_query(&text, &params, ctx)?;
        let first = r.first().ok_or_else(no_data)?;
        let head = first.rows().first().ok_or_else(no_data)?;
        if first.columns().len() == 1 {
            // Nothing was written: no stock voucher type (NULL), or an open session of the store (-1).
            return Err(StockError::Refused(if sql::value(head, 0).is_none() {
                reasons::VOUCHER_TYPE_UNKNOWN
            } else {
                reasons::COUNT_SESSION_OPEN
            }));
        }
        let session_seq = sql::int64(head, 0)?;
        let voucher_seq = opt_i64(head, 1)?;
        let voucher_no =
            sql::value(head, 2).map(|_| sql::text(head, 2).unwrap_or_default().trim().to_owned());
        let mut lines = Vec::new();
        if let Some(set) = r.sets().get(1) {
            for row in set.rows() {
                lines.push(StockCountDifference {
                    item_code: sql::text(row, 0).unwrap_or_default().trim().to_owned(),
                    system_qty: sql::number(row, 1)?,
                    counted_qty: sql::number(row, 2)?,
                });
            }
        }
        if let Some(vseq) = voucher_seq {
            self.post(&session.database, vseq, &session.user, &args, ctx)?;
        }
        Ok(StockCountPosted {
            session_seq,
            voucher_no,
            lines,
            store_rvc: None,
            reference: None,
        })
    }

    pub(super) fn read_count_impl(
        &mut self,
        database: &str,
        session_seq: i64,
        ctx: &Ctx,
    ) -> Result<Option<StockCountPosted>> {
        let r = self.stock_query(
            &bo(q::COUNT_READ_SQL, database),
            &[Param::new("Session", session_seq)],
            ctx,
        )?;
        let Some(first) = r.rows().first() else {
            return Ok(None);
        };
        let mut lines = Vec::new();
        for row in r.rows() {
            if sql::value(row, 1).is_none() {
                continue;
            }
            lines.push(StockCountDifference {
                item_code: sql::text(row, 1).unwrap_or_default().trim().to_owned(),
                system_qty: sql::number(row, 3)?,
                counted_qty: sql::number(row, 2)?,
            });
        }
        Ok(Some(StockCountPosted {
            session_seq,
            voucher_no: trimmed(first, 0),
            lines,
            store_rvc: opt_i32(first, 4)?,
            reference: trimmed(first, 5),
        }))
    }

    pub(super) fn receipt_ledger_impl(
        &mut self,
        database: &str,
        ctx: &Ctx,
    ) -> Result<StockReceiptLedger> {
        let not_ready = StockReceiptLedger {
            ready: false,
            voucher_type: None,
            sr_codes: Vec::new(),
        };
        let r = self.stock_query(&bo(q::RECEIPT_READY_SQL, database), &[], ctx)?;
        if Self::scalar_i32(&r)? != Some(1) {
            return Ok(not_ready);
        }
        let r = self.stock_query(&bo(q::RECEIPT_POST_PARAMETERS_SQL, database), &[], ctx)?;
        let names: Vec<String> = r
            .rows()
            .iter()
            .map(|row| sql::text(row, 0).unwrap_or_default())
            .collect();
        let wanted = ["@VNo", "@VPeriod", "@UpdateBy"];
        if names.len() != wanted.len()
            || !names
                .iter()
                .zip(wanted)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            return Ok(not_ready);
        }
        let r = self.stock_query(&bo(q::RECEIPT_VOUCHER_TYPE_SQL, database), &[], ctx)?;
        let voucher_type = match sql::scalar(&r) {
            Some(dl_tds::Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        let r = self.stock_query(&bo(q::RECEIPT_PROFILE_SQL, database), &[], ctx)?;
        Ok(StockReceiptLedger {
            ready: true,
            voucher_type,
            sr_codes: Self::code_counts(&r)?,
        })
    }

    pub(super) fn post_receipt_impl(
        &mut self,
        receipt: &StockReceipt,
        ctx: &Ctx,
    ) -> StockResult<StockReceiptPosted> {
        let text = bo(q::RECEIPT_SQL, &receipt.database)
            .replace("{1}", &value_rows(receipt.lines.len(), true));
        let mut params = vec![
            lock_prefix(&receipt.database),
            varchar("Init", RECEIPT_PREFIX),
            Param::new("Width", RECEIPT_DIGITS),
            Param::new("P", receipt.period),
            day_param("VDate", receipt.day),
            varchar("VDesc", &receipt.reference),
            varchar("VRef", &receipt.reference),
            varchar("User", &receipt.user),
            Param::new("StoreRvc", receipt.store_rvc),
            Param::new("Vendor", receipt.vendor_code),
        ];
        for (i, l) in receipt.lines.iter().enumerate() {
            params.push(varchar(&format!("C{i}"), &l.item_code));
            params.push(sql::money_param(&format!("Q{i}"), l.qty));
            params.push(sql::money_param(&format!("U{i}"), l.unit_price));
        }
        let r = self.stock_query(&text, &params, ctx)?;
        if let Some(first) = r.first()
            && first.columns().len() == 1
            && first.columns()[0].name() == "NoVoucherType"
        {
            return Err(StockError::Refused(reasons::VOUCHER_TYPE_UNKNOWN));
        }
        let (mut vseq, mut rc, mut left, mut posted, mut trn) = (-1_i64, -1, -1, -1, -1);
        for set in r.sets() {
            if check_set(set, 6)
                && let Some(row) = set.rows().first()
            {
                rc = opt_i32(row, 1)?.unwrap_or(-1);
                vseq = sql::int64(row, 2)?;
                left = sql::int32(row, 3)?;
                posted = sql::int32(row, 4)?;
                trn = sql::int32(row, 5)?;
            }
        }
        let n = receipt.lines.len();
        if left != 0 || posted != 1 || usize::try_from(trn).ok() != Some(n) {
            return Err(CommandError::invalid(format!(
                "RCVUpdateGL did not post receipt {vseq} in {} (return code {rc}, {left} un-posted header, {posted} posted header, {trn} of {n} ledger lines); the command is rolled back.",
                receipt.database
            ))
            .into());
        }
        match self.read_receipt_impl(&receipt.database, vseq, ctx)? {
            Some(posted) => Ok(posted),
            None => Err(CommandError::invalid(format!(
                "Receipt {vseq} of {} was posted but cannot be read back.",
                receipt.database
            ))
            .into()),
        }
    }

    pub(super) fn read_receipt_impl(
        &mut self,
        database: &str,
        receipt_seq: i64,
        ctx: &Ctx,
    ) -> Result<Option<StockReceiptPosted>> {
        let r = self.stock_query(
            &bo(q::RECEIPT_READ_SQL, database),
            &[Param::new("Seq", receipt_seq)],
            ctx,
        )?;
        let Some(first) = r.rows().first() else {
            return Ok(None);
        };
        let mut lines = Vec::new();
        let mut stores: Vec<i32> = Vec::new();
        for row in r.rows() {
            if sql::value(row, 3).is_none() {
                continue;
            }
            lines.push(StockReceiptPostedLine {
                item_code: sql::text(row, 3).unwrap_or_default().trim().to_owned(),
                qty: sql::number(row, 4)?,
                amount: sql::number(row, 5)?,
            });
            if let Some(s) = opt_i32(row, 6)?
                && !stores.contains(&s)
            {
                stores.push(s);
            }
        }
        Ok(Some(StockReceiptPosted {
            receipt_seq,
            voucher_no: trimmed(first, 0).unwrap_or_default(),
            lines,
            store_rvc: (stores.len() == 1).then(|| stores[0]),
            reference: trimmed(first, 1),
            vendor_code: opt_i64(first, 2)?,
        }))
    }
}

fn no_data() -> CommandError {
    CommandError::invalid("Invalid attempt to read when no data is present.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(n, t)| ((*n).to_owned(), (*t).to_owned()))
            .collect()
    }

    #[test]
    fn the_vendor_shape_binds_voucher_sale_flag_and_user() {
        assert_eq!(
            bind_post(&p(&[
                ("@vSeq", "bigint"),
                ("@IsSale", "bit"),
                ("@UserName", "varchar")
            ])),
            Some("@vSeq = @VSeq, @IsSale = 0, @UserName = @User".to_owned())
        );
        assert_eq!(
            bind_post(&p(&[("@VSeq", "INT")])),
            Some("@VSeq = @VSeq".to_owned())
        );
    }

    #[test]
    fn any_other_shape_is_unsupported() {
        assert_eq!(bind_post(&[]), None);
        assert_eq!(bind_post(&p(&[("@a", "varchar")])), None);
        assert_eq!(bind_post(&p(&[("@a", "int"), ("@b", "datetime")])), None);
        // A second text parameter, or a flag that is not a sale flag.
        assert_eq!(
            bind_post(&p(&[("@a", "int"), ("@u", "varchar"), ("@v", "nvarchar")])),
            None
        );
        assert_eq!(bind_post(&p(&[("@a", "int"), ("@flag", "bit")])), None);
        assert_eq!(bind_post(&p(&[("a", "int")])), None);
        assert_eq!(bind_post(&p(&[("@a b", "int")])), None);
        assert_eq!(bind_post(&p(&[("@", "int")])), None);
    }

    #[test]
    fn the_value_rows_are_indexed() {
        assert_eq!(value_rows(2, false), "(0, @C0, @Q0), (1, @C1, @Q1)");
        assert_eq!(value_rows(1, true), "(0, @C0, @Q0, @U0)");
    }
}
