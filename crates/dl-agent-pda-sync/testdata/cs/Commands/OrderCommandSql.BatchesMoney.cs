namespace MM.PdaOrder.SyncAgent.Commands;

// ADR 0174 (RT-1297): the money batches of a site_sync tenant, ported from the legacy PDA payment and close SQL (IIS tag
// onprem-final-2026-10-26: PaymentRepository.SqlAddPayment / SqlRemovePayment, OrderRepository.SqlCloseBill). The writes are the
// legacy ones, row for row, so POS WIN reads a PDAOrder tender or close exactly as its own. Changes from the IIS text, all forced
// by the agent running them inside its command transaction with its log row:
//  - each early refusal of the legacy text answers with a status row (refusal flag and reason) instead of raising an error,
//    so a refusal is a logged rejection.
//  - the close opens no transaction of its own (the agent's transaction is the unit) and holds an update lock on the header.
//  - the tender remove also deletes the tender's dbo.tmpPayment rows (the IIS text left them to be closed into dbo.Payment)
//    and re-totals the header with dbo.UpdateTmpInvoice, as the add does.
//  - the base tender only: no foreign-currency rows (the SPA cashier takes VND) and the tip branch is refused by the payload.
// The vendor objects written are granted to pda_sync by Bootstrap/Sql/180_money_grants.sql.
internal static partial class OrderCommandSql
{
    /// <summary>
    /// Legacy <c>SqlAddPayment</c>, base currency. Params: @RVCNo, @CheckNo, @Cashier, @PaymentType, @Amount, @Tendered,
    /// @Reference, @CardType (int, null), @BillNo (null). Last result set: (IsOut, Reason, TrnSeq); IsOut 0 = posted.
    /// </summary>
    internal const string TenderAdd = @"SET NOCOUNT ON;

DECLARE @AP_Status INT;
SELECT @AP_Status = Status FROM dbo.tmpInvoice WITH (UPDLOCK, HOLDLOCK) WHERE CheckNo = @CheckNo AND RVC = @RVCNo;
IF @AP_Status IS NULL BEGIN SELECT 1 AS IsOut, 'check_not_found' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;
IF @AP_Status <> 1 BEGIN SELECT 1 AS IsOut, 'bill_not_open' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;

-- Guard 3 (tender lock) is NOT applied to CASH / C.CARD: SMILE allows split tenders for these two types only.
IF UPPER(@PaymentType) NOT IN ('CASH', 'C.CARD') AND EXISTS (
    SELECT 1 FROM dbo.tmpTrn WHERE CheckNo = @CheckNo AND TrnCode >= 200 AND TrnCode <> 810
)
BEGIN SELECT 1 AS IsOut, 'tender_locked' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;

-- Guard 1 (Bill Number), before any write.
DECLARE @BillNoAlways BIT, @BillNoCash BIT, @BillNoLength INT;
SELECT @BillNoAlways = CASE WHEN ParaStr = '1' THEN 1 ELSE 0 END FROM dbo.GlobPara WHERE ParaName = 'BillNoAlways';
SELECT @BillNoCash   = CASE WHEN ParaStr = '1' THEN 1 ELSE 0 END FROM dbo.GlobPara WHERE ParaName = 'BillNoCash';
SELECT @BillNoLength = TRY_CAST(ParaStr AS INT) FROM dbo.GlobPara WHERE ParaName = 'BillNoLength';

DECLARE @RequireBillNo BIT = CASE
    WHEN ISNULL(@BillNoAlways, 0) = 1 THEN 1
    WHEN ISNULL(@BillNoCash, 0) = 1 AND UPPER(@PaymentType) IN ('CASH', 'C.CARD') THEN 1
    ELSE 0 END;

IF @RequireBillNo = 1
BEGIN
    IF ISNULL(@BillNo, '') = ''
    BEGIN SELECT 1 AS IsOut, 'bill_number_invalid' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;

    IF ISNULL(@BillNoLength, 0) > 0
    BEGIN
        IF LEN(@BillNo) <> @BillNoLength
        BEGIN SELECT 1 AS IsOut, 'bill_number_invalid' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;

        IF EXISTS (SELECT 1 FROM dbo.TaxTable WHERE BillNo = @BillNo)
        BEGIN SELECT 1 AS IsOut, 'bill_number_in_use' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;
    END;
END;

-- TrnCode from dbo.PaymentMode, no fallback (SMILE: 'Please setup Payment Trn Code!'); joined on PType.
DECLARE @TrnCode DECIMAL(18,0);
DECLARE @TrnSub  DECIMAL(18,0) = 0;
DECLARE @AP_PmtCode INT, @AP_PmtCrCode INT = 1, @AP_PMode VARCHAR(30), @AP_PType VARCHAR(20);
SELECT TOP 1
    @TrnCode      = COALESCE(NULLIF(pm.TrnCode, 0), NULLIF(pm.ZeroPayment, 0)),
    @TrnSub       = ISNULL(pm.TrnSub, 0),
    @AP_PMode     = LTRIM(RTRIM(pm.PMode)),
    @AP_PType     = NULLIF(LTRIM(RTRIM(CONVERT(VARCHAR(20), pm.TrnCode))), '')
FROM dbo.PaymentMode pm
WHERE LTRIM(RTRIM(pm.PType)) = LTRIM(RTRIM(@PaymentType));
-- RT-1297: the cloud sends the code of its payment-mode config (@CfgTrnCode): POS Win sites may keep PaymentMode.TrnCode = 0
-- (POS Win posts 200/400/900 itself). It wins when it is a settlement code of this store, else the tender is refused.
IF @CfgTrnCode IS NOT NULL
BEGIN
    IF NOT EXISTS (SELECT 1 FROM dbo.TransactionCode WHERE TrnCode = @CfgTrnCode AND TrnType >= 100)
    BEGIN SELECT 1 AS IsOut, 'tender_not_configured' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;
    SET @TrnCode = @CfgTrnCode;
    SET @TrnSub  = ISNULL(@CfgTrnSub, 0);
END;
SET @AP_PmtCode = @TrnCode;

IF @TrnCode IS NULL
BEGIN SELECT 1 AS IsOut, 'tender_not_configured' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;

IF @TrnCode = 400 AND @CardType IS NULL
BEGIN SELECT 1 AS IsOut, 'invalid_card_type' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;

DECLARE @AP_IYear INT, @AP_IMonth INT, @AP_IDay INT;
SELECT @AP_IYear = IYear, @AP_IMonth = IMonth, @AP_IDay = IDay FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo;

DECLARE @AP_BaseDecDiv NUMERIC(18,2);
SELECT @AP_BaseDecDiv = DecDiv FROM dbo.CurrencyDef WHERE CrCode = 1;
IF ISNULL(@AP_BaseDecDiv, 0) = 0 SET @AP_BaseDecDiv = 1;

DECLARE @AP_Amount NUMERIC(16,0) = -ROUND(@Amount * @AP_BaseDecDiv, 0);

DECLARE @AP_NewSeq TABLE (v BIGINT);
DECLARE @AP_ParaStr BIGINT;
INSERT INTO @AP_NewSeq (v) EXEC dbo.GetRVCSeqNumber 'LastTrn', @RVCNo, @Cashier, '';
SELECT @AP_ParaStr = v FROM @AP_NewSeq;

DECLARE @AP_TrnSeq NUMERIC(11,0) = CAST(@RVCNo AS BIGINT) * 100000000 + @AP_ParaStr;

DECLARE @AP_ItemCode VARCHAR(12) = CASE
    WHEN @TrnCode = 400 THEN CAST(@CardType AS VARCHAR(12))
    WHEN UPPER(@PaymentType) = 'FOC' THEN '0'
    ELSE LEFT(@PaymentType, 12) END;

DECLARE @AP_Desc VARCHAR(255) = LEFT(RTRIM(@PaymentType), 255);

INSERT INTO dbo.tmpTrn
    (NYear, NMonth, NDay, TrnSeq, CheckNo, TrnTime, Cashier, TrnCode, TrnSub,
     ItemCode, TrnDesc, ItemPrice, BaseSub, BaseTax, BaseTTL, TrnOrg, TrnBase, TrnPref,
     TrnQty, RVC, OrgCrCode, NewTrn, Refund, Status, OrderNo, OrgCheck, Rsv1)
VALUES
    (@AP_IYear, @AP_IMonth, @AP_IDay, @AP_TrnSeq, @CheckNo, GETDATE(), @Cashier, @TrnCode, @TrnSub,
     @AP_ItemCode, @AP_Desc, 0, 0, 0, @AP_Amount, @AP_Amount, @AP_Amount, 0,
     1, @RVCNo, 1, 1, 0, 1, 0, @CheckNo, LEFT(ISNULL(dbo.Unicode2TCVN(@Reference), ''), 50));

DECLARE @AP_NewPay TABLE (v BIGINT);
DECLARE @AP_PaymentNo BIGINT;
INSERT INTO @AP_NewPay(v) EXEC dbo.GetRVCSeqNumber 'LastPayment', @RVCNo, @Cashier, '';
SELECT @AP_PaymentNo = v FROM @AP_NewPay;

INSERT INTO dbo.tmpPayment
    (PaymentNo, TrnNo, CheckNo, PayTime, Cashier, PmtCode, PmtApp, CrCode,
     PmtDesc, PmtRef, AmountORG, AmountBase, AmountPref, RVC, Status, PYear, PMonth, PDay)
VALUES
    (@AP_PaymentNo, @AP_TrnSeq, @CheckNo, GETDATE(), @Cashier, @AP_PmtCode, 1, @AP_PmtCrCode,
     LEFT(RTRIM(@PaymentType), 30), LEFT(ISNULL(@Reference, N''), 30),
     ROUND(@Tendered * @AP_BaseDecDiv, 0), ROUND(@Tendered * @AP_BaseDecDiv, 0), 0,
     @RVCNo, 1, @AP_IYear, @AP_IMonth, @AP_IDay);

UPDATE dbo.tmpInvoice
   SET PaymentMode = ISNULL(@AP_PMode, @PaymentType),
       PaymentType = CASE WHEN ISNULL(@AP_PType, '') <> '' THEN @AP_PType ELSE PaymentType END,
       PaymentInfo = LEFT(ISNULL(@Reference, N''), 50)
 WHERE CheckNo = @CheckNo;

IF @RequireBillNo = 1 AND ISNULL(@BillNo, '') <> ''
BEGIN
    IF EXISTS (SELECT 1 FROM dbo.tmpTaxTable WHERE TCheckNo = @CheckNo)
        UPDATE dbo.tmpTaxTable SET BillNo = @BillNo WHERE TCheckNo = @CheckNo;
    ELSE
        INSERT INTO dbo.tmpTaxTable (TCheckNo, BillNo) VALUES (@CheckNo, @BillNo);
END;

EXEC dbo.UpdateTmpInvoice @CheckNo;

SELECT 0 AS IsOut, 'ok' AS Reason, CAST(@AP_TrnSeq AS BIGINT) AS TrnSeq;";

