-- txn.stock_receipt (ADR 0169, RT-1113): a cloud goods receipt without a PO is written into SMILE_BO the way the exe's receiving form does.
-- Like 175 and 176 this script acts in another database: every SMILE_BO named in dbo.Pda_TxnStockMap (170) plus the default SMILE_BO, each
-- name checked against ^[A-Za-z0-9_]+$ and bracketed, run from the POS database like the rest. It adds to what 175 already grants (GlobPara,
-- JType, IVoucherType, INVTrn, INVVoucher, ItemDef, DefRVCList, StockInterface, GLCOA, tmpINVVoucher). In each, [pda_sync] gets a user and
-- exactly: SELECT, INSERT on tmpReceivingHeader and tmpReceivingDetail (the un-posted receipt), SELECT on ReceivingHeader and ReceivingDetail
-- (the posted receipt, read back for a repeated command), CLIENT and APAccGroup (the vendor and its payable account), JVoucher and tmpJVoucher
-- (the receiving number is taken above every journal number of the month, the vendor post refuses a number a journal holds) and EXECUTE on
-- RCVUpdateGL (the vendor post: it moves the receipt to ReceivingHeader / ReceivingDetail, writes INVTrn 100 / 10 and Inventory itself, the agent
-- never writes a posted table). No DELETE, no UPDATE, no schema change, no vendor object is altered (docs/internal/pos-compat.md); idempotent
-- (GRANT is repeatable). A database or object that is not there gets no grant; the command is then refused (txn_stock_database_invalid /
-- txn_stock_ledger_missing).
DECLARE @names TABLE (Name sysname NOT NULL PRIMARY KEY);
INSERT INTO @names (Name) VALUES (N'SMILE_BO');
IF OBJECT_ID(N'dbo.Pda_TxnStockMap', N'U') IS NOT NULL
    INSERT INTO @names (Name) EXECUTE sys.sp_executesql N'SELECT DISTINCT LTRIM(RTRIM(BoDatabase)) FROM dbo.Pda_TxnStockMap
        WHERE BoDatabase IS NOT NULL AND LTRIM(RTRIM(BoDatabase)) NOT IN (N'''', N''SMILE_BO'')';
DECLARE @bo sysname, @sql nvarchar(max);
DECLARE names CURSOR LOCAL FAST_FORWARD FOR SELECT Name FROM @names;
OPEN names;
FETCH NEXT FROM names INTO @bo;
WHILE @@FETCH_STATUS = 0
BEGIN
    IF @bo NOT LIKE N'%[^A-Za-z0-9_]%' AND DB_ID(@bo) IS NOT NULL AND DB_ID(@bo) <> DB_ID()
    BEGIN
        SET @sql = N'USE ' + QUOTENAME(@bo) + N';
            IF NOT EXISTS (SELECT 1 FROM sys.database_principals WHERE name = N''pda_sync'')
                CREATE USER [pda_sync] FOR LOGIN [pda_sync];
            IF OBJECT_ID(N''dbo.tmpReceivingHeader'', N''U'') IS NOT NULL GRANT SELECT, INSERT ON OBJECT::dbo.tmpReceivingHeader TO [pda_sync];
            IF OBJECT_ID(N''dbo.tmpReceivingDetail'', N''U'') IS NOT NULL GRANT SELECT, INSERT ON OBJECT::dbo.tmpReceivingDetail TO [pda_sync];
            IF OBJECT_ID(N''dbo.ReceivingHeader'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.ReceivingHeader TO [pda_sync];
            IF OBJECT_ID(N''dbo.ReceivingDetail'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.ReceivingDetail TO [pda_sync];
            IF OBJECT_ID(N''dbo.CLIENT'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.CLIENT TO [pda_sync];
            IF OBJECT_ID(N''dbo.APAccGroup'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.APAccGroup TO [pda_sync];
            IF OBJECT_ID(N''dbo.JVoucher'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.JVoucher TO [pda_sync];
            IF OBJECT_ID(N''dbo.tmpJVoucher'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.tmpJVoucher TO [pda_sync];
            IF OBJECT_ID(N''dbo.RCVUpdateGL'', N''P'') IS NOT NULL GRANT EXECUTE ON OBJECT::dbo.RCVUpdateGL TO [pda_sync];';
        EXECUTE sys.sp_executesql @sql;
    END
    FETCH NEXT FROM names INTO @bo;
END
CLOSE names;
DEALLOCATE names;
