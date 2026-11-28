-- txn.room_charge, txn.room_charge_void (ADR 0162, RT-0908): the cloud bill's room charge posted into the Front Office folio the
-- way POS WIN does. Unlike every other script this one acts in the Front Office database named by dbo.Para 'InterfaceDB' (default
-- SMILE_FO; first RVC row, name checked against ^[A-Za-z0-9_]+$ and bracketed), run from the POS database like the rest. In that
-- database [pda_sync] gets a user and exactly: SELECT on Folio and FolioTransaction (the in-house lookup and the active-row
-- guard), INSERT on FolioTransaction (the mirror rows of a reversal), UPDATE of FolioBalanceCode only (A -> V; SQL Server column
-- grant, so no amount or key column can be changed) and EXECUTE on PostInterfaceTransactionToFolio (the vendor procedure POS WIN
-- posts through; the agent never writes a posting row itself). No schema change, no vendor object is altered
-- (docs/internal/pos-compat.md); idempotent (GRANT is repeatable). Without a usable Front Office database, table or procedure it
-- grants nothing for that; the command is then refused with txn_folio_database_invalid, never the bootstrap.
DECLARE @fo sysname = (SELECT TOP 1 LTRIM(RTRIM(ParaStr)) FROM dbo.Para WHERE ParaName = N'InterfaceDB' AND ParaStr IS NOT NULL AND LTRIM(RTRIM(ParaStr)) <> N'' ORDER BY RVC);
IF @fo IS NULL SET @fo = N'SMILE_FO';
IF @fo NOT LIKE N'%[^A-Za-z0-9_]%' AND DB_ID(@fo) IS NOT NULL AND DB_ID(@fo) <> DB_ID()
BEGIN
    DECLARE @sql nvarchar(max) = N'USE ' + QUOTENAME(@fo) + N';
        IF NOT EXISTS (SELECT 1 FROM sys.database_principals WHERE name = N''pda_sync'')
            CREATE USER [pda_sync] FOR LOGIN [pda_sync];
        IF OBJECT_ID(N''dbo.Folio'', N''U'') IS NOT NULL
            GRANT SELECT ON OBJECT::dbo.Folio TO [pda_sync];
        IF OBJECT_ID(N''dbo.FolioTransaction'', N''U'') IS NOT NULL
        BEGIN
            GRANT SELECT, INSERT ON OBJECT::dbo.FolioTransaction TO [pda_sync];
            IF COL_LENGTH(N''dbo.FolioTransaction'', N''FolioBalanceCode'') IS NOT NULL
                GRANT UPDATE ON OBJECT::dbo.FolioTransaction (FolioBalanceCode) TO [pda_sync];
        END
        IF OBJECT_ID(N''dbo.PostInterfaceTransactionToFolio'', N''P'') IS NOT NULL
            GRANT EXECUTE ON OBJECT::dbo.PostInterfaceTransactionToFolio TO [pda_sync];';
    EXECUTE sys.sp_executesql @sql;
END