    /// <summary>
    /// Legacy <c>SqlRemovePayment</c> plus the tender's <c>dbo.tmpPayment</c> rows and the header re-total. Params: @RVCNo,
    /// @CheckNo, @TrnSeq, @Cashier. A tender is a line whose TrnCode a <c>dbo.PaymentMode</c> row posts. Last result set:
    /// (IsOut, Reason).
    /// </summary>
    internal const string TenderRemove = @"SET NOCOUNT ON;

DECLARE @RP_Status INT;
SELECT @RP_Status = Status FROM dbo.tmpInvoice WITH (UPDLOCK, HOLDLOCK) WHERE CheckNo = @CheckNo AND RVC = @RVCNo;
IF @RP_Status IS NULL BEGIN SELECT 1 AS IsOut, 'check_not_found' AS Reason; RETURN; END;
IF @RP_Status <> 1 BEGIN SELECT 1 AS IsOut, 'bill_not_open' AS Reason; RETURN; END;

IF NOT EXISTS (
    SELECT 1 FROM dbo.tmpTrn t
    WHERE t.TrnSeq = @TrnSeq AND t.CheckNo = @CheckNo AND t.RVC = @RVCNo
      AND (t.TrnCode IN (SELECT COALESCE(NULLIF(pm.TrnCode, 0), NULLIF(pm.ZeroPayment, 0)) FROM dbo.PaymentMode pm)
           OR t.TrnCode IN (SELECT tc.TrnCode FROM dbo.TransactionCode tc WHERE tc.TrnType >= 100 AND tc.TrnCode NOT IN (800, 810)))
)
BEGIN SELECT 1 AS IsOut, 'tender_not_found' AS Reason; RETURN; END;

DELETE FROM dbo.tmpPayment WHERE TrnNo = @TrnSeq AND CheckNo = @CheckNo;
DELETE FROM dbo.tmpTrn WHERE TrnSeq = @TrnSeq AND CheckNo = @CheckNo AND RVC = @RVCNo;

-- ADR 0179: the room-charge line goes, the room, folio and payment mode it set on the header go with it (no FO reversal: the post is at close).
IF NOT EXISTS (SELECT 1 FROM dbo.tmpTrn tn JOIN dbo.TransactionCode tc ON tn.TrnCode = tc.TrnCode AND tn.TrnSub = tc.TrnSubCode
               WHERE tc.TrnType = 106 AND tn.CheckNo = @CheckNo)
   AND EXISTS (SELECT 1 FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo AND ISNULL(FolioNum, 0) <> 0)
    UPDATE dbo.tmpInvoice SET RoomCode = N'', FolioNum = 0, PaymentMode = NULL, PaymentType = NULL, PaymentInfo = NULL WHERE CheckNo = @CheckNo;

EXEC dbo.UpdateTmpInvoice @CheckNo;

SELECT 0 AS IsOut, 'ok' AS Reason;";

