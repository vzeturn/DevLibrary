//! The legacy SMILE_POS batches of the order and table verbs (`Commands/OrderCommandSql.Batches*.cs`),
//! verbatim. GENERATED from the C# sources by `%TEMP%\kx\c1-gen.ps1`; do not edit by hand:
//! POS WIN parity is the contract; the tests in `commands::tests::sql` hold every constant
//! against the C# text and the SHA-256 pins of the C# tests.

// ---- OrderCommandSql.Batches.cs

/// C# `OrderCommandSql.OpenCheck`, verbatim.
pub const OPEN_CHECK: &str = r#"SET NOCOUNT ON;
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

SELECT @OC_CheckNo AS CheckNo;"#;

/// C# `OrderCommandSql.PostItem`, verbatim.
pub const POST_ITEM: &str = r#"SET NOCOUNT ON;

-- For an open-name item (ItemDef.OpenItem=1) the caller supplies the description;
-- that is what makes the item "open". Everything else keeps ItemDef.IName. The
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
    ISNULL(@CheckNo,0) AS CheckNo, ISNULL(@ItemCode,'') AS ItemCode;"#;

/// C# `OrderCommandSql.PendingSendSeqs`, verbatim.
pub const PENDING_SEND_SEQS: &str = r#"
        SELECT t.TrnSeq
        FROM dbo.TmpTrn t
        LEFT JOIN dbo.tmpTrnInfo ti ON ti.TrnNo = t.TrnSeq AND ti.CheckNo = t.CheckNo
        WHERE t.CheckNo = @CheckNo AND t.RVC = @RVCNo AND t.TrnCode = 100
          AND ISNULL(ti.Void, 0) = 0
          AND ti.POrderTime IS NULL;"#;

/// C# `OrderCommandSql.SendOrder`, verbatim.
pub const SEND_ORDER: &str = r#"SET NOCOUNT ON;
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

-- Audit send. Non-fatal: the order-flag update above is the source of truth for "sent".
-- OW1b (ADR 0104 §6): the audit table is absent on some SMILE_POS builds. A missing object is a compile error the
-- TRY below cannot catch, so the insert only runs when the table exists (pda_sync's grant on it is conditional too).
BEGIN TRY
    IF OBJECT_ID(N'dbo.PDASendOrder', N'U') IS NOT NULL
    BEGIN
        -- RT-1371: POS WIN polls PDASendOrder (Coalesce(OrderSent,0)=0) and prints the order itself, then flags the row
        -- OrderSent=1 (OrderTime/OrderBy). When the cloud already printed the KOT (@PdaHandled=1) the row is written already
        -- flagged the way POS WIN flags it after printing, so it is not printed a second time. Needs the three columns.
        IF ISNULL(@PdaHandled, 0) = 1
           AND COL_LENGTH(N'dbo.PDASendOrder', N'OrderSent') IS NOT NULL
           AND COL_LENGTH(N'dbo.PDASendOrder', N'OrderTime') IS NOT NULL
           AND COL_LENGTH(N'dbo.PDASendOrder', N'OrderBy') IS NOT NULL
            EXEC sys.sp_executesql
                N'INSERT INTO dbo.PDASendOrder (RVC, CheckNo, SendBy, SendTime, OrderSent, OrderTime, OrderBy) VALUES (@r, @c, @s, @t, 1, @t, ''PDACLOUD'');',
                N'@r INT, @c DECIMAL(18,0), @s NVARCHAR(50), @t DATETIME',
                @RVCNo, @CheckNo, @Cashier, @SO_Now;
        ELSE
            INSERT INTO dbo.PDASendOrder (RVC, CheckNo, SendBy, SendTime)
            VALUES (@RVCNo, @CheckNo, @Cashier, @SO_Now);
    END
END TRY
BEGIN CATCH
END CATCH;

-- Status grid, verbatim from pda.SRsp_SendOrder. SendOrderAsync uses ExecuteAsync and ignores
-- it, but emitting it keeps byte-for-byte parity with the original proc's contract.
SELECT 0 AS IsOut, N'' AS MessageResult;"#;

/// C# `OrderCommandSql.VoidUnsent`, verbatim.
pub const VOID_UNSENT: &str = r#"SET NOCOUNT ON;
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

EXEC dbo.UpdateTmpInvoice @CheckNo;"#;

/// C# `OrderCommandSql.VoidSent`, verbatim.
pub const VOID_SENT: &str = r#"SET NOCOUNT ON;
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

SELECT 0 AS IsOut;"#;

// ---- OrderCommandSql.BatchesOw1c.cs

/// C# `OrderCommandSql.SetCovers`, verbatim.
pub const SET_COVERS: &str = r#"SET NOCOUNT ON;

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

SELECT 0 AS IsOut, N'OK' AS MessageResult;"#;

// ---- OrderCommandSql.BatchesOw1d.cs

/// C# `OrderCommandSql.PostCombo`, verbatim.
pub const POST_COMBO: &str = r#"SET NOCOUNT ON;

CREATE TABLE #ComboComponents (
    ItemCode NVARCHAR(50),
    ItemName NVARCHAR(200),
    Qty      DECIMAL(10,3),
    Price    DECIMAL(18,4)
);

