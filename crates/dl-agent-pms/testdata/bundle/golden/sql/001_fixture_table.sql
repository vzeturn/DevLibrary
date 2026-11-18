-- Fixture for the migration bundle tests (not a real migration).
SET LOCK_TIMEOUT 5000;
IF OBJECT_ID(N'dbo.MM_BundleFixture', N'U') IS NULL
  CREATE TABLE dbo.MM_BundleFixture (Id int NOT NULL PRIMARY KEY, Name nvarchar(50) NULL);
