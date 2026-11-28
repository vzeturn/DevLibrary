-- SA_ shared SMILE read layer, slice 2 (RT-1484): the ONE definition of a folio's bill lines.
-- A room charge posted from a restaurant check is shown as that check's items, and a minibar posting as its items,
-- each with name, unit price, quantity and the reference (check number for the restaurant, the posting's own
-- reference for the minibar). Every other line stays one line. Projects call the function; none keeps its own copy.
--
--   dbo.SA_FolioBill(@FolioNum, @BalanceCode)   inline table-valued function (a view that takes the folio)
--     one row per bill line: FolioNum, BalanceCode, TransactionID, LineSeq, Source, TransDate,
--     TransactionCode, TransactionSubCode, ItemCode, ItemName, Qty, UnitPrice, Amount, RefNo, CheckNo.
--     Source 'FO'      the folio line as SMILE_FO holds it (name is the folio's own text; the caller names it
--                      through SA_TrnCode_Resolve and defaults Qty / UnitPrice, as before).
--     Source 'POS'     an item of the restaurant check that the room charge paid (SMILE_POS dbo.Trn).
--     Source 'MINIBAR' an item of the minibar posting (SMILE_FO dbo.ItemPosted).
--     Source 'ADJ'     the difference between the room charge and its items (tax, service, discount, a part of the
--                      check paid another way), so the lines always add up to the folio amount.
--   The same C / V / P balance-code rule as the old bill procedure (RT-1348) lives here.
--   A check is expanded only when its items cost at least the room charge, so a number that merely looks like a
--   check number never replaces a line. SMILE_POS is found next to the FO database (SMILE_FO -> SMILE_POS,
--   CT_x_FO -> CT_x_POS); with no POS database, or no ItemPosted, the function still works with what exists.
--   Built by dynamic SQL because the POS database name differs per site; works on compatibility level 100.
IF OBJECT_ID('dbo.SA_FolioBill') IS NOT NULL DROP FUNCTION dbo.SA_FolioBill;
GO
-- A database without the folio table (a test fixture, a wrong database) just gets no bill function.
IF OBJECT_ID('dbo.FolioTransaction') IS NULL RETURN;
DECLARE @fo sysname = DB_NAME();
DECLARE @pos sysname = NULL;
IF @fo LIKE N'%[_]FO' AND DB_ID(STUFF(@fo, LEN(@fo) - 1, 2, N'POS')) IS NOT NULL SET @pos = STUFF(@fo, LEN(@fo) - 1, 2, N'POS');
ELSE IF DB_ID(N'SMILE_POS') IS NOT NULL SET @pos = N'SMILE_POS';
IF @pos IS NOT NULL AND (HAS_DBACCESS(@pos) <> 1 OR OBJECT_ID(QUOTENAME(@pos) + N'.dbo.Trn') IS NULL) SET @pos = NULL;
DECLARE @hasMb bit = CASE WHEN OBJECT_ID(N'dbo.ItemPosted') IS NOT NULL THEN 1 ELSE 0 END;

DECLARE @tpl nvarchar(max) = N'
CREATE FUNCTION dbo.SA_FolioBill(@FolioNum decimal(18,0), @BalanceCode varchar(20))
RETURNS TABLE AS RETURN
WITH ft AS (
    SELECT f.TransactionID, f.FolioNum, f.FolioBalanceCode, f.TransactionDate, f.TransactionCode, f.TransactionSubCode,
           f.RefNumber, f.Qty, f.UnitPrice, f.Description,
           Amt = ISNULL(f.TransactionAmount, 0),
           Ref = LTRIM(RTRIM(ISNULL(f.RefNumber, ~~))),
           Chk = CASE WHEN LTRIM(RTRIM(ISNULL(f.RefNumber, ~~))) NOT LIKE ~%[^0-9]%~
                       AND LTRIM(RTRIM(ISNULL(f.RefNumber, ~~))) <> ~~ AND LEN(LTRIM(RTRIM(f.RefNumber))) <= 15
                      THEN CONVERT(numeric(18,0), LTRIM(RTRIM(f.RefNumber))) END
    FROM dbo.FolioTransaction f WITH (NOLOCK)
    WHERE f.FolioNum = @FolioNum
      AND (ISNULL(@BalanceCode, ~~) = ~~ OR UPPER(LTRIM(RTRIM(f.FolioBalanceCode))) = UPPER(LTRIM(RTRIM(@BalanceCode))))
      AND UPPER(LTRIM(RTRIM(f.FolioBalanceCode))) NOT IN (~C~, ~V~, ~P~)
),
mb AS (
    {MBITEMS}
),
mbtot AS (SELECT FtId, Tot = SUM(Amount) FROM mb GROUP BY FtId),
ftc AS (SELECT Chk, FtTot = SUM(Amt), FirstId = MIN(TransactionID), FirstDate = MIN(TransactionDate)
        FROM ft WHERE Chk IS NOT NULL AND TransactionID NOT IN (SELECT FtId FROM mb) GROUP BY Chk),
