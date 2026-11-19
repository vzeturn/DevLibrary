namespace MM.PdaOrder.SyncAgent.Commands;

// OW1d (ADR 0110): the store batch of the combo verb, ported from the legacy PDA order repository (SqlPostItemCombo, build
// 3.30.32) like the OW1b batches. Kept verbatim except where ADR 0110 says otherwise; each change is marked "OW1d": the
// component-loop variables reset per component (the legacy batch could delete another line of the check), the selection
// filter, the header recreate branches dropped, the empty TRY/CATCH dropped, item_not_found and a status row. The legacy
// text is pinned in the tests and posts the same rows as this batch where the legacy bug cannot fire. Every vendor object
// it writes is granted to pda_sync by Bootstrap/Sql/070_order_grants.sql. Do not edit it to taste: POS WIN parity is the
// contract.
internal static partial class OrderCommandSql
{
    /// <summary>
    /// Legacy <c>SqlPostItemCombo</c> with the ADR 0110 changes. Params: @RVCNo, @CheckNo, @Cashier, @ComboCode, @Qty, @SeatNo,
    /// @Price, @Note, @Selected, @SelectedCount. The last result set is IsOut (0 posted, 1 check not found, 2 item not found;
    /// refusals write nothing), MessageResult, TrnSeq (the first line posted).
    /// </summary>
    internal const string PostCombo = @"SET NOCOUNT ON;

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
SELECT ISNULL(@CB_IsOut,0) AS IsOut, @CB_OutResult AS MessageResult, ISNULL(@CB_FirstTrnSeq,0) AS TrnSeq;";
}
