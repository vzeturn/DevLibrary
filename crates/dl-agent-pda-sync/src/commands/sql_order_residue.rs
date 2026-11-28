//! `SqlOrderResidueSource` (RT-1345, ADR 0182): the residue SELECTs and the guarded cleanup on
//! the POS database through the `pda_sync` login, same text as the .NET agent. Every
//! cleanup statement re-checks its row's condition inside one READ COMMITTED transaction,
//! so a row POS WIN touched meanwhile is skipped.

use std::collections::HashSet;
use std::time::Duration;

use dl_tds::{Client, Param, Row};

use super::batches as b;
use super::error::Result;
use super::order_residue::{
    OrderResidueSnapshot, OrderResidueSource, ResidueCheck, ResidueCleaned, ResidueInfoRow,
    check_no_of,
};
use super::sql::{self, COMMAND_TIMEOUT};
use super::sql_store::SqlSiteCommandStore;
use super::sql_table::lock_parameters;
use crate::contracts::time::{DateTime, DateTimeKind, UNIX_EPOCH_TICKS};
use crate::util::ctx::Ctx;

/// The applied command results: the only rows that can name a check this agent touched.
pub const APPLIED_LOG: &str = "SELECT ResultJson FROM dbo.Pda_SyncCommandLog WHERE Status = N'Applied' AND ResultJson IS NOT NULL;";

/// Open checks the PDA workstation opened at least `@MinAge` minutes ago that hold no line.
pub const EMPTY_CHECKS: &str = concat!(
    "SELECT i.RVC, i.CheckNo, i.OpenTime FROM dbo.tmpInvoice i WHERE i.Status = 1 AND i.WSID = 'NTPDA' ",
    "AND i.OpenTime <= DATEADD(MINUTE, -@MinAge, GETDATE()) ",
    "AND NOT EXISTS (SELECT 1 FROM dbo.tmpTrn t WHERE t.CheckNo = i.CheckNo);"
);

/// `tmpTrnInfo` rows whose `tmpTrn` line is gone, with whether the kitchen got them.
pub const ORPHAN_INFO: &str = concat!(
    "SELECT ti.CheckNo, ti.TrnNo, CASE WHEN ti.POrderTime IS NOT NULL OR ISNULL(ti.PToOrder, 0) = 1 THEN 1 ELSE 0 END ",
    "FROM dbo.tmpTrnInfo ti WHERE NOT EXISTS (SELECT 1 FROM dbo.tmpTrn t WHERE t.TrnSeq = ti.TrnNo);"
);

/// `checkopenlog` rows still open for a check that no longer exists.
pub const STALE_OPEN_LOGS: &str = concat!(
    "SELECT l.RVC, l.CheckNo, l.OpenTime FROM dbo.checkopenlog l WHERE ISNULL(l.isOpen, 0) = 1 ",
    "AND NOT EXISTS (SELECT 1 FROM dbo.tmpInvoice i WHERE i.CheckNo = l.CheckNo AND i.RVC = l.RVC);"
);

/// `PDASendOrder` check numbers whose check no longer exists (report only).
pub const ORPHAN_SEND_AUDITS: &str = concat!(
    "SELECT DISTINCT s.CheckNo FROM dbo.PDASendOrder s ",
    "WHERE NOT EXISTS (SELECT 1 FROM dbo.tmpInvoice i WHERE i.CheckNo = s.CheckNo AND i.RVC = s.RVC);"
);

/// `PDASendOrder` is the PDA module's table: an older store may not have it.
pub const PDA_SEND_ORDER_EXISTS: &str =
    "SELECT CASE WHEN OBJECT_ID(N'dbo.PDASendOrder', N'U') IS NULL THEN 0 ELSE 1 END;";

/// Re-checks, under lock, that the check is still an old empty PDA check.
pub const STILL_EMPTY: &str = concat!(
    "SELECT COUNT(*) FROM dbo.tmpInvoice WITH (UPDLOCK, HOLDLOCK) WHERE CheckNo = @CheckNo AND RVC = @RVCNo AND Status = 1 ",
    "AND WSID = 'NTPDA' AND OpenTime <= DATEADD(MINUTE, -@MinAge, GETDATE()) ",
    "AND NOT EXISTS (SELECT 1 FROM dbo.tmpTrn t WHERE t.CheckNo = @CheckNo);"
);

/// Deletes one never-sent orphan info row.
pub const DROP_INFO: &str = concat!(
    "DELETE FROM dbo.tmpTrnInfo WHERE CheckNo = @CheckNo AND TrnNo = @TrnNo AND ISNULL(PToOrder, 0) = 0 AND POrderTime IS NULL ",
    "AND NOT EXISTS (SELECT 1 FROM dbo.tmpTrn t WHERE t.TrnSeq = @TrnNo);"
);

/// Closes one stale open-log row.
pub const CLOSE_OPEN_LOG: &str = concat!(
    "UPDATE dbo.checkopenlog SET isOpen = 0, CloseTime = GETDATE(), LastAction = GETDATE() ",
    "WHERE RVC = @RVCNo AND CheckNo = @CheckNo AND ISNULL(isOpen, 0) = 1 ",
    "AND NOT EXISTS (SELECT 1 FROM dbo.tmpInvoice i WHERE i.CheckNo = @CheckNo AND i.RVC = @RVCNo);"
);

/// SqlClient's default `CommandTimeout`, which the .NET source keeps.
const TIMEOUT: Duration = Duration::from_secs(30);

