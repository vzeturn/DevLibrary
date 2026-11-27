//! The OW1e store writes of one command (`SqlTableCommandUnit.cs`), on the connection and
//! transaction of [`super::SqlSiteCommandStore`]. Every parameter list is exactly the
//! free variables of its legacy statement (a test pins every pairing).

use dl_tds::Param;

use super::batches as b;
use super::error::{CommandError, Result};
use super::sql;
use super::sql_store::SqlSiteCommandUnit;
use super::sql_text as q;
use super::units::{CheckMoneyFacts, EmptyBillFacts, StoreColumn, TableCommandUnit};
use crate::contracts::wire::{
    OrderCheckCancelEmptyPayload, OrderCheckMergePayload, OrderItemsMovePayload,
    OrderPoNumberPayload, OrderTableChangePayload, OrderTableTransferPayload,
};
use crate::util::ctx::Ctx;

/// The parameters of [`b::LOCK_CHECK`], [`q::MONEY_FACTS`] and [`b::MOVE_SOURCE_LINES`].
pub fn lock_parameters(rvc_no: i32, check_no: i64) -> Vec<Param> {
    vec![Param::new("CheckNo", check_no), Param::new("RVCNo", rvc_no)]
}

/// The parameters of [`b::TABLE_INFO`].
pub fn table_info_parameters(p: &OrderTableChangePayload) -> Vec<Param> {
    let mut v = lock_parameters(p.rvc_no, p.check_no);
    v.push(sql::text_param("TableNo", Some(&p.table_no)));
    v.push(Param::new("NumGuest", p.num_guest));
    v
}

/// The parameters of [`b::TRANSFER_HEADER`] (the table trimmed).
pub fn transfer_header_parameters(p: &OrderTableTransferPayload) -> Vec<Param> {
    let mut v = vec![sql::text_param("ToTableNo", Some(p.to_table_no.trim()))];
    v.extend(lock_parameters(p.rvc_no, p.check_no));
    v
}

/// The parameters of [`b::TRANSFER_LINE_INFO`].
pub fn transfer_line_info_parameters(p: &OrderTableTransferPayload) -> Vec<Param> {
    vec![
        sql::text_param("ToTableNo", Some(p.to_table_no.trim())),
        Param::new("CheckNo", p.check_no),
    ]
}

/// The parameters of [`b::MERGE_LINES`].
pub fn merge_lines_parameters(p: &OrderCheckMergePayload) -> Vec<Param> {
    let mut v = merge_line_info_parameters(p);
    v.push(Param::new("RVCNo", p.rvc_no));
    v
}

/// The parameters of [`b::MERGE_LINE_INFO`].
pub fn merge_line_info_parameters(p: &OrderCheckMergePayload) -> Vec<Param> {
    vec![
        Param::new("Target", p.target_check_no),
        Param::new("Source", p.source_check_no),
    ]
}

/// The parameters of [`b::CANCEL_ORDER`]: the emptied source, cancelled by the cashier.
pub fn cancel_parameters(p: &OrderCheckMergePayload) -> Vec<Param> {
    let mut v = lock_parameters(p.rvc_no, p.source_check_no);
    v.push(sql::text_param("CancelBy", Some(&p.cashier)));
    v
}

/// The parameters of [`q::EMPTY_BILL_FACTS`].
pub fn empty_bill_facts_parameters(rvc_no: i32, check_no: i64, opened_before: &str) -> Vec<Param> {
    let mut v = lock_parameters(rvc_no, check_no);
    v.push(sql::text_param("OpenedBefore", Some(opened_before)));
    v
}

/// The parameters of [`b::CANCEL_ORDER`] for the empty-bill cleanup: the check is the one
/// cancelled, the cashier the canceller.
pub fn cancel_empty_parameters(p: &OrderCheckCancelEmptyPayload) -> Vec<Param> {
    let mut v = lock_parameters(p.rvc_no, p.check_no);
    v.push(sql::text_param("CancelBy", Some(&p.cashier)));
    v
}

/// The parameters of [`q::SET_PO_NUMBER`] (RT-1413).
pub fn po_number_parameters(p: &OrderPoNumberPayload) -> Vec<Param> {
    let mut v = lock_parameters(p.rvc_no, p.check_no);
    v.push(sql::text_param("PONumber", p.po_number.as_deref()));
    v
}

/// The parameters of [`b::MOVE_LINE`] for line `trn_seq`.
pub fn move_line_parameters(p: &OrderItemsMovePayload, trn_seq: i64) -> Vec<Param> {
    vec![
        Param::new("ToCheckNo", p.to_check_no),
        Param::new("TrnSeq", trn_seq),
        Param::new("FromCheckNo", p.from_check_no),
        Param::new("RVCNo", p.rvc_no),
    ]
}