posi AS (
    {POSITEMS}
),
pc AS (
    SELECT p.Chk, p.RVC, p.ItemTot, c.FtTot, c.FirstId, c.FirstDate,
           rn = ROW_NUMBER() OVER (PARTITION BY p.Chk ORDER BY ABS(p.ItemTot - c.FtTot), p.RVC)
    FROM (SELECT Chk, RVC, ItemTot = SUM(Amount) FROM posi GROUP BY Chk, RVC) p
    JOIN ftc c ON c.Chk = p.Chk
    WHERE c.FtTot > 0 AND p.ItemTot >= c.FtTot - 0.5
),
chosen AS (SELECT Chk, RVC, ItemTot, FtTot, FirstId, FirstDate FROM pc WHERE rn = 1)
SELECT FolioNum = CAST(f.FolioNum AS decimal(18,0)), BalanceCode = f.FolioBalanceCode, f.TransactionID,
       LineSeq = CAST(0 AS int), Source = CAST(~FO~ AS varchar(8)), TransDate = f.TransactionDate,
       f.TransactionCode, f.TransactionSubCode, ItemCode = CAST(NULL AS varchar(40)),
       ItemName = CAST(f.Description AS nvarchar(400)), Qty = CAST(f.Qty AS decimal(18,4)),
       UnitPrice = CAST(f.UnitPrice AS decimal(18,4)), Amount = CAST(f.Amt AS decimal(18,4)),
       RefNo = CAST(f.Ref AS varchar(30)), CheckNo = CAST(f.Chk AS numeric(18,0))
FROM ft f
WHERE f.TransactionID NOT IN (SELECT FtId FROM mb)
  AND NOT EXISTS (SELECT 1 FROM chosen ch WHERE ch.Chk = f.Chk)
UNION ALL
SELECT CAST(f.FolioNum AS decimal(18,0)), f.FolioBalanceCode, f.TransactionID,
       CAST(1 + ROW_NUMBER() OVER (PARTITION BY m.FtId ORDER BY m.Seq) AS int), CAST(~MINIBAR~ AS varchar(8)),
       f.TransactionDate, f.TransactionCode, f.TransactionSubCode, CAST(m.ItemCode AS varchar(40)),
       CAST(m.ItemName AS nvarchar(400)), CAST(ISNULL(NULLIF(m.Qty, 0), 1) AS decimal(18,4)),
       CAST(ISNULL(NULLIF(m.Price, 0), m.Amount / ISNULL(NULLIF(m.Qty, 0), 1)) AS decimal(18,4)),
       CAST(m.Amount AS decimal(18,4)), CAST(ISNULL(NULLIF(LTRIM(RTRIM(m.RefNo)), ~~), f.Ref) AS varchar(30)),
       CAST(f.Chk AS numeric(18,0))
FROM mb m JOIN ft f ON f.TransactionID = m.FtId
UNION ALL
SELECT CAST(f.FolioNum AS decimal(18,0)), f.FolioBalanceCode, f.TransactionID, CAST(1000000 AS int), CAST(~ADJ~ AS varchar(8)),
       f.TransactionDate, f.TransactionCode, f.TransactionSubCode, CAST(NULL AS varchar(40)),
       CAST(~Tax, service and other adjustment~ AS nvarchar(400)), CAST(1 AS decimal(18,4)),
       CAST(f.Amt - t.Tot AS decimal(18,4)), CAST(f.Amt - t.Tot AS decimal(18,4)), CAST(f.Ref AS varchar(30)),
       CAST(f.Chk AS numeric(18,0))
FROM ft f JOIN mbtot t ON t.FtId = f.TransactionID
WHERE ABS(f.Amt - t.Tot) > 0.005
UNION ALL
SELECT CAST(f.FolioNum AS decimal(18,0)), f.FolioBalanceCode, ch.FirstId,
       CAST(1 + ROW_NUMBER() OVER (PARTITION BY ch.Chk ORDER BY i.Seq) AS int), CAST(~POS~ AS varchar(8)),
       ch.FirstDate, f.TransactionCode, f.TransactionSubCode, CAST(i.ItemCode AS varchar(40)),
       CAST(i.ItemName AS nvarchar(400)), CAST(i.Qty AS decimal(18,4)), CAST(i.UnitPrice AS decimal(18,4)),
       CAST(i.Amount AS decimal(18,4)), CAST(ch.Chk AS varchar(30)), CAST(ch.Chk AS numeric(18,0))
