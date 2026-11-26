-- Money verbs (ADR 0174, RT-1297): order.tender_add / tender_remove / check_close on a site-authoritative tenant beside POS WIN.
-- The batches are the constants of Commands/OrderCommandSql.BatchesMoney.cs, ported from the legacy PDA payment and close SQL;
-- these are exactly the table writes they make outside the vendor procedures (GetRVCSeqNumber, UpdateTmpInvoice and Unicode2TCVN
-- are granted by 070_order_grants.sql). No schema change and no vendor object is altered (docs/internal/pos-compat.md);
-- idempotent (GRANT is repeatable). A table a SMILE_POS build lacks is skipped (the batches skip it too, OBJECT_ID), so every
-- grant is dynamic and guarded.
-- A column grant is one row per column (RT-1297): a column a build lacks is skipped alone instead of failing the
-- GRANT of its siblings.
DECLARE @grants TABLE (obj sysname, perm nvarchar(10), col sysname NULL);
INSERT INTO @grants (obj, perm, col) VALUES
    -- tender_add / tender_remove: the tender line, its payment row, the header payment columns and the bill number.
    (N'tmpTrn',         N'INSERT', NULL),
    (N'tmpTrn',         N'DELETE', NULL),
    (N'tmpPayment',     N'INSERT', NULL),
    (N'tmpPayment',     N'DELETE', NULL),
    (N'tmpInvoice',     N'UPDATE', N'PaymentMode'),
    (N'tmpInvoice',     N'UPDATE', N'PaymentType'),
    (N'tmpInvoice',     N'UPDATE', N'PaymentInfo'),
    (N'tmpTaxTable',    N'INSERT', NULL),
    (N'tmpTaxTable',    N'UPDATE', N'BillNo'),
    (N'tmpTaxTable',    N'UPDATE', N'Sub2'),
    -- check_close: the permanent rows, the loyalty award, then the tmp rows and the open log.
    (N'Trn',            N'INSERT', NULL),
    (N'Invoice',        N'INSERT', NULL),
    (N'Payment',        N'INSERT', NULL),
    (N'ARInv',          N'INSERT', NULL),
    (N'CCInv',          N'INSERT', NULL),
    (N'TaxTable',       N'INSERT', NULL),
    (N'MemberPoint',    N'INSERT', NULL),
    (N'tmpInvoice',     N'DELETE', NULL),
    (N'tmpInvoiceInfo', N'DELETE', NULL),
    (N'tmpTrnInfo',     N'DELETE', NULL),
    (N'tmpTaxTable',    N'DELETE', NULL),
    (N'tmpARInv',       N'DELETE', NULL),
    (N'tmpCCInv',       N'DELETE', NULL),
    (N'checkopenlog',   N'UPDATE', N'isOpen'),
    (N'checkopenlog',   N'UPDATE', N'CloseTime'),
    (N'checkopenlog',   N'UPDATE', N'LastAction');

DECLARE @obj sysname, @perm nvarchar(10), @col sysname, @sql nvarchar(max);
DECLARE g CURSOR LOCAL FAST_FORWARD FOR SELECT obj, perm, col FROM @grants;
OPEN g;
FETCH NEXT FROM g INTO @obj, @perm, @col;
WHILE @@FETCH_STATUS = 0
BEGIN
    IF OBJECT_ID(N'dbo.' + QUOTENAME(@obj), N'U') IS NOT NULL
       AND (@col IS NULL OR COL_LENGTH(N'dbo.' + QUOTENAME(@obj), @col) IS NOT NULL)
    BEGIN
        -- A column a build lacks fails only its own grant (logged), never the bootstrap; the command is then refused
        -- with SQL_GRANTS_MISSING like the other verbs.
        SET @sql = N'GRANT ' + @perm + N' ON OBJECT::dbo.' + QUOTENAME(@obj)
            + CASE WHEN @col IS NULL THEN N'' ELSE N' (' + QUOTENAME(@col) + N')' END + N' TO [pda_sync];';
        BEGIN TRY
            EXECUTE sys.sp_executesql @sql;
        END TRY
        BEGIN CATCH
            PRINT N'money grant skipped on dbo.' + @obj + N': ' + ERROR_MESSAGE();
        END CATCH;
    END;
    FETCH NEXT FROM g INTO @obj, @perm, @col;
END;
CLOSE g;
DEALLOCATE g;
