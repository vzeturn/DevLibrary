//! The agent's own SQL of the command verbs (read side, command log, KDS, reservations),
//! byte for byte the C# constants of `SqlSiteCommandStore.cs`, `SqlOrderCommandUnit.cs`
//! and `SqlTableCommandUnit.cs`. The legacy batches are in [`super::batches`].

// ---- SqlSiteCommandStore (dbo.Pda_SyncCommandLog)

/// `SqlSiteCommandStore.ReadLogSql`.
pub const READ_LOG: &str =
    "SELECT Status, ResultJson, AtUtc FROM dbo.Pda_SyncCommandLog WHERE CommandId = @CommandId;";

/// `SqlSiteCommandStore.LockLogSql`: UPDLOCK + HOLDLOCK takes a key-range lock even when
/// the row is missing, so a second delivery of the same id waits for the first
/// transaction and then reads its log row instead of executing again.
pub const LOCK_LOG: &str = "SELECT Status, ResultJson, AtUtc FROM dbo.Pda_SyncCommandLog WITH (UPDLOCK, HOLDLOCK) WHERE CommandId = @CommandId;";

/// `SqlSiteCommandStore.InsertLogSql`.
pub const INSERT_LOG: &str = "INSERT INTO dbo.Pda_SyncCommandLog (CommandId, Status, ResultJson, AtUtc) VALUES (@CommandId, @Status, @ResultJson, @AtUtc);";

// ---- SqlSiteCommandUnit

/// `SqlSiteCommandUnit.OpenCheckSql`.
pub const OPEN_CHECK_BELONGS: &str = "SELECT COUNT(*) FROM dbo.TmpInvoice WHERE CheckNo = @CheckNo AND LTRIM(RTRIM(CAST(RVC AS NVARCHAR(20)))) = @Rvc;";

/// `SqlSiteCommandUnit.CheckStateSql` (ADR 0100 §4: the header row is update-locked).
pub const CHECK_STATE: &str = concat!(
    "SELECT CheckNo, TableNo, NGuest, SubTotal, TotalPaid FROM dbo.TmpInvoice WITH (UPDLOCK) WHERE CheckNo = @CheckNo; ",
    "SELECT TrnSeq, TrnCode, ItemCode, TrnQty, ItemPrice FROM dbo.TmpTrn WHERE CheckNo = @CheckNo;"
);

/// `SqlSiteCommandUnit.KdsAcceptColumnsSql`.
pub const KDS_ACCEPT_COLUMNS: &str = concat!(
    "SELECT CASE WHEN COL_LENGTH(N'dbo.tmpTrnInfo', N'KdsAcceptedTime') IS NOT NULL ",
    "AND COL_LENGTH(N'dbo.tmpTrnInfo', N'KdsAcceptedBy') IS NOT NULL THEN 1 ELSE 0 END;"
);

/// `SqlSiteCommandUnit.KdsLineSql[accept]`.
pub const KDS_ACCEPT: &str = concat!(
    "UPDATE dbo.tmpTrnInfo SET KdsAcceptedTime = GETDATE(), KdsAcceptedBy = @Cashier ",
    "WHERE CheckNo = @CheckNo AND TrnNo = @TrnNo AND KdsAcceptedTime IS NULL;"
);

/// `SqlSiteCommandUnit.KdsLineSql[unaccept]`.
pub const KDS_UNACCEPT: &str = concat!(
    "UPDATE dbo.tmpTrnInfo SET KdsAcceptedTime = NULL, KdsAcceptedBy = NULL ",
    "WHERE CheckNo = @CheckNo AND TrnNo = @TrnNo AND KdsAcceptedTime IS NOT NULL;"
);

/// `SqlSiteCommandUnit.KdsLineSql[done]`.
pub const KDS_DONE: &str = concat!(
    "UPDATE dbo.tmpTrnInfo SET IsDone = 1, DoneTime = GETDATE() ",
    "WHERE CheckNo = @CheckNo AND TrnNo = @TrnNo AND ISNULL(IsDone, 0) = 0;"
);