    /// <summary>
    /// Legacy <c>SqlCloseBill</c> without its own transaction. Params: @RVCNo, @CheckNo, @CloseBy. Last result set:
    /// (IsOut, Reason, CloseNo, SubTotal, TotalPaid); IsOut 0 = closed now, 2 = already closed (dbo.Invoice has it).
    /// </summary>
    internal const string CheckClose = @"SET NOCOUNT ON;

IF NOT EXISTS (SELECT 1 FROM dbo.tmpInvoice WITH (UPDLOCK, HOLDLOCK) WHERE CheckNo = @CheckNo AND RVC = @RVCNo)
BEGIN
    IF EXISTS (SELECT 1 FROM dbo.Invoice WHERE CheckNo = @CheckNo AND RVC = @RVCNo)
        SELECT TOP 1 2 AS IsOut, 'already_closed' AS Reason,
               CAST(ISNULL((SELECT TOP 1 Sub2 FROM dbo.TaxTable WHERE TCheckNo = @CheckNo), 0) AS BIGINT) AS CloseNo,
               CAST(ISNULL(SubTotal, 0) AS DECIMAL(18,2)) AS SubTotal, CAST(ISNULL(TotalPaid, 0) AS DECIMAL(18,2)) AS TotalPaid
        FROM dbo.Invoice WHERE CheckNo = @CheckNo AND RVC = @RVCNo;
    ELSE
        SELECT 1 AS IsOut, 'check_not_found' AS Reason, CAST(0 AS BIGINT) AS CloseNo, CAST(0 AS DECIMAL(18,2)) AS SubTotal,
               CAST(0 AS DECIMAL(18,2)) AS TotalPaid;
    RETURN;
END;

IF EXISTS (SELECT 1 FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo AND RVC = @RVCNo AND Status <> 1)
BEGIN
    SELECT 1 AS IsOut, 'bill_not_open' AS Reason, CAST(0 AS BIGINT) AS CloseNo, CAST(0 AS DECIMAL(18,2)) AS SubTotal,
           CAST(0 AS DECIMAL(18,2)) AS TotalPaid;
    RETURN;
END;

EXEC dbo.UpdateTmpInvoice @CheckNo;

DECLARE @Close_Now DATETIME = GETDATE();

-- Guard 2 (SMILE parity): a bill closes iff SUM(BaseTTL) = 0 exactly (items positive, tenders and 810 rounding negative).
DECLARE @Close_Balance NUMERIC(18,2) = (SELECT ISNULL(SUM(BaseTTL), 0) FROM dbo.tmpTrn WHERE CheckNo = @CheckNo);
IF @Close_Balance <> 0
BEGIN
    SELECT 1 AS IsOut, CASE WHEN @Close_Balance > 0 THEN 'not_balanced_underpaid' ELSE 'not_balanced_overpaid' END AS Reason,
           CAST(0 AS BIGINT) AS CloseNo, CAST(0 AS DECIMAL(18,2)) AS SubTotal, CAST(0 AS DECIMAL(18,2)) AS TotalPaid;
    RETURN;
END;

-- PostToSmile (ADR 0179, RT-1297): POS WIN's mdlMain.PostToSmile (mdlMain.cs:1252-1633), run in the close transaction before the bill
-- moves to the permanent tables, as POS WIN does (BeginTransaction, PostToSmile, ProcessMbrPoint, CloseCheck, Commit). Gate, as POS WIN:
-- dbo.Para InterfaceType = 1 and InterfaceDB set for the RVC, and the check's payment mode has PaymentMode.Interface = 1; a check
-- with a room-charge line (TrnType 106) whose gate is off is refused, never closed without its post. Folio, in POS WIN's order: the
-- TrnType 106 line (ItemCode = folio, StsCode2 = balance code; the line itself is not posted), the first TrnLinkByRVC.LinkFolio,
-- the city-ledger folio (GlobPara CityLedgerFolio), PaymentMode.IntFolio. Posted: the TrnLinkByRVC lines (TrnType 110, 190, 104, 107
-- and TrnCode 800, 810, 710), credit-card lines, the tender currencies, the revenue by RVCLinkCode and the discounts by DiscountDtl;
-- the lines must sum to zero (POS WIN: 'Unbalance Interface!'). Every post goes through the vendor procedure
-- PostInterfaceTransactionToFolio of the Front Office database named by Para InterfaceDB (checked against ^[A-Za-z0-9_]+$,
-- HAS_DBACCESS = 1 and bracketed: the only text interpolated), with named parameters. A failed post rolls back to a savepoint and
-- answers folio_post_required (a status row, the close then writes nothing); a missing grant is re-raised so the agent answers
-- sql_grants_missing. Not carried over from the PDA-original procedure: the PostToSmileEnabled switch and the GuestID argument.
DECLARE @PS_Fail BIT = 0;
DECLARE @PS_IfType INT, @PS_FODB VARCHAR(128), @PS_Iface INT, @PS_IntFolio VARCHAR(50);
DECLARE @PS_Meal INT = 0, @PS_NGuest INT = 0, @PS_PONumber VARCHAR(200) = '', @PS_ExRate NUMERIC(28,16) = 1;
DECLARE @PS_S1 VARCHAR(20) = '', @PS_S2 VARCHAR(20) = '', @PS_S3 VARCHAR(20) = '', @PS_S4 VARCHAR(20) = '',
        @PS_S5 VARCHAR(20) = '', @PS_S6 VARCHAR(20) = '', @PS_S7 VARCHAR(20) = '', @PS_S8 VARCHAR(20) = CAST(@RVCNo AS VARCHAR(20));
SELECT TOP 1 @PS_IfType = TRY_CONVERT(INT, LTRIM(RTRIM(ParaStr))) FROM dbo.Para WHERE ParaName = 'InterfaceType' AND RVC = @RVCNo;
SELECT TOP 1 @PS_FODB = LEFT(LTRIM(RTRIM(ParaStr)), 128) FROM dbo.Para WHERE ParaName = 'InterfaceDB' AND RVC = @RVCNo;
SELECT TOP 1 @PS_Iface = ISNULL(P.Interface, 0), @PS_IntFolio = LTRIM(RTRIM(P.IntFolio)),
       @PS_Meal = ISNULL(I.MealPeriod, 0), @PS_NGuest = ISNULL(I.NGuest, 0), @PS_PONumber = ISNULL(I.PONumber, ''), @PS_ExRate = ISNULL(I.ExRate, 1),
       @PS_S1 = LEFT(ISNULL(I.StsCode1, ''), 20), @PS_S2 = LEFT(ISNULL(I.StsCode2, ''), 20), @PS_S3 = LEFT(ISNULL(I.StsCode3, ''), 20),
       @PS_S4 = LEFT(ISNULL(I.StsCode4, ''), 20), @PS_S5 = LEFT(ISNULL(I.StsCode5, ''), 20), @PS_S6 = LEFT(ISNULL(I.StsCode6, ''), 20),
       @PS_S7 = LEFT(ISNULL(I.StsCode7, ''), 20)
FROM dbo.tmpInvoice I
LEFT JOIN dbo.PaymentMode P ON LTRIM(RTRIM(ISNULL(I.PaymentMode, ''))) = LTRIM(RTRIM(ISNULL(P.PMode, '')))
WHERE I.CheckNo = @CheckNo AND I.RVC = @RVCNo;

DECLARE @PS_On BIT = CASE WHEN ISNULL(@PS_IfType, 0) = 1 AND ISNULL(@PS_FODB, '') <> '' AND ISNULL(@PS_Iface, 0) = 1 THEN 1 ELSE 0 END;
IF @PS_On = 0 AND EXISTS (SELECT 1 FROM dbo.tmpTrn tn JOIN dbo.TransactionCode tc ON tn.TrnCode = tc.TrnCode AND tn.TrnSub = tc.TrnSubCode
                          WHERE tc.TrnType = 106 AND tn.CheckNo = @CheckNo)
    SET @PS_Fail = 1;

DECLARE @PS_DecDiv NUMERIC(18,2) = ISNULL((SELECT DecDiv FROM dbo.CurrencyDef WHERE CrCode = 1), 1);
IF @PS_DecDiv = 0 SET @PS_DecDiv = 1;
DECLARE @PS_Now DATETIME = GETDATE();
DECLARE @PS_Sql NVARCHAR(MAX), @PS_FO NVARCHAR(300), @PS_x INT = 0;
DECLARE @PS_Folio FLOAT = 0, @PS_Run NUMERIC(18,2) = 0, @PS_Bal VARCHAR(10) = '', @PS_HasMarker BIT = 0;
DECLARE @PS_Lines TABLE (Seq INT IDENTITY(1,1) PRIMARY KEY, TCode INT, Link BIGINT, LinkFolio VARCHAR(50), LinkTax VARCHAR(30),
                         BaseTTL NUMERIC(18,2), TrnDesc VARCHAR(255), CrCode VARCHAR(20), OrgAmount NUMERIC(18,2));
DECLARE @PS_Rev TABLE (IsDsc INT, LinkCode VARCHAR(30), LinkTax VARCHAR(30), LinkSrc VARCHAR(30), LinkSTax VARCHAR(30),
                       SubAmount NUMERIC(18,2), SrcAmount NUMERIC(18,2), TaxAmount NUMERIC(18,2), STaxAmount NUMERIC(18,2), TTLAmount NUMERIC(18,2));
DECLARE @PS_Post TABLE (Seq INT IDENTITY(1,1) PRIMARY KEY, Major VARCHAR(10), Minor VARCHAR(10), Amount NUMERIC(18,2), Sub NUMERIC(18,2),
                        Tax NUMERIC(18,2), TaxCode VARCHAR(10), Srv NUMERIC(18,2), SrvCode VARCHAR(10), Cnt INT, Comment NVARCHAR(200),
                        Org NUMERIC(18,9), OrgCur VARCHAR(20), STax NUMERIC(18,2), STaxCode VARCHAR(10));
DECLARE @PS_FolioText VARCHAR(10), @PS_Major VARCHAR(10), @PS_Minor VARCHAR(10), @PS_Ref VARCHAR(30), @PS_Amount NUMERIC(18,2),
        @PS_Sub NUMERIC(18,2), @PS_Tax NUMERIC(18,2), @PS_TaxCode VARCHAR(10), @PS_Srv NUMERIC(18,2), @PS_SrvCode VARCHAR(10),
        @PS_Count INT, @PS_Comment NVARCHAR(200), @PS_Org NUMERIC(18,9), @PS_OrgCur VARCHAR(20), @PS_STax NUMERIC(18,2), @PS_STaxCode VARCHAR(10);
DECLARE @PS_i INT = 1, @PS_n INT = 0;

WHILE @PS_On = 1 AND @PS_Fail = 0
BEGIN
    IF @@TRANCOUNT = 0 OR @PS_FODB LIKE '%[^A-Za-z0-9_]%' OR ISNULL(HAS_DBACCESS(@PS_FODB), 0) <> 1
    BEGIN SET @PS_Fail = 1; BREAK; END;
    SET @PS_FO = QUOTENAME(@PS_FODB) + N'.dbo.';

