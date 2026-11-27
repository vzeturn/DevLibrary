namespace MM.PdaOrder.SyncAgent.Commands;

// RT-1413: the guest of a check (picked in the PDA) written to the store's TmpInvoice.PONumber as "room-name", so POS WIN shows
// whose bill it is. Not a legacy PDA batch: the legacy PDA never wrote the column. It writes PONumber and nothing else (never
// TableNo or NGuest), encodes the text with the vendor dbo.Unicode2TCVN (granted in 070) like the other batches that store text,
// and cuts it to the column's own length (unknown length: the 50 the cloud already caps at). Granted by 200_po_number_grants.sql.
internal static partial class OrderCommandSql
{
    /// <summary>
    /// Sets <c>TmpInvoice.PONumber</c> of an open check. Params: @CheckNo, @RVCNo, @PONumber (empty clears: NULL, or '' when the
    /// column is NOT NULL).
    /// </summary>
    internal const string SetPoNumber = @"SET NOCOUNT ON;
DECLARE @PoMax int = ISNULL(COLUMNPROPERTY(OBJECT_ID(N'dbo.tmpInvoice'), N'PONumber', 'CharMaxLen'), 50);
DECLARE @PoText varchar(4000) = dbo.Unicode2TCVN(NULLIF(LTRIM(RTRIM(@PONumber)), N''));
IF @PoMax > 0 SET @PoText = RTRIM(LEFT(@PoText, @PoMax));
IF @PoText IS NULL AND ISNULL(COLUMNPROPERTY(OBJECT_ID(N'dbo.tmpInvoice'), N'PONumber', 'AllowsNull'), 0) = 0 SET @PoText = '';
UPDATE dbo.tmpInvoice
    SET PONumber = @PoText
    WHERE CheckNo = @CheckNo AND RVC = @RVCNo;";
}
