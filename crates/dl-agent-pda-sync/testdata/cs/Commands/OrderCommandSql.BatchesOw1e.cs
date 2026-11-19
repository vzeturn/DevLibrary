namespace MM.PdaOrder.SyncAgent.Commands;

// OW1e (ADR 0112): the store batches of the table and multi-check verbs, ported from the legacy PDA services (tag
// onprem-final-2026-10-26) like the OW1b/OW1c batches. Kept verbatim: the multi-line batches are the legacy constants byte
// for byte (SHA-256 pinned by tests), the one-statement ones are the legacy service statements unchanged. The vendor columns
// they write are granted to pda_sync by Bootstrap/Sql/090_order_grants_ow1e.sql. What OW1e changes is around them, never in
// them: the agent locks every touched check header first (ascending check number), checks each expected state, refuses a
// check with a payment, and probes the touched columns before running anything (TableSchemaGuard).
// Do not edit them to taste: POS WIN parity is the contract.
internal static partial class OrderCommandSql
{
    /// <summary>
    /// Legacy <c>SqlTableInfo</c> (table change), verbatim. Params: @CheckNo, @RVCNo, @TableNo, @NumGuest. Sets the table and
    /// guest count of the check header and marks the new table busy in <c>dbo.tablelist</c> when it is listed there.
    /// </summary>
    internal const string TableInfo = @"SET NOCOUNT ON;
-- pda.spTableInfo body inlined (P6a): @RVC=@RVCNo, @CheckNo=@CheckNo, @ClerkID=@Cashier,
-- @TableNo=@TableNo, @Adult=@NumGuest, @Child=0, @Note=N''.
UPDATE dbo.tmpinvoice
    SET TableNo = @TableNo,
        NGuest   = @NumGuest,
        NChild   = 0
    WHERE CheckNo = @CheckNo AND RVC = @RVCNo;

IF EXISTS (SELECT 1 FROM dbo.tablelist WHERE TableNum = @TableNo AND RVC = @RVCNo)
    UPDATE dbo.tablelist
        SET TableStatus = 1
        WHERE TableNum = @TableNo AND RVC = @RVCNo;";

    /// <summary>Legacy transfer, header statement. Params: @ToTableNo (trimmed), @CheckNo, @RVCNo.</summary>
    internal const string TransferHeader =
        "UPDATE dbo.TmpInvoice SET TableNo = @ToTableNo WHERE CheckNo = @CheckNo AND RVC = @RVCNo";

    /// <summary>Legacy transfer, line-info statement. Params: @ToTableNo (trimmed), @CheckNo.</summary>
    internal const string TransferLineInfo =
        "UPDATE dbo.tmpTrnInfo SET TableNo = @ToTableNo WHERE CheckNo = @CheckNo";

    /// <summary>
    /// Legacy merge/move header lock, verbatim. Params: @CheckNo, @RVCNo. Returns the header's CheckNo and Status, update-locked
    /// and range-held until commit; no row when the check is not in the RVC. OW1e runs it per check in ascending check order.
    /// </summary>
    internal const string LockCheck =
        "SELECT CheckNo, Status FROM dbo.TmpInvoice WITH (UPDLOCK, HOLDLOCK) WHERE CheckNo = @CheckNo AND RVC = @RVCNo";

    /// <summary>Legacy merge, line statement. Params: @Target, @Source, @RVCNo.</summary>
    internal const string MergeLines =
        "UPDATE dbo.TmpTrn SET CheckNo = @Target WHERE CheckNo = @Source AND RVC = @RVCNo";

    /// <summary>Legacy merge, line-info statement. Params: @Target, @Source.</summary>
    internal const string MergeLineInfo =
        "UPDATE dbo.tmpTrnInfo SET CheckNo = @Target WHERE CheckNo = @Source";

    /// <summary>
    /// Legacy <c>SqlCancelOrder</c>, verbatim: closes the emptied merge source. Params: @CheckNo, @RVCNo, @CancelBy. Raises
    /// HAS_ORDERS when the check still carries an item line (never after a merge moved every line).
    /// </summary>
    internal const string CancelOrder = @"SET NOCOUNT ON;
-- pda.spCancelOrder body inlined (P6a): @RVC=@RVCNo, @CheckNo=@CheckNo.
-- 5★ guard: refuse to delete a check that already carries order lines (TrnCode=100).
IF EXISTS (SELECT 1 FROM dbo.tmpTrn
           WHERE CheckNo = @CheckNo AND RVC = @RVCNo AND TrnCode = 100)
BEGIN
    RAISERROR('HAS_ORDERS', 16, 1);
    RETURN;
END;

-- Desktop parity (frmCheck.HideCheck, frmCheck.cs:3993-4001): archive the cancelled
-- empty check to InvoiceCxl before the tmpInvoice header is deleted below, so an
-- empty-check cancel stays visible in the vendor cancel-audit trail. Guarded by
-- OBJECT_ID so this stays a safe no-op on a deployment without the table.
IF OBJECT_ID('dbo.InvoiceCxl') IS NOT NULL
BEGIN
    -- // lint:allow-tracked-write: INSERT INTO InvoiceCxl — the cancelled-invoice audit table. Additive audit copy ported from frmCheck.HideCheck; cooperative write on the SMILE money path.
    INSERT INTO dbo.InvoiceCxl SELECT * FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo;
    UPDATE dbo.InvoiceCxl SET PaymentMode = 'Z_CANCEL', CloseBy = @CancelBy WHERE CheckNo = @CheckNo;
END;

-- // lint:allow-tracked-write: DELETE FROM tmpTrnInfo — the open-bill line-info staging table. Inlined body of the legacy POS proc this call replaces (schema pda dropped); parity-tested against the original. Cooperative write on the SMILE money path.
DELETE FROM dbo.tmpTrnInfo WHERE CheckNo = @CheckNo;
-- // lint:allow-tracked-write: DELETE FROM tmpTrn — the open-bill line staging table. Inlined body of the legacy POS proc this call replaces (schema pda dropped); parity-tested against the original. Cooperative write on the SMILE money path.
DELETE FROM dbo.tmpTrn WHERE CheckNo = @CheckNo AND RVC = @RVCNo;
DELETE FROM dbo.tmpInvoiceInfo WHERE CheckNo = @CheckNo;
-- // lint:allow-tracked-write: DELETE FROM tmpInvoice — the open-bill header staging table. Inlined body of the legacy POS proc this call replaces (schema pda dropped); parity-tested against the original. Cooperative write on the SMILE money path.
DELETE FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo AND RVC = @RVCNo;

UPDATE dbo.checkopenlog
    SET isOpen = 0, CloseTime = GETDATE(), LastAction = GETDATE()
    WHERE CheckNo = @CheckNo AND RVC = @RVCNo;";