    -- (a) the room-charge line: its ItemCode is the folio, its StsCode2 the balance code; the line itself is not posted.
    SELECT TOP 1 @PS_Folio = ISNULL(TRY_CONVERT(FLOAT, LTRIM(RTRIM(tn.ItemCode))), 0), @PS_Run = CAST(tn.BaseTTL / @PS_DecDiv AS NUMERIC(18,2)),
           @PS_Bal = UPPER(LTRIM(RTRIM(ISNULL(tn.StsCode2, '')))), @PS_HasMarker = 1
    FROM dbo.tmpTrn tn JOIN dbo.TransactionCode tc ON tn.TrnCode = tc.TrnCode AND tn.TrnSub = tc.TrnSubCode
    WHERE tc.TrnType = 106 AND tn.CheckNo = @CheckNo ORDER BY tn.TrnSeq;
    IF @PS_HasMarker = 1 AND @PS_Folio = 0
    BEGIN SET @PS_Fail = 1; BREAK; END;

    IF @PS_Bal = '' OR LEN(@PS_Bal) > 1 OR @PS_Bal IN ('P', 'V')
        SET @PS_Bal = 'A';
    ELSE
    BEGIN
        SET @PS_Sql = N'SELECT @PS_x = COUNT(*) FROM ' + @PS_FO + N'FolioBalanceCode WHERE BalanceCode = @PS_Bal;';
        EXEC sp_executesql @PS_Sql, N'@PS_Bal VARCHAR(10), @PS_x INT OUTPUT', @PS_Bal = @PS_Bal, @PS_x = @PS_x OUTPUT;
        IF @PS_x = 0 SET @PS_Bal = 'A';
    END;

    IF @PS_HasMarker = 1
    BEGIN
        SET @PS_Sql = N'SELECT @PS_x = COUNT(*) FROM ' + @PS_FO + N'Folio WHERE COALESCE(NoPostFlag, 0) = 0 AND FolioStatus = 2 AND FolioNum = @PS_Folio;';
        EXEC sp_executesql @PS_Sql, N'@PS_Folio FLOAT, @PS_x INT OUTPUT', @PS_Folio = @PS_Folio, @PS_x = @PS_x OUTPUT;
        IF @PS_x = 0
        BEGIN SET @PS_Fail = 1; BREAK; END;
    END;

    -- (b) the link lines of the RVC, card lines and the tender currencies.
    INSERT INTO @PS_Lines (TCode, Link, LinkFolio, LinkTax, BaseTTL, TrnDesc, CrCode, OrgAmount)
    SELECT u.TCode, TRY_CONVERT(BIGINT, ROUND(TRY_CONVERT(FLOAT, LTRIM(RTRIM(u.LinkCode))), 0)), LTRIM(RTRIM(u.LinkFolio)), LTRIM(RTRIM(u.LinkTax)),
           CAST(u.BaseTTL / @PS_DecDiv AS NUMERIC(18,2)), u.TrnDesc, LTRIM(RTRIM(u.CrCode)), CAST(u.OrgAmount AS NUMERIC(18,2))
    FROM (
        SELECT tc.TrnCode AS TCode, L.LinkCode AS LinkCode, L.LinkFolio AS LinkFolio, L.LinkTaxCode AS LinkTax,
               tn.BaseTTL AS BaseTTL, tn.TrnDesc AS TrnDesc, CAST('' AS VARCHAR(20)) AS CrCode, CAST(0 AS NUMERIC(18,2)) AS OrgAmount, tn.TrnSeq AS Ord
        FROM dbo.tmpTrn tn
        JOIN dbo.TransactionCode tc ON tn.TrnCode = tc.TrnCode AND tn.TrnSub = tc.TrnSubCode
        JOIN (SELECT * FROM dbo.TrnLinkByRVC WHERE RVC = @RVCNo) L ON L.TrnCode = tc.TrnCode AND L.TrnSubCode = tc.TrnSubCode
        WHERE tn.TrnCode <> 720 AND (tc.TrnType IN (110, 190, 104, 107) OR tn.TrnCode IN (800, 810, 710)) AND tn.CheckNo = @CheckNo
        UNION ALL
        SELECT tc.TrnCode, cc.MidAccLink, cc.PostAccLink, '', tn.BaseTTL, tn.TrnDesc, '', 0, tn.TrnSeq
        FROM dbo.tmpTrn tn
        JOIN dbo.TransactionCode tc ON tn.TrnCode = tc.TrnCode AND tn.TrnSub = tc.TrnSubCode
        JOIN dbo.CCardDef cc ON CAST(cc.CCardNo AS VARCHAR(20)) = LTRIM(RTRIM(tn.ItemCode))
        WHERE tc.TrnType = 102 AND tn.CheckNo = @CheckNo
        UNION ALL
        SELECT 200, cd.MidAccLink, cd.PostAccLink, '', SUM(pm.AmountBase) * -1, '', cd.PreAccLink, SUM(pm.AmountOrg / cd.DecDiv) * -1, 2147483647
        FROM dbo.tmpPayment pm JOIN dbo.CurrencyDef cd ON pm.CrCode = cd.CrCode
        WHERE pm.PmtApp = 1 AND pm.CheckNo = @CheckNo
        GROUP BY cd.PreAccLink, cd.MidAccLink, cd.PostAccLink
    ) u ORDER BY u.Ord;

    IF EXISTS (SELECT 1 FROM @PS_Lines WHERE ISNULL(Link, 0) < 100)
    BEGIN SET @PS_Fail = 1; BREAK; END;

    -- (b) the folio of the first link line.
    IF @PS_Folio = 0
        SELECT TOP 1 @PS_Folio = TRY_CONVERT(FLOAT, LinkFolio) FROM @PS_Lines
        WHERE TCode NOT IN (800, 810) AND ISNULL(TRY_CONVERT(FLOAT, LinkFolio), 0) <> 0 ORDER BY Seq;

    SET @PS_Run = @PS_Run + ISNULL((SELECT SUM(BaseTTL) FROM @PS_Lines), 0);
    INSERT INTO @PS_Post (Major, Minor, Amount, Sub, Tax, TaxCode, Srv, SrvCode, Cnt, Comment, Org, OrgCur, STax, STaxCode)
    SELECT CAST(Link / 100 AS VARCHAR(10)), CAST(Link % 100 AS VARCHAR(10)), BaseTTL, 0, 0, LEFT(ISNULL(LinkTax, ''), 10), 0, '', 0,
           LEFT(CASE WHEN TCode = 810 THEN @PS_PONumber ELSE LTRIM(RTRIM(ISNULL(TrnDesc, ''))) END, 200),
           CASE WHEN ISNULL(CrCode, '') <> '' THEN OrgAmount ELSE 0 END, ISNULL(CrCode, ''), NULL, ''
    FROM @PS_Lines ORDER BY Seq;

    -- (c) the city ledger: a 104/107 line without a link posts nothing itself but names the folio.
    IF @PS_HasMarker = 0 AND NOT EXISTS (SELECT 1 FROM @PS_Post)
        AND EXISTS (SELECT 1 FROM dbo.tmpTrn tn JOIN dbo.TransactionCode tc ON tn.TrnCode = tc.TrnCode AND tn.TrnSub = tc.TrnSubCode
                    WHERE tc.TrnType IN (104, 107) AND tn.CheckNo = @CheckNo)
    BEGIN
        SELECT TOP 1 @PS_Run = CAST(tn.BaseTTL / @PS_DecDiv AS NUMERIC(18,2))
        FROM dbo.tmpTrn tn JOIN dbo.TransactionCode tc ON tn.TrnCode = tc.TrnCode AND tn.TrnSub = tc.TrnSubCode
        WHERE tc.TrnType IN (104, 107) AND tn.CheckNo = @CheckNo ORDER BY tn.TrnSeq;
        SELECT TOP 1 @PS_Folio = ISNULL(TRY_CONVERT(FLOAT, LTRIM(RTRIM(ParaStr))), 0) FROM dbo.GlobPara WHERE ParaName = 'CityLedgerFolio';
        IF @PS_Folio = 0
        BEGIN SET @PS_Fail = 1; BREAK; END;
    END;

    -- (d) the payment mode's own folio.
    IF @PS_Folio = 0
        SET @PS_Folio = ISNULL(TRY_CONVERT(FLOAT, @PS_IntFolio), 0);
    IF @PS_Folio = 0
    BEGIN SET @PS_Fail = 1; BREAK; END;

