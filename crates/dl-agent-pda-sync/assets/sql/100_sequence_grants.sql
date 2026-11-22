-- Per-RVC number objects of dbo.GetRVCSeqNumber (RT-0654): some SMILE POS builds keep the check and line counters of each
-- RVC in an object of its own, dbo.LastCheck<rvc> and dbo.LastTrn<rvc>, instead of dbo.Para, and the vendor procedure
-- reaches them through dynamic SQL. That breaks the ownership chain, so EXECUTE on the procedure (070) is not enough:
-- pda_sync gets error 229 on the counter and the open check is inserted without a check number. This script grants
-- pda_sync what each counter needs: UPDATE on a sequence (what NEXT VALUE FOR requires; SELECT does not exist for a
-- sequence), SELECT and UPDATE on a counter table. Only grants: no vendor object is created, altered or dropped (ADR 0100
-- allow-list, docs/internal/pos-compat.md). Idempotent (GRANT is repeatable): an RVC added later is covered by the next
-- run, and a build without these objects grants nothing. Names match exactly (the prefix and digits only), each is
-- quoted with QUOTENAME and the kind is read from sys.objects before anything is granted.
DECLARE @name sysname, @type char(2), @sql nvarchar(400);
DECLARE counters CURSOR LOCAL FAST_FORWARD FOR
    SELECT o.name, o.type FROM sys.objects o
    WHERE o.schema_id = SCHEMA_ID(N'dbo') AND o.type IN ('SO', 'U')
      AND ((o.name LIKE N'LastCheck[0-9]%' AND SUBSTRING(o.name, 10, 128) NOT LIKE N'%[^0-9]%')
        OR (o.name LIKE N'LastTrn[0-9]%' AND SUBSTRING(o.name, 8, 128) NOT LIKE N'%[^0-9]%'));
OPEN counters;
FETCH NEXT FROM counters INTO @name, @type;
WHILE @@FETCH_STATUS = 0
BEGIN
    SET @sql = CASE WHEN @type = 'SO' THEN N'GRANT UPDATE ON OBJECT::dbo.' ELSE N'GRANT SELECT, UPDATE ON OBJECT::dbo.' END
        + QUOTENAME(@name) + N' TO [pda_sync];';
    EXECUTE sys.sp_executesql @sql;
    FETCH NEXT FROM counters INTO @name, @type;
END
CLOSE counters;
DEALLOCATE counters;
