-- Companion marker table: which SMILE bills the agent has already pushed.
-- Kept OUTSIDE SMILE's own tables so we never overload BillInfor.UpInvE
-- (a single bit that cannot hold invoice number / mã CQT and may already be
-- used by SMILE). One row per pushed invoice; presence = pushed.
--
-- SourceHash makes re-collection change-aware: it fingerprints the SMILE
-- source rows that were pushed. A bill is re-collected when it was never
-- pushed OK, OR when its current source fingerprint differs from the stored
-- one (i.e. the source data changed since we last pushed). Combined with the
-- Hub's lifecycle gate, this yields: new -> push; edited-but-unissued ->
-- update; already-issued -> a new version is created Hub-side, never an
-- overwrite of the issued invoice. See dbo.MMEI_SourceHash below.
IF OBJECT_ID('dbo.MMEI_Collected', 'U') IS NULL
CREATE TABLE dbo.MMEI_Collected (
    Source        varchar(4)    NOT NULL,   -- 'FO' | 'POS'
    ID            varchar(64)   NOT NULL,   -- FO: BillID ; POS: '<BillSeri>/<BillNo>'
    PushedAt      datetime      NOT NULL DEFAULT GETDATE(),
    Ok            bit           NOT NULL,
    Message       nvarchar(400) NULL,
    InvoiceNumber varchar(30)   NULL,
    KyHieu        varchar(30)   NULL,
    MaCQT         varchar(60)   NULL,
    IssuedAt      datetime      NULL,
    SourceHash    varbinary(32) NULL,        -- SHA-256 of the pushed source rows
    CONSTRAINT PK_MMEI_Collected PRIMARY KEY (Source, ID)
);

-- Idempotent add for installs created before SourceHash existed.
IF COL_LENGTH('dbo.MMEI_Collected', 'SourceHash') IS NULL
    ALTER TABLE dbo.MMEI_Collected ADD SourceHash varbinary(32) NULL;
GO