    SET @PS_Sql = N'SELECT @PS_x = COUNT(*) FROM ' + @PS_FO + N'Folio WHERE FolioStatus IN (2, 6) AND FolioNum = @PS_Folio;';
    EXEC sp_executesql @PS_Sql, N'@PS_Folio FLOAT, @PS_x INT OUTPUT', @PS_Folio = @PS_Folio, @PS_x = @PS_x OUTPUT;
    IF @PS_x = 0
    BEGIN SET @PS_Fail = 1; BREAK; END;

    -- The revenue by RVCLinkCode and the discounts by DiscountDtl, with their tax and service links.
    INSERT INTO @PS_Rev (IsDsc, LinkCode, LinkTax, LinkSrc, LinkSTax, SubAmount, SrcAmount, TaxAmount, STaxAmount, TTLAmount)
    SELECT 0, rl.LinkCode, td.PreAccLink, sd.PreAccLink, st.LinkCode,
           SUM(tn.BaseSub) / @PS_DecDiv, SUM(tn.BaseSrc) / @PS_DecDiv, SUM(tn.BaseTax) / @PS_DecDiv, SUM(tn.BaseSTax) / @PS_DecDiv, SUM(tn.BaseTTL) / @PS_DecDiv
    FROM (SELECT ClassCode, TrnCode, TrnSub, BaseSub, BaseSrc, BaseTax, BaseSTax, BaseTTL FROM dbo.tmpTrn WHERE CheckNo = @CheckNo) tn
    JOIN dbo.IClassDef ic ON ic.ClsCode = tn.ClassCode
    JOIN dbo.TaxClassDef td ON td.TaxCode = ic.TaxClass
    JOIN dbo.STaxDef st ON st.STaxCode = ic.STaxClass
    JOIN dbo.SurChgDef sd ON sd.SurChgCode = ic.SurChgCls
    JOIN (SELECT LinkItem, LinkCode FROM dbo.RVCLinkCode WHERE RVC = @RVCNo AND LinkType = 0 AND MealPeriod = @PS_Meal) rl ON rl.LinkItem = tn.ClassCode
    WHERE (tn.TrnCode < 200 OR tn.TrnCode = 720) AND tn.TrnCode <> 110
    GROUP BY rl.LinkCode, td.PreAccLink, sd.PreAccLink, st.LinkCode;

    INSERT INTO @PS_Rev (IsDsc, LinkCode, LinkTax, LinkSrc, LinkSTax, SubAmount, SrcAmount, TaxAmount, STaxAmount, TTLAmount)
    SELECT 1, rl.LinkCode, td.PreAccLink, sd.PreAccLink, st.LinkCode,
           SUM(tn.BaseSub) / @PS_DecDiv, SUM(tn.BaseSrc) / @PS_DecDiv, SUM(tn.BaseTax) / @PS_DecDiv, SUM(tn.BaseSTax) / @PS_DecDiv, SUM(tn.BaseTTL) / @PS_DecDiv
    FROM (SELECT ItemCode, ClassCode, TrnCode, TrnSub, BaseSub, BaseSrc, BaseTax, BaseSTax, BaseTTL FROM dbo.tmpTrn WHERE CheckNo = @CheckNo) tn
    JOIN dbo.IClassDef ic ON ic.ClsCode = tn.ClassCode
    JOIN dbo.TaxClassDef td ON td.TaxCode = ic.TaxClass
    JOIN dbo.STaxDef st ON st.STaxCode = ic.STaxClass
    JOIN dbo.SurChgDef sd ON sd.SurChgCode = ic.SurChgCls
    JOIN (SELECT DscCode, ClsCode, LinkCode FROM dbo.DiscountDtl WHERE RVC = @RVCNo AND MealPeriod = @PS_Meal) rl
         ON rl.ClsCode = tn.ClassCode AND LTRIM(RTRIM(tn.ItemCode)) = LTRIM(RTRIM(rl.DscCode))
    WHERE tn.TrnCode = 110
    GROUP BY rl.LinkCode, td.PreAccLink, sd.PreAccLink, st.LinkCode;

    IF EXISTS (SELECT 1 FROM @PS_Rev WHERE ISNULL(TRY_CONVERT(BIGINT, ROUND(TRY_CONVERT(FLOAT, LTRIM(RTRIM(LinkCode))), 0)), 0) < 100)
    BEGIN SET @PS_Fail = 1; BREAK; END;

    SET @PS_Run = @PS_Run + ISNULL((SELECT SUM(TTLAmount) FROM @PS_Rev), 0);
    INSERT INTO @PS_Post (Major, Minor, Amount, Sub, Tax, TaxCode, Srv, SrvCode, Cnt, Comment, Org, OrgCur, STax, STaxCode)
    SELECT CAST(x.Link / 100 AS VARCHAR(10)), CAST(x.Link % 100 AS VARCHAR(10)), SUM(x.TTLAmount), SUM(x.SubAmount), SUM(x.TaxAmount),
           LEFT(ISNULL(x.LinkTax, ''), 10), SUM(x.SrcAmount), LEFT(ISNULL(x.LinkSrc, ''), 10), @PS_NGuest, LEFT(@PS_PONumber, 200), 0, '',
           SUM(x.STaxAmount), LEFT(ISNULL(x.LinkSTax, ''), 10)
    FROM (SELECT TRY_CONVERT(BIGINT, ROUND(TRY_CONVERT(FLOAT, LTRIM(RTRIM(LinkCode))), 0)) AS Link, * FROM @PS_Rev) x
    GROUP BY x.Link, x.LinkTax, x.LinkSrc, x.LinkSTax;

    -- POS WIN: 'Unbalance Interface!' when the lines do not sum to zero.
    IF ROUND(@PS_Run, 2) <> 0
    BEGIN SET @PS_Fail = 1; BREAK; END;

    SELECT @PS_n = COUNT(*) FROM @PS_Post;
    IF @PS_n = 0 BREAK;

    SET @PS_FolioText = CONVERT(VARCHAR(10), CONVERT(BIGINT, @PS_Folio));
    SET @PS_Ref = CONVERT(VARCHAR(30), @CheckNo);
    SET @PS_Sql = N'EXEC ' + @PS_FO + N'PostInterfaceTransactionToFolio @FolioNum = @PS_FolioText, @FolioBalanceCode = @PS_Bal, '
        + N'@TransactionCode = @PS_Major, @TransactionSubCode = @PS_Minor, @RefNumber = @PS_Ref, @TransactionAmount = @PS_Amount, '
        + N'@SubAmount = @PS_Sub, @TaxAmount = @PS_Tax, @TaxCode = @PS_TaxCode, @ServiceAmount = @PS_Srv, @ServiceChargeCode = @PS_SrvCode, '
        + N'@Count = @PS_Count, @MealPeriodNumber = @PS_Meal, @Comment = @PS_Comment, @OriginAmount = @PS_Org, @OriginCurrency = @PS_OrgCur, '
        + N'@TransactionTime = @PS_Now, @RptExRate = @PS_ExRate, @StsCode1 = @PS_S1, @StsCode2 = @PS_S2, @StsCode3 = @PS_S3, @StsCode4 = @PS_S4, '
        + N'@StsCode5 = @PS_S5, @StsCode6 = @PS_S6, @StsCode7 = @PS_S7, @StsCode8 = @PS_S8, @STaxAmount = @PS_STax, @STaxCode = @PS_STaxCode;';

