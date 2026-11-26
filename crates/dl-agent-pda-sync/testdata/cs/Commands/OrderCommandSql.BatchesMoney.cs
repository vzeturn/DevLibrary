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
      AND t.TrnCode IN (SELECT COALESCE(NULLIF(pm.TrnCode, 0), NULLIF(pm.ZeroPayment, 0)) FROM dbo.PaymentMode pm)
)
BEGIN SELECT 1 AS IsOut, 'tender_not_found' AS Reason; RETURN; END;

DELETE FROM dbo.tmpPayment WHERE TrnNo = @TrnSeq AND CheckNo = @CheckNo;
DELETE FROM dbo.tmpTrn WHERE TrnSeq = @TrnSeq AND CheckNo = @CheckNo AND RVC = @RVCNo;

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
}