/// `SqlSiteCommandUnit.KdsLineSql[undone]`.
pub const KDS_UNDONE: &str = concat!(
    "UPDATE dbo.tmpTrnInfo SET IsDone = 0, DoneTime = NULL ",
    "WHERE CheckNo = @CheckNo AND TrnNo = @TrnNo AND ISNULL(IsDone, 0) = 1;"
);

/// `SqlSiteCommandUnit.KdsLineSql`: per action, ported from the legacy SRsp_Kds*
/// procedures; a line already in the target state is not touched.
pub fn kds_line_sql(action: &str) -> Option<&'static str> {
    use crate::contracts::wire::kds_line_actions as a;
    match action {
        a::ACCEPT => Some(KDS_ACCEPT),
        a::UNACCEPT => Some(KDS_UNACCEPT),
        a::DONE => Some(KDS_DONE),
        a::UNDONE => Some(KDS_UNDONE),
        _ => None,
    }
}

/// `SqlSiteCommandUnit.InsertReservationSql` (ported from `pda.SRsp_SaveReservation`).
pub const INSERT_RESERVATION: &str = concat!(
    "INSERT INTO dbo.TableReservation (GuestInfo, NumAdult, NumChild, FromDateTime, ToDateTime, TableNum, TelNum, Booker, ",
    "CompanyName, VATCode, Address, RVC) VALUES (@GuestInfo, @NumAdult, @NumChild, @FromDateTime, @ToDateTime, @TableNum, ",
    "@TelNum, @Booker, @CompanyName, @VATCode, @Address, @Rvc); SELECT CAST(SCOPE_IDENTITY() AS INT);"
);

/// `SqlSiteCommandUnit.DeleteReservationSql`.
pub const DELETE_RESERVATION: &str =
    "DELETE FROM dbo.TableReservation WHERE nID = @Id AND LTRIM(RTRIM(ISNULL(RVC, N''))) = @Rvc;";

// ---- SqlOrderCommandUnit (read side, ADR 0104 §4)

/// `OrderCommandSql.CheckStatus`.
pub const CHECK_STATUS: &str =
    "SELECT TOP 1 ISNULL(Status, 0) FROM dbo.TmpInvoice WHERE CheckNo = @CheckNo AND RVC = @RVCNo;";

/// `OrderCommandSql.ItemExists`.
pub const ITEM_EXISTS: &str = "SELECT COUNT(*) FROM dbo.ItemDef WHERE ICode = @ItemCode;";

/// `OrderCommandSql.FindLine`.
pub const FIND_LINE: &str = concat!(
    "SELECT t.TrnCode, CASE WHEN i.POrderTime IS NOT NULL OR ISNULL(i.PToOrder, 0) = 1 THEN 1 ELSE 0 END ",
    "FROM dbo.TmpTrn t OUTER APPLY (SELECT TOP 1 ti.POrderTime, ti.PToOrder FROM dbo.tmpTrnInfo ti ",
    "WHERE ti.TrnNo = t.TrnSeq AND ti.CheckNo = t.CheckNo) i ",
    "WHERE t.CheckNo = @CheckNo AND t.TrnSeq = @TrnSeq AND t.RVC = @RVCNo;"
);

/// `OrderCommandSql.CgTrnExists`.
pub const CG_TRN_EXISTS: &str =
    "SELECT CASE WHEN OBJECT_ID(N'dbo.CGTrn', N'U') IS NULL THEN 0 ELSE 1 END;";

/// `OrderCommandSql.ReadHeader`.
pub const READ_HEADER: &str = "SELECT CheckNo, TableNo, NGuest, SubTotal, TotalPaid FROM dbo.TmpInvoice WHERE CheckNo = @CheckNo AND RVC = @RVCNo;";

macro_rules! read_lines_select {
    () => {
        concat!(
            "SELECT t.TrnSeq, t.TrnCode, t.ItemCode, CAST(t.TrnDesc AS VARBINARY(510)), t.TrnQty, t.ItemPrice, t.BaseTTL, ",
            "t.MealPeriod, t.Cashier, CASE WHEN i.POrderTime IS NOT NULL OR ISNULL(i.PToOrder, 0) = 1 THEN 1 ELSE 0 END"
        )
    };
}

