-- OW1b order verbs (ADR 0104): the store-side writes of order.check_open / item_post / send / item_void, as narrow grants
-- to [pda_sync]. No schema change and no vendor object is altered (docs/internal/pos-compat.md); idempotent (GRANT is
-- repeatable). The batches that use these grants are the constants of Commands/OrderCommandSql*.cs, ported verbatim from
-- the legacy PDA order repository (a test pins the pairing). Reads are db_datareader (050_grants.sql).

-- The allow-listed vendor procedures and the TCVN3 note function. Writes they make inside run under ownership chaining
-- (dbo owns both the procedure and the tables), so they need no table grant of their own.
GRANT EXECUTE ON OBJECT::dbo.GetRVCSeqNumber TO [pda_sync];
GRANT EXECUTE ON OBJECT::dbo.PostITrn TO [pda_sync];
GRANT EXECUTE ON OBJECT::dbo.UpdateTmpInvoice TO [pda_sync];
GRANT EXECUTE ON OBJECT::dbo.Unicode2TCVN TO [pda_sync];

-- order.check_open: the check header, its info row and the open log, as POS WIN writes them.
GRANT INSERT ON OBJECT::dbo.tmpInvoice TO [pda_sync];
GRANT INSERT ON OBJECT::dbo.tmpInvoiceInfo TO [pda_sync];
GRANT INSERT ON OBJECT::dbo.checkopenlog TO [pda_sync];

-- order.item_post (menu backfill, merged-row cleanup) and order.item_void (line removal, discount unlink).
GRANT UPDATE ON OBJECT::dbo.tmpTrn (Rsv2, DscRef) TO [pda_sync];
GRANT DELETE ON OBJECT::dbo.tmpTrn TO [pda_sync];

-- order.send (info-row backfill, sent flags) and order.item_void (the Void flag; the row is kept for the void chit).
GRANT INSERT ON OBJECT::dbo.tmpTrnInfo TO [pda_sync];
GRANT UPDATE ON OBJECT::dbo.tmpTrnInfo (PToOrder, POrderTime, OrderQty, Void) TO [pda_sync];

-- order.item_void: the void archive.
GRANT INSERT ON OBJECT::dbo.VoidTrn TO [pda_sync];

-- Item notes and the send audit live in tables some SMILE_POS builds lack; the batches skip them there (OBJECT_ID), so the
-- grants are only made where the table exists. Dynamic so a missing table cannot fail the batch at compile time.
IF OBJECT_ID(N'dbo.CGTrn', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT, DELETE ON OBJECT::dbo.CGTrn TO [pda_sync];';
IF OBJECT_ID(N'dbo.PDASendOrder', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.PDASendOrder TO [pda_sync];';
