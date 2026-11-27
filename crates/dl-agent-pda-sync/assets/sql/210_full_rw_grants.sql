-- RT-1425 (owner decision 2026-11-27, temporary): [pda_sync] gets full read/write on the store's SMILE databases: the POS
-- database this runs in, and SMILE_FO and SMILE_BO when the server has them. db_datareader + db_datawriter + EXECUTE at
-- database scope (every schema). Never db_owner or sysadmin; no schema change, no vendor object altered, no other login or
-- password touched; idempotent (role membership and GRANT are repeatable). Applied by the Rust agent (enroll, grants and the
-- service self-repair); the .NET agent has no step for it. The narrow per-verb scripts (050..200) stay: tightening again is
-- the revert of this one script (its step, its existence check and the grants probe's full read/write check).
DECLARE @db sysname, @proc nvarchar(300);
DECLARE @sql nvarchar(max) = N'IF DATABASE_PRINCIPAL_ID(N''pda_sync'') IS NULL CREATE USER [pda_sync] FOR LOGIN [pda_sync];
ALTER ROLE db_datareader ADD MEMBER [pda_sync];
ALTER ROLE db_datawriter ADD MEMBER [pda_sync];
GRANT EXECUTE TO [pda_sync];';
DECLARE @dbs TABLE (name sysname NOT NULL PRIMARY KEY);
INSERT INTO @dbs (name)
SELECT DISTINCT d.name FROM (VALUES (DB_NAME()), (N'SMILE_FO'), (N'SMILE_BO')) d(name) WHERE DB_ID(d.name) IS NOT NULL;
WHILE EXISTS (SELECT 1 FROM @dbs)
BEGIN
    SELECT TOP (1) @db = name FROM @dbs ORDER BY name;
    SET @proc = QUOTENAME(@db) + N'.sys.sp_executesql';
    EXECUTE @proc @sql;
    DELETE FROM @dbs WHERE name = @db;
END
