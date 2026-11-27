-- SA_ shared SMILE read layer, slice 1 (RT-1432): the ONE definition of how a
-- (TransactionCode, TransactionSubCode) pair is matched against dbo.TransactionCode.
-- Installed and self-repaired by dl-agent; no project keeps its own copy of this join.
-- Works on every SMILE_FO schema variant (only Code/Sub/Description are required) and
-- on compatibility level 100 (no 2012+ conversion functions).
--
--   dbo.SA_TrnKey(@v)  normalised key of a code or sub code:
--       NULL / blank / '0' / '00' / ' 000 '  -> '0'
--       digits only (<= 18)                  -> leading zeros stripped ('007' -> '7')
--       anything else                        -> UPPER(LTRIM(RTRIM(v)))
--     so int, decimal and varchar columns, padded or not, compare equal.
--
--   dbo.SA_TrnCode_Resolve(@Code, @Sub, @FallbackSub0, @FallbackCode)
--     returns at most ONE row: TransactionCode, TransactionSubCode (as stored),
--     Description, MatchRank (0 exact sub, 1 fell back to sub 0, 2 fell back to code only).
--     Several matching rows -> non-empty Description first, then lowest sub code.
--     @FallbackSub0 = 1: no exact sub -> the code's sub 0 row.
--     @FallbackCode = 1: still nothing -> the code's lowest sub row.
--     No match -> zero rows (use OUTER APPLY).
IF OBJECT_ID('dbo.SA_TrnCode_Resolve') IS NOT NULL DROP FUNCTION dbo.SA_TrnCode_Resolve;
GO
IF OBJECT_ID('dbo.SA_TrnKey') IS NOT NULL DROP FUNCTION dbo.SA_TrnKey;
GO
CREATE FUNCTION dbo.SA_TrnKey(@v nvarchar(40))
RETURNS TABLE AS RETURN
SELECT K = CASE
    WHEN t.s = N'' THEN CAST('0' AS varchar(40))
    WHEN t.s NOT LIKE N'%[^0-9]%' AND LEN(t.s) <= 18 THEN CONVERT(varchar(40), CONVERT(bigint, t.s))
    ELSE CAST(UPPER(t.s) AS varchar(40)) END
FROM (SELECT s = LTRIM(RTRIM(ISNULL(@v, N'')))) t;
GO
CREATE FUNCTION dbo.SA_TrnCode_Resolve(@Code nvarchar(40), @Sub nvarchar(40), @FallbackSub0 bit, @FallbackCode bit)
RETURNS TABLE AS RETURN
SELECT TOP 1 r.TransactionCode, r.TransactionSubCode, r.Description, r.MatchRank
FROM (
    SELECT tc.TransactionCode, tc.TransactionSubCode, tc.Description,
           MatchRank = CASE WHEN ks.K = kq.K THEN 0 WHEN ks.K = '0' THEN 1 ELSE 2 END
    FROM dbo.TransactionCode tc
    CROSS APPLY dbo.SA_TrnKey(tc.TransactionCode) kc
    CROSS APPLY dbo.SA_TrnKey(tc.TransactionSubCode) ks
    CROSS APPLY dbo.SA_TrnKey(@Sub) kq
    CROSS APPLY dbo.SA_TrnKey(@Code) kw
    WHERE kc.K = kw.K
      AND (ks.K = kq.K
           OR (@FallbackSub0 = 1 AND ks.K = '0')
           OR @FallbackCode = 1)
) r
ORDER BY r.MatchRank,
         CASE WHEN LTRIM(RTRIM(ISNULL(r.Description, N''))) = N'' THEN 1 ELSE 0 END,
         r.TransactionSubCode;
GO
