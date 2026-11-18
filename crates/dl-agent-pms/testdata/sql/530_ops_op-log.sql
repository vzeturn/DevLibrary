/* ---------------------------------------------------------------------------
   MM_PMS — request-key idempotency log of the OperationRunner seam
   (cloud SaaS spec 2026-11-04 §6, plan Sprint 1 S1.2).

   A write op run with a request key records one MM_OpLog row in the SAME
   transaction as the write, so the row exists exactly when the write
   committed. A second run with the same key reads the row and returns the
   stored result without executing again (InProcessRunner today, the on-prem
   agent later: "consult the idempotency log first", spec §5).

   Columns: RequestKey (PK), Op, Status, ResultHash, AtUtc as the spec names
   them, plus ResultJson: replay must return the stored result and a hash
   cannot be turned back into one. ResultHash is the sha256 (hex) of
   ResultJson, checked on replay so a row edited by hand is refused rather
   than served.

   Concurrency: MM_Op_GetLog takes an exclusive applock on the key, owned by
   the transaction, before it reads. A concurrent run of the same key waits
   for the first to commit and then sees its row; different keys never wait
   on each other (a key-range lock on the PK would make two new keys in the
   same gap deadlock on insert). MM_Op_CreateLog refuses unless the caller's
   transaction holds that applock.

   Only repo-owned objects: no vendor table, synonym or session context.

   THROW band 59520-59539 (OPS-OPLOG), a contract with OP_LOG_THROW in
   packages/db/src/ops/op-log.ts: never renumber.
       59520  must run inside the caller's write transaction
       59521  a required argument is missing or out of range
       59522  the request key is not locked by this transaction
              (MM_Op_GetLog first)
       59523  the request key was already applied
   50000 (shared): the applock could not be taken.
   Never: dynamic SQL, vendor procs, Clerk SecurityCode, EmailConfig,
   VendorClerk, GlobPara, connection strings.
   --------------------------------------------------------------------------- */
SET NOCOUNT ON;
SET LOCK_TIMEOUT 10000;
GO

IF OBJECT_ID(N'dbo.MM_OpLog', N'U') IS NULL
BEGIN
    CREATE TABLE dbo.MM_OpLog (
        RequestKey varchar(100)  NOT NULL CONSTRAINT PK_MM_OpLog PRIMARY KEY CLUSTERED,
        Op         varchar(100)  NOT NULL,
        Status     varchar(16)   NOT NULL CONSTRAINT CK_MM_OpLog_Status CHECK (Status IN ('applied', 'rejected')),
        ResultHash char(64)      NOT NULL,
        ResultJson nvarchar(max) NOT NULL,
        AtUtc      datetime2(3)  NOT NULL CONSTRAINT DF_MM_OpLog_AtUtc DEFAULT SYSUTCDATETIME()
    );
END;
GO

/* MM_Op_GetLog — lock the request key for the caller's transaction, then return its log row
   (none when the key was never applied). Must be the first statement of the write's transaction. */
CREATE OR ALTER PROCEDURE dbo.MM_Op_GetLog
    @RequestKey varchar(200)
AS
BEGIN
    SET NOCOUNT ON;
    IF @@TRANCOUNT = 0
        THROW 59520, 'MM_Op_GetLog: must run inside the caller''s write transaction', 1;
    IF @RequestKey IS NULL OR LEN(@RequestKey) = 0 OR LEN(@RequestKey) > 100
        THROW 59521, 'MM_Op_GetLog: @RequestKey (1..100 characters) is required', 1;

    DECLARE @LockName nvarchar(255) = N'MM_OpLog|' + @RequestKey;
    DECLARE @Lock int;
    EXEC @Lock = sp_getapplock @Resource = @LockName, @LockMode = 'Exclusive',
                               @LockOwner = 'Transaction', @LockTimeout = 10000;
    IF @Lock < 0
        THROW 50000, 'MM_Op_GetLog: the request key is busy (applock timeout)', 1;

    SELECT RequestKey, Op, Status, ResultHash, ResultJson, AtUtc
      FROM dbo.MM_OpLog
     WHERE RequestKey = @RequestKey;
END;
GO

/* MM_Op_CreateLog — record a request key as applied, inside the transaction that made the write
   and after MM_Op_GetLog locked the key in it. */
CREATE OR ALTER PROCEDURE dbo.MM_Op_CreateLog
    @RequestKey varchar(200),
    @Op         varchar(200),
    @Status     varchar(16),
    @ResultHash varchar(100),
    @ResultJson nvarchar(max)
AS
BEGIN
    SET NOCOUNT ON;
    IF @@TRANCOUNT = 0
        THROW 59520, 'MM_Op_CreateLog: must run inside the caller''s write transaction', 1;
    IF @RequestKey IS NULL OR LEN(@RequestKey) = 0 OR LEN(@RequestKey) > 100
       OR @Op IS NULL OR LEN(@Op) = 0 OR LEN(@Op) > 100
       OR @Status IS NULL OR @Status NOT IN ('applied', 'rejected')
       OR @ResultHash IS NULL OR LEN(@ResultHash) <> 64 OR @ResultHash LIKE '%[^0-9a-f]%'
       OR @ResultJson IS NULL
        THROW 59521, 'MM_Op_CreateLog: @RequestKey, @Op (1..100), @Status (applied|rejected), a sha256 hex @ResultHash and @ResultJson are required', 1;

    DECLARE @LockName nvarchar(255) = N'MM_OpLog|' + @RequestKey;
    IF APPLOCK_MODE('public', @LockName, 'Transaction') <> 'Exclusive'
        THROW 59522, 'MM_Op_CreateLog: the request key is not locked by this transaction (call MM_Op_GetLog first)', 1;
    IF EXISTS (SELECT 1 FROM dbo.MM_OpLog WHERE RequestKey = @RequestKey)
        THROW 59523, 'MM_Op_CreateLog: the request key was already applied', 1;

    INSERT INTO dbo.MM_OpLog (RequestKey, Op, Status, ResultHash, ResultJson)
    VALUES (@RequestKey, @Op, @Status, @ResultHash, @ResultJson);
END;
GO