/// The parameters of [`b::MOVE_LINE_INFO`] for line `trn_no`.
pub fn move_line_info_parameters(p: &OrderItemsMovePayload, trn_no: i64) -> Vec<Param> {
    vec![
        Param::new("ToCheckNo", p.to_check_no),
        Param::new("TrnNo", trn_no),
        Param::new("FromCheckNo", p.from_check_no),
    ]
}

/// The parameters of [`b::LOG_TRN_MOVE`]: the moved lines as `5,6`.
pub fn log_trn_move_parameters(p: &OrderItemsMovePayload, trn_seqs: &[i64]) -> Vec<Param> {
    let csv = trn_seqs
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    vec![
        Param::new("RVC", p.rvc_no),
        Param::new("FromCheckNo", p.from_check_no),
        Param::new("ToCheckNo", p.to_check_no),
        sql::text_param("Cashier", Some(&p.cashier)),
        sql::text_param("TrnSeqCsv", Some(&csv)),
    ]
}

/// The parameters of the column probe for `columns`.
pub fn missing_columns_parameters(columns: &[StoreColumn]) -> Vec<Param> {
    let mut v = Vec::with_capacity(columns.len() * 2);
    for (i, col) in columns.iter().enumerate() {
        v.push(sql::text_param(&format!("t{i}"), Some(col.table)));
        v.push(sql::text_param(&format!("c{i}"), Some(col.column)));
    }
    v
}

impl SqlSiteCommandUnit<'_> {
    /// The legacy MOVE audit, best-effort as the legacy service had it: a failure rolls
    /// back to a savepoint so the move's own writes stand. A missing grant is not
    /// swallowed. Returns the SQL error number of a refused audit.
    fn audit_move(
        &mut self,
        p: &OrderItemsMovePayload,
        trn_seqs: &[i64],
        ctx: &Ctx,
    ) -> Result<Option<i32>> {
        sql::batch(self.c, &format!("SAVE TRANSACTION {};", q::AUDIT_SAVEPOINT))?;
        match sql::execute(
            self.c,
            b::LOG_TRN_MOVE,
            &log_trn_move_parameters(p, trn_seqs),
            ctx,
        ) {
            Ok(_) => Ok(None),
            Err(e @ CommandError::Sql(..)) if !e.is_permission_denied() => {
                sql::batch(
                    self.c,
                    &format!("ROLLBACK TRANSACTION {};", q::AUDIT_SAVEPOINT),
                )?;
                Ok(Some(e.sql_number().unwrap_or(0)))
            }
            Err(e) => Err(e),
        }
    }

    /// Legacy recalculation of one check header; a reported failure fails the command so
    /// it rolls back.
    fn recalc(&mut self, check_no: i64, ctx: &Ctx) -> Result<()> {
        let r = sql::query(
            self.c,
            b::RE_CALC_BILL,
            &[Param::new("CheckNo", check_no)],
            ctx,
        )?;
        // The status grid is the batch's last one; a grid the vendor procedure returns
        // first is skipped.
        let mut status: Option<(i32, Option<String>)> = None;
        for set in r.sets() {
            if !sql::first_column_is(set, "RecalcFailed", 3) {
                continue;
            }
            if let Some(row) = set.rows().first() {
                status = Some((sql::int32(row, 0)?, sql::text(row, 2)));
            }
        }
        match status {
            None => Err(CommandError::invalid(format!(
                "The recalculation of check {check_no} returned no status row."
            ))),
            Some((0, _)) => Ok(()),
            Some((_, message)) => Err(CommandError::invalid(format!(
                "The store refused to recalculate check {check_no}: {}; nothing is committed.",
                message.unwrap_or_default()
            ))),
        }
    }
}

