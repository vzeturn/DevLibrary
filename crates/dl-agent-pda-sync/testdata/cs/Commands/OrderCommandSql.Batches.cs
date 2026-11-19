namespace MM.PdaOrder.SyncAgent.Commands;

// The order verbs' SMILE_POS batches (OW1b, ADR 0104), ported from the legacy PDA order repository so a site_sync store
// writes exactly what an on-prem PDA wrote. Kept verbatim except where ADR 0104 says otherwise (each change is marked
// "OW1b"). Every vendor object they write is granted to pda_sync by Bootstrap/Sql/070_order_grants.sql; the vendor procs
// they call run under ownership chaining. Do not edit these to taste: POS WIN parity is the contract.
internal static partial class OrderCommandSql
{
    /// <summary>Legacy <c>SqlOpenCheckNo</c>, verbatim. Params: @RVCNo, @TableNo, @Cashier, @NumGuest. Returns CheckNo.</summary>
    internal const string OpenCheck = @"SET NOCOUNT ON;
-- pda.spOpenCheckNo body inlined (P6a): @RVC=@RVCNo, @OpenBy=@Cashier, @ExRate=1 (default),
-- @ClientCode=0 (default), @Note=N'' (default), @StsCode1..7=N'' (defaults), @WSID='NTPDA' (default),
-- @ShiftPeriod=0 (default), @TableNo=@TableNo, @SubTableNo=0 (default), @NGuest=@NumGuest,
-- @NChild=0 (default), @PostedToInterface=0 (default), @TableID=0 (default), @RoomCode=N'' (default),
-- @FolioNum=0 (default), @MemberID='' (default), @MemberSubID=0 (default).
DECLARE @OC_ExRate NUMERIC(18,5) = 1;
DECLARE @OC_NGuest INT = @NumGuest;
IF @OC_ExRate IS NULL OR @OC_ExRate = 0 SET @OC_ExRate = 1;
IF @OC_NGuest IS NULL OR @OC_NGuest = 0 SET @OC_NGuest = 1;

DECLARE @OC_Now DATETIME = GETDATE();

-- PMS-C-01: business date for this RVC comes from the native PMS night-audit output
-- (dbo.Para.CurYear/CurMonth/CurDay per RVC), not the wall clock — a post-midnight
-- sale before night audit runs must still land on the day that's still open.
-- Falls back to GETDATE() if the RVC has no Para row (never blocks check-open).
DECLARE @OC_IYear INT, @OC_IMonth INT, @OC_IDay INT;
SELECT @OC_IYear  = MAX(CASE WHEN ParaName = 'CurYear'  THEN TRY_CAST(ParaStr AS INT) END),
       @OC_IMonth = MAX(CASE WHEN ParaName = 'CurMonth' THEN TRY_CAST(ParaStr AS INT) END),
       @OC_IDay   = MAX(CASE WHEN ParaName = 'CurDay'   THEN TRY_CAST(ParaStr AS INT) END)
FROM dbo.Para
WHERE RVC = @RVCNo AND ParaName IN ('CurYear', 'CurMonth', 'CurDay');

SET @OC_IYear  = ISNULL(@OC_IYear,  DATEPART(YEAR,  @OC_Now));
SET @OC_IMonth = ISNULL(@OC_IMonth, DATEPART(MONTH, @OC_Now));
SET @OC_IDay   = ISNULL(@OC_IDay,   DATEPART(DAY,   @OC_Now));

-- MealPeriod resolved inline (P6c): the body of pda.fnPda_ResolveMealPeriod flattened in-place —
-- current DB-time as HHMM matched against dbo.MealPeriod bands (native table), incl. the
-- overnight (ToTime < FromTime) wrap; ISNULL(NULLIF(...,0),1) defaults to period 1. Kept in SQL
-- (not C#) to preserve DB-clock semantics inside the open-check transaction. See [[pda-native-proc-mapping]].
DECLARE @OC_MealPeriod INT = ISNULL(NULLIF((SELECT TOP 1 mp.MealNo FROM dbo.MealPeriod mp WHERE mp.RVCNo = @RVCNo AND ((DATEPART(HOUR,GETDATE())*100+DATEPART(MINUTE,GETDATE())) BETWEEN mp.FromTime AND mp.ToTime OR (mp.ToTime < mp.FromTime AND (DATEPART(HOUR,GETDATE())*100+DATEPART(MINUTE,GETDATE())) < mp.ToTime) OR (mp.ToTime < mp.FromTime AND (DATEPART(HOUR,GETDATE())*100+DATEPART(MINUTE,GETDATE())) > mp.FromTime)) ORDER BY mp.MealNo), 0), 1);
IF @OC_MealPeriod IS NULL OR @OC_MealPeriod <= 0 SET @OC_MealPeriod = 1;

DECLARE @OC_SeqTbl TABLE (v BIGINT);
INSERT INTO @OC_SeqTbl (v) EXEC dbo.GetRVCSeqNumber 'LastCheck', @RVCNo, @Cashier, 'NTPDA';
DECLARE @OC_LastCheck BIGINT = (SELECT TOP 1 v FROM @OC_SeqTbl);

DECLARE @OC_CheckNo NUMERIC(11,0) = CAST(@RVCNo AS BIGINT) * 100000000 + @OC_LastCheck;

-- // lint:allow-tracked-write: INSERT INTO tmpInvoice — the open-bill header staging table. Inlined body of the legacy POS proc this call replaces (schema pda dropped); parity-tested against the original. Cooperative write on the SMILE money path.
INSERT INTO dbo.tmpInvoice
    (CheckNo, IYear, IMonth, IDay, OpenTime, OpenBy, RVC, TableNo, SubTableNo,
     NGuest, NChild, ExRate, ClientCode, WSID, MealPeriod, ShiftPeriod, RoomCode, FolioNum,
     MemberID, MemberSubID, PostedToInterface, Status, InvStatus,
     SubTotal, Discount, SurCharge, BeforeTax, TaxTotal, STaxTotal, TotalBase,
     TotalPref, TotalPaid, OrderTime, StsCode1, StsCode2, StsCode3, StsCode4,
     LastChgCashier, LastChgTime)
VALUES
    (@OC_CheckNo, @OC_IYear, @OC_IMonth, @OC_IDay, @OC_Now, @Cashier, @RVCNo, @TableNo, 0,
     @OC_NGuest, 0, @OC_ExRate, 0, 'NTPDA', @OC_MealPeriod, 0, N'', 0,
     '', 0, 0, 1, 0,
     0, 0, 0, 0, 0, 0, 0,
     0, 0, @OC_Now, N'', N'', N'', N'',
     @Cashier, @OC_Now);

IF NOT EXISTS (SELECT 1 FROM dbo.tmpInvoiceInfo WHERE CheckNo = @OC_CheckNo)
    INSERT INTO dbo.tmpInvoiceInfo (CheckNo, CurPage, CurLine, Printed, Ordered, PrintType)
    VALUES (@OC_CheckNo, 1, 0, 0, 0, 0);

INSERT INTO dbo.checkopenlog (RVC, CheckNo, OpenBy, OpenPC, OpenTime, isOpen, LastAction)
VALUES (@RVCNo, @OC_CheckNo, @Cashier, 'NTPDA', @OC_Now, 1, @OC_Now);

SELECT @OC_CheckNo AS CheckNo;";

    /// <summary>Legacy <c>SqlPostItem</c>; the recreate-header branch is replaced by CHECK_NOT_FOUND (ADR 0104 §3). Params: @RVCNo, @CheckNo, @Cashier, @ItemCode, @Qty, @SeatNo, @Price, @Note, @OpenName, @AllowZeroOpenPrice. The last result set is IsOut, MessageResult, TrnSeq, Seq, CheckNo, ItemCode.</summary>
    internal const string PostItem = @"SET NOCOUNT ON;

-- For an open-name item (ItemDef.OpenItem=1) the caller supplies the description;
-- that is what makes the item ""open"". Everything else keeps ItemDef.IName. The
-- seed only matters for OpenItem rows: the SELECT further down overwrites @TrnDesc
-- with IName for every ordinary item, so a stray @OpenName can never leak into one.
DECLARE @TrnDesc NVARCHAR(30);
IF LEN(LTRIM(RTRIM(ISNULL(@OpenName, '')))) > 0
   AND EXISTS (SELECT 1 FROM dbo.ItemDef WITH (NOLOCK)
               WHERE ICode = @ItemCode AND ISNULL(OpenItem, 0) = 1)
    SET @TrnDesc = LEFT(LTRIM(RTRIM(@OpenName)), 30);
ELSE
BEGIN
    SELECT TOP 1 @TrnDesc = RTRIM(ISNULL(IName, @ItemCode))
    FROM dbo.ItemDef WITH (NOLOCK)
    WHERE ICode = @ItemCode;
END
IF @TrnDesc IS NULL SET @TrnDesc = RTRIM(@ItemCode);

-- Open-item / open-price gate. Only ItemDef knows which items are open, so the check
-- lives here rather than in the API filter: posting an open-name item without a name
-- would silently bury the item code on the kitchen chit. An open-price item at 0 is
-- refused too, unless the outlet opted into it via order.allowZeroOpenPrice (0đ comps
-- and promo giveaways are a normal use of open-price items at some outlets).
DECLARE @PI_OpenName BIT = 0, @PI_OpenPrice BIT = 0;
SELECT TOP 1 @PI_OpenName = ISNULL(OpenItem, 0), @PI_OpenPrice = ISNULL(OpenPrice, 0)
FROM dbo.ItemDef WITH (NOLOCK) WHERE ICode = @ItemCode;

IF @PI_OpenName = 1 AND LEN(LTRIM(RTRIM(ISNULL(@OpenName, '')))) = 0
    THROW 51101, N'Món này cần nhập tên trước khi order.', 1;
IF @PI_OpenPrice = 1 AND @AllowZeroOpenPrice = 0 AND ISNULL(@Price, 0) <= 0
    THROW 51102, N'Món này cần nhập giá lớn hơn 0 trước khi order.', 1;
IF @PI_OpenPrice = 1 AND @AllowZeroOpenPrice = 1 AND ISNULL(@Price, 0) < 0
    THROW 51102, N'Món này cần nhập giá lớn hơn 0 trước khi order.', 1;

DECLARE @SubMarker VARCHAR(50);
IF LEN(LTRIM(RTRIM(@Note))) = 0
    SET @SubMarker = '';
ELSE
    SET @SubMarker = LEFT(CONVERT(VARCHAR(50), HASHBYTES('MD5', CAST(@Note AS NVARCHAR(250))), 2), 50);

-- pda.spPostItem body inlined (P6a): @CheckNo=@CheckNo, @RVC=@RVCNo, @Cashier=@Cashier,
-- @ItemCode=@ItemCode, @TrnDesc=@TrnDesc, @Price=@Price, @Qty=@Qty, @OrderNo=@PI_OrderNo,
-- @GuestNo=@SeatNo, @PPolicy='' (default), @ItemDiscount=0 (default), @ISubCode=@SubMarker,
-- @Note=@Note.
--
-- @OrderNo is the order-round counter within the bill, exactly as POS WIN derives it in
-- frmCheck.MyOrderNo: Max(OrderNo) over the bill, +1 unless OrderByCourse, floor 1. It is
-- NOT the MenuNo -- dbo.PostItem has no MenuNo parameter at all. MenuNo reaches tmpTrn.Rsv2
-- only via the OrderPrintByMenu backfill further down, carried over from the merged row.
DECLARE @PI_OrderNo INT;
SELECT @PI_OrderNo = ISNULL(MAX(OrderNo), 0) FROM dbo.tmpTrn WITH (NOLOCK) WHERE CheckNo = @CheckNo;
IF ISNULL((SELECT TOP 1 ParaStr FROM dbo.Para WITH (NOLOCK)
           WHERE ParaName = 'OrderByCourse' AND RVC = @RVCNo), '0') IN ('0', '')
    SET @PI_OrderNo = @PI_OrderNo + 1;
IF ISNULL(@PI_OrderNo, 0) <= 0 SET @PI_OrderNo = 1;
DECLARE @PI_IsOut INT = 0, @PI_OutResult VARCHAR(MAX) = '';
DECLARE @PI_TrnSeq BIGINT, @PI_ParaStr BIGINT;
DECLARE @PI_SeqOut TABLE (v BIGINT);

DECLARE @PI_qrCodeNeedConfirmOrder INT;
SELECT TOP 1 @PI_qrCodeNeedConfirmOrder = CAST(ISNULL(ParaStr,'0') AS INT)
  FROM dbo.GlobPara WITH (NOLOCK) WHERE RVC = @RVCNo AND ParaName = 'qrCodeNeedConfirmOrder';

DECLARE @PI_CheckLog TABLE (nSeq DECIMAL(18,0), RVC INT, CheckNo DECIMAL(18,0),
    OpenBy VARCHAR(50), OpenPC VARCHAR(50), isOpen INT);
INSERT INTO @PI_CheckLog (nSeq, RVC, CheckNo, OpenBy, OpenPC, isOpen)
SELECT nSeq, RVC, CheckNo, OpenBy, ISNULL(OpenPC,''), ISNULL(isOpen,0)
FROM dbo.CheckOpenLog WITH (NOLOCK)
WHERE RVC = @RVCNo AND CheckNo = @CheckNo;

IF (ISNULL(@CheckNo,0) = 0 AND ISNULL(@PI_IsOut,0) = 0)
BEGIN
    SET @PI_IsOut = 1;
    SET @PI_OutResult = 'We apologize for the inconvenience. The connection to the server is temporarily interrupted.
        </br> Please try again later. Thank you for your understanding.';
END

-- OW1b (ADR 0104 §3, owner decision): a check missing from tmpInvoice is refused, never recreated. The legacy
-- spReCreateCheckNo branch (a new tmpInvoice header from IOrgInvoice) is dropped; the executor maps IsOut = 1 to
-- check_not_found and the cloud answers it in Vietnamese.
IF (ISNULL(@PI_IsOut,0) = 0 AND NOT EXISTS(SELECT TOP 1 * FROM dbo.TmpInvoice WITH (NOLOCK) WHERE CheckNo = @CheckNo AND RVC = @RVCNo))
BEGIN
    SET @PI_IsOut = 1;
    SET @PI_OutResult = 'CHECK_NOT_FOUND';
END

IF (ISNULL(@PI_IsOut,0) = 0)
BEGIN
    IF (ISNULL(@CheckNo,0) > 0 AND EXISTS (SELECT TOP 1 * FROM dbo.TmpInvoice WITH (NOLOCK) WHERE CHECKNO = ISNULL(@CheckNo,0) AND RVC = @RVCNo))
    BEGIN
        DELETE FROM @PI_SeqOut;
        INSERT INTO @PI_SeqOut (v) EXEC dbo.GetRVCSeqNumber 'LastTrn', @RVCNo, @Cashier, '';
        SELECT @PI_ParaStr = v FROM @PI_SeqOut;

        SET @PI_TrnSeq = CAST(@RVCNo AS BIGINT) * 100000000 + @PI_ParaStr;

        IF @PI_TrnSeq > 9223372036854775807
        BEGIN
            RAISERROR('Generated TrnSeq exceeds bigint capacity', 16, 1);
            RETURN;
        END

        DECLARE @PI_LastQty NUMERIC(18,2);
        DECLARE @PI_LastTrn BIGINT;
        DECLARE @PI_GroupTrn VARCHAR(50);
        DECLARE @PI_DecDiv INT;
        DECLARE @PI_LastMenuNo VARCHAR(30);

        SELECT @PI_GroupTrn = ParaStr
        FROM dbo.Para WITH (NOLOCK)
        WHERE ParaName = 'GroupChkTrn' AND RVC = (SELECT RVC FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo);

        SELECT @PI_DecDiv = C.DecDiv, @TrnDesc = (CASE WHEN ISNULL(OpenItem,0) = 1 THEN dbo.Unicode2TCVN(@TrnDesc) ELSE IName END)
        FROM dbo.ItemDef I WITH (NOLOCK)
        JOIN dbo.CurrencyDef C WITH (NOLOCK) ON I.ICurrency = C.CrCode
        WHERE I.ICode = @ItemCode;

        IF COALESCE(@PI_DecDiv, 0) = 0 SET @PI_DecDiv = 1;

        IF @Qty <> 0 AND @PI_GroupTrn = '1' AND 0 = 0 AND @SubMarker = ''
        BEGIN
            SELECT @PI_LastTrn = TrnSeq, @PI_LastQty = TrnQty, @PI_LastMenuNo = COALESCE(Rsv2, '')
            FROM dbo.tmpTrn WITH (NOLOCK)
            WHERE TrnCode = 100
              AND CheckNo = @CheckNo
              AND ItemCode = @ItemCode
              AND OrderNo = @PI_OrderNo
              AND ItemCode NOT IN (SELECT ICode FROM dbo.ItemDef WITH (NOLOCK) WHERE OpenPrice = 1 OR OpenItem = 1 OR IsPkg = 1)
              AND OrgPrice = @Price * @PI_DecDiv
              AND COALESCE(MealPeriod, 0) = @SeatNo
              AND COALESCE(StsCode4, '') = '';

            IF COALESCE(@PI_LastQty, 0) = 0
                SET @PI_LastTrn = 0;

            IF COALESCE(@PI_LastTrn, 0) > 0
            BEGIN
                IF EXISTS(SELECT TrnNo FROM dbo.tmpTrnInfo WITH (NOLOCK) WHERE TrnNo = @PI_LastTrn AND (PToOrder = 1 OR PToCheck = 1))
                BEGIN
                    SET @PI_LastTrn = 0;
                END
                ELSE IF EXISTS(SELECT TrnSeq FROM dbo.tmpTrn WITH (NOLOCK) WHERE Rsv1 = CAST(@PI_LastTrn AS VARCHAR(50)))
                BEGIN
                    SET @PI_LastTrn = 0;
                END
                ELSE
                BEGIN
                    SET @PI_TrnSeq = @PI_LastTrn;
                    DELETE FROM dbo.tmpTrn WHERE TrnSeq = @PI_TrnSeq;
                    SET @Qty = @Qty + COALESCE(@PI_LastQty, 0);
                END
            END
        END

        DECLARE @PI_TrnQty NUMERIC(18,2);
        SET @PI_TrnQty = @Qty;
        IF @Qty = 0 SET @Qty = 1;

        EXEC dbo.PostITrn @CheckNo, @PI_TrnSeq, @Cashier, @ItemCode, @TrnDesc, @Price, @Qty, @PI_OrderNo, '', 0, @PI_TrnQty, @SeatNo, '', 0, @SubMarker;

        DECLARE @PI_IsPkg BIT;
        DECLARE @PI_PkgRVC INT = @RVCNo;
        SELECT @PI_IsPkg = COALESCE(IsPkg,0) FROM dbo.ItemDef WHERE ICode = @ItemCode;
        IF @PI_IsPkg = 1
        BEGIN
            SELECT @PI_PkgRVC = RVC FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo;
            DECLARE @PI_pICode VARCHAR(30);
            DECLARE @PI_pQty NUMERIC(18,2);
            DECLARE @PI_pPrice NUMERIC(18,2);
            DECLARE @PI_pTrnSeq BIGINT;
            DECLARE @PI_TmpNum BIGINT;
            DECLARE @PI_HasTrn INT = 0;
            DECLARE PCur CURSOR FOR SELECT P.ItemCode, P.iQty, (CASE WHEN P.UseItemPrice = 1 THEN I.Price ELSE P.IPrice END)/COALESCE(C.DecDiv,1)
                FROM dbo.PkgDef P JOIN dbo.ItemDef I ON P.ItemCode = I.ICode
                LEFT JOIN dbo.CurrencyDef C ON I.ICurrency = C.CrCode
                WHERE PkgCode = @ItemCode
                ORDER BY P.DisplayOrder;
            OPEN PCur;
            FETCH NEXT FROM PCur INTO @PI_pICode, @PI_pQty, @PI_pPrice;
            WHILE @@FETCH_STATUS = 0
            BEGIN
                SET @PI_HasTrn = 1;
                IF @PI_pQty = 0 SET @PI_pQty = 1;
                SET @PI_pQty = @PI_pQty * @Qty;
                DELETE FROM @PI_SeqOut;
                INSERT INTO @PI_SeqOut (v) EXEC dbo.GetRVCSeqNumber 'LastTrn', @PI_PkgRVC, @Cashier, '';
                SELECT @PI_TmpNum = v FROM @PI_SeqOut;
                SET @PI_pTrnSeq = CAST(@PI_PkgRVC AS BIGINT) * 100000000 + @PI_TmpNum;
                EXEC dbo.PostITrn @CheckNo, @PI_pTrnSeq, @Cashier, @PI_pICode, '', @PI_pPrice, @PI_pQty, @PI_OrderNo, @PI_TrnSeq, 0;
                FETCH NEXT FROM PCur INTO @PI_pICode, @PI_pQty, @PI_pPrice;
            END
            CLOSE PCur;
            DEALLOCATE PCur;
            IF @PI_HasTrn > 0
            BEGIN
                DELETE FROM @PI_SeqOut;
                INSERT INTO @PI_SeqOut (v) EXEC dbo.GetRVCSeqNumber 'LastTrn', @PI_PkgRVC, @Cashier, '';
                SELECT @PI_TmpNum = v FROM @PI_SeqOut;
                SET @PI_pTrnSeq = CAST(@PI_PkgRVC AS BIGINT) * 100000000 + @PI_TmpNum;
                SET @PI_pQty = 0;
                EXEC dbo.PostITrn @CheckNo, @PI_pTrnSeq, @Cashier, @ItemCode, @TrnDesc, @Price, @PI_pQty, @PI_OrderNo, @PI_TrnSeq, 1;
            END
        END

        DECLARE @PI_OrderByMenu VARCHAR(10);
        SELECT @PI_OrderByMenu = ParaStr FROM dbo.Para WITH (NOLOCK)
        WHERE ParaName = 'OrderPrintByMenu' AND RVC = @RVCNo;
        IF COALESCE(@PI_OrderByMenu,'') NOT IN ('0','')
        BEGIN
            -- Rsv2 carries the MenuNo that routes the line to its kitchen/bar printer when the
            -- RVC runs OrderPrintByMenu. Both the order chit and the VOID chit join
            -- ItemMenu on tmpTrn.Rsv2 = op.MenuNo (mdlPrintOrder.cs:319 and :360), so a line
            -- left with an empty Rsv2 silently prints nothing -- including its void chit,
            -- which is then lost for good because mdlPrintOrder exits quietly on an empty #O
            -- (mdlPrintOrder.cs:395-401).
            --
            -- POS WIN never leaves it empty: right after posting a line it writes the menu the
            -- operator currently has open, unconditionally
            -- (frmCheck.cs:6205,6357,6392,6446 -- Update tmpTrn Set Rsv2='<mCurMenu>').
            -- PDA has no such notion of a currently-open menu -- PostItemRequest.MenuNo is an
            -- int while ItemMenu.MenuNo is varchar(30) ('M09','N14'), so it cannot supply one.
            -- Carrying it over from the merged row (@PI_LastMenuNo) only works when an
            -- identical unsent line already exists, which is never true for the first order of
            -- an item; that is what left Rsv2 empty and killed the void chits.
            --
            -- So: prefer the merged row's menu, and otherwise resolve the item's own menu from
            -- RVCItemMenu -- the same table POS WIN falls back to when printing without a menu
            -- context (mdlPrintOrder.cs:344-348). Only ever fills a blank; never overwrites.
            DECLARE @PI_MenuNo VARCHAR(30) = NULL;

            IF COALESCE(@PI_LastMenuNo,'') <> ''
               AND EXISTS (SELECT 1 FROM dbo.ItemMenu WITH (NOLOCK)
                           WHERE MenuNo = @PI_LastMenuNo AND MenuActive = 1
                             AND COALESCE(PNo,0) <> 0 AND RVC = @RVCNo)
                SET @PI_MenuNo = @PI_LastMenuNo;

            IF @PI_MenuNo IS NULL
                SELECT TOP 1 @PI_MenuNo = m.ItemMenu
                FROM dbo.RVCItemMenu m WITH (NOLOCK)
                JOIN dbo.ItemMenu im WITH (NOLOCK) ON im.MenuNo = m.ItemMenu AND im.RVC = m.RVC
                WHERE m.RVC = @RVCNo
                  AND RTRIM(m.ItemCode) = RTRIM(@ItemCode)
                  AND im.MenuActive = 1 AND COALESCE(im.PNo,0) <> 0
                ORDER BY im.MenuOrder, m.ItemMenu;

            IF COALESCE(@PI_MenuNo,'') <> ''
            BEGIN
                -- // lint:allow-tracked-write: UPDATE tmpTrn — the open-bill line staging table. Inlined body of the legacy POS proc this call replaces (schema pda dropped); parity-tested against the original. Cooperative write on the SMILE money path.
                UPDATE dbo.tmpTrn SET Rsv2 = @PI_MenuNo
                WHERE TrnSeq = @PI_TrnSeq AND CheckNo = @CheckNo AND COALESCE(Rsv2,'') = '';
            END
        END

        EXEC dbo.UpdateTmpInvoice @CheckNo;

        IF ISNULL(@Note,'') <> ''
        BEGIN
            BEGIN TRY
                INSERT INTO dbo.CGTrn ([TrnNo],[ItemCode],[CGCode],[CGName])
                VALUES (@PI_TrnSeq, @ItemCode, 0, dbo.Unicode2TCVN(@Note));
            END TRY
            BEGIN CATCH
                -- Best-effort note write; a failure here must not fail the item post.
                -- (P6b: pda.Pda_ErrorLog telemetry retired — the note is non-critical.)
                SET @PI_TrnSeq = @PI_TrnSeq;
            END CATCH
        END
    END
END

SELECT ISNULL(@PI_IsOut,0) AS IsOut, @PI_OutResult AS MessageResult,
    ISNULL(@PI_TrnSeq,0) AS TrnSeq, ISNULL(@PI_TrnSeq,0) AS Seq,
    ISNULL(@CheckNo,0) AS CheckNo, ISNULL(@ItemCode,'') AS ItemCode;";

    /// <summary>Legacy <c>SqlPendingSendSeqs</c>: the item lines a send will fire. Params: @RVCNo, @CheckNo.</summary>
    internal const string PendingSendSeqs = @"
        SELECT t.TrnSeq
        FROM dbo.TmpTrn t
        LEFT JOIN dbo.tmpTrnInfo ti ON ti.TrnNo = t.TrnSeq AND ti.CheckNo = t.CheckNo
        WHERE t.CheckNo = @CheckNo AND t.RVC = @RVCNo AND t.TrnCode = 100
          AND ISNULL(ti.Void, 0) = 0
          AND ti.POrderTime IS NULL;";

    /// <summary>Legacy <c>SqlSendOrder</c>; the PDASendOrder audit insert is guarded by OBJECT_ID. Params: @RVCNo, @CheckNo, @Cashier.</summary>
    internal const string SendOrder = @"SET NOCOUNT ON;
-- pda.spSendOrder body inlined (P6a): @RVC=@RVCNo, @CheckNo=@CheckNo, @SendBy=@Cashier, @LogOff=0.
DECLARE @SO_Now DATETIME = GETDATE();

-- Backfill a tmpTrnInfo row for any item line that lacks one (item lines: TrnCode=100).
-- // lint:allow-tracked-write: INSERT INTO tmpTrnInfo — the open-bill line-info staging table. Inlined body of the legacy POS proc this call replaces (schema pda dropped); parity-tested against the original. Cooperative write on the SMILE money path.
INSERT INTO dbo.tmpTrnInfo (CheckNo, TrnNo, PToOrder, POrderTime, PToCheck, Void, OrderQty)
SELECT t.CheckNo, t.TrnSeq, 0, NULL, 0, 0, t.TrnQty
FROM dbo.tmpTrn t
WHERE t.CheckNo = @CheckNo
  AND t.TrnCode = 100
  AND NOT EXISTS (SELECT 1 FROM dbo.tmpTrnInfo i WHERE i.TrnNo = t.TrnSeq);

-- Flag everything not yet sent as ordered now.
UPDATE i
    SET i.PToOrder   = 1,
        i.POrderTime = @SO_Now,
        i.OrderQty   = t.TrnQty
FROM dbo.tmpTrnInfo i
JOIN dbo.tmpTrn t ON t.TrnSeq = i.TrnNo AND t.CheckNo = @CheckNo
WHERE i.CheckNo = @CheckNo
  AND ISNULL(i.PToOrder, 0) = 0
  AND ISNULL(i.Void, 0) = 0;

-- Audit send. Non-fatal: the order-flag update above is the source of truth for ""sent"".
-- OW1b (ADR 0104 §6): the audit table is absent on some SMILE_POS builds. A missing object is a compile error the
-- TRY below cannot catch, so the insert only runs when the table exists (pda_sync's grant on it is conditional too).
BEGIN TRY
    IF OBJECT_ID(N'dbo.PDASendOrder', N'U') IS NOT NULL
        INSERT INTO dbo.PDASendOrder (RVC, CheckNo, SendBy, SendTime)
        VALUES (@RVCNo, @CheckNo, @Cashier, @SO_Now);
END TRY
BEGIN CATCH
END CATCH;

-- Status grid, verbatim from pda.SRsp_SendOrder. SendOrderAsync uses ExecuteAsync and ignores
-- it, but emitting it keeps byte-for-byte parity with the original proc's contract.
SELECT 0 AS IsOut, N'' AS MessageResult;";

    /// <summary>Legacy <c>SqlVoidItem</c> (unsent removal, group expansion) without the payment-table deletes (ADR 0104 §5). Params: @CheckNo, @TrnSeq, @RVCNo, @VoidBy, @VoidReason, @AuthorCashier.</summary>
    internal const string VoidUnsent = @"SET NOCOUNT ON;
DECLARE @EffectiveAuthor NVARCHAR(50) =
    CASE WHEN NULLIF(@AuthorCashier,N'') IS NULL THEN @VoidBy ELSE @AuthorCashier END;
-- pda.spVoidItem body inlined (P6a): @CheckNo=@CheckNo, @TrnSeq=@TrnSeq, @RVC=@RVCNo,
-- @VoidReason=@VoidReason, @AuthorCashier=@EffectiveAuthor.
IF NOT EXISTS (SELECT 1 FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo AND RVC = @RVCNo AND Status = 1)
BEGIN
    RAISERROR('BILL_NOT_OPEN', 16, 1);
    RETURN;
END;

DECLARE @Void_Sts3 VARCHAR(50) = (SELECT StsCode3 FROM dbo.tmpTrn WHERE TrnSeq = @TrnSeq AND CheckNo = @CheckNo);
DECLARE @Void_Seq  VARCHAR(50) = CAST(@TrnSeq AS VARCHAR(50));

DECLARE @Void_HdrShiftPeriod TINYINT = (SELECT ShiftPeriod FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo);

DECLARE @Void_StsCode1  VARCHAR(50) = LEFT(dbo.Unicode2TCVN(ISNULL(@VoidReason, N'')), 50);
DECLARE @Void_AuthCash5 CHAR(5)     = LEFT(ISNULL(@EffectiveAuthor, N''), 5);
DECLARE @Void_Cashier5  CHAR(5)     = (SELECT LEFT(ISNULL(Cashier, ''), 5) FROM dbo.tmpTrn WHERE TrnSeq = @TrnSeq AND CheckNo = @CheckNo);
DECLARE @Void_Now DATETIME = GETDATE();

CREATE TABLE #Void (TrnSeq BIGINT PRIMARY KEY);
INSERT INTO #Void (TrnSeq)
SELECT TrnSeq FROM dbo.tmpTrn
WHERE CheckNo = @CheckNo
  AND (
        TrnSeq = @TrnSeq
     OR (ISNULL(@Void_Sts3,'') <> '' AND StsCode3 = @Void_Sts3)
     OR (ISNULL(RTRIM(Rsv1),'') = @Void_Seq)
     OR (TrnCode = 100 AND RefundRef = @TrnSeq)
      );

-- // lint:allow-tracked-write: INSERT INTO VoidTrn — the void archive. Inlined body of the legacy POS proc this call replaces (schema pda dropped); parity-tested against the original. Cooperative write on the SMILE money path.
INSERT INTO dbo.VoidTrn
    (NYear, NMonth, NDay, TrnSeq, CheckNo, TrnTime, Cashier, TrnCode, TrnSub, ItemCode,
     TrnDesc, ItemPrice, BaseSub, BaseTax, BaseTTL, TaxNo, TrnOrg, TrnBase, TrnPref, TrnQty,
     RVC, OrgCrCode, NewTrn, Refund, RefundRef, AuthorCashier, Status, ClassCode, GroupCode,
     StsCode1, StsCode2, StsCode3, StsCode4, BaseSrc, SrcNo, MealPeriod, ShiftPeriod,
     Split, FromRVC, BaseSTax, STaxNo, OrderNo, OrgCheck, OrgPrice, DscRef, Rsv1, Rsv2, Rsv3, Rsv4)
SELECT
     t.NYear, t.NMonth, t.NDay, t.TrnSeq, t.CheckNo, @Void_Now, @Void_Cashier5, t.TrnCode, t.TrnSub, t.ItemCode,
     LEFT(t.TrnDesc,30), t.ItemPrice, t.BaseSub, t.BaseTax, t.BaseTTL, t.TaxNo, t.TrnOrg, t.TrnBase, t.TrnPref, t.TrnQty,
     t.RVC, t.OrgCrCode, t.NewTrn, t.Refund, t.RefundRef, @Void_AuthCash5, t.Status, t.ClassCode, t.GroupCode,
     @Void_StsCode1, t.StsCode2, CONVERT(VARCHAR(50), @Void_Now), t.StsCode4, t.BaseSrc, t.SrcNo,
     t.MealPeriod,
     ISNULL(@Void_HdrShiftPeriod, t.MealPeriod),
     t.Split, t.FromRVC, t.BaseSTax, t.STaxNo, t.OrderNo, t.OrgCheck, t.OrgPrice, t.DscRef,
     t.Rsv1, t.Rsv2, t.Rsv3, t.Rsv4
FROM dbo.tmpTrn t
WHERE t.CheckNo = @CheckNo AND t.TrnSeq IN (SELECT TrnSeq FROM #Void);

-- OW1b (ADR 0104 §5): the legacy batch also deleted the line's rows from TmpPayment, TmpARINV, TmpCCINV, Card_Trans and
-- VoucherDtl. An item line owns no tender row and pda_sync has no grant on the payment tables, so those deletes are gone.
IF OBJECT_ID('dbo.CGTrn','U') IS NOT NULL
    DELETE FROM dbo.CGTrn WHERE TrnNo IN (SELECT TrnSeq FROM #Void);

-- Flag, never remove: POS WIN keeps the info row and sets Void=1 (frmCheck.cs:3839). The void
-- chit query inner-joins tmpTrnInfo and requires PToOrder=1 (mdlPrintOrder.cs:378), so a
-- deleted row silently suppresses the kitchen's void chit for this line from then on.
-- // lint:allow-tracked-write: UPDATE tmpTrnInfo — the open-bill line-info staging table. Inlined body of the legacy POS proc this call replaces (schema pda dropped); parity-tested against the original. Cooperative write on the SMILE money path.
UPDATE dbo.tmpTrnInfo SET Void = 1 WHERE TrnNo IN (SELECT TrnSeq FROM #Void);
-- // lint:allow-tracked-write: DELETE FROM tmpTrn — the open-bill line staging table. Inlined body of the legacy POS proc this call replaces (schema pda dropped); parity-tested against the original. Cooperative write on the SMILE money path.
DELETE FROM dbo.tmpTrn     WHERE CheckNo = @CheckNo AND TrnSeq IN (SELECT TrnSeq FROM #Void);

-- // lint:allow-tracked-write: UPDATE tmpTrn — the open-bill line staging table. Inlined body of the legacy POS proc this call replaces (schema pda dropped); parity-tested against the original. Cooperative write on the SMILE money path.
UPDATE dbo.tmpTrn
   SET DscRef = NULL
 WHERE CheckNo = @CheckNo
   AND DscRef IN (SELECT TrnSeq FROM #Void);

DROP TABLE #Void;

EXEC dbo.UpdateTmpInvoice @CheckNo;";

    /// <summary>Legacy <c>SqlVoid</c>, verbatim (sent void with a reason). Params: @TrnSeq, @CheckNo, @RVCNo, @VoidBy, @VoidReason.</summary>
    internal const string VoidSent = @"SET NOCOUNT ON;
DECLARE @TrnCode INT;
SELECT TOP 1 @TrnCode = TrnCode
FROM dbo.TmpTrn WITH (NOLOCK)
WHERE TrnSeq = @TrnSeq AND CheckNo = @CheckNo AND RVC = @RVCNo;
IF @TrnCode IS NULL
BEGIN
    SELECT 1 AS IsOut;
    RETURN;
END;
-- pda.spVoid body inlined (P6a): @RVC=@RVCNo, @CheckNo=@CheckNo, @TrnSeq=@TrnSeq,
-- @TrnCode=@TrnCode, @RefundRef=0, @IsVoucher=0, @IsReOpen=0, @QtyVoid=0,
-- @ClerkID=@VoidBy, @AuthorCashier=@VoidBy, @VoidResion=@VoidReason.
IF NOT EXISTS (SELECT 1 FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo AND RVC = @RVCNo AND Status = 1)
BEGIN
    RAISERROR('BILL_NOT_OPEN', 16, 1);
    RETURN;
END;

DECLARE @SqlVoid_HdrShiftPeriod TINYINT = (SELECT ShiftPeriod FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo);

-- Carry the full row, exactly as POS WIN does (frmCheck.cs:3747-3762 copies tmpTrn into
-- VoidTrn with Rsv1..Rsv4 in the column list). Rsv2 is load-bearing: under OrderPrintByMenu
-- the void chit joins ItemMenu on VoidTrn.Rsv2 = op.MenuNo (mdlPrintOrder.cs:360), so a
-- VoidTrn row written without it can never print its void chit -- and mdlPrintOrder exits
-- silently on an empty result (mdlPrintOrder.cs:395-401), so nobody finds out.
-- // lint:allow-tracked-write: INSERT INTO VoidTrn — the void archive. Inlined body of the legacy POS proc this call replaces (schema pda dropped); parity-tested against the original. Cooperative write on the SMILE money path.
INSERT INTO dbo.voidtrn
    (NYear, NMonth, NDay, TrnSeq, CheckNo, TrnTime, Cashier, TrnCode, TrnSub, ItemCode,
     TrnDesc, ItemPrice, BaseSub, BaseTax, BaseTTL, TaxNo, TrnOrg, TrnBase, TrnPref, TrnQty,
     RVC, OrgCrCode, NewTrn, Refund, RefundRef, AuthorCashier, Status, ClassCode, GroupCode,
     StsCode1, StsCode2, StsCode3, StsCode4, BaseSrc, SrcNo, MealPeriod, ShiftPeriod,
     Split, FromRVC, BaseSTax, STaxNo, OrderNo, OrgCheck, OrgPrice, DscRef, Rsv1, Rsv2, Rsv3, Rsv4)
SELECT
     NYear, NMonth, NDay, TrnSeq, CheckNo, GETDATE(), LEFT(@VoidBy,5), TrnCode, TrnSub, ItemCode,
     LEFT(TrnDesc,30), ItemPrice, BaseSub, BaseTax, BaseTTL, TaxNo, TrnOrg, TrnBase, TrnPref, TrnQty,
     RVC, OrgCrCode, NewTrn, Refund, RefundRef, LEFT(@VoidBy,5), Status, ClassCode, GroupCode,
     LEFT(ISNULL(@VoidReason,N''),50), StsCode2, CONVERT(VARCHAR(50), GETDATE()), StsCode4, BaseSrc, SrcNo,
     MealPeriod,
     ISNULL(@SqlVoid_HdrShiftPeriod, MealPeriod),
     Split, FromRVC, BaseSTax, STaxNo, OrderNo, OrgCheck, OrgPrice, DscRef,
     Rsv1, Rsv2, Rsv3, Rsv4
FROM dbo.tmpTrn
WHERE CheckNo = @CheckNo AND TrnSeq = @TrnSeq;

-- Flag the info row instead of removing it, as POS WIN does (frmCheck.cs:3839): the void
-- chit query inner-joins tmpTrnInfo and requires PToOrder=1 (mdlPrintOrder.cs:378).
-- // lint:allow-tracked-write: UPDATE tmpTrnInfo.Void — open-bill line staging, not posted revenue. Verbatim inlined body of the legacy POS proc this call replaces (schema pda dropped); parity-tested. Void is set, never cleared, and the row is kept precisely so the void chit can still print.
UPDATE dbo.tmpTrnInfo SET Void = 1 WHERE TrnNo = @TrnSeq;
DELETE FROM dbo.tmpTrn WHERE CheckNo = @CheckNo AND TrnSeq = @TrnSeq;

EXEC dbo.UpdateTmpInvoice @CheckNo;

SELECT 0 AS IsOut;";
}
