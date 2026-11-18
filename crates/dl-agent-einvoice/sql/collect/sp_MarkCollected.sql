CREATE OR ALTER PROCEDURE dbo.sp_MarkCollected
    @Source varchar(4), @ID varchar(64), @Ok bit, @Message nvarchar(400) = NULL AS
BEGIN
    SET NOCOUNT ON;
    -- Record the fingerprint of the source rows we just pushed, so the next
    -- select can tell "unchanged since push" (skip) from "edited since push"
    -- (re-collect). Computed from the SAME function the select SPs use — the
    -- two MUST agree or change-detection breaks.
    --
    -- A re-push whose source CHANGED (fingerprint differs from the stored one)
    -- also CLEARS the issued-value flags: the bill is treated as a NEW invoice
    -- and the old number/mã CQT no longer describe it. The Hub issues a new
    -- version and the writeback re-populates these columns. An unchanged
    -- re-mark (same hash) keeps them.
    DECLARE @Hash varbinary(32) = dbo.MMEI_SourceHash(@Source, @ID);
    MERGE dbo.MMEI_Collected AS tgt
    USING (SELECT @Source AS Source, @ID AS ID) AS src
        ON tgt.Source = src.Source AND tgt.ID = src.ID
    WHEN MATCHED THEN UPDATE SET
        Ok = @Ok, Message = @Message, PushedAt = GETDATE(),
        InvoiceNumber = CASE WHEN tgt.SourceHash = @Hash THEN tgt.InvoiceNumber END,
        KyHieu        = CASE WHEN tgt.SourceHash = @Hash THEN tgt.KyHieu END,
        MaCQT         = CASE WHEN tgt.SourceHash = @Hash THEN tgt.MaCQT END,
        IssuedAt      = CASE WHEN tgt.SourceHash = @Hash THEN tgt.IssuedAt END,
        SourceHash = @Hash
    WHEN NOT MATCHED THEN INSERT (Source, ID, Ok, Message, SourceHash)
        VALUES (@Source, @ID, @Ok, @Message, @Hash);
END;
