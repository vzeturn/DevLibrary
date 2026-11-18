-- sp_ProbeFOSchema reports, for THIS SMILE_FO install, which columns/tables the
-- ĐỢT 2 (2026-07-25 plan) FO revenue predicates need before any of them are
-- wired into sp_FetchFO/01_source_hash. It changes nothing and reads no row
-- data beyond a COUNT — safe to run on every enroll/collect cycle so the Hub
-- log carries the real schema shape from a customer install the dev box
-- cannot see (no live SMILE available in dev).
--
-- Every optional column/table is probed via COL_LENGTH/OBJECT_ID and only
-- referenced from DYNAMIC SQL (sp_executesql) gated on that probe — so a
-- missing column/table never breaks this SP itself, whatever the schema
-- variant. All checks are gathered into local variables, then returned as a
-- single one-row result set.
--
-- Columns returned:
--   HasTransactionGroup       bit    -- dbo.TransactionCode.TransactionGroup exists
--   HasFolioBalanceCode       bit    -- dbo.FolioTransaction.FolioBalanceCode exists
--   HasIsHKPost               bit    -- dbo.FolioTransaction.isHKPost exists
--   HasTaxDef                 bit    -- dbo.TaxDef table exists
--   HasTaxDefRate              bit    -- dbo.TaxDef.TaxRate column exists
--   DistinctFolioBalanceCodes nvarchar(400) -- STRING_AGG of DISTINCT FolioBalanceCode seen, '' if column absent
--   Group1Count                int    -- COUNT(*) of dbo.TransactionCode rows with TransactionGroup=1, -1 if column absent
CREATE OR ALTER PROCEDURE dbo.sp_ProbeFOSchema AS
BEGIN
    SET NOCOUNT ON;

    DECLARE @HasTransactionGroup BIT = CASE WHEN COL_LENGTH('dbo.TransactionCode', 'TransactionGroup') IS NOT NULL THEN 1 ELSE 0 END;
    DECLARE @HasFolioBalanceCode BIT = CASE WHEN COL_LENGTH('dbo.FolioTransaction', 'FolioBalanceCode') IS NOT NULL THEN 1 ELSE 0 END;
    DECLARE @HasIsHKPost BIT = CASE WHEN COL_LENGTH('dbo.FolioTransaction', 'isHKPost') IS NOT NULL THEN 1 ELSE 0 END;
    DECLARE @HasTaxDef BIT = CASE WHEN OBJECT_ID('dbo.TaxDef') IS NOT NULL THEN 1 ELSE 0 END;
    DECLARE @HasTaxDefRate BIT = CASE WHEN COL_LENGTH('dbo.TaxDef', 'TaxRate') IS NOT NULL THEN 1 ELSE 0 END;

    DECLARE @DistinctFolioBalanceCodes NVARCHAR(400) = N'';
    IF @HasFolioBalanceCode = 1
    BEGIN
        -- STRING_AGG is SQL Server 2017+ only — unavailable on 2016 RTM/SP1/SP2
        -- customer installs. FOR XML PATH('') is the pre-2017 comma-join idiom
        -- (works back to SQL Server 2005); STUFF(...,1,1,'') strips the leading
        -- comma the same way STRING_AGG's separator placement would not add.
        DECLARE @sqlCodes NVARCHAR(MAX) = N'SELECT @out = ISNULL(STUFF((SELECT DISTINCT N'','' + CAST(RTRIM(FolioBalanceCode) AS nvarchar(50)) FROM dbo.FolioTransaction FOR XML PATH('''')), 1, 1, N''''), N'''')';
        EXEC sp_executesql @sqlCodes, N'@out NVARCHAR(400) OUTPUT', @out = @DistinctFolioBalanceCodes OUTPUT;
    END

    DECLARE @Group1Count INT = -1;
    IF @HasTransactionGroup = 1
    BEGIN
        DECLARE @sqlGroup1 NVARCHAR(MAX) = N'SELECT @out = COUNT(*) FROM dbo.TransactionCode WHERE TransactionGroup = 1';
        EXEC sp_executesql @sqlGroup1, N'@out INT OUTPUT', @out = @Group1Count OUTPUT;
    END

    SELECT
        HasTransactionGroup       = @HasTransactionGroup,
        HasFolioBalanceCode       = @HasFolioBalanceCode,
        HasIsHKPost               = @HasIsHKPost,
        HasTaxDef                 = @HasTaxDef,
        HasTaxDefRate             = @HasTaxDefRate,
        DistinctFolioBalanceCodes = @DistinctFolioBalanceCodes,
        Group1Count               = @Group1Count;
END
GO
