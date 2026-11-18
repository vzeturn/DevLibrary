SET LOCK_TIMEOUT 10000;
DECLARE @sql nvarchar(max) = N'CREATE INDEX IX_MM_BundleFixture_Name ON dbo.MM_BundleFixture (Name)' +
  CASE WHEN CAST(SERVERPROPERTY('EngineEdition') AS int) = 3 THEN N' WITH (ONLINE = ON)' ELSE N'' END;
IF NOT EXISTS (SELECT 1 FROM sys.indexes WHERE name = N'IX_MM_BundleFixture_Name')
  EXEC sys.sp_executesql @sql;
