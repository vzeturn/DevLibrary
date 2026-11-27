//! [`OrderCommandUnit`] on the command's SQL connection and transaction, with the legacy
//! parameter shapes (`SqlOrderCommandUnit.cs`, `SqlOrderCommandUnit.Combo.cs`).

use dl_tds::{Param, Results};

use super::batches as b;
use super::error::{CommandError, Result};
use super::sql;
use super::sql_money;
use super::sql_store::SqlSiteCommandUnit;
use super::sql_text as q;
use super::units::{OrderCloseOutcome, OrderCommandUnit, OrderLineFacts, OrderPostOutcome};
use crate::contracts::mapping::tcvn3;
use crate::contracts::wire::{
    CheckStateHeader, OrderCheckClosePayload, OrderCheckLine, OrderCheckOpenPayload,
    OrderCheckResult, OrderComboPostPayload, OrderCoversPayload, OrderItemPostPayload,
    OrderItemVoidPayload, OrderRoomChargePayload, OrderSendPayload, OrderTenderAddPayload,
    OrderTenderRemovePayload, order_command_reasons as reasons,
};
use crate::util::ctx::Ctx;
use crate::util::store_text;

/// SQL error number the legacy post batch THROWs before any write (open name).
pub const OPEN_NAME_REQUIRED_ERROR: i32 = 51101;
/// SQL error number the legacy post batch THROWs before any write (open price).
pub const OPEN_PRICE_REQUIRED_ERROR: i32 = 51102;

/// Status codes of the combo batch's status row (`@CB_IsOut`): posted.
pub const COMBO_POSTED: i32 = 0;
/// Combo status: the check is not in `tmpInvoice`.
pub const COMBO_CHECK_NOT_FOUND: i32 = 1;
/// Combo status: a code is not in `ItemDef`.
pub const COMBO_ITEM_NOT_FOUND: i32 = 2;

fn rvc(rvc_no: i32) -> Param {
    Param::new("RVCNo", rvc_no)
}

fn check(check_no: i64) -> Param {
    Param::new("CheckNo", check_no)
}

/// The parameters of [`b::OPEN_CHECK`].
pub fn open_parameters(p: &OrderCheckOpenPayload) -> Vec<Param> {
    vec![
        rvc(p.rvc_no),
        sql::text_param("TableNo", Some(&p.table_no)),
        sql::text_param("Cashier", Some(&p.cashier)),
        Param::new("NumGuest", p.num_guest),
    ]
}

/// The parameters of [`b::POST_ITEM`].
pub fn post_parameters(p: &OrderItemPostPayload) -> Vec<Param> {
    vec![
        rvc(p.rvc_no),
        check(p.check_no),
        sql::text_param("Cashier", Some(&p.cashier)),
        sql::text_param("ItemCode", Some(&p.item_code)),
        sql::money_param("Qty", p.qty),
        Param::new("SeatNo", p.seat_no),
        sql::money_param("Price", p.price),
        sql::text_param("Note", Some(p.note.as_deref().unwrap_or(""))),
        sql::text_param("OpenName", p.open_name.as_deref()),
        Param::new("AllowZeroOpenPrice", p.allow_zero_open_price),
    ]
}

/// The parameters of [`b::SEND_ORDER`].
pub fn send_parameters(p: &OrderSendPayload) -> Vec<Param> {
    vec![
        rvc(p.rvc_no),
        check(p.check_no),
        sql::text_param("Cashier", Some(&p.cashier)),
        Param::new("PdaHandled", p.pda_handled == Some(true)),
    ]
}

/// The parameters of [`b::VOID_UNSENT`].
pub fn void_unsent_parameters(p: &OrderItemVoidPayload) -> Vec<Param> {
    let mut v = void_parameters(p);
    v.push(sql::text_param(
        "AuthorCashier",
        Some(p.author_cashier.as_deref().unwrap_or(&p.cashier)),
    ));
    v
}

/// The parameters of [`b::VOID_SENT`].
pub fn void_parameters(p: &OrderItemVoidPayload) -> Vec<Param> {
    vec![
        rvc(p.rvc_no),
        check(p.check_no),
        Param::new("TrnSeq", p.trn_seq),
        sql::text_param("VoidBy", Some(&p.cashier)),
        sql::text_param("VoidReason", p.reason.as_deref()),
    ]
}