-- Bill order-round counter, POS WIN's frmCheck.MyOrderNo (Max(OrderNo) over the bill, +1
-- unless OrderByCourse, floor 1). Derived once for the whole combo so the parent and all
-- components share a round, matching how POS WIN posts a package with a single MyOrderNo.
DECLARE @CB_OrderNo INT;
SELECT @CB_OrderNo = ISNULL(MAX(OrderNo), 0) FROM dbo.tmpTrn WITH (NOLOCK) WHERE CheckNo = @CheckNo;
IF ISNULL((SELECT TOP 1 ParaStr FROM dbo.Para WITH (NOLOCK)
           WHERE ParaName = 'OrderByCourse' AND RVC = @RVCNo), '0') IN ('0', '')
    SET @CB_OrderNo = @CB_OrderNo + 1;
IF ISNULL(@CB_OrderNo, 0) <= 0 SET @CB_OrderNo = 1;

-- OW1d (ADR 0110 §3, as ADR 0104 §3): a check missing from tmpInvoice is refused before anything is written, never
-- recreated. Both spReCreateCheckNo branches below (a new tmpInvoice header from IOrgInvoice) are dropped; the executor
-- maps IsOut 1 to check_not_found and IsOut 2 to item_not_found. The status row at the end is the only result set.
DECLARE @CB_IsOut INT = 0, @CB_OutResult VARCHAR(50) = '', @CB_FirstTrnSeq BIGINT = NULL;
IF (ISNULL(@CheckNo,0) = 0 OR NOT EXISTS(SELECT TOP 1 * FROM dbo.TmpInvoice WITH (NOLOCK) WHERE CheckNo = @CheckNo AND RVC = @RVCNo))
    SELECT @CB_IsOut = 1, @CB_OutResult = 'CHECK_NOT_FOUND';

-- OW1d (ADR 0110 §4): TRY_CAST yields the legacy CAST-in-TRY/CATCH value (0 for a code that is not an INT) without
-- raising a conversion error inside the command's transaction.
DECLARE @ComboId INT;
SET @ComboId = ISNULL(TRY_CAST(@ComboCode AS INT), 0);

IF @CB_IsOut = 0 AND @ComboId > 0
BEGIN
    -- OW1d (ADR 0110 §4): the legacy empty TRY/CATCH around this INSERT is dropped. A failed expansion fails the command
    -- instead of silently posting the combo item itself at the client's price.
    -- pda.spComboDetails body inlined (P6a): @ComboId=@ComboId, @RVCNo=@RVCNo.
    INSERT INTO #ComboComponents (ItemCode, ItemName, Qty, Price)
    SELECT
        RTRIM(d.ICode)                                              AS ItemCode,
        ISNULL(NULLIF(RTRIM(d.IName), ''), RTRIM(d.ICode))          AS ItemName,
        CAST(ISNULL(p.IQty, 1) AS DECIMAL(10,3))                    AS Qty,
        CAST(CASE WHEN ISNULL(p.UseItemPrice, 0) = 1 THEN d.Price ELSE p.IPrice END AS DECIMAL(18,4)) AS Price
    FROM dbo.PkgDef p
    INNER JOIN dbo.ItemDef d ON RTRIM(d.ICode) = RTRIM(p.ItemCode)
    WHERE RTRIM(p.PkgCode) = CAST(@ComboId AS VARCHAR(12))
    ORDER BY p.DisplayOrder;
END;

