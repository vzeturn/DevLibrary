-- Shares number 003 with the file before it, like the shipped 389/405 groups.
SET LOCK_TIMEOUT 5000;
CREATE TABLE #scratch (Id int);
DROP TABLE IF EXISTS #scratch;
