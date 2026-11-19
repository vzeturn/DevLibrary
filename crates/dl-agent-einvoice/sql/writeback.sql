-- MM-EINVOICE agent — write-back procedure
--
-- Embedded in the agent binary (agentsql.Writeback). The agent installs it
-- into each SMILE database it collects from (SMILE_FO and SMILE_POS) together
-- with the collect set at enroll, and re-installs it on its own when a
-- write-back finds the procedure missing (SQL Server error 2812) — sites
-- enrolled before it was embedded heal on their first write-back. Only the
-- default name is self-installed: an AgentConfig.WritebackSP override names a
-- DBA-owned procedure the agent never overwrites.
--
-- The write-back target is dbo.MMEI_Collected (created by
-- collect/00_marker_table.sql): the same per-bill marker row that already
-- carries the "pushed" flag (Ok/PushedAt/SourceHash) gains the issued values,
-- so one row answers all three states an operator cares about:
--   * pushed?          -> Ok = 1, PushedAt
--   * has a number?    -> InvoiceNumber / KyHieu / MaCQT / IssuedAt set
--   * changed since?   -> MMEI_SourceHash(...) <> SourceHash (the select SPs
--                         re-collect it; sp_MarkCollected then clears the
--                         issued values so the bill reads as a NEW invoice)
--
-- The agent calls this SP (default name, configurable via
-- AgentConfig.WritebackSP) once per write-back command received from the
-- platform, idempotently by command_id:
--   EXEC dbo.sp_ApplyInvoiceWriteback
--       @BillID = ?, @BillType = ?, @InvoiceNumber = ?, @KyHieu = ?,
--       @MaCQT = ?, @IssuedAt = ?
-- A SET_INVOICE_NUMBER command supplies @InvoiceNumber/@KyHieu (NULL @MaCQT);
-- a SET_MA_CQT command supplies @MaCQT only. Each command OVERWRITES the
-- column(s) it carries — a re-issued (changed) bill must show the NEW number,
-- never keep a stale one — and leaves the other side untouched so the two
-- command types are safe in either order.
--
-- An install that also wants the values on its own eInvoice/interface table
-- appends an UPDATE to that table inside this SP.

CREATE OR ALTER PROCEDURE dbo.sp_ApplyInvoiceWriteback
    @BillID         varchar(64),
    @BillType       varchar(4),
    @InvoiceNumber  varchar(30)  = NULL,
    @KyHieu         varchar(30)  = NULL,
    @MaCQT          varchar(60)  = NULL,
    @IssuedAt       datetime     = NULL
AS
BEGIN
    SET NOCOUNT ON;

    -- The Go agent binds string params as '' (never SQL NULL) for the fields a
    -- command does not carry — VERIFIED LIVE 2026-07-22: a SET_MA_CQT command
    -- arriving after SET_INVOICE_NUMBER blanked the stored number because ''
    -- passed the IS NOT NULL gate. Normalize '' to NULL first so "absent" and
    -- "not carried" are the same thing.
    SET @InvoiceNumber = NULLIF(@InvoiceNumber, '');
    SET @KyHieu        = NULLIF(@KyHieu, '');
    SET @MaCQT         = NULLIF(@MaCQT, '');

    UPDATE dbo.MMEI_Collected
    SET
        -- SET_INVOICE_NUMBER: overwrite number + ký hiệu (stale values must
        -- not survive a re-issue). SET_MA_CQT: leave them untouched.
        InvoiceNumber = CASE WHEN @InvoiceNumber IS NOT NULL THEN @InvoiceNumber ELSE InvoiceNumber END,
        KyHieu        = CASE WHEN @InvoiceNumber IS NOT NULL THEN @KyHieu        ELSE KyHieu        END,
        -- SET_MA_CQT: overwrite mã CQT; otherwise keep.
        MaCQT         = COALESCE(@MaCQT, MaCQT),
        IssuedAt      = COALESCE(@IssuedAt, IssuedAt, GETDATE())
    WHERE Source = @BillType
      AND ID = @BillID;
END;
