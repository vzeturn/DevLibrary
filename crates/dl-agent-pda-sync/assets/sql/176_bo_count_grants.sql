-- txn.stock_count (ADR 0167, RT-1114): a cloud physical count is written into SMILE_BO the way the exe's count session does. Like
-- 175 this script acts in another database: every SMILE_BO named in dbo.Pda_TxnStockMap (170) plus the default SMILE_BO, each name
-- checked against ^[A-Za-z0-9_]+$ and bracketed, run from the POS database like the rest. It adds to what 175 already grants (the tmp
-- voucher tables, the vendor post, JType, ItemDef, DefRVCList, INVTrn ...). In each, [pda_sync] gets a user and exactly: SELECT, INSERT
-- on InvSession and InvSSDtl (the count session and its lines, read back for a repeated command), UPDATE of the InvStatus and VNo
-- columns of InvSession only (the session is marked updated, InvStatus 9), and SELECT on Inventory (the system quantity, VEnd of the
-- period), Department and IUnitDef (the session's department check and the unit snapshot of a line). No DELETE, no other column, no
-- schema change, no vendor object is altered (docs/internal/pos-compat.md); idempotent (GRANT is repeatable). A database or object
-- that is not there gets no grant; the command is then refused (txn_stock_database_invalid / txn_stock_ledger_missing).
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
            IF OBJECT_ID(N''dbo.InvSession'', N''U'') IS NOT NULL GRANT SELECT, INSERT ON OBJECT::dbo.InvSession TO [pda_sync];
            IF OBJECT_ID(N''dbo.InvSSDtl'', N''U'') IS NOT NULL GRANT SELECT, INSERT ON OBJECT::dbo.InvSSDtl TO [pda_sync];
            IF COL_LENGTH(N''dbo.InvSession'', N''InvStatus'') IS NOT NULL AND COL_LENGTH(N''dbo.InvSession'', N''VNo'') IS NOT NULL
                GRANT UPDATE ON OBJECT::dbo.InvSession (InvStatus, VNo) TO [pda_sync];
            IF OBJECT_ID(N''dbo.Inventory'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.Inventory TO [pda_sync];
            IF OBJECT_ID(N''dbo.Department'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.Department TO [pda_sync];
            IF OBJECT_ID(N''dbo.IUnitDef'', N''U'') IS NOT NULL GRANT SELECT ON OBJECT::dbo.IUnitDef TO [pda_sync];';
        EXECUTE sys.sp_executesql @sql;
    END
    FETCH NEXT FROM names INTO @bo;
END
CLOSE names;
DEALLOCATE names;
