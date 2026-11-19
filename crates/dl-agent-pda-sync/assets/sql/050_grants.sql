-- Least privilege: read the POS database, write only the agent's own tables. Idempotent (GRANT/ALTER ROLE are repeatable).
IF NOT EXISTS (
    SELECT 1 FROM sys.database_role_members m
    JOIN sys.database_principals r ON r.principal_id = m.role_principal_id AND r.name = N'db_datareader'
    JOIN sys.database_principals u ON u.principal_id = m.member_principal_id AND u.name = N'pda_sync')
    ALTER ROLE [db_datareader] ADD MEMBER [pda_sync];
GRANT SELECT, INSERT, UPDATE ON OBJECT::dbo.Pda_SyncCommandLog TO [pda_sync];
GRANT SELECT, INSERT, UPDATE, DELETE ON OBJECT::dbo.Pda_SyncState TO [pda_sync];
GRANT VIEW DATABASE STATE TO [pda_sync];