impl TableCommandUnit for SqlSiteCommandUnit<'_> {
    fn missing_columns(&mut self, columns: &[StoreColumn], ctx: &Ctx) -> Result<Vec<String>> {
        if columns.is_empty() {
            return Ok(Vec::new());
        }
        let r = sql::query(
            self.c,
            &q::missing_columns_sql(columns.len()),
            &missing_columns_parameters(columns),
            ctx,
        )?;
        Ok(r.rows()
            .iter()
            .map(|row| {
                format!(
                    "{}.{}",
                    sql::text(row, 0).unwrap_or_default(),
                    sql::text(row, 1).unwrap_or_default()
                )
            })
            .collect())
    }

    fn lock_check(&mut self, rvc_no: i32, check_no: i64, ctx: &Ctx) -> Result<Option<i32>> {
        let r = sql::query(
            self.c,
            b::LOCK_CHECK,
            &lock_parameters(rvc_no, check_no),
            ctx,
        )?;
        let Some(row) = r.rows().first() else {
            return Ok(None);
        };
        match sql::value(row, 1) {
            None => Ok(Some(0)),
            Some(v) => sql::to_i32(v).map(Some),
        }
    }

    fn money_facts(&mut self, rvc_no: i32, check_no: i64, ctx: &Ctx) -> Result<CheckMoneyFacts> {
        let r = sql::query(
            self.c,
            q::MONEY_FACTS,
            &lock_parameters(rvc_no, check_no),
            ctx,
        )?;
        let Some(row) = r.rows().first() else {
            return Err(CommandError::invalid(
                "The money facts query returned no row.",
            ));
        };
        let paid = sql::number(row, 0)?;
        let tender = sql::int32(row, 1)? == 1;
        let discount = sql::int32(row, 2)? == 1;
        Ok(CheckMoneyFacts {
            has_payment: tender || !paid.is_zero(),
            has_discount: discount,
        })
    }

    fn empty_bill_facts(
        &mut self,
        rvc_no: i32,
        check_no: i64,
        opened_before: &str,
        ctx: &Ctx,
    ) -> Result<Option<EmptyBillFacts>> {
        let r = sql::query(
            self.c,
            q::EMPTY_BILL_FACTS,
            &empty_bill_facts_parameters(rvc_no, check_no, opened_before),
            ctx,
        )?;
        let Some(row) = r.rows().first() else {
            return Ok(None);
        };
        Ok(Some(EmptyBillFacts {
            table_no: sql::text(row, 0).unwrap_or_default().trim().to_owned(),
            opened_before_cutoff: sql::int32(row, 1)? == 1,
            line_count: sql::int32(row, 2)?,
        }))
    }

    fn cancel_empty(&mut self, payload: &OrderCheckCancelEmptyPayload, ctx: &Ctx) -> Result<()> {
        sql::execute(
            self.c,
            b::CANCEL_ORDER,
            &cancel_empty_parameters(payload),
            ctx,
        )
        .map(|_| ())
    }

    fn set_po_number(&mut self, payload: &OrderPoNumberPayload, ctx: &Ctx) -> Result<()> {
        sql::execute(
            self.c,
            q::SET_PO_NUMBER,
            &po_number_parameters(payload),
            ctx,
        )
        .map(|_| ())
    }

    fn change_table(&mut self, payload: &OrderTableChangePayload, ctx: &Ctx) -> Result<()> {
        sql::execute(self.c, b::TABLE_INFO, &table_info_parameters(payload), ctx).map(|_| ())
    }

    fn transfer(&mut self, payload: &OrderTableTransferPayload, ctx: &Ctx) -> Result<()> {
        sql::execute(
            self.c,
            b::TRANSFER_HEADER,
            &transfer_header_parameters(payload),
            ctx,
        )?;
        sql::execute(
            self.c,
            b::TRANSFER_LINE_INFO,
            &transfer_line_info_parameters(payload),
            ctx,
        )
        .map(|_| ())
    }

    fn merge(&mut self, payload: &OrderCheckMergePayload, ctx: &Ctx) -> Result<()> {
        sql::execute(
            self.c,
            b::MERGE_LINES,
            &merge_lines_parameters(payload),
            ctx,
        )?;
        sql::execute(
            self.c,
            b::MERGE_LINE_INFO,
            &merge_line_info_parameters(payload),
            ctx,
        )?;
        sql::execute(self.c, b::CANCEL_ORDER, &cancel_parameters(payload), ctx)?;
        // Legacy order: the target (now holding every line), then the emptied source.
        self.recalc(payload.target_check_no, ctx)?;
        self.recalc(payload.source_check_no, ctx)
    }

    fn lock_item_lines(&mut self, rvc_no: i32, check_no: i64, ctx: &Ctx) -> Result<Vec<i64>> {
        let r = sql::query(
            self.c,
            b::MOVE_SOURCE_LINES,
            &lock_parameters(rvc_no, check_no),
            ctx,
        )?;
        r.rows().iter().map(|row| sql::int64(row, 0)).collect()
    }

    fn move_items(
        &mut self,
        payload: &OrderItemsMovePayload,
        trn_seqs: &[i64],
        ctx: &Ctx,
    ) -> Result<Option<i32>> {
        for seq in trn_seqs {
            if sql::execute(
                self.c,
                b::MOVE_LINE,
                &move_line_parameters(payload, *seq),
                ctx,
            )? == 0
            {
                return Err(CommandError::invalid(format!(
                    "Line {seq} left check {} while it was locked for the move; nothing is committed.",
                    payload.from_check_no
                )));
            }
        }
        for seq in trn_seqs {
            sql::execute(
                self.c,
                b::MOVE_LINE_INFO,
                &move_line_info_parameters(payload, *seq),
                ctx,
            )?;
        }
        let audit_error = self.audit_move(payload, trn_seqs, ctx)?;
        // Legacy order: the source, then the target.
        self.recalc(payload.from_check_no, ctx)?;
        self.recalc(payload.to_check_no, ctx)?;
        Ok(audit_error)
    }
}