    /// <summary>
    /// Legacy <c>SqlReCalcBill</c>, verbatim. Params: @CheckNo. Returns one row whose first column is RecalcFailed (0 ok); the
    /// agent refuses to commit a failed recalculation (the transaction rolls back).
    /// </summary>
    internal const string ReCalcBill = @"SET NOCOUNT ON;
    IF OBJECT_ID('dbo.UpdateTmpInvoice', 'P') IS NOT NULL
    BEGIN
        BEGIN TRY
            EXEC dbo.UpdateTmpInvoice @CheckNo;
        END TRY
        BEGIN CATCH
            SELECT
                1                                   AS RecalcFailed,
                ERROR_NUMBER()                      AS ErrNo,
                ERROR_MESSAGE()                     AS ErrMsg;
            RETURN;
        END CATCH
    END;
    SELECT 0 AS RecalcFailed, 0 AS ErrNo, CAST('' AS NVARCHAR(400)) AS ErrMsg;";

    /// <summary>
    /// Legacy move source lock, verbatim. Params: @CheckNo, @RVCNo. The item lines (TrnCode 100) of the source check,
    /// update-locked and range-held until commit.
    /// </summary>
    internal const string MoveSourceLines =
        "SELECT TrnSeq FROM dbo.TmpTrn WITH (UPDLOCK, HOLDLOCK) WHERE CheckNo = @CheckNo AND RVC = @RVCNo AND TrnCode = 100";

    /// <summary>Legacy move, per-line statement. Params: @ToCheckNo, @TrnSeq, @FromCheckNo, @RVCNo.</summary>
    internal const string MoveLine =
        "UPDATE dbo.TmpTrn SET CheckNo = @ToCheckNo WHERE TrnSeq = @TrnSeq AND CheckNo = @FromCheckNo AND RVC = @RVCNo";

    /// <summary>Legacy move, per-line info statement (KDS keeps finding the moved line). Params: @ToCheckNo, @TrnNo, @FromCheckNo.</summary>
    internal const string MoveLineInfo =
        "UPDATE dbo.tmpTrnInfo SET CheckNo = @ToCheckNo WHERE TrnNo = @TrnNo AND CheckNo = @FromCheckNo";

    /// <summary>
    /// Legacy <c>SqlLogTrnMove</c>, verbatim: one <c>dbo.TrnActivities</c> MOVE row per moved line, keyed by the destination.
    /// Params: @RVC, @FromCheckNo, @ToCheckNo, @Cashier, @TrnSeqCsv. Best-effort as in the legacy service: a failure rolls back
    /// to a savepoint and the move stands.
    /// </summary>
    internal const string LogTrnMove = @"SET NOCOUNT ON;
    IF ISNULL(LTRIM(RTRIM(@TrnSeqCsv)), '') = '' RETURN;
    -- // lint:allow-tracked-write: INSERT INTO TrnActivities — the transaction audit log. Inlined body of the legacy POS proc this call replaces (schema pda dropped); parity-tested against the original. Cooperative write on the SMILE money path.
    INSERT INTO dbo.TrnActivities (tTime, RVC, Cashier, CheckNo, ToCheckNo, TrnNo, ActCode, ActName, TrnAmount)
    SELECT
        GETDATE(), @RVC, LEFT(@Cashier, 30), @FromCheckNo, @ToCheckNo, t.TrnSeq, 3, 'MOVE', t.BaseTTL
    FROM dbo.TmpTrn t
    JOIN (SELECT CAST(TRIM(value) AS BIGINT) AS TrnSeq
          FROM STRING_SPLIT(@TrnSeqCsv, ',')
          WHERE TRIM(value) <> '' AND ISNUMERIC(TRIM(value)) = 1) m
      ON m.TrnSeq = t.TrnSeq
    WHERE t.CheckNo = @ToCheckNo AND t.RVC = @RVC;";
}
