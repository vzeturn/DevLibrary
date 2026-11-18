/* ---------------------------------------------------------------------------
   MM_PMS — retention of the request-key log MM_OpLog (530), cloud SaaS S6.1.

   Window: 30 days by default (the caller passes it; never fewer than 7). A
   request key is idempotent only inside this window: a client retries a key
   for minutes or hours, never weeks, so a row older than the window is never
   replayed and only grows the table. A key reused after its row was purged
   runs again as a new write; that is the documented contract.

   Who calls it: the site agent (cloud mode), on its own schedule, outside any
   transaction, one batch per call until a call deletes fewer rows than the
   batch. Each batch commits on its own, so the delete holds few locks (below
   lock escalation at the default 1000) and the log of a busy site keeps
   taking writes between batches. SET LOCK_TIMEOUT keeps a batch from queueing
   behind a write that holds MM_OpLog rows: the call fails with 1222 and the
   next run retries.

   A concurrent MM_Op_GetLog on a key in the batch either reads the row before
   it goes (replay) or after (no row): both are valid outcomes for a key older
   than the window.

   THROW band 59520-59539 (OPS-OPLOG), contract with OP_LOG_THROW in
   packages/db/src/ops/op-log.ts:
       59521  a required argument is missing or out of range
       59524  must run outside a transaction (one committed batch per call)
   Never: dynamic SQL, vendor procs, connection strings.
   --------------------------------------------------------------------------- */
SET NOCOUNT ON;
SET LOCK_TIMEOUT 10000;
GO

IF NOT EXISTS (SELECT 1 FROM sys.indexes
                WHERE object_id = OBJECT_ID(N'dbo.MM_OpLog') AND name = N'IX_MM_OpLog_AtUtc')
    CREATE NONCLUSTERED INDEX IX_MM_OpLog_AtUtc ON dbo.MM_OpLog (AtUtc);
GO

/* MM_Op_DeleteExpiredLog — delete at most @BatchSize log rows older than @RetainDays days and
   return how many went (Deleted). The caller repeats while Deleted = @BatchSize. */
CREATE OR ALTER PROCEDURE dbo.MM_Op_DeleteExpiredLog
    @RetainDays int,
    @BatchSize  int
AS
BEGIN
    SET NOCOUNT ON;
    SET LOCK_TIMEOUT 5000;
    IF @@TRANCOUNT > 0
        THROW 59524, 'MM_Op_DeleteExpiredLog: must run outside a transaction', 1;
    IF @RetainDays IS NULL OR @RetainDays < 7 OR @RetainDays > 3650
       OR @BatchSize IS NULL OR @BatchSize < 1 OR @BatchSize > 4000
        THROW 59521, 'MM_Op_DeleteExpiredLog: @RetainDays (7..3650) and @BatchSize (1..4000) are required', 1;

    DECLARE @Cutoff datetime2(3) = DATEADD(day, -@RetainDays, SYSUTCDATETIME());
    DECLARE @Deleted int;

    DELETE TOP (@BatchSize) FROM dbo.MM_OpLog WHERE AtUtc < @Cutoff;
    SET @Deleted = @@ROWCOUNT;

    SELECT @Deleted AS Deleted;
END;
GO
