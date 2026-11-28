-- txn.stock_deplete, txn.stock_restore (ADR 0164, RT-1022): the bill's recipe stock is written into SMILE_BO the way the vendor
-- POS stock link does. Like 160 this script acts in another database: every SMILE_BO named in dbo.Pda_TxnStockMap (170) plus the
-- default SMILE_BO, each name checked against ^[A-Za-z0-9_]+$ and bracketed, run from the POS database like the rest. In each,
-- [pda_sync] gets a user and exactly: SELECT on GlobPara (INVCostMethod, INVPeriod, GLPeriod), JType (voucher type and counter)
-- plus UPDATE of its LastVoucher column only (the never-reset counter moves forward like the vendor stock link),
-- IVoucherType and INVTrn (the site profile read before the first voucher), INVVoucher (voucher numbering), ItemDef and DefRVCList
-- (item and store checks), StockInterface and GLCOA (why a voucher the vendor post refused stayed un-posted); SELECT, INSERT on tmpINVVoucher and tmpINVTrn (the un-posted voucher) and EXECUTE on INVUpdateGLAvg
-- (the vendor weighted-average post, which moves the voucher to INVVoucher / INVTrn itself: the agent never writes INVTrn). No
-- schema change, no vendor object is altered (docs/internal/pos-compat.md); idempotent (GRANT is repeatable). A database or
-- object that is not there gets no grant; the command is then refused (txn_stock_database_invalid / txn_stock_ledger_missing).
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
            IF OBJECT_ID(N''dbo.GlobPara'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.GlobPara TO [pda_sync];
            IF OBJECT_ID(N''dbo.JType'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.JType TO [pda_sync];
            IF COL_LENGTH(N''dbo.JType'', N''LastVoucher'') IS NOT NULL GRANT UPDATE ON OBJECT::dbo.JType (LastVoucher) TO [pda_sync];
            IF OBJECT_ID(N''dbo.IVoucherType'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.IVoucherType TO [pda_sync];
            IF OBJECT_ID(N''dbo.INVTrn'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.INVTrn TO [pda_sync];
            IF OBJECT_ID(N''dbo.INVVoucher'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.INVVoucher TO [pda_sync];
            IF OBJECT_ID(N''dbo.ItemDef'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.ItemDef TO [pda_sync];
            IF OBJECT_ID(N''dbo.DefRVCList'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.DefRVCList TO [pda_sync];
            IF OBJECT_ID(N''dbo.StockInterface'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.StockInterface TO [pda_sync];
            IF OBJECT_ID(N''dbo.GLCOA'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.GLCOA TO [pda_sync];
            IF OBJECT_ID(N''dbo.tmpINVVoucher'', N''U'') IS NOT NULL GRANT SELECT, INSERT ON OBJECT::dbo.tmpINVVoucher TO [pda_sync];
            IF OBJECT_ID(N''dbo.tmpINVTrn'', N''U'') IS NOT NULL GRANT SELECT, INSERT ON OBJECT::dbo.tmpINVTrn TO [pda_sync];
            IF OBJECT_ID(N''dbo.INVUpdateGLAvg'', N''P'') IS NOT NULL GRANT EXECUTE ON OBJECT::dbo.INVUpdateGLAvg TO [pda_sync];';
        EXECUTE sys.sp_executesql @sql;
    END
    FETCH NEXT FROM names INTO @bo;
END
CLOSE names;
DEALLOCATE names;
