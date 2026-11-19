namespace MM.PdaOrder.SyncAgent.Commands;

// OW1c (ADR 0107): the store batch of the covers verb, ported from the legacy PDA order repository like the OW1b batches.
// Kept verbatim; the only vendor columns it writes are granted to pda_sync by Bootstrap/Sql/080_order_grants_ow1c.sql.
// Do not edit it to taste: POS WIN parity is the contract.
internal static partial class OrderCommandSql
{
    /// <summary>
    /// Legacy <c>SqlSetCovers</c>, verbatim. Params: @RVCNo, @CheckNo, @NumGuest, @NumChild, @Cashier. Returns one status row
    /// (IsOut, MessageResult): IsOut 1 refuses without writing (counts negative, check missing or not open).
    /// </summary>
    internal const string SetCovers = @"SET NOCOUNT ON;

IF @NumGuest < 0 OR @NumChild < 0
BEGIN
    SELECT 1 AS IsOut, N'Số khách không hợp lệ' AS MessageResult;
    RETURN;
END;

DECLARE @Status INT;
SELECT @Status = Status
FROM dbo.TmpInvoice
WHERE CheckNo = @CheckNo AND RVC = @RVCNo;

IF @Status IS NULL
BEGIN
    SELECT 1 AS IsOut, N'Không tìm thấy hóa đơn' AS MessageResult;
    RETURN;
END;

IF @Status <> 1
BEGIN
    SELECT 1 AS IsOut, N'Hóa đơn đã đóng, không thể sửa số khách' AS MessageResult;
    RETURN;
END;

UPDATE dbo.TmpInvoice
   SET NGuest         = @NumGuest,
       NChild         = ISNULL(@NumChild, 0),
       LastChgCashier = @Cashier,
       LastChgTime    = GETDATE()
 WHERE RVC = @RVCNo AND CheckNo = @CheckNo;

SELECT 0 AS IsOut, N'OK' AS MessageResult;";
}
