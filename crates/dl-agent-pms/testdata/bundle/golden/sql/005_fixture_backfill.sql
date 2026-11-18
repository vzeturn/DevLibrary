-- Tên khách: a backfill rewrites existing rows, so the file needs a backup.
SET LOCK_TIMEOUT 5000;
UPDATE dbo.MM_BundleFixture SET Name = N'Khách' WHERE Name IS NULL;
