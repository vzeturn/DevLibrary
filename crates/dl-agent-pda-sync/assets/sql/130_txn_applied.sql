-- txn.snapshot (ADR 0159, RT-0849): the last version of each aggregate the agent wrote, its store hash and the command that
-- wrote it. A snapshot at or below the stored version changes nothing. Additive, agent-owned; the grant is in this script so
-- a store that already has the table (step skipped) also has the grant. Idempotent.
IF OBJECT_ID(N'dbo.Pda_TxnApplied', N'U') IS NULL
    CREATE TABLE dbo.Pda_TxnApplied (
        AggregateKey NVARCHAR(100)    NOT NULL CONSTRAINT PK_Pda_TxnApplied PRIMARY KEY,
        Version      BIGINT           NOT NULL,
        RowHash      CHAR(64)         NOT NULL,
        CommandId    UNIQUEIDENTIFIER NOT NULL,
        AtUtc        DATETIME2(3)     NOT NULL CONSTRAINT DF_Pda_TxnApplied_AtUtc DEFAULT SYSUTCDATETIME()
    );
GRANT SELECT, INSERT, UPDATE ON OBJECT::dbo.Pda_TxnApplied TO [pda_sync];
