-- sp_ListTrnTypes returns the DISTINCT dbo.TransactionCode.TransactionType
-- values this SMILE install uses, each with a representative human label. It is
-- the catalog the agent publishes to the platform (POST /api/v1/agent/trn-types)
-- so an operator can tick which TransactionType a folio-merge rule applies to —
-- the merge engine never hard-codes which code is "room revenue" because each
-- property classifies revenue differently.
--
-- 2 columns, scan order in ListTrnTypes: TrnType, Description. Description is the
-- MIN(Description) among the codes sharing a TransactionType — a stable label,
-- not authoritative (the operator sees the raw TransactionType too). Blank
-- TransactionType rows are excluded; the platform also drops blanks defensively.
-- The SAME collect script set installs into BOTH SMILE_FO and SMILE_POS. Only
-- SMILE_FO's TransactionCode carries TransactionType; the POS variant lacks that
-- column, so a static reference would fail to CREATE there. Guard the whole body
-- behind a COL_LENGTH probe and run the query from dynamic SQL — on POS the SP
-- installs cleanly and simply returns an empty catalog.
CREATE OR ALTER PROCEDURE dbo.sp_ListTrnTypes AS
BEGIN
    SET NOCOUNT ON;
    IF COL_LENGTH('dbo.TransactionCode', 'TransactionType') IS NULL
    BEGIN
        SELECT TrnType = CAST(N'' AS nvarchar(50)), Description = CAST(N'' AS nvarchar(200))
        WHERE 1 = 0;
        RETURN;
    END
    EXEC sp_executesql N'
        SELECT
            TrnType     = RTRIM(tc.TransactionType),
            Description = ISNULL(MIN(RTRIM(tc.Description)), '''')
        FROM dbo.TransactionCode tc
        WHERE tc.TransactionType IS NOT NULL
          AND RTRIM(tc.TransactionType) <> ''''
        GROUP BY RTRIM(tc.TransactionType)
        ORDER BY TrnType;';
END
