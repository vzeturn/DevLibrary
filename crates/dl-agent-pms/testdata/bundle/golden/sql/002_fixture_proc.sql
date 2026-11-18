/* A procedure body runs at call time, so its DELETE and its own
   SET LOCK_TIMEOUT are not migration-time work. DROP TABLE dbo.X in a comment is ignored. */
SET LOCK_TIMEOUT 10000;
GO
CREATE OR ALTER PROCEDURE dbo.MM_BundleFixture_Purge
AS
BEGIN
  SET LOCK_TIMEOUT 30000;
  DELETE FROM dbo.MM_BundleFixture WHERE Id < 0;
END
GO