/// The parameters of [`b::SET_COVERS`].
pub fn covers_parameters(p: &OrderCoversPayload) -> Vec<Param> {
    vec![
        rvc(p.rvc_no),
        check(p.check_no),
        Param::new("NumGuest", p.num_guest),
        Param::new("NumChild", p.num_child),
        sql::text_param("Cashier", Some(&p.cashier)),
    ]
}

/// The legacy combo parameter shapes plus the ADR 0110 §5 selection: `@Selected` is
/// `|code|code|` (empty when the whole combo is posted) and `@SelectedCount` the number
/// of distinct codes in it.
pub fn combo_parameters(p: &OrderComboPostPayload) -> Vec<Param> {
    let selected = p.selected_codes();
    let d = OrderComboPostPayload::SELECTED_DELIMITER.to_string();
    let joined = if selected.is_empty() {
        String::new()
    } else {
        format!("{d}{}{d}", selected.join(&d))
    };
    vec![
        rvc(p.rvc_no),
        check(p.check_no),
        sql::text_param("Cashier", Some(&p.cashier)),
        sql::text_param("ComboCode", Some(&p.combo_code)),
        sql::money_param("Qty", p.qty),
        Param::new("SeatNo", p.seat_no),
        sql::money_param("Price", p.price),
        sql::text_param("Note", Some(p.note.as_deref().unwrap_or(""))),
        sql::text_param("Selected", Some(&joined)),
        Param::new(
            "SelectedCount",
            i32::try_from(selected.len()).unwrap_or(i32::MAX),
        ),
    ]
}

/// The last status row of a batch: the last row of the last result set whose first
/// column is `name` and that has at least `min_columns` columns, as `(col0, col2)`.
fn status_row(r: &Results, name: &str, min_columns: usize) -> Result<Option<(i32, i64)>> {
    let mut status = None;
    for set in r.sets() {
        if !sql::first_column_is(set, name, min_columns) {
            continue;
        }
        for row in set.rows() {
            let a = sql::int32(row, 0)?;
            let c = if min_columns >= 3 {
                sql::int64(row, 2)?
            } else {
                0
            };
            status = Some((a, c));
        }
    }
    Ok(status)
}

fn tcvn(row: &dl_tds::Row, i: usize) -> Option<String> {
    sql::bytes(row, i).map(tcvn_text)
}

/// TCVN3 bytes of a store column as result text: decoded, without NUL characters
/// ([`store_text::clean`], RT-1309) and trimmed at both ends.
fn tcvn_text(raw: &[u8]) -> String {
    store_text::clean(tcvn3::decode(raw)).trim().to_owned()
}

