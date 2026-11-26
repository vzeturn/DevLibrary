-- fo.inhouse_search (RT-1296, ADR 0172 §7): the spa desk finds in-house guests in the Front Office folios. This script acts in the
-- Front Office database named by dbo.Para 'InterfaceDB' (default SMILE_FO; first RVC row, name checked against ^[A-Za-z0-9_]+$ and
-- bracketed), run from the POS database like the rest. In that database [pda_sync] gets a user and exactly SELECT on dbo.Folio
-- (read only; the C# agent's 160 room-charge grants are not part of this). No schema change, no vendor object is altered
-- (docs/internal/pos-compat.md); idempotent. Without a usable Front Office database or Folio table it grants nothing; the search
-- is then refused with txn_folio_database_invalid, never the bootstrap.
DECLARE @fo sysname = (SELECT TOP 1 LTRIM(RTRIM(ParaStr)) FROM dbo.Para WHERE ParaName = N'InterfaceDB' AND ParaStr IS NOT NULL AND LTRIM(RTRIM(ParaStr)) <> N'' ORDER BY RVC);
IF @fo IS NULL SET @fo = N'SMILE_FO';
IF @fo NOT LIKE N'%[^A-Za-z0-9_]%' AND DB_ID(@fo) IS NOT NULL AND DB_ID(@fo) <> DB_ID()
BEGIN
    DECLARE @sql nvarchar(max) = N'USE ' + QUOTENAME(@fo) + N';
        IF OBJECT_ID(N''dbo.Folio'', N''U'') IS NOT NULL
        BEGIN
            IF NOT EXISTS (SELECT 1 FROM sys.database_principals WHERE name = N''pda_sync'')
                CREATE USER [pda_sync] FOR LOGIN [pda_sync];
            GRANT SELECT ON OBJECT::dbo.Folio TO [pda_sync];
        END';
    EXECUTE sys.sp_executesql @sql;
END
