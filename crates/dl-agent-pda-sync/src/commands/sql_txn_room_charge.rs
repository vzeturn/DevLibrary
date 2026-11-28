//! [`TxnRoomChargeUnit`] on the command transaction (`SqlTxnRoomChargeUnit.cs`, ADR 0162):
//! the Front Office database is the RVC's `dbo.Para InterfaceDB` (else `SMILE_FO`), checked
//! against `^[A-Za-z0-9_]+$` and `HAS_DBACCESS` (G-03) and bracketed; it is the only
//! interpolated text. The posting goes through the vendor `PostInterfaceTransactionToFolio`;
//! a reversal voids the bill's active rows and appends their negated mirrors, as the FO void
//! does. Same SQL text as the .NET agent.

use dl_tds::{Param, ParamValue};

use super::error::{CommandError, Result};
use super::fo_inhouse;
use super::sql;
use super::sql_store::SqlSiteCommandUnit;
use super::txn_room_charge::{FoActiveRow, FoFolio, FoPostLine, TxnRoomChargeUnit};
use crate::util::ctx::Ctx;

/// `LinkSql`.
pub const LINK_SQL: &str = "SELECT TOP 1 LinkCode FROM dbo.TrnLinkByRVC WHERE RVC = @Rvc AND TrnCode = @TrnCode AND TrnSubCode = @TrnSub;";

/// `FoliosByRoomSql`.
pub const FOLIOS_BY_ROOM_SQL: &str = "SELECT FolioNum, RoomCode FROM {0}.dbo.Folio WHERE RTRIM(RoomCode) = @Room AND FolioStatus = 2 AND COALESCE(NoPostFlag, 0) = 0;";

/// `FolioByNumberSql`.
pub const FOLIO_BY_NUMBER_SQL: &str = "SELECT FolioNum, RoomCode FROM {0}.dbo.Folio WHERE FolioNum = @FolioNum AND FolioStatus = 2 AND COALESCE(NoPostFlag, 0) = 0;";

/// `ActiveSql`.
pub const ACTIVE_SQL: &str = "SELECT TransactionID, TransactionAmount FROM {0}.dbo.FolioTransaction WITH (UPDLOCK, HOLDLOCK) WHERE FolioNum = @FolioNum AND RefNumber = @Ref AND FolioBalanceCode = 'A' ORDER BY TransactionID;";

/// `ReverseSql`: `{1}` is the `@Id<n>` list.
pub const REVERSE_SQL: &str =
    "UPDATE {0}.dbo.FolioTransaction SET FolioBalanceCode = 'V' WHERE TransactionID IN ({1});
INSERT INTO {0}.dbo.FolioTransaction (
    FolioNum, TransactionCode, TransactionSubCode, TransactionDate, RoomCode, RefNumber,
    TransactionAmount, FolioBalanceCode, MealPeriodNumber, Comment, TaxAmount, ServiceCharge,
    SubAmount, SystemTime, OriginAmount, OriginCurrency, TaxCode, OriginFolioNumber, OriginRoom,
    PostingClerkID, Quantity, RptExRate, SrcCode, BillAmount, BillExRate,
    StsCode1, StsCode2, StsCode3, StsCode4, StsCode5, StsCode6, StsCode7, StsCode8,
    STaxCode, STaxAmount, NumberCover, GuestID, OrgGuestID)
SELECT
    FolioNum, TransactionCode, TransactionSubCode, GETDATE(), RoomCode, RefNumber,
    TransactionAmount * -1, 'V', MealPeriodNumber, @Comment,
    TaxAmount * -1, ServiceCharge * -1, SubAmount * -1, GETDATE(), OriginAmount * -1,
    OriginCurrency, TaxCode, OriginFolioNumber, OriginRoom, PostingClerkID, Quantity * -1,
    RptExRate, SrcCode, BillAmount * -1, BillExRate,
    StsCode1, StsCode2, StsCode3, StsCode4, StsCode5, StsCode6, StsCode7, StsCode8,
    STaxCode, STaxAmount * -1, NumberCover * -1, GuestID, OrgGuestID
FROM {0}.dbo.FolioTransaction WHERE TransactionID IN ({1});";

/// `PostSql`.
pub const POST_SQL: &str = "EXEC {0}.dbo.PostInterfaceTransactionToFolio
    @FolioNum = @FolioNum, @FolioBalanceCode = 'A',
    @TransactionCode = @TrnCode, @TransactionSubCode = @TrnSub,
    @RefNumber = @Ref, @TransactionAmount = @Amount,
    @Count = 0, @MealPeriodNumber = @MealPeriod, @Comment = @Comment,
    @ClerkID = @Clerk, @TransactionTime = @At, @StsCode8 = @Sts8;";

/// `Fo`: `sql` on `[database]`.
pub fn fo(sql: &str, database: &str) -> String {
    sql.replace("{0}", &format!("[{database}]"))
}

