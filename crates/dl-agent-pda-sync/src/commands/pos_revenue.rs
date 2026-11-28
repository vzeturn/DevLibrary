//! `pos.revenue_summary` (ADR 0149, RT-0843; ported from the PDAOrder agent by RT-1427):
//! read-only daily revenue totals of one RVC from SMILE_POS, so an acceptance check can
//! compare the cloud mirror with what POS WIN reports. One SELECT through `pda_sync`'s
//! `db_datareader` role: no write transaction and no `Pda_SyncCommandLog` row (like
//! `sync.resnapshot`, a redelivery simply reads again and answers the current sums). The
//! result holds per-day sums only: never a bill row, a check number or a name, and the log
//! line names the range and the day count only. The verb itself runs in
//! `executor/pos_reads.rs`.

use std::sync::Arc;

use dl_tds::Param;

use super::error::Result;
use super::sql;
use super::sql_store::SqlSiteCommandStore;
use crate::contracts::wire::PosRevenueDay;
use crate::util::ctx::Ctx;

/// Closed bills of one RVC summed per store-local day (`dbo.Invoice`, the table the
/// Invoice stream mirrors as `app.closed_checks`; closed bills only: open ones live in
/// `TmpInvoice`, cancelled ones in `InvoiceCxl`).
///
/// - day: `CloseTime` (site-local wall clock, as the capture reads it) formatted
///   `yyyy-MM-dd`, the day POS WIN files a bill under; the range is sargable
///   (`CloseTime >= @From AND CloseTime < @To + 1 day`).
/// - `bills` = `COUNT(*)`; `gross` = `SUM(SubTotal)` (the bill total before discount,
///   surcharge and tax); `net` = `SUM(TotalBase)` (the base total POS WIN prints as the
///   bill total, mirrored as `closed_checks.total_base`); `paid` = `SUM(TotalPaid)` (the
///   tender sum, mirrored as `closed_checks.total_paid`). These are the columns POS WIN's
///   own sales report sums per closed bill, so the two agree bill for bill.
/// - Parameterized: `@Rvc` (text, as every RVC bind), `@From`/`@To` (`yyyy-MM-dd` text the
///   payload validation already checked, cast to `date` server side).
pub const DAILY_REVENUE: &str = concat!(
    "SELECT CONVERT(char(10), CloseTime, 23) AS Day, COUNT(*) AS Bills, ",
    "ISNULL(SUM(SubTotal), 0) AS Gross, ISNULL(SUM(TotalBase), 0) AS Net, ",
    "ISNULL(SUM(TotalPaid), 0) AS Paid ",
    "FROM dbo.Invoice ",
    "WHERE RVC = @Rvc AND CloseTime >= CAST(@From AS date) ",
    "AND CloseTime < DATEADD(DAY, 1, CAST(@To AS date)) ",
    "GROUP BY CONVERT(char(10), CloseTime, 23) ORDER BY 1;"
);

/// Where the daily sums come from (the SELECT of [`DAILY_REVENUE`] on the real store).
pub trait PosRevenueSource: Send + Sync {
    /// One entry per day with closed bills in `[date_from, date_to]` (`yyyy-MM-dd`),
    /// ascending.
    fn daily(
        &self,
        rvc_no: i32,
        date_from: &str,
        date_to: &str,
        ctx: &Ctx,
    ) -> Result<Vec<PosRevenueDay>>;
}

/// The parameters of [`DAILY_REVENUE`].
pub fn daily_revenue_parameters(rvc_no: i32, date_from: &str, date_to: &str) -> Vec<Param> {
    vec![
        sql::text_param("Rvc", Some(&rvc_no.to_string())),
        sql::text_param("From", Some(date_from)),
        sql::text_param("To", Some(date_to)),
    ]
}

impl PosRevenueSource for SqlSiteCommandStore {
    fn daily(
        &self,
        rvc_no: i32,
        date_from: &str,
        date_to: &str,
        ctx: &Ctx,
    ) -> Result<Vec<PosRevenueDay>> {
        let mut c = self.open(ctx)?;
        let r = (|| {
            let results = sql::query(
                &mut c,
                DAILY_REVENUE,
                &daily_revenue_parameters(rvc_no, date_from, date_to),
                ctx,
            )?;
            let mut days = Vec::with_capacity(results.rows().len());
            for row in results.rows() {
                days.push(PosRevenueDay {
                    date: sql::text(row, 0).unwrap_or_default(),
                    bills: sql::int32(row, 1)?,
                    gross: sql::number(row, 2)?,
                    net: sql::number(row, 3)?,
                    paid: sql::number(row, 4)?,
                });
            }
            Ok(days)
        })();
        // A read leaves no transaction behind, so the connection is kept unless it broke.
        self.release(c);
        r
    }
}

/// Convenience for the host: the store as a revenue source.
pub fn source_of(store: &Arc<SqlSiteCommandStore>) -> Arc<dyn PosRevenueSource> {
    store.clone()
}