    SAVE TRANSACTION PS_Post;
    BEGIN TRY
        WHILE @PS_i <= @PS_n
        BEGIN
            SELECT @PS_Major = Major, @PS_Minor = Minor, @PS_Amount = Amount, @PS_Sub = Sub, @PS_Tax = Tax, @PS_TaxCode = TaxCode,
                   @PS_Srv = Srv, @PS_SrvCode = SrvCode, @PS_Count = Cnt, @PS_Comment = Comment, @PS_Org = Org, @PS_OrgCur = OrgCur,
                   @PS_STax = STax, @PS_STaxCode = STaxCode
            FROM @PS_Post WHERE Seq = @PS_i;
            EXEC sp_executesql @PS_Sql,
                N'@PS_FolioText VARCHAR(10), @PS_Bal VARCHAR(10), @PS_Major VARCHAR(10), @PS_Minor VARCHAR(10), @PS_Ref VARCHAR(30), @PS_Amount NUMERIC(18,2), @PS_Sub NUMERIC(18,2), @PS_Tax NUMERIC(18,2), @PS_TaxCode VARCHAR(10), @PS_Srv NUMERIC(18,2), @PS_SrvCode VARCHAR(10), @PS_Count INT, @PS_Meal INT, @PS_Comment NVARCHAR(200), @PS_Org NUMERIC(18,9), @PS_OrgCur VARCHAR(20), @PS_Now DATETIME, @PS_ExRate NUMERIC(28,16), @PS_S1 VARCHAR(20), @PS_S2 VARCHAR(20), @PS_S3 VARCHAR(20), @PS_S4 VARCHAR(20), @PS_S5 VARCHAR(20), @PS_S6 VARCHAR(20), @PS_S7 VARCHAR(20), @PS_S8 VARCHAR(20), @PS_STax NUMERIC(18,2), @PS_STaxCode VARCHAR(10)',
                @PS_FolioText = @PS_FolioText, @PS_Bal = @PS_Bal, @PS_Major = @PS_Major, @PS_Minor = @PS_Minor, @PS_Ref = @PS_Ref, @PS_Amount = @PS_Amount,
                @PS_Sub = @PS_Sub, @PS_Tax = @PS_Tax, @PS_TaxCode = @PS_TaxCode, @PS_Srv = @PS_Srv, @PS_SrvCode = @PS_SrvCode, @PS_Count = @PS_Count,
                @PS_Meal = @PS_Meal, @PS_Comment = @PS_Comment, @PS_Org = @PS_Org, @PS_OrgCur = @PS_OrgCur, @PS_Now = @PS_Now, @PS_ExRate = @PS_ExRate,
                @PS_S1 = @PS_S1, @PS_S2 = @PS_S2, @PS_S3 = @PS_S3, @PS_S4 = @PS_S4, @PS_S5 = @PS_S5, @PS_S6 = @PS_S6, @PS_S7 = @PS_S7, @PS_S8 = @PS_S8,
                @PS_STax = @PS_STax, @PS_STaxCode = @PS_STaxCode;
            SET @PS_i = @PS_i + 1;
        END;
    END TRY
    BEGIN CATCH
        IF ERROR_NUMBER() IN (229, 230, 262, 297, 300) THROW;
        IF XACT_STATE() <> 1 THROW;
        ROLLBACK TRANSACTION PS_Post;
        SET @PS_Fail = 1;
        BREAK;
    END CATCH;
    BREAK;
END;

IF @PS_Fail = 1
BEGIN
    SELECT 1 AS IsOut, 'folio_post_required' AS Reason, CAST(0 AS BIGINT) AS CloseNo, CAST(0 AS DECIMAL(18,2)) AS SubTotal,
           CAST(0 AS DECIMAL(18,2)) AS TotalPaid;
    RETURN;
END;

INSERT INTO dbo.Trn
    (NYear, NMonth, NDay, TrnSeq, CheckNo, TrnTime, Cashier, TrnCode, TrnSub, ItemCode,
     TrnDesc, ItemPrice, BaseSub, BaseTax, BaseTTL, TaxNo, TrnOrg, TrnBase, TrnPref, TrnQty,
     RVC, OrgCrCode, NewTrn, Refund, Status, ClassCode, GroupCode, StsCode1, StsCode2,
     StsCode3, StsCode4, BaseSrc, SrcNo, MealPeriod, ShiftPeriod, BaseSTax, STaxNo, OrderNo,
     OrgCheck, SubR, SrcR, STaxR, TaxR, TotalR, OrgPrice, Rsv1, Rsv2, Rsv3, Rsv4)
SELECT
     NYear, NMonth, NDay, TrnSeq, CheckNo, TrnTime, Cashier, TrnCode, TrnSub, ItemCode,
     TrnDesc, ItemPrice, BaseSub, BaseTax, BaseTTL, TaxNo, TrnOrg, TrnBase, TrnPref, TrnQty,
     RVC, OrgCrCode, NewTrn, Refund, Status, ClassCode, GroupCode, StsCode1, StsCode2,
     StsCode3, StsCode4, BaseSrc, SrcNo, MealPeriod, ShiftPeriod, BaseSTax, STaxNo, OrderNo,
     OrgCheck, SubR, SrcR, STaxR, TaxR, TotalR, OrgPrice, Rsv1, Rsv2, Rsv3, Rsv4
FROM dbo.tmpTrn
WHERE CheckNo = @CheckNo;

INSERT INTO dbo.Invoice
    (CheckNo, IYear, IMonth, IDay, OpenTime, OpenBy, CloseTime, CloseBy, SubTotal, Discount,
     SurCharge, BeforeTax, TaxTotal, TotalBase, TotalPref, TotalPaid, ClientCode, InvStatus,
     RVC, Status, WSID, TableNo, SubTableNo, NGuest, NChild, ExRate, RoomCode, FolioNum,
     PaymentMode, PaymentType, MemberID, MemberSubID, OrderTime, STaxTotal,
     ISubR, ISrcR, ISTaxR, ITaxR, ITotalR)
SELECT
     CheckNo, IYear, IMonth, IDay, OpenTime, OpenBy, @Close_Now, @CloseBy, SubTotal, Discount,
     SurCharge, BeforeTax, TaxTotal, TotalBase, TotalPref, TotalPaid, ClientCode, 1,
     RVC, 1, WSID, TableNo, SubTableNo, NGuest, NChild, ExRate, RoomCode, FolioNum,
     PaymentMode, PaymentType, MemberID, MemberSubID, OrderTime, STaxTotal,
     ISubR, ISrcR, ISTaxR, ITaxR, ITotalR
FROM dbo.tmpInvoice
WHERE CheckNo = @CheckNo;

DECLARE @Close_CloseNo BIGINT = 0;
IF OBJECT_ID('dbo.tmpTaxTable') IS NOT NULL
BEGIN
    DECLARE @Close_ExistingSub2 NUMERIC(18,0);
    DECLARE @Close_HasTaxRow BIT = 0;

    SELECT @Close_ExistingSub2 = Sub2, @Close_HasTaxRow = 1 FROM dbo.tmpTaxTable WHERE TCheckNo = @CheckNo;
    SET @Close_CloseNo = ISNULL(@Close_ExistingSub2, 0);

    IF @Close_HasTaxRow = 0 OR ISNULL(@Close_ExistingSub2, 0) = 0
    BEGIN
        DECLARE @Close_CloseSeq TABLE (v BIGINT);
        INSERT INTO @Close_CloseSeq (v) EXEC dbo.GetRVCSeqNumber 'CloseNumber', @RVCNo, @CloseBy, '';

        SET @Close_CloseNo = CAST(@RVCNo AS BIGINT) * CAST(100000000 AS BIGINT) + (SELECT TOP 1 v FROM @Close_CloseSeq);

        IF @Close_HasTaxRow = 0
            INSERT INTO dbo.tmpTaxTable (RVC, TCheckNo, Sub2) VALUES (@RVCNo, @CheckNo, @Close_CloseNo);
        ELSE
            UPDATE dbo.tmpTaxTable SET Sub2 = @Close_CloseNo WHERE TCheckNo = @CheckNo;
    END;
END;

IF OBJECT_ID('dbo.tmpPayment') IS NOT NULL AND OBJECT_ID('dbo.Payment') IS NOT NULL
    INSERT INTO dbo.Payment  SELECT * FROM dbo.tmpPayment WHERE CheckNo  = @CheckNo;

IF OBJECT_ID('dbo.tmpARInv') IS NOT NULL AND OBJECT_ID('dbo.ARInv') IS NOT NULL
    INSERT INTO dbo.ARInv    SELECT * FROM dbo.tmpARInv   WHERE CheckNo  = @CheckNo;

IF OBJECT_ID('dbo.tmpCCInv') IS NOT NULL AND OBJECT_ID('dbo.CCInv') IS NOT NULL
    INSERT INTO dbo.CCInv    SELECT * FROM dbo.tmpCCInv   WHERE CheckNo  = @CheckNo;

IF OBJECT_ID('dbo.tmpTaxTable') IS NOT NULL AND OBJECT_ID('dbo.TaxTable') IS NOT NULL
    INSERT INTO dbo.TaxTable SELECT * FROM dbo.tmpTaxTable WHERE TCheckNo = @CheckNo;

-- ProcessMbrPoint (mdlMain.cs:2036-2130): loyalty points on close, gated by GlobPara.MemberControl.
IF OBJECT_ID('dbo.MemberPoint') IS NOT NULL
   AND ISNULL((SELECT ParaStr FROM dbo.GlobPara WHERE ParaName = 'MemberControl'), '0') = '1'
BEGIN
    DECLARE @MbrPt_MemberID VARCHAR(30) = (SELECT MemberID FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo);

