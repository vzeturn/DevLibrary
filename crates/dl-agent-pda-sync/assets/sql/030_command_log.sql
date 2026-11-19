-- Exactly-once command log (spec section 5). Additive, agent-owned.
IF OBJECT_ID(N'dbo.Pda_SyncCommandLog', N'U') IS NULL
    CREATE TABLE dbo.Pda_SyncCommandLog (
        CommandId  UNIQUEIDENTIFIER NOT NULL CONSTRAINT PK_Pda_SyncCommandLog PRIMARY KEY,
        Status     NVARCHAR(16)     NOT NULL,
        ResultJson NVARCHAR(MAX)    NULL,
        AtUtc      DATETIME2(3)     NOT NULL CONSTRAINT DF_Pda_SyncCommandLog_AtUtc DEFAULT SYSUTCDATETIME()
    );
