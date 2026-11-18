-- @CollectFrom filters by issue date; NULL = no window. KEEP IN SYNC with AgentConfig.CollectFrom.
CREATE OR ALTER PROCEDURE dbo.sp_SelectPendingFO @Limit int = 50, @CollectFrom datetime = NULL AS
BEGIN
    SET NOCOUNT ON;
    -- Change-aware pending set: select a bill when it was never pushed OK, OR
    -- when its current source fingerprint differs from the one last pushed
    -- (i.e. the source data changed and the Hub should see the update). An
    -- already-issued invoice is handled Hub-side (a changed payload creates a
    -- new version; the issued one is never overwritten).
    -- PERF: same shape as sp_SelectPendingPOS — the hash re-reads every child
    -- row, so it may only run for bills that already carry an Ok mark; a plain
    -- NOT EXISTS evaluated it per candidate and grows linearly with the
    -- never-pushed backlog.
    SELECT TOP (@Limit) ID = CAST(b.BillID AS varchar(64))
    FROM dbo.BillInfor b
    LEFT JOIN dbo.MMEI_Collected c
        ON c.Source = 'FO'
       AND c.ID = CAST(b.BillID AS varchar(64))
       AND c.Ok = 1
       AND c.SourceHash IS NOT NULL
    WHERE b.Printed = 1
      AND (b.Cancel = 0 OR b.Cancel IS NULL)
      AND b.BillID IS NOT NULL
      AND (@CollectFrom IS NULL OR b.InvDate >= @CollectFrom)
      AND (c.ID IS NULL
           OR c.SourceHash <> dbo.MMEI_SourceHash('FO', CAST(b.BillID AS varchar(64))))
    ORDER BY b.BillID DESC;
END;
GO
