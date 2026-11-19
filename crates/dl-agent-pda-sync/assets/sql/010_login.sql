-- Server level. Idempotent. {{PASSWORD}} is a generated alphanumeric secret (never logged).
IF NOT EXISTS (SELECT 1 FROM sys.server_principals WHERE name = N'pda_sync')
    CREATE LOGIN [pda_sync] WITH PASSWORD = N'{{PASSWORD}}', CHECK_POLICY = OFF, CHECK_EXPIRATION = OFF, DEFAULT_DATABASE = [master];
ELSE
    -- Login exists but the agent lost its stored password (re-install): reset it to the new secret.
    ALTER LOGIN [pda_sync] WITH PASSWORD = N'{{PASSWORD}}';