    IF @MbrPt_MemberID IS NOT NULL AND LTRIM(RTRIM(@MbrPt_MemberID)) <> ''
    BEGIN
        DECLARE @MbrPt_BaseDecDiv NUMERIC(18,2);
        SELECT @MbrPt_BaseDecDiv = DecDiv FROM dbo.CurrencyDef WHERE CrCode = 1;
        IF ISNULL(@MbrPt_BaseDecDiv, 0) = 0 SET @MbrPt_BaseDecDiv = 1;

        DECLARE @MbrPt_IncSrc BIT = CASE WHEN ISNULL(
            (SELECT ParaStr FROM dbo.GlobPara WHERE ParaName = 'MBRPointIncSrc'), '0') <> '0' THEN 1 ELSE 0 END;

        DECLARE @MbrPt_Amount NUMERIC(18,2) = (
            SELECT (ISNULL(I.SubTotal,0) + ISNULL(I.Discount,0)
                    + CASE WHEN @MbrPt_IncSrc = 1 THEN ISNULL(I.SurCharge,0) ELSE 0 END) / @MbrPt_BaseDecDiv
            FROM dbo.tmpInvoice I
            JOIN dbo.PaymentMode P ON I.PaymentMode = P.PMode
            WHERE I.CheckNo = @CheckNo AND P.MbrPoint = 1);

        IF ISNULL(@MbrPt_Amount, 0) <> 0
        BEGIN
            DECLARE @MbrPt_PolicyAmount NUMERIC(18,2), @MbrPt_PolicyPoint NUMERIC(18,2);
            SELECT @MbrPt_PolicyAmount = P.Amount, @MbrPt_PolicyPoint = P.Point
            FROM dbo.MemberList M
            JOIN dbo.MBRPointPolicy P ON M.MemberType = P.MType
            WHERE ISNULL(P.IsCancel,0) = 0
              AND GETDATE() BETWEEN P.ValidFrom AND P.ValidTo
              AND P.PType = 'POS' AND M.MemberSubID = 0 AND M.MemberID = @MbrPt_MemberID;

            IF ISNULL(@MbrPt_PolicyAmount, 0) <> 0 AND ISNULL(@MbrPt_PolicyPoint, 0) <> 0
                INSERT INTO dbo.MemberPoint (PType, PApp, RefNo, MemberID, Amount, Point, UserID, PTime, PDate, PDesc, IsQpoint)
                VALUES (0, 'POS', @CheckNo, @MbrPt_MemberID, @MbrPt_Amount,
                        ROUND(@MbrPt_Amount / @MbrPt_PolicyAmount * @MbrPt_PolicyPoint, 2),
                        @CloseBy, GETDATE(), GETDATE(), N'Close Bill', 0);
        END;
    END;
END;

DECLARE @Close_SubTotal DECIMAL(18,2), @Close_TotalPaid DECIMAL(18,2);
SELECT @Close_SubTotal = ISNULL(SubTotal, 0), @Close_TotalPaid = ISNULL(TotalPaid, 0) FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo;

DELETE FROM dbo.tmpTrnInfo     WHERE CheckNo  = @CheckNo;
DELETE FROM dbo.tmpInvoiceInfo WHERE CheckNo  = @CheckNo;
DELETE FROM dbo.tmpTrn         WHERE CheckNo  = @CheckNo;
IF OBJECT_ID('dbo.tmpPayment')  IS NOT NULL DELETE FROM dbo.tmpPayment  WHERE CheckNo  = @CheckNo;
IF OBJECT_ID('dbo.tmpARInv')    IS NOT NULL DELETE FROM dbo.tmpARInv    WHERE CheckNo  = @CheckNo;
IF OBJECT_ID('dbo.tmpCCInv')    IS NOT NULL DELETE FROM dbo.tmpCCInv    WHERE CheckNo  = @CheckNo;
IF OBJECT_ID('dbo.tmpTaxTable') IS NOT NULL DELETE FROM dbo.tmpTaxTable WHERE TCheckNo = @CheckNo;
DELETE FROM dbo.tmpInvoice     WHERE CheckNo  = @CheckNo;

UPDATE dbo.checkopenlog
    SET isOpen = 0, CloseTime = @Close_Now, LastAction = @Close_Now
    WHERE CheckNo = @CheckNo AND RVC = @RVCNo;

SELECT 0 AS IsOut, 'ok' AS Reason, @Close_CloseNo AS CloseNo, @Close_SubTotal AS SubTotal, @Close_TotalPaid AS TotalPaid;";