impl SqlSiteCommandStore {
    /// Runs `work` on a pooled connection at the .NET source's timeout, then hands the
    /// connection back (rolled back first when a transaction is left open).
    fn residue<T>(&self, ctx: &Ctx, work: impl FnOnce(&mut Client) -> Result<T>) -> Result<T> {
        let mut c = self.open(ctx)?;
        let r = c
            .set_timeout(Some(TIMEOUT))
            .map_err(Into::into)
            .and_then(|()| work(&mut c));
        if c.is_broken()
            || (c.in_transaction() && sql::rollback(&mut c).is_err())
            || c.set_timeout(Some(COMMAND_TIMEOUT)).is_err()
        {
            return r;
        }
        self.release(c);
        r
    }
}

impl OrderResidueSource for SqlSiteCommandStore {
    fn our_check_nos(&self, ctx: &Ctx) -> Result<HashSet<i64>> {
        self.residue(ctx, |c| {
            let r = sql::query(c, APPLIED_LOG, &[], ctx)?;
            Ok(r.rows()
                .iter()
                .filter_map(|row| sql::text(row, 0))
                .filter_map(|json| check_no_of(&json))
                .collect())
        })
    }

    fn read(&self, min_age_minutes: i32, ctx: &Ctx) -> Result<OrderResidueSnapshot> {
        self.residue(ctx, |c| {
            let age = [Param::new("MinAge", min_age_minutes)];
            let empty = sql::query(c, EMPTY_CHECKS, &age, ctx)?;
            let empty_open_checks = empty.rows().iter().map(read_check).collect::<Result<_>>()?;
            let info = sql::query(c, ORPHAN_INFO, &[], ctx)?;
            let orphan_info_rows = info
                .rows()
                .iter()
                .map(|row| {
                    Ok(ResidueInfoRow {
                        check_no: sql::int64(row, 0)?,
                        trn_no: sql::int64(row, 1)?,
                        sent: sql::int64(row, 2)? == 1,
                    })
                })
                .collect::<Result<_>>()?;
            let logs = sql::query(c, STALE_OPEN_LOGS, &[], ctx)?;
            let stale_open_logs = logs.rows().iter().map(read_check).collect::<Result<_>>()?;
            let probe = sql::query(c, PDA_SEND_ORDER_EXISTS, &[], ctx)?;
            let mut orphan_send_audits = Vec::new();
            if sql::scalar(&probe).map_or(Ok(0), sql::to_i32)? == 1 {
                let audits = sql::query(c, ORPHAN_SEND_AUDITS, &[], ctx)?;
                for row in audits.rows() {
                    orphan_send_audits.push(sql::int64(row, 0)?);
                }
            }
            Ok(OrderResidueSnapshot {
                empty_open_checks,
                orphan_info_rows,
                stale_open_logs,
                orphan_send_audits,
            })
        })
    }

    fn clean(
        &self,
        ours: &OrderResidueSnapshot,
        cleaned_by: &str,
        min_age_minutes: i32,
        ctx: &Ctx,
    ) -> Result<ResidueCleaned> {
        self.residue(ctx, |c| {
            sql::begin(c)?;
            let mut done = ResidueCleaned::default();
            for chk in &ours.empty_open_checks {
                let mut guard = lock_parameters(chk.rvc_no, chk.check_no);
                guard.push(Param::new("MinAge", min_age_minutes));
                let still = sql::query(c, STILL_EMPTY, &guard, ctx)?;
                if sql::scalar(&still).map_or(Ok(0), sql::to_i32)? != 1 {
                    continue;
                }
                let mut cancel = lock_parameters(chk.rvc_no, chk.check_no);
                cancel.push(sql::text_param("CancelBy", Some(cleaned_by)));
                sql::execute(c, b::CANCEL_ORDER, &cancel, ctx)?;
                done.empty_checks += 1;
            }
            for row in &ours.orphan_info_rows {
                let params = [
                    Param::new("CheckNo", row.check_no),
                    Param::new("TrnNo", row.trn_no),
                ];
                done.info_rows += affected(sql::execute(c, DROP_INFO, &params, ctx)?);
            }
            for log in &ours.stale_open_logs {
                let params = lock_parameters(log.rvc_no, log.check_no);
                done.open_logs += affected(sql::execute(c, CLOSE_OPEN_LOG, &params, ctx)?);
            }
            sql::commit(c)?;
            Ok(done)
        })
    }
}

fn affected(n: u64) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// `ReadCheck`: RVC, check number and `OpenTime` (`DateTime.MinValue` when NULL).
fn read_check(row: &Row) -> Result<ResidueCheck> {
    Ok(ResidueCheck {
        rvc_no: sql::int32(row, 0)?,
        check_no: sql::int64(row, 1)?,
        opened_local: match sql::value(row, 2) {
            Some(dl_tds::Value::DateTime(t)) => wall_clock(*t)?,
            Some(other) => {
                return Err(super::error::CommandError::invalid(format!(
                    "The store returned {} where a date was expected.",
                    other.kind()
                )));
            }
            None => DateTime::default(),
        },
    })
}

/// A `datetime` column as the wall-clock `DateTime` SqlClient reads (kind Unspecified).
pub(super) fn wall_clock(t: dl_time::DateTime) -> Result<DateTime> {
    let ticks = t.assume_utc().unix_nanos().div_euclid(100);
    i64::try_from(ticks)
        .ok()
        .and_then(|t| t.checked_add(UNIX_EPOCH_TICKS))
        .and_then(|t| DateTime::from_ticks(t, DateTimeKind::Unspecified))
        .ok_or_else(|| super::error::CommandError::invalid("The store holds a time out of range."))
}
