-- Agent bookkeeping (watermarks, bootstrap marker). Additive, agent-owned.
IF OBJECT_ID(N'dbo.Pda_SyncState', N'U') IS NULL
    CREATE TABLE dbo.Pda_SyncState (
        StateKey   NVARCHAR(100) NOT NULL CONSTRAINT PK_Pda_SyncState PRIMARY KEY,
        StateValue NVARCHAR(MAX) NULL,
        UpdatedUtc DATETIME2(3)  NOT NULL CONSTRAINT DF_Pda_SyncState_UpdatedUtc DEFAULT SYSUTCDATETIME()
    );
