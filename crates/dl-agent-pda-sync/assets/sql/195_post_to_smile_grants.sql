-- order.check_close and order.room_charge post to the Front Office folio like POS WIN (ADR 0179, RT-1297). Acts in the Front
-- Office database named by dbo.Para 'InterfaceDB' (default SMILE_FO; first RVC row, name checked against ^[A-Za-z0-9_]+$ and
-- bracketed), run from the POS database like the rest. [pda_sync] there gets a user (when it has none) and exactly: SELECT on
-- Folio (the in-house, postable folio check), SELECT on FolioBalanceCode (the balance code of the room-charge line) and EXECUTE
-- on PostInterfaceTransactionToFolio (the vendor procedure POS WIN posts through; the agent never writes a posting row itself).
-- Self-contained, so it does not depend on 160 or 190; no schema change, no vendor object is altered (docs/internal/pos-compat.md);
-- idempotent (GRANT is repeatable). Without a usable Front Office database, table or procedure it grants nothing for that; the
-- close or room charge is then refused with folio_post_required, never the bootstrap.
DECLARE @fo sysname = (SELECT TOP 1 LTRIM(RTRIM(ParaStr)) FROM dbo.Para WHERE ParaName = N'InterfaceDB' AND ParaStr IS NOT NULL AND LTRIM(RTRIM(ParaStr)) <> N'' ORDER BY RVC);
IF @fo IS NULL SET @fo = N'SMILE_FO';
IF @fo NOT LIKE N'%[^A-Za-z0-9_]%' AND DB_ID(@fo) IS NOT NULL AND DB_ID(@fo) <> DB_ID()
BEGIN
    DECLARE @sql nvarchar(max) = N'USE ' + QUOTENAME(@fo) + N';
        IF NOT EXISTS (SELECT 1 FROM sys.database_principals WHERE name = N''pda_sync'')
            CREATE USER [pda_sync] FOR LOGIN [pda_sync];
        IF OBJECT_ID(N''dbo.Folio'', N''U'') IS NOT NULL
            GRANT SELECT ON OBJECT::dbo.Folio TO [pda_sync];
        IF OBJECT_ID(N''dbo.FolioBalanceCode'', N''U'') IS NOT NULL
            GRANT SELECT ON OBJECT::dbo.FolioBalanceCode TO [pda_sync];
        IF OBJECT_ID(N''dbo.PostInterfaceTransactionToFolio'', N''P'') IS NOT NULL
            GRANT EXECUTE ON OBJECT::dbo.PostInterfaceTransactionToFolio TO [pda_sync];';
    EXECUTE sys.sp_executesql @sql;
END
