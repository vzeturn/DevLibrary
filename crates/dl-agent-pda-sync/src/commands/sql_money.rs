//! The money verbs of [`OrderCommandUnit`] on the command's SQL connection and
//! transaction (`SqlOrderCommandUnit.Money.cs`, ADR 0174, RT-1297): each batch answers
//! with a status row whose `IsOut` carries the refusal.

use dl_tds::{Param, ParamValue, Results, Value};

use super::batches_money as b;
use super::error::{CommandError, Result};
use super::sql;
use super::sql_store::SqlSiteCommandUnit;
use super::units::{OrderCloseOutcome, OrderPostOutcome};
use crate::contracts::wire::{
    OrderCheckClosePayload, OrderCheckCloseResult, OrderTenderAddPayload, OrderTenderRemovePayload,
};
use crate::util::ctx::Ctx;

fn rvc(rvc_no: i32) -> Param {
    Param::new("RVCNo", rvc_no)
}

fn check(check_no: i64) -> Param {
    Param::new("CheckNo", check_no)
}

/// The legacy tender parameters of [`b::TENDER_ADD`], base currency: `@Tendered` is the
/// cash handed over, `@Amount` what it settles.
pub fn tender_add_parameters(p: &OrderTenderAddPayload) -> Vec<Param> {
    let bill_no = p
        .bill_no
        .as_deref()
        .filter(|b| !b.trim().is_empty())
        .map(|b| b.trim().to_owned());
    vec![
        rvc(p.rvc_no),
        check(p.check_no),
        sql::text_param("Cashier", Some(&p.cashier)),
        sql::text_param("PaymentType", Some(p.payment_type.trim())),
        sql::money_param("Amount", p.amount),
        sql::money_param("Tendered", p.tendered),
        sql::text_param("Reference", Some(p.reference.as_deref().unwrap_or(""))),
        Param::new("CardType", p.card_type),
        Param::new("BillNo", ParamValue::VarChar(bill_no)),
    ]
}

/// The parameters of [`b::TENDER_REMOVE`].
pub fn tender_remove_parameters(p: &OrderTenderRemovePayload) -> Vec<Param> {
    vec![
        rvc(p.rvc_no),
        check(p.check_no),
        Param::new("TrnSeq", p.trn_seq),
        sql::text_param("Cashier", Some(&p.cashier)),
    ]
}

/// The parameters of [`b::CHECK_CLOSE`].
pub fn check_close_parameters(p: &OrderCheckClosePayload) -> Vec<Param> {
    vec![
        rvc(p.rvc_no),
        check(p.check_no),
        sql::text_param("CloseBy", Some(&p.cashier)),
    ]
}

/// A money batch's status row: `IsOut`, the reason text and the whole row.
struct MoneyStatusRow<'a> {
    is_out: i32,
    reason: String,
    values: &'a dl_tds::Row,
}

impl MoneyStatusRow<'_> {
    fn value(&self, i: usize) -> Option<&Value> {
        sql::value(self.values, i)
    }
}

/// The last row of the last result set whose first column is `IsOut` and that has at
/// least `columns` columns (a vendor proc the batch calls may emit sets of its own
/// before it).
fn status_row(r: &Results, columns: usize) -> Result<Option<MoneyStatusRow<'_>>> {
    let mut status = None;
    for set in r.sets() {
        if !sql::first_column_is(set, "IsOut", columns) {
            continue;
        }
        for row in set.rows() {
            status = Some(MoneyStatusRow {
                is_out: sql::int32(row, 0)?,
                reason: sql::text(row, 1).unwrap_or_default(),
                values: row,
            });
        }
    }
    Ok(status)
}

/// `Convert.ToDecimal` of a column that must not be NULL.
fn decimal(row: &MoneyStatusRow<'_>, i: usize) -> Result<dl_decimal::Decimal> {
    row.value(i).map_or_else(
        || {
            Err(CommandError::invalid(
                "Object cannot be cast from DBNull to other types.",
            ))
        },
        sql::to_decimal,
    )
}

pub(super) fn add_tender(
    u: &mut SqlSiteCommandUnit<'_>,
    p: &OrderTenderAddPayload,
    ctx: &Ctx,
) -> Result<OrderPostOutcome> {
    let r = sql::query(u.c, b::TENDER_ADD, &tender_add_parameters(p), ctx)?;
    let Some(row) = status_row(&r, 3)? else {
        return Err(CommandError::invalid(
            "The tender batch returned no status row.",
        ));
    };
    if row.is_out != 0 {
        return Ok(OrderPostOutcome::refused(&row.reason));
    }
    let trn_seq = sql::int64(row.values, 2)?;
    if trn_seq > 0 {
        Ok(OrderPostOutcome::posted(trn_seq))
    } else {
        Err(CommandError::invalid(
            "The tender batch reported success without a line number.",
        ))
    }
}

pub(super) fn remove_tender(
    u: &mut SqlSiteCommandUnit<'_>,
    p: &OrderTenderRemovePayload,
    ctx: &Ctx,
) -> Result<Option<String>> {
    let r = sql::query(u.c, b::TENDER_REMOVE, &tender_remove_parameters(p), ctx)?;
    let Some(row) = status_row(&r, 2)? else {
        return Err(CommandError::invalid(
            "The tender-remove batch returned no status row.",
        ));
    };
    Ok((row.is_out != 0).then_some(row.reason))
}

pub(super) fn close_check(
    u: &mut SqlSiteCommandUnit<'_>,
    p: &OrderCheckClosePayload,
    ctx: &Ctx,
) -> Result<OrderCloseOutcome> {
    let r = sql::query(u.c, b::CHECK_CLOSE, &check_close_parameters(p), ctx)?;
    let Some(row) = status_row(&r, 5)? else {
        return Err(CommandError::invalid(
            "The close batch returned no status row.",
        ));
    };
    if row.is_out == 1 {
        return Ok(OrderCloseOutcome::refused(&row.reason));
    }
    Ok(OrderCloseOutcome::done(OrderCheckCloseResult {
        rvc_no: p.rvc_no,
        check_no: p.check_no,
        close_no: sql::int64(row.values, 2)?,
        sub_total: decimal(&row, 3)?,
        total_paid: decimal(&row, 4)?,
        already_closed: row.is_out == 2,
    }))
}