-- OW1d (ADR 0110 §5): a selection (@Selected = '|code|code|', @SelectedCount distinct codes) keeps only the chosen
-- components, at their package price and quantity. Every chosen code must be a component of this combo, else the command
-- is refused before any write. The legacy selection path posted each code through the item post at price 0 without its
-- open-item parameters and always failed.
IF @CB_IsOut = 0 AND ISNULL(@SelectedCount, 0) > 0
BEGIN
    DELETE FROM #ComboComponents WHERE CHARINDEX(N'|' + ItemCode + N'|', @Selected) = 0;
    IF (SELECT COUNT(DISTINCT ItemCode) FROM #ComboComponents) <> @SelectedCount
        SELECT @CB_IsOut = 2, @CB_OutResult = 'ITEM_NOT_FOUND';
END;

-- OW1d (ADR 0110 §6): with no components the combo item itself is posted; a code missing from ItemDef is refused like
-- the item post, instead of writing a line for an unknown item.
IF @CB_IsOut = 0 AND (SELECT COUNT(*) FROM #ComboComponents) = 0
   AND NOT EXISTS (SELECT 1 FROM dbo.ItemDef WITH (NOLOCK) WHERE ICode = @ComboCode)
    SELECT @CB_IsOut = 2, @CB_OutResult = 'ITEM_NOT_FOUND';

IF @CB_IsOut = 0 AND (SELECT COUNT(*) FROM #ComboComponents) = 0
BEGIN
    -- pda.SRsp_PostItem body inlined (P6a): @RVCNo=@RVCNo, @CheckNo=@CheckNo, @TableNo=@TableNo,
    -- @Cashier=@Cashier, @ItemCode=@ComboCode, @Qty=@Qty, @SeatNo=@SeatNo, @Price=@Price,
    -- @MenuNo=@MenuNo, @Note=@Note. Nested pda.spPostItem inlined below it (@PC1_ prefix),
    -- which itself inlines pda.spReCreateCheckNo (@PC1RCN_ prefix).
    DECLARE @PC1_TrnDesc NVARCHAR(30);
    SELECT TOP 1 @PC1_TrnDesc = RTRIM(ISNULL(IName, @ComboCode))
    FROM dbo.ItemDef WITH (NOLOCK)
    WHERE ICode = @ComboCode;
    IF @PC1_TrnDesc IS NULL SET @PC1_TrnDesc = RTRIM(@ComboCode);

    DECLARE @PC1_SubMarker VARCHAR(50);
    IF LEN(LTRIM(RTRIM(@Note))) = 0
        SET @PC1_SubMarker = '';
    ELSE
        SET @PC1_SubMarker = LEFT(CONVERT(VARCHAR(50), HASHBYTES('MD5', CAST(@Note AS NVARCHAR(250))), 2), 50);

    -- pda.spPostItem body inlined (P6a): @CheckNo=@CheckNo, @RVC=@RVCNo, @Cashier=@Cashier,
    -- @ItemCode=@ComboCode, @TrnDesc=@PC1_TrnDesc, @Price=@Price, @Qty=@Qty, @OrderNo=@CB_OrderNo,
    -- @GuestNo=@SeatNo, @PPolicy='' (default), @ItemDiscount=0 (default), @ISubCode=@PC1_SubMarker,
    -- @Note=@Note. @OrderNo is the bill order-round counter (frmCheck.MyOrderNo), not MenuNo --
    -- see the note on @PI_OrderNo in SqlPostItem. Derived once above so the combo parent and
    -- every component land in the same round, as they do when POS WIN posts a package.
    DECLARE @PC1_IsOut INT = 0;
    DECLARE @PC1_TrnSeq BIGINT, @PC1_ParaStr BIGINT;
    DECLARE @PC1_SeqOut TABLE (v BIGINT);
    DECLARE @PC1_Qty DECIMAL(18,3) = @Qty;

    IF (ISNULL(@CheckNo,0) = 0)
        SET @PC1_IsOut = 1;

    -- OW1d (ADR 0110 §3): the spReCreateCheckNo branch is dropped; the guard at the top refused a missing check.

    IF (ISNULL(@PC1_IsOut,0) = 0)
    BEGIN
        IF (ISNULL(@CheckNo,0) > 0 AND EXISTS (SELECT TOP 1 * FROM dbo.TmpInvoice WITH (NOLOCK) WHERE CHECKNO = ISNULL(@CheckNo,0) AND RVC = @RVCNo))
        BEGIN
            DELETE FROM @PC1_SeqOut;
            INSERT INTO @PC1_SeqOut (v) EXEC dbo.GetRVCSeqNumber 'LastTrn', @RVCNo, @Cashier, '';
            SELECT @PC1_ParaStr = v FROM @PC1_SeqOut;

            SET @PC1_TrnSeq = @RVCNo * 100000000 + @PC1_ParaStr;

            IF @PC1_TrnSeq > 9223372036854775807
            BEGIN
                RAISERROR('Generated TrnSeq exceeds bigint capacity', 16, 1);
                RETURN;
            END

            DECLARE @PC1_LastQty NUMERIC(18,2);
            DECLARE @PC1_LastTrn BIGINT;
            DECLARE @PC1_GroupTrn VARCHAR(50);
            DECLARE @PC1_DecDiv INT;
            DECLARE @PC1_LastMenuNo VARCHAR(30);

            SELECT @PC1_GroupTrn = ParaStr
            FROM dbo.Para WITH (NOLOCK)
            WHERE ParaName = 'GroupChkTrn' AND RVC = (SELECT RVC FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo);

            SELECT @PC1_DecDiv = C.DecDiv, @PC1_TrnDesc = (CASE WHEN ISNULL(OpenItem,0) = 1 THEN dbo.Unicode2TCVN(@PC1_TrnDesc) ELSE IName END)
            FROM dbo.ItemDef I WITH (NOLOCK)
            JOIN dbo.CurrencyDef C WITH (NOLOCK) ON I.ICurrency = C.CrCode
            WHERE I.ICode = @ComboCode;

            IF COALESCE(@PC1_DecDiv, 0) = 0 SET @PC1_DecDiv = 1;

            IF @PC1_Qty <> 0 AND @PC1_GroupTrn = '1' AND @PC1_SubMarker = ''
            BEGIN
                SELECT @PC1_LastTrn = TrnSeq, @PC1_LastQty = TrnQty, @PC1_LastMenuNo = COALESCE(Rsv2, '')
                FROM dbo.tmpTrn WITH (NOLOCK)
                WHERE TrnCode = 100
                  AND CheckNo = @CheckNo
                  AND ItemCode = @ComboCode
                  AND OrderNo = @CB_OrderNo
                  AND ItemCode NOT IN (SELECT ICode FROM dbo.ItemDef WITH (NOLOCK) WHERE OpenPrice = 1 OR OpenItem = 1 OR IsPkg = 1)
                  AND OrgPrice = @Price * @PC1_DecDiv
                  AND COALESCE(MealPeriod, 0) = @SeatNo
                  AND COALESCE(StsCode4, '') = '';

                IF COALESCE(@PC1_LastQty, 0) = 0
                    SET @PC1_LastTrn = 0;

                IF COALESCE(@PC1_LastTrn, 0) > 0
                BEGIN
                    IF EXISTS(SELECT TrnNo FROM dbo.tmpTrnInfo WITH (NOLOCK) WHERE TrnNo = @PC1_LastTrn AND (PToOrder = 1 OR PToCheck = 1))
                    BEGIN
                        SET @PC1_LastTrn = 0;
                    END
                    ELSE IF EXISTS(SELECT TrnSeq FROM dbo.tmpTrn WITH (NOLOCK) WHERE Rsv1 = CAST(@PC1_LastTrn AS VARCHAR(50)))
                    BEGIN
                        SET @PC1_LastTrn = 0;
                    END
                    ELSE
                    BEGIN
                        SET @PC1_TrnSeq = @PC1_LastTrn;
                        DELETE FROM dbo.tmpTrn WHERE TrnSeq = @PC1_TrnSeq;
                        SET @PC1_Qty = @PC1_Qty + COALESCE(@PC1_LastQty, 0);
                    END
                END
            END

            DECLARE @PC1_TrnQty NUMERIC(18,2);
            SET @PC1_TrnQty = @PC1_Qty;
            IF @PC1_Qty = 0 SET @PC1_Qty = 1;

            EXEC dbo.PostITrn @CheckNo, @PC1_TrnSeq, @Cashier, @ComboCode, @PC1_TrnDesc, @Price, @PC1_Qty, @CB_OrderNo, '', 0, @PC1_TrnQty, @SeatNo, '', 0, @PC1_SubMarker;
            SET @CB_FirstTrnSeq = ISNULL(@CB_FirstTrnSeq, @PC1_TrnSeq); -- OW1d: status row

            DECLARE @PC1_IsPkg BIT;
            DECLARE @PC1_PkgRVC INT = @RVCNo;
            SELECT @PC1_IsPkg = COALESCE(IsPkg,0) FROM dbo.ItemDef WHERE ICode = @ComboCode;
            IF @PC1_IsPkg = 1
            BEGIN
                SELECT @PC1_PkgRVC = RVC FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo;
                DECLARE @PC1_pICode VARCHAR(30);
                DECLARE @PC1_pQty NUMERIC(18,2);
                DECLARE @PC1_pPrice NUMERIC(18,2);
                DECLARE @PC1_pTrnSeq BIGINT;
                DECLARE @PC1_TmpNum BIGINT;
                DECLARE @PC1_HasTrn INT = 0;
                DECLARE PC1Cur CURSOR FOR SELECT P.ItemCode, P.iQty, (CASE WHEN P.UseItemPrice = 1 THEN I.Price ELSE P.IPrice END)/COALESCE(C.DecDiv,1)
                    FROM dbo.PkgDef P JOIN dbo.ItemDef I ON P.ItemCode = I.ICode
                    LEFT JOIN dbo.CurrencyDef C ON I.ICurrency = C.CrCode
                    WHERE PkgCode = @ComboCode ORDER BY P.DisplayOrder;
                OPEN PC1Cur;
                FETCH NEXT FROM PC1Cur INTO @PC1_pICode, @PC1_pQty, @PC1_pPrice;
                WHILE @@FETCH_STATUS = 0
                BEGIN
                    SET @PC1_HasTrn = 1;
                    IF @PC1_pQty = 0 SET @PC1_pQty = 1;
                    SET @PC1_pQty = @PC1_pQty * @PC1_Qty;
                    DELETE FROM @PC1_SeqOut;
                    INSERT INTO @PC1_SeqOut (v) EXEC dbo.GetRVCSeqNumber 'LastTrn', @PC1_PkgRVC, @Cashier, '';
                    SELECT @PC1_TmpNum = v FROM @PC1_SeqOut;
                    SET @PC1_pTrnSeq = @PC1_PkgRVC * 100000000 + @PC1_TmpNum;
                    EXEC dbo.PostITrn @CheckNo, @PC1_pTrnSeq, @Cashier, @PC1_pICode, '', @PC1_pPrice, @PC1_pQty, @CB_OrderNo, @PC1_TrnSeq, 0;
                    FETCH NEXT FROM PC1Cur INTO @PC1_pICode, @PC1_pQty, @PC1_pPrice;
                END
                CLOSE PC1Cur;
                DEALLOCATE PC1Cur;
                IF @PC1_HasTrn > 0
                BEGIN
                    DELETE FROM @PC1_SeqOut;
                    INSERT INTO @PC1_SeqOut (v) EXEC dbo.GetRVCSeqNumber 'LastTrn', @PC1_PkgRVC, @Cashier, '';
                    SELECT @PC1_TmpNum = v FROM @PC1_SeqOut;
                    SET @PC1_pTrnSeq = @PC1_PkgRVC * 100000000 + @PC1_TmpNum;
                    SET @PC1_pQty = 0;
                    EXEC dbo.PostITrn @CheckNo, @PC1_pTrnSeq, @Cashier, @ComboCode, @PC1_TrnDesc, @Price, @PC1_pQty, @CB_OrderNo, @PC1_TrnSeq, 1;
                END
            END

            DECLARE @PC1_OrderByMenu VARCHAR(10);
            SELECT @PC1_OrderByMenu = ParaStr FROM dbo.Para WITH (NOLOCK)
            WHERE ParaName = 'OrderPrintByMenu' AND RVC = @RVCNo;
            IF COALESCE(@PC1_OrderByMenu,'') NOT IN ('0','')
            BEGIN
                -- Same printer-routing rule as SqlPostItem: never leave Rsv2 blank, or this
                -- combo line loses both its order chit and its void chit. See the long note there.
                DECLARE @PC1_MenuNo VARCHAR(30) = NULL;

                IF COALESCE(@PC1_LastMenuNo,'') <> ''
                   AND EXISTS (SELECT 1 FROM dbo.ItemMenu WITH (NOLOCK)
                               WHERE MenuNo = @PC1_LastMenuNo AND MenuActive = 1
                                 AND COALESCE(PNo,0) <> 0 AND RVC = @RVCNo)
                    SET @PC1_MenuNo = @PC1_LastMenuNo;

                IF @PC1_MenuNo IS NULL
                    SELECT TOP 1 @PC1_MenuNo = m.ItemMenu
                    FROM dbo.RVCItemMenu m WITH (NOLOCK)
                    JOIN dbo.ItemMenu im WITH (NOLOCK) ON im.MenuNo = m.ItemMenu AND im.RVC = m.RVC
                    WHERE m.RVC = @RVCNo
                      AND RTRIM(m.ItemCode) = RTRIM(@ComboCode)
                      AND im.MenuActive = 1 AND COALESCE(im.PNo,0) <> 0
                    ORDER BY im.MenuOrder, m.ItemMenu;

                IF COALESCE(@PC1_MenuNo,'') <> ''
                -- // lint:allow-tracked-write: UPDATE tmpTrn — the open-bill line staging table. Inlined body of the legacy POS proc this call replaces (schema pda dropped); parity-tested against the original. Cooperative write on the SMILE money path.
                UPDATE dbo.tmpTrn SET Rsv2 = @PC1_MenuNo
                WHERE TrnSeq = @PC1_TrnSeq AND CheckNo = @CheckNo AND COALESCE(Rsv2,'') = '';
            END

            EXEC dbo.UpdateTmpInvoice @CheckNo;

            IF ISNULL(@Note,'') <> ''
            BEGIN
                BEGIN TRY
                    INSERT INTO dbo.CGTrn ([TrnNo],[ItemCode],[CGCode],[CGName])
                    VALUES (@PC1_TrnSeq, @ComboCode, 0, dbo.Unicode2TCVN(@Note));
                END TRY
                BEGIN CATCH
                    -- Best-effort note write; a failure here must not fail the item post.
                    -- (P6b: pda.Pda_ErrorLog telemetry retired — the note is non-critical.)
                    SET @PC1_TrnSeq = @PC1_TrnSeq;
                END CATCH
            END
        END
    END
END
ELSE IF @CB_IsOut = 0
BEGIN
    DECLARE @CompCode  NVARCHAR(50);
    DECLARE @CompQty   DECIMAL(10,3);
    DECLARE @CompPrice DECIMAL(18,4);

    DECLARE cur CURSOR LOCAL FAST_FORWARD FOR
        SELECT ItemCode, Qty, Price FROM #ComboComponents;

    OPEN cur;
    FETCH NEXT FROM cur INTO @CompCode, @CompQty, @CompPrice;

    DECLARE @LineQty DECIMAL(18,3);
    WHILE @@FETCH_STATUS = 0
    BEGIN
        SET @LineQty = @CompQty * @Qty;

        -- pda.SRsp_PostItem body inlined (P6a): @RVCNo=@RVCNo, @CheckNo=@CheckNo, @TableNo=@TableNo,
        -- @Cashier=@Cashier, @ItemCode=@CompCode, @Qty=@LineQty, @SeatNo=@SeatNo, @Price=@CompPrice,
        -- @MenuNo=@MenuNo, @Note=@Note. Nested pda.spPostItem inlined below it (@PC2_ prefix),
        -- which itself inlines pda.spReCreateCheckNo (@PC2RCN_ prefix). Runs once per combo
        -- component fetched from the cursor above.
        -- OW1d (ADR 0110 §2): inside the component loop a DECLARE without an initializer does not reset its variable, and a
        -- SELECT @v = ... that matches no row leaves it unchanged. So every @PC2_ variable assigned by a SELECT is declared
        -- = NULL and starts each component empty. In the legacy batch @PC2_LastTrn / @PC2_LastQty of a component merged
        -- under GroupChkTrn = '1' carried into the next component, which then deleted that merged line of the check and
        -- re-posted it as itself with the merged quantity added; @PC2_LastMenuNo and @PC2_DecDiv carried over the same way.
        DECLARE @PC2_TrnDesc NVARCHAR(30) = NULL;
        SELECT TOP 1 @PC2_TrnDesc = RTRIM(ISNULL(IName, @CompCode))
        FROM dbo.ItemDef WITH (NOLOCK)
        WHERE ICode = @CompCode;
        IF @PC2_TrnDesc IS NULL SET @PC2_TrnDesc = RTRIM(@CompCode);

        DECLARE @PC2_SubMarker VARCHAR(50);
        IF LEN(LTRIM(RTRIM(@Note))) = 0
            SET @PC2_SubMarker = '';
        ELSE
            SET @PC2_SubMarker = LEFT(CONVERT(VARCHAR(50), HASHBYTES('MD5', CAST(@Note AS NVARCHAR(250))), 2), 50);

        -- pda.spPostItem body inlined (P6a): @CheckNo=@CheckNo, @RVC=@RVCNo, @Cashier=@Cashier,
        -- @ItemCode=@CompCode, @TrnDesc=@PC2_TrnDesc, @Price=@CompPrice, @Qty=@LineQty,
        -- @OrderNo=@CB_OrderNo, @GuestNo=@SeatNo, @PPolicy='' (default), @ItemDiscount=0 (default),
        -- @ISubCode=@PC2_SubMarker, @Note=@Note. @CB_OrderNo is the shared bill order round --
        -- not MenuNo; see the note where it is derived at the top of this statement.
        DECLARE @PC2_IsOut INT = 0;
        DECLARE @PC2_TrnSeq BIGINT = NULL, @PC2_ParaStr BIGINT = NULL;
        DECLARE @PC2_SeqOut TABLE (v BIGINT);
        DECLARE @PC2_Qty DECIMAL(18,3) = @LineQty;

        IF (ISNULL(@CheckNo,0) = 0)
            SET @PC2_IsOut = 1;

        -- OW1d (ADR 0110 §3): the spReCreateCheckNo branch is dropped; the guard at the top refused a missing check.

        IF (ISNULL(@PC2_IsOut,0) = 0)
        BEGIN
            IF (ISNULL(@CheckNo,0) > 0 AND EXISTS (SELECT TOP 1 * FROM dbo.TmpInvoice WITH (NOLOCK) WHERE CHECKNO = ISNULL(@CheckNo,0) AND RVC = @RVCNo))
            BEGIN
                DELETE FROM @PC2_SeqOut;
                INSERT INTO @PC2_SeqOut (v) EXEC dbo.GetRVCSeqNumber 'LastTrn', @RVCNo, @Cashier, '';
                SELECT @PC2_ParaStr = v FROM @PC2_SeqOut;

                SET @PC2_TrnSeq = @RVCNo * 100000000 + @PC2_ParaStr;

                IF @PC2_TrnSeq > 9223372036854775807
                BEGIN
                    RAISERROR('Generated TrnSeq exceeds bigint capacity', 16, 1);
                    RETURN;
                END

                DECLARE @PC2_LastQty NUMERIC(18,2) = NULL;
                DECLARE @PC2_LastTrn BIGINT = NULL;
                DECLARE @PC2_LastMenuNo VARCHAR(30) = NULL;
                DECLARE @PC2_GroupTrn VARCHAR(50) = NULL;
                DECLARE @PC2_DecDiv INT = NULL;

                SELECT @PC2_GroupTrn = ParaStr
                FROM dbo.Para WITH (NOLOCK)
                WHERE ParaName = 'GroupChkTrn' AND RVC = (SELECT RVC FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo);

                SELECT @PC2_DecDiv = C.DecDiv, @PC2_TrnDesc = (CASE WHEN ISNULL(OpenItem,0) = 1 THEN dbo.Unicode2TCVN(@PC2_TrnDesc) ELSE IName END)
                FROM dbo.ItemDef I WITH (NOLOCK)
                JOIN dbo.CurrencyDef C WITH (NOLOCK) ON I.ICurrency = C.CrCode
                WHERE I.ICode = @CompCode;

                IF COALESCE(@PC2_DecDiv, 0) = 0 SET @PC2_DecDiv = 1;

                IF @PC2_Qty <> 0 AND @PC2_GroupTrn = '1' AND @PC2_SubMarker = ''
                BEGIN
                    SELECT @PC2_LastTrn = TrnSeq, @PC2_LastQty = TrnQty, @PC2_LastMenuNo = COALESCE(Rsv2, '')
                    FROM dbo.tmpTrn WITH (NOLOCK)
                    WHERE TrnCode = 100
                      AND CheckNo = @CheckNo
                      AND ItemCode = @CompCode
                      AND OrderNo = @CB_OrderNo
                      AND ItemCode NOT IN (SELECT ICode FROM dbo.ItemDef WITH (NOLOCK) WHERE OpenPrice = 1 OR OpenItem = 1 OR IsPkg = 1)
                      AND OrgPrice = @CompPrice * @PC2_DecDiv
                      AND COALESCE(MealPeriod, 0) = @SeatNo
                      AND COALESCE(StsCode4, '') = '';

                    IF COALESCE(@PC2_LastQty, 0) = 0
                        SET @PC2_LastTrn = 0;

                    IF COALESCE(@PC2_LastTrn, 0) > 0
                    BEGIN
                        IF EXISTS(SELECT TrnNo FROM dbo.tmpTrnInfo WITH (NOLOCK) WHERE TrnNo = @PC2_LastTrn AND (PToOrder = 1 OR PToCheck = 1))
                        BEGIN
                            SET @PC2_LastTrn = 0;
                        END
                        ELSE IF EXISTS(SELECT TrnSeq FROM dbo.tmpTrn WITH (NOLOCK) WHERE Rsv1 = CAST(@PC2_LastTrn AS VARCHAR(50)))
                        BEGIN
                            SET @PC2_LastTrn = 0;
                        END
                        ELSE
                        BEGIN
                            SET @PC2_TrnSeq = @PC2_LastTrn;
                            DELETE FROM dbo.tmpTrn WHERE TrnSeq = @PC2_TrnSeq;
                            SET @PC2_Qty = @PC2_Qty + COALESCE(@PC2_LastQty, 0);
                        END
                    END
                END

                DECLARE @PC2_TrnQty NUMERIC(18,2);
                SET @PC2_TrnQty = @PC2_Qty;
                IF @PC2_Qty = 0 SET @PC2_Qty = 1;

                EXEC dbo.PostITrn @CheckNo, @PC2_TrnSeq, @Cashier, @CompCode, @PC2_TrnDesc, @CompPrice, @PC2_Qty, @CB_OrderNo, '', 0, @PC2_TrnQty, @SeatNo, '', 0, @PC2_SubMarker;
                SET @CB_FirstTrnSeq = ISNULL(@CB_FirstTrnSeq, @PC2_TrnSeq); -- OW1d: status row

                DECLARE @PC2_IsPkg BIT = NULL;
                DECLARE @PC2_PkgRVC INT = @RVCNo;
                SELECT @PC2_IsPkg = COALESCE(IsPkg,0) FROM dbo.ItemDef WHERE ICode = @CompCode;
                IF @PC2_IsPkg = 1
                BEGIN
                    SELECT @PC2_PkgRVC = RVC FROM dbo.tmpInvoice WHERE CheckNo = @CheckNo;
                    DECLARE @PC2_pICode VARCHAR(30);
                    DECLARE @PC2_pQty NUMERIC(18,2);
                    DECLARE @PC2_pPrice NUMERIC(18,2);
                    DECLARE @PC2_pTrnSeq BIGINT;
                    DECLARE @PC2_TmpNum BIGINT = NULL;
                    DECLARE @PC2_HasTrn INT = 0;
                    DECLARE PC2Cur CURSOR FOR SELECT P.ItemCode, P.iQty, (CASE WHEN P.UseItemPrice = 1 THEN I.Price ELSE P.IPrice END)/COALESCE(C.DecDiv,1)
                        FROM dbo.PkgDef P JOIN dbo.ItemDef I ON P.ItemCode = I.ICode
                        LEFT JOIN dbo.CurrencyDef C ON I.ICurrency = C.CrCode
                        WHERE PkgCode = @CompCode ORDER BY P.DisplayOrder;
                    OPEN PC2Cur;
                    FETCH NEXT FROM PC2Cur INTO @PC2_pICode, @PC2_pQty, @PC2_pPrice;
                    WHILE @@FETCH_STATUS = 0
                    BEGIN
                        SET @PC2_HasTrn = 1;
                        IF @PC2_pQty = 0 SET @PC2_pQty = 1;
                        SET @PC2_pQty = @PC2_pQty * @PC2_Qty;
                        DELETE FROM @PC2_SeqOut;
                        INSERT INTO @PC2_SeqOut (v) EXEC dbo.GetRVCSeqNumber 'LastTrn', @PC2_PkgRVC, @Cashier, '';
                        SELECT @PC2_TmpNum = v FROM @PC2_SeqOut;
                        SET @PC2_pTrnSeq = @PC2_PkgRVC * 100000000 + @PC2_TmpNum;
                        EXEC dbo.PostITrn @CheckNo, @PC2_pTrnSeq, @Cashier, @PC2_pICode, '', @PC2_pPrice, @PC2_pQty, @CB_OrderNo, @PC2_TrnSeq, 0;
                        FETCH NEXT FROM PC2Cur INTO @PC2_pICode, @PC2_pQty, @PC2_pPrice;
                    END
                    CLOSE PC2Cur;
                    DEALLOCATE PC2Cur;
                    IF @PC2_HasTrn > 0
                    BEGIN
                        DELETE FROM @PC2_SeqOut;
                        INSERT INTO @PC2_SeqOut (v) EXEC dbo.GetRVCSeqNumber 'LastTrn', @PC2_PkgRVC, @Cashier, '';
                        SELECT @PC2_TmpNum = v FROM @PC2_SeqOut;
                        SET @PC2_pTrnSeq = @PC2_PkgRVC * 100000000 + @PC2_TmpNum;
                        SET @PC2_pQty = 0;
                        EXEC dbo.PostITrn @CheckNo, @PC2_pTrnSeq, @Cashier, @CompCode, @PC2_TrnDesc, @CompPrice, @PC2_pQty, @CB_OrderNo, @PC2_TrnSeq, 1;
                    END
                END

                DECLARE @PC2_OrderByMenu VARCHAR(10) = NULL;
                SELECT @PC2_OrderByMenu = ParaStr FROM dbo.Para WITH (NOLOCK) WHERE ParaName = 'OrderPrintByMenu' AND RVC = @RVCNo;
                IF COALESCE(@PC2_OrderByMenu,'') NOT IN ('0','')
                BEGIN
                    -- Same printer-routing rule as SqlPostItem; see the note there.
                    DECLARE @PC2_MenuNo VARCHAR(30) = NULL;

                    IF COALESCE(@PC2_LastMenuNo,'') <> ''
                       AND EXISTS (SELECT 1 FROM dbo.ItemMenu WITH (NOLOCK) WHERE MenuNo = @PC2_LastMenuNo AND MenuActive = 1 AND COALESCE(PNo,0) <> 0 AND RVC = @RVCNo)
                        SET @PC2_MenuNo = @PC2_LastMenuNo;

                    IF @PC2_MenuNo IS NULL
                        SELECT TOP 1 @PC2_MenuNo = m.ItemMenu
                        FROM dbo.RVCItemMenu m WITH (NOLOCK)
                        JOIN dbo.ItemMenu im WITH (NOLOCK) ON im.MenuNo = m.ItemMenu AND im.RVC = m.RVC
                        WHERE m.RVC = @RVCNo AND RTRIM(m.ItemCode) = RTRIM(@CompCode)
                          AND im.MenuActive = 1 AND COALESCE(im.PNo,0) <> 0
                        ORDER BY im.MenuOrder, m.ItemMenu;

                    IF COALESCE(@PC2_MenuNo,'') <> ''
                    -- // lint:allow-tracked-write: UPDATE tmpTrn — the open-bill line staging table. Inlined body of the legacy POS proc this call replaces (schema pda dropped); parity-tested against the original. Cooperative write on the SMILE money path.
                    UPDATE dbo.tmpTrn SET Rsv2 = @PC2_MenuNo WHERE TrnSeq = @PC2_TrnSeq AND CheckNo = @CheckNo AND COALESCE(Rsv2,'') = '';
                END

                EXEC dbo.UpdateTmpInvoice @CheckNo;

                IF ISNULL(@Note,'') <> ''
                BEGIN
                    BEGIN TRY
                        INSERT INTO dbo.CGTrn ([TrnNo],[ItemCode],[CGCode],[CGName])
                        VALUES (@PC2_TrnSeq, @CompCode, 0, dbo.Unicode2TCVN(@Note));
                    END TRY
                    BEGIN CATCH
                        -- Best-effort note write; a failure here must not fail the item post.
                        -- (P6b: pda.Pda_ErrorLog telemetry retired — the note is non-critical.)
                        SET @PC2_TrnSeq = @PC2_TrnSeq;
                    END CATCH
                END
            END
        END

        FETCH NEXT FROM cur INTO @CompCode, @CompQty, @CompPrice;
    END;

    CLOSE cur;
    DEALLOCATE cur;
END;

DROP TABLE #ComboComponents;

-- OW1d: the status row the executor reads (the legacy batch returned nothing). TrnSeq: the first line posted.
SELECT ISNULL(@CB_IsOut,0) AS IsOut, @CB_OutResult AS MessageResult, ISNULL(@CB_FirstTrnSeq,0) AS TrnSeq;"#;

// ---- OrderCommandSql.BatchesOw1e.cs

/// C# `OrderCommandSql.TableInfo`, verbatim.
pub const TABLE_INFO: &str = r#"SET NOCOUNT ON;
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
        WHERE TableNum = @TableNo AND RVC = @RVCNo;"#;

/// C# `OrderCommandSql.TransferHeader`, verbatim.
pub const TRANSFER_HEADER: &str =
    r#"UPDATE dbo.TmpInvoice SET TableNo = @ToTableNo WHERE CheckNo = @CheckNo AND RVC = @RVCNo"#;

/// C# `OrderCommandSql.TransferLineInfo`, verbatim.
pub const TRANSFER_LINE_INFO: &str =
    r#"UPDATE dbo.tmpTrnInfo SET TableNo = @ToTableNo WHERE CheckNo = @CheckNo"#;

/// C# `OrderCommandSql.LockCheck`, verbatim.
pub const LOCK_CHECK: &str = r#"SELECT CheckNo, Status FROM dbo.TmpInvoice WITH (UPDLOCK, HOLDLOCK) WHERE CheckNo = @CheckNo AND RVC = @RVCNo"#;

/// C# `OrderCommandSql.MergeLines`, verbatim.
pub const MERGE_LINES: &str =
    r#"UPDATE dbo.TmpTrn SET CheckNo = @Target WHERE CheckNo = @Source AND RVC = @RVCNo"#;

/// C# `OrderCommandSql.MergeLineInfo`, verbatim.
pub const MERGE_LINE_INFO: &str =
    r#"UPDATE dbo.tmpTrnInfo SET CheckNo = @Target WHERE CheckNo = @Source"#;

/// C# `OrderCommandSql.CancelOrder`, verbatim.
pub const CANCEL_ORDER: &str = r#"SET NOCOUNT ON;
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
    WHERE CheckNo = @CheckNo AND RVC = @RVCNo;"#;

/// C# `OrderCommandSql.ReCalcBill`, verbatim.
pub const RE_CALC_BILL: &str = r#"SET NOCOUNT ON;
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
    SELECT 0 AS RecalcFailed, 0 AS ErrNo, CAST('' AS NVARCHAR(400)) AS ErrMsg;"#;

/// C# `OrderCommandSql.MoveSourceLines`, verbatim.
pub const MOVE_SOURCE_LINES: &str = r#"SELECT TrnSeq FROM dbo.TmpTrn WITH (UPDLOCK, HOLDLOCK) WHERE CheckNo = @CheckNo AND RVC = @RVCNo AND TrnCode = 100"#;

/// C# `OrderCommandSql.MoveLine`, verbatim.
pub const MOVE_LINE: &str = r#"UPDATE dbo.TmpTrn SET CheckNo = @ToCheckNo WHERE TrnSeq = @TrnSeq AND CheckNo = @FromCheckNo AND RVC = @RVCNo"#;

/// C# `OrderCommandSql.MoveLineInfo`, verbatim.
pub const MOVE_LINE_INFO: &str = r#"UPDATE dbo.tmpTrnInfo SET CheckNo = @ToCheckNo WHERE TrnNo = @TrnNo AND CheckNo = @FromCheckNo"#;

/// C# `OrderCommandSql.LogTrnMove`, verbatim.
pub const LOG_TRN_MOVE: &str = r#"SET NOCOUNT ON;
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
    WHERE t.CheckNo = @ToCheckNo AND t.RVC = @RVC;"#;
