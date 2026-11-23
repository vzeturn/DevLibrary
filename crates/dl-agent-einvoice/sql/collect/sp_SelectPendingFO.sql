-- @CollectFrom filters by issue date; NULL = no window. KEEP IN SYNC with AgentConfig.CollectFrom.
--
-- @Mode splits the cheap "never sent" selection from the costly edit detection
-- (RT-0784). Old agents pass only @Limit/@CollectFrom and get mode 0, the
-- original combined query, unchanged. New agents probe sys.parameters for
-- @Mode and call:
--   1 = never sent OK (no hash work)               -> one column ID
--   2 = sent, dated >= @ChangedFrom, source edited  -> one column ID
--   3 = sweep page: the next @Limit sent bills below key @After (BillID DESC),
--       each with Changed = 1 when its source was edited -> columns ID, Changed
-- Modes 2 and 3 copy their candidates into a table variable using only cheap
-- filters, then hash, so dbo.MMEI_SourceHash never runs over the whole table.
CREATE OR ALTER PROCEDURE dbo.sp_SelectPendingFO @Limit int = 50, @CollectFrom datetime = NULL,
    @Mode tinyint = 0, @ChangedFrom datetime = NULL, @After varchar(64) = NULL AS
BEGIN
    SET NOCOUNT ON;
    IF @Mode = 1
    BEGIN
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
          AND c.ID IS NULL
        ORDER BY b.BillID DESC;
        RETURN;
    END;

    IF @Mode IN (2, 3)
    BEGIN
        DECLARE @cand TABLE (BillID numeric(38, 0) NOT NULL, SourceHash varbinary(32) NOT NULL);
        DECLARE @AfterID numeric(38, 0);
        IF @Mode = 3 SET @AfterID = CONVERT(numeric(38, 0), @After);

        IF @Mode = 2
            INSERT @cand (BillID, SourceHash)
            SELECT b.BillID, c.SourceHash
            FROM dbo.BillInfor b
            JOIN dbo.MMEI_Collected c
                ON c.Source = 'FO'
               AND c.ID = CAST(b.BillID AS varchar(64))
               AND c.Ok = 1
               AND c.SourceHash IS NOT NULL
            WHERE b.Printed = 1
              AND (b.Cancel = 0 OR b.Cancel IS NULL)
              AND b.BillID IS NOT NULL
              AND (@CollectFrom IS NULL OR b.InvDate >= @CollectFrom)
              AND (@ChangedFrom IS NULL OR b.InvDate >= @ChangedFrom);
        ELSE
            INSERT @cand (BillID, SourceHash)
            SELECT TOP (@Limit) b.BillID, c.SourceHash
            FROM dbo.BillInfor b
            JOIN dbo.MMEI_Collected c
                ON c.Source = 'FO'
               AND c.ID = CAST(b.BillID AS varchar(64))
               AND c.Ok = 1
               AND c.SourceHash IS NOT NULL
            WHERE b.Printed = 1
              AND (b.Cancel = 0 OR b.Cancel IS NULL)
              AND b.BillID IS NOT NULL
              AND (@CollectFrom IS NULL OR b.InvDate >= @CollectFrom)
              AND (@AfterID IS NULL OR b.BillID < @AfterID)
            ORDER BY b.BillID DESC;

        IF @Mode = 2
            SELECT TOP (@Limit) ID = CAST(x.BillID AS varchar(64))
            FROM @cand x
            WHERE x.SourceHash <> dbo.MMEI_SourceHash('FO', CAST(x.BillID AS varchar(64)))
            ORDER BY x.BillID DESC;
        ELSE
            SELECT ID = CAST(x.BillID AS varchar(64)),
                   Changed = CAST(CASE WHEN x.SourceHash <> dbo.MMEI_SourceHash('FO', CAST(x.BillID AS varchar(64)))
                                       THEN 1 ELSE 0 END AS bit)
            FROM @cand x
            ORDER BY x.BillID DESC;
        RETURN;
    END;

    -- Mode 0 (old agents). Change-aware pending set: select a bill when it was
    -- never pushed OK, OR when its current source fingerprint differs from the
    -- one last pushed (i.e. the source data changed and the Hub should see the
    -- update). An already-issued invoice is handled Hub-side (a changed payload
    -- creates a new version; the issued one is never overwritten).
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
