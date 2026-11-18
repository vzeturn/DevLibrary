-- @CollectFrom filters by check date. NULL = no window. KEEP IN SYNC with AgentConfig.CollectFrom.
--
-- POS quirk (verified live on SMILE_POS, July 2026): dbo.TaxTable is a sparse
-- per-check VAT record whose money/bill-number columns (BillSeri, BillNo,
-- ClientVAT, Company, TSubTTL, TTaxTTL, TTotal, TStatus) are NULL for a check
-- until the property separately issues a VAT bill through its own flow — it
-- is NOT a reliable "is this check collectable" gate (VATBillInfor is even
-- sparser: zero rows this period). The real check list with real money lives
-- on dbo.Invoice, so the pending set is driven off dbo.Invoice directly and
-- keyed by CheckNo (not BillSeri/BillNo, which are frequently NULL).
CREATE OR ALTER PROCEDURE dbo.sp_SelectPendingPOS @Limit int = 50, @CollectFrom datetime = NULL AS
BEGIN
    SET NOCOUNT ON;
    -- Change-aware pending set: same contract as sp_SelectPendingFO. Select a
    -- check when it was never pushed OK, OR when its current source
    -- fingerprint differs from the one last pushed.
    --
    -- Status filter: InvStatus=0 AND Status=1 covers 100% of the verified
    -- July 2026 checks (169/169, all closed/finalized). Refund=0 excludes
    -- refund checks. KEEP IN SYNC with the property's void/cancel convention
    -- if this install ever produces other InvStatus/Status/Refund values.
    -- FO-dedup gate: a folio-charged check (FolioNum set) is invoiced by the FO
    -- side as part of the guest's folio bill — excluding it here prevents
    -- double-issuance. The source-hash covers FolioNum, so a later unlink
    -- (FolioNum cleared) re-admits the check to this pending set.
    -- PERF: dbo.MMEI_SourceHash re-reads every child row of a check, so it must
    -- run ONLY for checks that already have an Ok mark (change detection), never
    -- for the never-pushed majority — a plain NOT EXISTS with the hash inside
    -- the predicate let the optimizer evaluate it per candidate row and timed
    -- out live (>120s over ~5k pending checks, 2026-07-22). LEFT JOIN the mark
    -- first; the CASE only invokes the hash when a mark row exists.
    SELECT TOP (@Limit) ID = CAST(i.CheckNo AS varchar(64))
    FROM dbo.Invoice i
    LEFT JOIN dbo.MMEI_Collected c
        ON c.Source = 'POS'
       AND c.ID = CAST(i.CheckNo AS varchar(64))
       AND c.Ok = 1
       AND c.SourceHash IS NOT NULL
    WHERE i.InvStatus = 0
      AND i.Status = 1
      AND (i.Refund = 0 OR i.Refund IS NULL)
      AND i.CheckNo IS NOT NULL
      AND ISNULL(i.FolioNum, 0) = 0
      AND (@CollectFrom IS NULL OR ISNULL(i.CloseTime, i.OpenTime) >= @CollectFrom)
      AND (c.ID IS NULL
           OR c.SourceHash <> dbo.MMEI_SourceHash('POS', CAST(i.CheckNo AS varchar(64))))
    ORDER BY i.CheckNo DESC;
END;
GO