    /// <summary>
    /// ADR 0179 (RT-1297): a room charge of an open check, in POS WIN's shape: one tmpTrn line of the room-charge TransactionCode
    /// (TrnType 106; ItemCode = the folio, StsCode2 = the balance code) and the tmpInvoice header (RoomCode, FolioNum, PaymentMode,
    /// PaymentType, PaymentInfo), no tmpPayment row. The folio is re-validated by SELECTs against the Front Office database named
    /// by dbo.Para InterfaceDB; nothing is posted there: the post happens in the close (<see cref="CheckClose"/>), as in POS WIN.
    /// Params: @RVCNo, @CheckNo, @Cashier, @RoomNo, @FolioNum, @Amount, @Reference, @CfgTrnCode (int, null), @CfgTrnSub (int, null).
    /// Last result set: (IsOut, Reason, TrnSeq); IsOut 0 = written. An interface that is not configured is folio_post_required.
    /// </summary>
    internal const string RoomChargeAdd = @"SET NOCOUNT ON;

DECLARE @RC_Status INT;
SELECT @RC_Status = Status FROM dbo.tmpInvoice WITH (UPDLOCK, HOLDLOCK) WHERE CheckNo = @CheckNo AND RVC = @RVCNo;
IF @RC_Status IS NULL BEGIN SELECT 1 AS IsOut, 'check_not_found' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;
IF @RC_Status <> 1 BEGIN SELECT 1 AS IsOut, 'bill_not_open' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;

-- Tender lock: a room charge is the only settlement of its check.
IF EXISTS (SELECT 1 FROM dbo.tmpTrn WHERE CheckNo = @CheckNo AND TrnCode >= 200 AND TrnCode <> 810)
   OR EXISTS (SELECT 1 FROM dbo.tmpTrn tn JOIN dbo.TransactionCode tc ON tn.TrnCode = tc.TrnCode AND tn.TrnSub = tc.TrnSubCode
              WHERE tc.TrnType = 106 AND tn.CheckNo = @CheckNo)
BEGIN SELECT 1 AS IsOut, 'tender_locked' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;

-- The room-charge TransactionCode (TrnType 106): the cloud's code when this store has it, else the store's own (900 first).
DECLARE @RC_TrnCode DECIMAL(18,0), @RC_TrnSub DECIMAL(18,0) = 0;
IF @CfgTrnCode IS NOT NULL
    SELECT TOP 1 @RC_TrnCode = TrnCode, @RC_TrnSub = TrnSubCode FROM dbo.TransactionCode
    WHERE TrnCode = @CfgTrnCode AND TrnSubCode = ISNULL(@CfgTrnSub, 0) AND TrnType = 106;
ELSE
    SELECT TOP 1 @RC_TrnCode = TrnCode, @RC_TrnSub = TrnSubCode FROM dbo.TransactionCode
    WHERE TrnType = 106 ORDER BY CASE WHEN TrnCode = 900 THEN 0 ELSE 1 END, TrnCode, TrnSubCode;
IF @RC_TrnCode IS NULL
BEGIN SELECT 1 AS IsOut, 'tender_not_configured' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;

-- The ROOM CHARGE payment mode: it names the header's PaymentMode and must be an interface mode (Interface = 1).
DECLARE @RC_PMode VARCHAR(30), @RC_PType VARCHAR(20), @RC_Iface INT;
SELECT TOP 1 @RC_PMode = LTRIM(RTRIM(pm.PMode)), @RC_PType = NULLIF(LTRIM(RTRIM(CONVERT(VARCHAR(20), pm.TrnCode))), ''), @RC_Iface = ISNULL(pm.Interface, 0)
FROM dbo.PaymentMode pm
WHERE UPPER(LTRIM(RTRIM(pm.PType))) IN ('ROOM CHARGE', 'ROOM') OR UPPER(LTRIM(RTRIM(pm.PMode))) IN ('ROOM CHARGE', 'ROOM')
ORDER BY CASE WHEN UPPER(LTRIM(RTRIM(pm.PType))) = 'ROOM CHARGE' THEN 0 ELSE 1 END;
IF @RC_PMode IS NULL
BEGIN SELECT 1 AS IsOut, 'tender_not_configured' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;

-- Fail closed: the interface (Para InterfaceType = 1, InterfaceDB, PaymentMode.Interface = 1) must be configured and reachable.
DECLARE @RC_IfType INT, @RC_FODB VARCHAR(128);
SELECT TOP 1 @RC_IfType = TRY_CONVERT(INT, LTRIM(RTRIM(ParaStr))) FROM dbo.Para WHERE ParaName = 'InterfaceType' AND RVC = @RVCNo;
SELECT TOP 1 @RC_FODB = LEFT(LTRIM(RTRIM(ParaStr)), 128) FROM dbo.Para WHERE ParaName = 'InterfaceDB' AND RVC = @RVCNo;
IF ISNULL(@RC_IfType, 0) <> 1 OR ISNULL(@RC_Iface, 0) <> 1 OR ISNULL(@RC_FODB, '') = ''
   OR @RC_FODB LIKE '%[^A-Za-z0-9_]%' OR ISNULL(HAS_DBACCESS(@RC_FODB), 0) <> 1
BEGIN SELECT 1 AS IsOut, 'folio_post_required' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;

-- The folio, re-validated in the Front Office database (SELECTs only): an in-house (FolioStatus 2), postable (NoPostFlag 0) folio of the room.
DECLARE @RC_Sql NVARCHAR(MAX), @RC_Room NVARCHAR(10) = @RoomNo;
DECLARE @RC_Live INT = 0, @RC_NoPost INT = 0, @RC_Pick FLOAT = 0, @RC_Given INT = 0, @RC_GivenAny INT = 0, @RC_Folio FLOAT = 0;
SET @RC_Sql = N'SELECT @RC_Live = SUM(CASE WHEN FolioStatus = 2 AND COALESCE(NoPostFlag, 0) = 0 THEN 1 ELSE 0 END), '
    + N'@RC_NoPost = SUM(CASE WHEN FolioStatus = 2 AND COALESCE(NoPostFlag, 0) <> 0 THEN 1 ELSE 0 END), '
    + N'@RC_Pick = ISNULL(MIN(CASE WHEN FolioStatus = 2 AND COALESCE(NoPostFlag, 0) = 0 THEN FolioNum END), 0), '
    + N'@RC_Given = SUM(CASE WHEN FolioStatus = 2 AND COALESCE(NoPostFlag, 0) = 0 AND FolioNum = @RC_Folio THEN 1 ELSE 0 END) '
    + N'FROM ' + QUOTENAME(@RC_FODB) + N'.dbo.Folio WHERE LTRIM(RTRIM(RoomCode)) = @RC_Room;'
    + N' SELECT @RC_GivenAny = COUNT(*) FROM ' + QUOTENAME(@RC_FODB) + N'.dbo.Folio WHERE FolioNum = @RC_Folio;';
SET @RC_Folio = @FolioNum;
EXEC sp_executesql @RC_Sql,
    N'@RC_Room NVARCHAR(10), @RC_Folio FLOAT, @RC_Live INT OUTPUT, @RC_NoPost INT OUTPUT, @RC_Pick FLOAT OUTPUT, @RC_Given INT OUTPUT, @RC_GivenAny INT OUTPUT',
    @RC_Room = @RC_Room, @RC_Folio = @RC_Folio, @RC_Live = @RC_Live OUTPUT, @RC_NoPost = @RC_NoPost OUTPUT, @RC_Pick = @RC_Pick OUTPUT,
    @RC_Given = @RC_Given OUTPUT, @RC_GivenAny = @RC_GivenAny OUTPUT;
SET @RC_Live = ISNULL(@RC_Live, 0); SET @RC_NoPost = ISNULL(@RC_NoPost, 0); SET @RC_Given = ISNULL(@RC_Given, 0);

IF @RC_Live = 0 AND @RC_NoPost > 0
BEGIN SELECT 1 AS IsOut, 'room_no_post' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;
IF @FolioNum = 0
BEGIN
    IF @RC_Live = 0 BEGIN SELECT 1 AS IsOut, 'room_not_in_house' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;
    IF @RC_Live > 1 BEGIN SELECT 1 AS IsOut, 'folio_ambiguous' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;
    SET @RC_Folio = @RC_Pick;
END
ELSE IF @RC_Given = 0
BEGIN
    SELECT 1 AS IsOut, CASE WHEN @RC_GivenAny > 0 THEN 'folio_mismatch' ELSE 'room_not_in_house' END AS Reason, CAST(0 AS BIGINT) AS TrnSeq;
    RETURN;
END;

DECLARE @RC_BaseDecDiv NUMERIC(18,2);
SELECT @RC_BaseDecDiv = DecDiv FROM dbo.CurrencyDef WHERE CrCode = 1;
IF ISNULL(@RC_BaseDecDiv, 0) = 0 SET @RC_BaseDecDiv = 1;

DECLARE @RC_Amount NUMERIC(16,0) = -ROUND(@Amount * @RC_BaseDecDiv, 0);
EXEC dbo.UpdateTmpInvoice @CheckNo;
IF -@RC_Amount > ISNULL((SELECT SUM(BaseTTL) FROM dbo.tmpTrn WHERE CheckNo = @CheckNo), 0)
BEGIN SELECT 1 AS IsOut, 'room_charge_above_balance' AS Reason, CAST(0 AS BIGINT) AS TrnSeq; RETURN; END;

DECLARE @RC_IYear INT, @RC_IMonth INT, @RC_IDay INT;
SELECT @RC_IYear = IYear, @RC_IMonth = IMonth, @RC_IDay = IDay FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo;

DECLARE @RC_NewSeq TABLE (v BIGINT);
DECLARE @RC_ParaStr BIGINT;
INSERT INTO @RC_NewSeq (v) EXEC dbo.GetRVCSeqNumber 'LastTrn', @RVCNo, @Cashier, '';
SELECT @RC_ParaStr = v FROM @RC_NewSeq;
DECLARE @RC_TrnSeq NUMERIC(11,0) = CAST(@RVCNo AS BIGINT) * 100000000 + @RC_ParaStr;

INSERT INTO dbo.tmpTrn
    (NYear, NMonth, NDay, TrnSeq, CheckNo, TrnTime, Cashier, TrnCode, TrnSub,
     ItemCode, TrnDesc, ItemPrice, BaseSub, BaseTax, BaseTTL, TrnOrg, TrnBase, TrnPref,
     TrnQty, RVC, OrgCrCode, NewTrn, Refund, Status, OrderNo, OrgCheck, StsCode2, Rsv1)
VALUES
    (@RC_IYear, @RC_IMonth, @RC_IDay, @RC_TrnSeq, @CheckNo, GETDATE(), @Cashier, @RC_TrnCode, @RC_TrnSub,
     CONVERT(VARCHAR(12), CONVERT(BIGINT, @RC_Folio)), LEFT(@RC_PMode, 255), 0, 0, 0, @RC_Amount, @RC_Amount, @RC_Amount, 0,
     1, @RVCNo, 1, 1, 0, 1, 0, @CheckNo, 'A', LEFT(ISNULL(dbo.Unicode2TCVN(@Reference), ''), 50));

UPDATE dbo.tmpInvoice
   SET RoomCode = @RC_Room,
       FolioNum = @RC_Folio,
       PaymentMode = @RC_PMode,
       PaymentType = CASE WHEN ISNULL(@RC_PType, '') <> '' THEN @RC_PType ELSE PaymentType END,
       PaymentInfo = LEFT(ISNULL(@Reference, N''), 50)
 WHERE CheckNo = @CheckNo;

EXEC dbo.UpdateTmpInvoice @CheckNo;

SELECT 0 AS IsOut, 'ok' AS Reason, CAST(@RC_TrnSeq AS BIGINT) AS TrnSeq;";
}