/// The reversal of `count` rows on `database` (`{1}` = `@Id0, @Id1, ...`).
pub fn reverse_sql(database: &str, count: usize) -> String {
    let ids: Vec<String> = (0..count).map(|i| format!("@Id{i}")).collect();
    fo(REVERSE_SQL, database).replace("{1}", &ids.join(", "))
}

/// The parameters of [`POST_SQL`] (`PostAsync`).
pub fn post_parameters(line: &FoPostLine) -> Result<Vec<Param>> {
    let varchar = |name: &str, v: String| Param::new(name, ParamValue::VarChar(Some(v)));
    Ok(vec![
        varchar("FolioNum", line.folio_num.to_string()),
        varchar("TrnCode", line.transaction_code.to_string()),
        varchar("TrnSub", line.transaction_sub_code.to_string()),
        varchar("Ref", line.ref_number.clone()),
        Param::new("Amount", ParamValue::Decimal(Some(line.amount))),
        Param::new("MealPeriod", ParamValue::I32(Some(line.meal_period))),
        Param::new("Comment", ParamValue::String(Some(line.comment.clone()))),
        varchar("Clerk", line.clerk_id.clone()),
        Param::new(
            "At",
            ParamValue::DateTime(Some(sql::wall_datetime(line.transaction_time)?)),
        ),
        varchar("Sts8", line.sts_code8.clone()),
    ])
}

impl TxnRoomChargeUnit for SqlSiteCommandUnit<'_> {
    fn resolve_database(&mut self, rvc: i32, ctx: &Ctx) -> Result<Option<String>> {
        fo_inhouse::resolve_database(self.c, rvc, ctx)
    }

    fn link_code(
        &mut self,
        rvc: i32,
        trn_code: i32,
        trn_sub: i32,
        ctx: &Ctx,
    ) -> Result<Option<i32>> {
        let params = [
            Param::new("Rvc", ParamValue::I32(Some(rvc))),
            Param::new("TrnCode", ParamValue::I32(Some(trn_code))),
            Param::new("TrnSub", ParamValue::I32(Some(trn_sub))),
        ];
        let r = sql::query(self.c, LINK_SQL, &params, ctx)?;
        match sql::scalar(&r) {
            None | Some(dl_tds::Value::Null) => Ok(None),
            Some(v) => sql::to_i32(v).map(Some),
        }
    }

    fn in_house_folios(
        &mut self,
        database: &str,
        room: &str,
        folio_num: i64,
        ctx: &Ctx,
    ) -> Result<Vec<FoFolio>> {
        let r = if folio_num > 0 {
            sql::query(
                self.c,
                &fo(FOLIO_BY_NUMBER_SQL, database),
                &[Param::new("FolioNum", ParamValue::I64(Some(folio_num)))],
                ctx,
            )?
        } else {
            sql::query(
                self.c,
                &fo(FOLIOS_BY_ROOM_SQL, database),
                &[Param::new(
                    "Room",
                    ParamValue::VarChar(Some(room.to_owned())),
                )],
                ctx,
            )?
        };
        r.rows()
            .iter()
            .map(|row| {
                Ok(FoFolio {
                    folio_num: sql::int64(row, 0)?,
                    room_code: sql::text(row, 1).unwrap_or_default().trim().to_owned(),
                })
            })
            .collect()
    }

    fn lock_active(
        &mut self,
        database: &str,
        folio_num: i64,
        ref_number: &str,
        ctx: &Ctx,
    ) -> Result<Vec<FoActiveRow>> {
        let params = [
            Param::new("FolioNum", ParamValue::I64(Some(folio_num))),
            Param::new("Ref", ParamValue::VarChar(Some(ref_number.to_owned()))),
        ];
        let r = sql::query(self.c, &fo(ACTIVE_SQL, database), &params, ctx)?;
        r.rows()
            .iter()
            .map(|row| {
                Ok(FoActiveRow {
                    transaction_id: sql::int64(row, 0)?,
                    amount: sql::number(row, 1)?,
                })
            })
            .collect()
    }

    fn reverse(
        &mut self,
        database: &str,
        transaction_ids: &[i64],
        comment: &str,
        ctx: &Ctx,
    ) -> Result<()> {
        if transaction_ids.is_empty() {
            return Ok(());
        }
        let mut params: Vec<Param> = transaction_ids
            .iter()
            .enumerate()
            .map(|(i, id)| Param::new(&format!("Id{i}"), ParamValue::I64(Some(*id))))
            .collect();
        params.push(Param::new(
            "Comment",
            ParamValue::String(Some(comment.to_owned())),
        ));
        sql::execute(
            self.c,
            &reverse_sql(database, transaction_ids.len()),
            &params,
            ctx,
        )?;
        Ok(())
    }

    fn post(&mut self, database: &str, line: &FoPostLine, ctx: &Ctx) -> Result<()> {
        if !fo_inhouse::is_database_name(database) {
            return Err(CommandError::invalid(
                "The Front Office database name is not valid.",
            ));
        }
        sql::execute(
            self.c,
            &fo(POST_SQL, database),
            &post_parameters(line)?,
            ctx,
        )?;
        Ok(())
    }
}
