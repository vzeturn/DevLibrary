-- Database level. Idempotent.
IF NOT EXISTS (SELECT 1 FROM sys.database_principals WHERE name = N'pda_sync')
    CREATE USER [pda_sync] FOR LOGIN [pda_sync];
