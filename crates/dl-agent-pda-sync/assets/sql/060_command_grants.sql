-- Sync S9 command router (ADR 0029): the store-side writes of the site commands, as narrow grants to [pda_sync].
-- No schema change and no vendor object is altered (docs/internal/pos-compat.md); idempotent (GRANT is repeatable).
-- The statements that use these grants are the constants of Commands/SqlSiteCommandStore.cs (a test pins the pairing).

-- kds.line_state done / undone: the vendor done flags of dbo.tmpTrnInfo.
GRANT UPDATE ON OBJECT::dbo.tmpTrnInfo (IsDone, DoneTime) TO [pda_sync];

-- kds.line_state accept / unaccept: the additive PDA columns, granted only where they exist (the executor rejects the
-- command with kds_accept_unsupported otherwise). Dynamic so a missing column cannot fail the batch at compile time.
IF COL_LENGTH(N'dbo.tmpTrnInfo', N'KdsAcceptedTime') IS NOT NULL AND COL_LENGTH(N'dbo.tmpTrnInfo', N'KdsAcceptedBy') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.tmpTrnInfo (KdsAcceptedTime, KdsAcceptedBy) TO [pda_sync];';

-- reservation.save / reservation.cancel.
GRANT INSERT, DELETE ON OBJECT::dbo.TableReservation TO [pda_sync];