impl OrderCommandUnit for SqlSiteCommandUnit<'_> {
    fn check_status(&mut self, rvc_no: i32, check_no: i64, ctx: &Ctx) -> Result<Option<i32>> {
        let r = sql::query(
            self.c,
            q::CHECK_STATUS,
            &[rvc(rvc_no), check(check_no)],
            ctx,
        )?;
        sql::scalar(&r).map(sql::to_i32).transpose()
    }

    fn item_exists(&mut self, item_code: &str, ctx: &Ctx) -> Result<bool> {
        let r = sql::query(
            self.c,
            q::ITEM_EXISTS,
            &[sql::text_param("ItemCode", Some(item_code))],
            ctx,
        )?;
        Ok(sql::scalar(&r).map_or(Ok(0), sql::to_i32)? > 0)
    }

    fn open_check(&mut self, payload: &OrderCheckOpenPayload, ctx: &Ctx) -> Result<i64> {
        let r = sql::query(self.c, b::OPEN_CHECK, &open_parameters(payload), ctx)?;
        match sql::scalar(&r) {
            None => Err(CommandError::invalid(
                "The open-check batch returned no check number.",
            )),
            Some(v) => sql::to_i64(v),
        }
    }

    fn find_line(
        &mut self,
        rvc_no: i32,
        check_no: i64,
        trn_seq: i64,
        ctx: &Ctx,
    ) -> Result<Option<OrderLineFacts>> {
        let r = sql::query(
            self.c,
            q::FIND_LINE,
            &[rvc(rvc_no), check(check_no), Param::new("TrnSeq", trn_seq)],
            ctx,
        )?;
        let Some(row) = r.rows().first() else {
            return Ok(None);
        };
        Ok(Some(OrderLineFacts {
            transaction_code: sql::text(row, 0),
            sent: sql::int32(row, 1)? == 1,
        }))
    }

    fn post_item(&mut self, payload: &OrderItemPostPayload, ctx: &Ctx) -> Result<OrderPostOutcome> {
        let r = match sql::query(self.c, b::POST_ITEM, &post_parameters(payload), ctx) {
            Ok(r) => r,
            // THROW runs before any write and, with XACT_ABORT off, leaves the
            // transaction committable for the log row.
            Err(e) if e.sql_number() == Some(OPEN_NAME_REQUIRED_ERROR) => {
                return Ok(OrderPostOutcome::refused(reasons::OPEN_NAME_REQUIRED));
            }
            Err(e) if e.sql_number() == Some(OPEN_PRICE_REQUIRED_ERROR) => {
                return Ok(OrderPostOutcome::refused(reasons::OPEN_PRICE_REQUIRED));
            }
            Err(e) => return Err(e),
        };
        // The batch's status row is its last result set (IsOut, MessageResult, TrnSeq,
        // Seq, CheckNo, ItemCode); a vendor proc it calls may emit sets of its own first.
        let Some((is_out, trn_seq)) = status_row(&r, "IsOut", 3)? else {
            return Err(CommandError::invalid(
                "The post-item batch returned no status row.",
            ));
        };
        if is_out != 0 {
            return Ok(OrderPostOutcome::refused(reasons::CHECK_NOT_FOUND));
        }
        if trn_seq > 0 {
            Ok(OrderPostOutcome::posted(trn_seq))
        } else {
            Err(CommandError::invalid(
                "The post-item batch reported success without a line number.",
            ))
        }
    }

    fn pending_send_seqs(&mut self, rvc_no: i32, check_no: i64, ctx: &Ctx) -> Result<Vec<i64>> {
        let r = sql::query(
            self.c,
            b::PENDING_SEND_SEQS,
            &[rvc(rvc_no), check(check_no)],
            ctx,
        )?;
        r.rows().iter().map(|row| sql::int64(row, 0)).collect()
    }

    fn send(&mut self, payload: &OrderSendPayload, ctx: &Ctx) -> Result<()> {
        // The batch runs to its end, so an error in a later statement surfaces here.
        sql::query(self.c, b::SEND_ORDER, &send_parameters(payload), ctx).map(|_| ())
    }

    fn void_unsent(&mut self, payload: &OrderItemVoidPayload, ctx: &Ctx) -> Result<()> {
        sql::query(
            self.c,
            b::VOID_UNSENT,
            &void_unsent_parameters(payload),
            ctx,
        )
        .map(|_| ())
    }

    fn void_sent(&mut self, payload: &OrderItemVoidPayload, ctx: &Ctx) -> Result<()> {
        sql::query(self.c, b::VOID_SENT, &void_parameters(payload), ctx).map(|_| ())
    }

    fn set_covers(&mut self, payload: &OrderCoversPayload, ctx: &Ctx) -> Result<bool> {
        let r = sql::query(self.c, b::SET_COVERS, &covers_parameters(payload), ctx)?;
        match status_row(&r, "IsOut", 1)?.map(|s| s.0) {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            other => Err(CommandError::invalid(format!(
                "The covers batch returned no usable status row (IsOut={}).",
                other.map_or_else(|| "none".to_owned(), |n| n.to_string())
            ))),
        }
    }

    fn post_combo(
        &mut self,
        payload: &OrderComboPostPayload,
        ctx: &Ctx,
    ) -> Result<OrderPostOutcome> {
        let r = sql::query(self.c, b::POST_COMBO, &combo_parameters(payload), ctx)?;
        match status_row(&r, "IsOut", 3)? {
            None => Err(CommandError::invalid(
                "The combo batch returned no status row.",
            )),
            Some((COMBO_POSTED, seq)) if seq > 0 => Ok(OrderPostOutcome::posted(seq)),
            Some((COMBO_POSTED, _)) => Err(CommandError::invalid(
                "The combo batch reported success without a line number.",
            )),
            Some((COMBO_CHECK_NOT_FOUND, _)) => {
                Ok(OrderPostOutcome::refused(reasons::CHECK_NOT_FOUND))
            }
            Some((COMBO_ITEM_NOT_FOUND, _)) => {
                Ok(OrderPostOutcome::refused(reasons::ITEM_NOT_FOUND))
            }
            Some((n, _)) => Err(CommandError::invalid(format!(
                "The combo batch returned an unknown status IsOut={n}."
            ))),
        }
    }

    fn read_check(
        &mut self,
        rvc_no: i32,
        check_no: i64,
        ctx: &Ctx,
    ) -> Result<Option<OrderCheckResult>> {
        let r = sql::query(self.c, q::READ_HEADER, &[rvc(rvc_no), check(check_no)], ctx)?;
        let Some(head) = r.rows().first() else {
            return Ok(None);
        };
        let header = CheckStateHeader {
            check_no: sql::int64(head, 0)?,
            table_no: sql::text(head, 1),
            guests: sql::decimal_to_i32(sql::number(head, 2)?)?,
            sub_total: sql::number(head, 3)?,
            total_paid: sql::number(head, 4)?,
        };

        let probe = sql::query(self.c, q::CG_TRN_EXISTS, &[], ctx)?;
        let notes = sql::scalar(&probe).map_or(Ok(0), sql::to_i32)? == 1;
        let lines_sql = if notes {
            q::READ_LINES_WITH_NOTES
        } else {
            q::READ_LINES
        };
        let r = sql::query(self.c, lines_sql, &[check(check_no)], ctx)?;
        let mut lines = Vec::with_capacity(r.rows().len());
        for row in r.rows() {
            lines.push(OrderCheckLine {
                line_seq: sql::int64(row, 0)?,
                transaction_code: sql::text(row, 1),
                item_code: sql::text(row, 2).map(|s| s.trim().to_owned()),
                item_name: tcvn(row, 3),
                qty: sql::number(row, 4)?,
                price: sql::number(row, 5)?,
                amount: sql::number(row, 6)?,
                seat_no: sql::decimal_to_i32(sql::number(row, 7)?)?,
                cashier: sql::text(row, 8).map(|s| s.trim().to_owned()),
                sent: sql::int32(row, 9)? == 1,
                note: tcvn(row, 10),
            });
        }
        let fingerprint = OrderCheckResult::fingerprint(&header, &lines);
        Ok(Some(OrderCheckResult {
            rvc_no,
            check_no: header.check_no,
            table_no: header.table_no.as_deref().map(|s| s.trim().to_owned()),
            guests: header.guests,
            sub_total: header.sub_total,
            total_paid: header.total_paid,
            lines,
            fingerprint_after: fingerprint,
            trn_seq: None,
            sent_seqs: None,
        }))
    }

    fn add_tender(
        &mut self,
        payload: &OrderTenderAddPayload,
        ctx: &Ctx,
    ) -> Result<OrderPostOutcome> {
        sql_money::add_tender(self, payload, ctx)
    }

    fn add_room_charge(
        &mut self,
        payload: &OrderRoomChargePayload,
        ctx: &Ctx,
    ) -> Result<OrderPostOutcome> {
        sql_money::add_room_charge(self, payload, ctx)
    }

    fn remove_tender(
        &mut self,
        payload: &OrderTenderRemovePayload,
        ctx: &Ctx,
    ) -> Result<Option<String>> {
        sql_money::remove_tender(self, payload, ctx)
    }

    fn close_check(
        &mut self,
        payload: &OrderCheckClosePayload,
        ctx: &Ctx,
    ) -> Result<OrderCloseOutcome> {
        sql_money::close_check(self, payload, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::tcvn_text;

    #[test]
    fn a_tcvn3_name_with_nul_characters_reaches_the_result_without_them() {
        // RT-1309: `C` followed by NULs broke the cloud's jsonb (22P05).
        assert_eq!(tcvn_text(b"C\0\0\0\0"), "C");
        assert_eq!(tcvn_text(b" Cafe\0 sua   "), "Cafe sua");
    }
}