macro_rules! read_lines_from {
    () => {
        concat!(
            " FROM dbo.TmpTrn t OUTER APPLY (SELECT TOP 1 ti.POrderTime, ti.PToOrder FROM dbo.tmpTrnInfo ti ",
            "WHERE ti.TrnNo = t.TrnSeq AND ti.CheckNo = t.CheckNo) i WHERE t.CheckNo = @CheckNo ORDER BY t.TrnSeq;"
        )
    };
}

/// `OrderCommandSql.ReadLines` (TrnDesc / CGName are TCVN3 bytes: read raw).
pub const READ_LINES: &str = concat!(
    read_lines_select!(),
    ", CAST(NULL AS VARBINARY(300))",
    read_lines_from!()
);

/// `OrderCommandSql.ReadLinesWithNotes`.
pub const READ_LINES_WITH_NOTES: &str = concat!(
    read_lines_select!(),
    ", (SELECT TOP 1 CAST(g.CGName AS VARBINARY(300)) FROM dbo.CGTrn g WHERE g.TrnNo = t.TrnSeq AND g.CGCode = 0)",
    read_lines_from!()
);

// ---- SqlTableCommandUnit (read side, OW1e)

/// `OrderCommandSql.MoneyFacts`: the paid amount on the header, any tender line (TrnCode
/// 200 and up) and any discount line (TrnCode 101..189). Params: @CheckNo, @RVCNo.
pub const MONEY_FACTS: &str = concat!(
    "SELECT ISNULL((SELECT TOP 1 TotalPaid FROM dbo.TmpInvoice WHERE CheckNo = @CheckNo AND RVC = @RVCNo), 0), ",
    "CASE WHEN EXISTS (SELECT 1 FROM dbo.TmpTrn WHERE CheckNo = @CheckNo AND RVC = @RVCNo AND TrnCode >= 200) THEN 1 ELSE 0 END, ",
    "CASE WHEN EXISTS (SELECT 1 FROM dbo.TmpTrn WHERE CheckNo = @CheckNo AND RVC = @RVCNo AND TrnCode BETWEEN 101 AND 189) ",
    "THEN 1 ELSE 0 END;"
);

/// `OrderCommandSql.EmptyBillFacts` (ADR 0144): the trimmed table, 1 when `OpenTime` is
/// earlier than the cutoff, the count of every `TmpTrn` line. Params: @CheckNo, @RVCNo,
/// @OpenedBefore (`yyyy-MM-ddTHH:mm:ss`, store local); no row when the check is not in
/// the RVC.
pub const EMPTY_BILL_FACTS: &str = concat!(
    "SELECT ISNULL(RTRIM(LTRIM(i.TableNo)), ''), ",
    "CASE WHEN i.OpenTime < CONVERT(datetime, @OpenedBefore, 126) THEN 1 ELSE 0 END, ",
    "(SELECT COUNT(*) FROM dbo.TmpTrn t WHERE t.CheckNo = @CheckNo AND t.RVC = @RVCNo) ",
    "FROM dbo.TmpInvoice i WHERE i.CheckNo = @CheckNo AND i.RVC = @RVCNo;"
);

/// `OrderCommandSql.MissingColumnsHead`.
pub const MISSING_COLUMNS_HEAD: &str = "SELECT need.t, need.c FROM (VALUES ";

/// `OrderCommandSql.MissingColumnsTail`.
pub const MISSING_COLUMNS_TAIL: &str = concat!(
    ") AS need(t, c) WHERE NOT EXISTS (SELECT 1 FROM INFORMATION_SCHEMA.COLUMNS ic ",
    "WHERE ic.TABLE_SCHEMA = N'dbo' AND ic.TABLE_NAME = need.t AND ic.COLUMN_NAME = need.c);"
);

/// `SqlTableCommandUnit.MissingColumnsSql`: the column probe for `count` columns,
/// parameter names only, never a value in the text.
pub fn missing_columns_sql(count: usize) -> String {
    let mut sql = String::from(MISSING_COLUMNS_HEAD);
    for i in 0..count {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!("(@t{i}, @c{i})"));
    }
    sql.push_str(MISSING_COLUMNS_TAIL);
    sql
}

/// `SqlTableCommandUnit.AuditSavepoint`.
pub const AUDIT_SAVEPOINT: &str = "ow1e_move_audit";