FROM chosen ch
JOIN posi i ON i.Chk = ch.Chk AND i.RVC = ch.RVC
JOIN ft f ON f.TransactionID = ch.FirstId
UNION ALL
SELECT CAST(f.FolioNum AS decimal(18,0)), f.FolioBalanceCode, ch.FirstId, CAST(1000000 AS int), CAST(~ADJ~ AS varchar(8)),
       ch.FirstDate, f.TransactionCode, f.TransactionSubCode, CAST(NULL AS varchar(40)),
       CAST(~Tax, service and other adjustment~ AS nvarchar(400)), CAST(1 AS decimal(18,4)),
       CAST(ch.FtTot - ch.ItemTot AS decimal(18,4)), CAST(ch.FtTot - ch.ItemTot AS decimal(18,4)),
       CAST(ch.Chk AS varchar(30)), CAST(ch.Chk AS numeric(18,0))
FROM chosen ch JOIN ft f ON f.TransactionID = ch.FirstId
WHERE ABS(ch.FtTot - ch.ItemTot) > 0.005';

DECLARE @mbSql nvarchar(max) = CASE WHEN @hasMb = 1 THEN N'
    SELECT FtId = CAST(i.TrnID AS numeric(18,0)), Seq = i.nSeq, ItemCode = i.ICode, ItemName = i.IName, Qty = i.Qty,
           Price = i.Price, Amount = ISNULL(i.Amount, 0), RefNo = i.RefNumber, PostedTime = i.PostedTime
    FROM dbo.ItemPosted i WITH (NOLOCK)
    WHERE i.FolioNum = @FolioNum AND i.TrnID IN (SELECT TransactionID FROM ft)'
  ELSE N'
    SELECT FtId = CAST(NULL AS numeric(18,0)), Seq = CAST(NULL AS bigint), ItemCode = CAST(NULL AS varchar(40)),
           ItemName = CAST(NULL AS nvarchar(400)), Qty = CAST(NULL AS numeric(18,4)), Price = CAST(NULL AS numeric(18,4)),
           Amount = CAST(NULL AS numeric(18,4)), RefNo = CAST(NULL AS varchar(40)), PostedTime = CAST(NULL AS datetime)
    WHERE 1 = 0' END;
DECLARE @posSql nvarchar(max) = CASE WHEN @pos IS NOT NULL THEN N'
    SELECT Chk = t.CheckNo, RVC = CAST(t.RVC AS int), Seq = t.TrnSeq, ItemCode = LTRIM(RTRIM(t.ItemCode)),
           ItemName = LTRIM(RTRIM(t.TrnDesc)), Qty = ISNULL(NULLIF(t.TrnQty, 0), 1),
           UnitPrice = CASE WHEN ABS(ISNULL(t.ItemPrice, 0) * ISNULL(NULLIF(t.TrnQty, 0), 1) - ISNULL(t.BaseTTL, 0)) < 0.5
                            THEN t.ItemPrice ELSE ISNULL(t.BaseTTL, 0) / ISNULL(NULLIF(t.TrnQty, 0), 1) END,
           Amount = ISNULL(t.BaseTTL, 0)
    FROM ' + QUOTENAME(@pos) + N'.dbo.Trn t WITH (NOLOCK)
    WHERE t.CheckNo IN (SELECT Chk FROM ftc) AND t.TrnCode < 200 AND LTRIM(RTRIM(ISNULL(t.ItemCode, ~~))) <> ~~'
  ELSE N'
    SELECT Chk = CAST(NULL AS numeric(18,0)), RVC = CAST(NULL AS int), Seq = CAST(NULL AS numeric(18,0)),
           ItemCode = CAST(NULL AS varchar(40)), ItemName = CAST(NULL AS nvarchar(400)), Qty = CAST(NULL AS numeric(18,4)),
           UnitPrice = CAST(NULL AS numeric(18,4)), Amount = CAST(NULL AS numeric(18,4))
    WHERE 1 = 0' END;

DECLARE @full nvarchar(max) = REPLACE(REPLACE(REPLACE(@tpl, N'{MBITEMS}', @mbSql), N'{POSITEMS}', @posSql), N'~', N'''');
DECLARE @noPos nvarchar(max) = REPLACE(REPLACE(REPLACE(@tpl, N'{MBITEMS}', @mbSql), N'{POSITEMS}', N'
    SELECT Chk = CAST(NULL AS numeric(18,0)), RVC = CAST(NULL AS int), Seq = CAST(NULL AS numeric(18,0)),
           ItemCode = CAST(NULL AS varchar(40)), ItemName = CAST(NULL AS nvarchar(400)), Qty = CAST(NULL AS numeric(18,4)),
           UnitPrice = CAST(NULL AS numeric(18,4)), Amount = CAST(NULL AS numeric(18,4)) WHERE 1 = 0'), N'~', N'''');
BEGIN TRY
    EXEC (@full);
END TRY
BEGIN CATCH
    -- The POS database could not be read at create time: the bill still works, from the folio and the minibar only.
    IF OBJECT_ID('dbo.SA_FolioBill') IS NULL EXEC (@noPos);
END CATCH
GO
