//! `pos.bill_list` (RT-0842, ADR 0149 addendum; ported from the PDAOrder agent by
//! RT-1427): read-only POS WIN bills of one RVC filed under one store business date, so the
//! cloud's `bill_recon` can list the bills that differ from the mirror. SELECTs through
//! `pda_sync`'s `db_datareader` role: no write transaction and no `Pda_SyncCommandLog` row
//! (a redelivery simply reads again). The result holds bill numbers, statuses, totals and
//! tenders per method only, never a name, a table, a cashier or a card; the log line names
//! the RVC, the date and the bill count only. The verb itself runs in
//! `executor/pos_reads.rs`.

use std::collections::BTreeMap;
use std::sync::Arc;

use dl_tds::Param;

use super::error::Result;
use super::sql;
use super::sql_store::SqlSiteCommandStore;
use crate::contracts::wire::{PosBill, PosBillPayment};
use crate::util::ctx::Ctx;

/// The bills: closed ones from `dbo.Invoice`, cancelled ones from `dbo.InvoiceCxl`, by the
/// business date POS WIN files them under (`IYear/IMonth/IDay`, mirrored as
/// `i_year/i_month/i_day`), at most `@Cap` by bill number. The total is `TotalBase`
/// (`closed_checks.total_base`). Same text as the .NET agent's first SELECT.
pub const POS_BILLS: &str = concat!(
    "SELECT TOP (@Cap) b.CheckNo, b.Status, b.Total FROM (",
    "SELECT CAST(CheckNo AS bigint) AS CheckNo, 'closed' AS Status, ISNULL(TotalBase, 0) AS Total ",
    "FROM dbo.Invoice WHERE RVC = @Rvc AND IYear = @Year AND IMonth = @Month AND IDay = @Day ",
    "UNION ALL ",
    "SELECT CAST(CheckNo AS bigint), 'cancelled', ISNULL(TotalBase, 0) ",
    "FROM dbo.InvoiceCxl WHERE RVC = @Rvc AND IYear = @Year AND IMonth = @Month AND IDay = @Day",
    ") b ORDER BY b.CheckNo;"
);

/// The tenders of the closed bills of that date: `dbo.Trn` rows with `TrnCode >= 200`, per
/// method (trimmed `ItemCode`, or the `TrnCode` when blank), amount `-SUM(BaseTTL)`: the
/// sign and method rule the cloud mirror uses for its tender totals.
pub const POS_BILL_TENDERS: &str = concat!(
    "SELECT CAST(t.CheckNo AS bigint) AS CheckNo, ",
    "COALESCE(NULLIF(LTRIM(RTRIM(t.ItemCode)), ''), CAST(t.TrnCode AS varchar(12))) AS Method, ",
    "-SUM(ISNULL(t.BaseTTL, 0)) AS Amount ",
    "FROM dbo.Trn t JOIN dbo.Invoice i ON i.CheckNo = t.CheckNo ",
    "WHERE i.RVC = @Rvc AND i.IYear = @Year AND i.IMonth = @Month AND i.IDay = @Day ",
    "AND t.TrnCode >= 200 ",
    "GROUP BY t.CheckNo, COALESCE(NULLIF(LTRIM(RTRIM(t.ItemCode)), ''), CAST(t.TrnCode AS varchar(12))) ",
    "ORDER BY 1, 2;"
);

/// Where the bills come from (the SELECTs above on the real store).
pub trait PosBillSource: Send + Sync {
    /// At most `cap` bills of `rvc_no` filed under `business_date` (`yyyy-MM-dd`, already
    /// validated), by bill number, each with its tenders.
    fn bills(
        &self,
        rvc_no: i32,
        business_date: &str,
        cap: usize,
        ctx: &Ctx,
    ) -> Result<Vec<PosBill>>;
}

/// The parameters of [`POS_BILLS`] (and, without `Cap`, of [`POS_BILL_TENDERS`]).
pub fn pos_bill_parameters(rvc_no: i32, business_date: &str, cap: Option<usize>) -> Vec<Param> {
    let part = |r: std::ops::Range<usize>| -> i32 {
        business_date
            .get(r)
            .and_then(|t| t.parse().ok())
            .unwrap_or(0)
    };
    let mut v = vec![
        Param::new("Rvc", rvc_no),
        Param::new("Year", part(0..4)),
        Param::new("Month", part(5..7)),
        Param::new("Day", part(8..10)),
    ];
    if let Some(cap) = cap {
        v.push(Param::new("Cap", i32::try_from(cap).unwrap_or(i32::MAX)));
    }
    v
}

impl PosBillSource for SqlSiteCommandStore {
    fn bills(
        &self,
        rvc_no: i32,
        business_date: &str,
        cap: usize,
        ctx: &Ctx,
    ) -> Result<Vec<PosBill>> {
        let mut c = self.open(ctx)?;
        let r = (|| {
            let heads = sql::query(
                &mut c,
                POS_BILLS,
                &pos_bill_parameters(rvc_no, business_date, Some(cap)),
                ctx,
            )?;
            let tenders = sql::query(
                &mut c,
                POS_BILL_TENDERS,
                &pos_bill_parameters(rvc_no, business_date, None),
                ctx,
            )?;
            let mut by_check: BTreeMap<i64, Vec<PosBillPayment>> = BTreeMap::new();
            for row in tenders.rows() {
                by_check
                    .entry(sql::int64(row, 0)?)
                    .or_default()
                    .push(PosBillPayment {
                        method: sql::text(row, 1).unwrap_or_default(),
                        amount: sql::number(row, 2)?,
                    });
            }
            let mut bills = Vec::with_capacity(heads.rows().len());
            for row in heads.rows() {
                let check_no = sql::int64(row, 0)?;
                let status = sql::text(row, 1).unwrap_or_default();
                let payments = if status == crate::contracts::wire::pos_bill_statuses::CLOSED {
                    by_check.remove(&check_no).unwrap_or_default()
                } else {
                    Vec::new()
                };
                bills.push(PosBill {
                    check_no,
                    status,
                    total: sql::number(row, 2)?,
                    payments,
                });
            }
            Ok(bills)
        })();
        // A read leaves no transaction behind, so the connection is kept unless it broke.
        self.release(c);
        r
    }
}

/// Convenience for the host: the store as a bill source.
pub fn source_of(store: &Arc<SqlSiteCommandStore>) -> Arc<dyn PosBillSource> {
    store.clone()
}
