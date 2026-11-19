-- OW1e table and multi-check verbs (ADR 0112): the store-side writes of order.table_change, order.table_transfer,
-- order.check_merge and order.items_move, as narrow grants to [pda_sync]. No schema change and no vendor object is altered
-- (docs/internal/pos-compat.md); idempotent (GRANT is repeatable). The batches that use them are in
-- Commands/OrderCommandSql.BatchesOw1e.cs, ported verbatim from the legacy PDA services (a test pins the pairing). Reads are
-- db_datareader (050_grants.sql). 070 and 080 are applied and stay as they are. Each column grant is guarded by COL_LENGTH
-- and each optional table by OBJECT_ID, so a build without one still installs; the agent then refuses the verb on that store
-- (schema_unsupported) instead of guessing another column. Nothing here touches payments, tax or the closed ledger.

-- order.table_change (legacy SqlTableInfo) and order.table_transfer: the table of the open check header and of its line
-- info, and the busy flag of the new table.
IF COL_LENGTH(N'dbo.tmpInvoice', N'TableNo') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.tmpInvoice (TableNo) TO [pda_sync];';
IF COL_LENGTH(N'dbo.tmpTrnInfo', N'TableNo') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.tmpTrnInfo (TableNo) TO [pda_sync];';
IF COL_LENGTH(N'dbo.tablelist', N'TableStatus') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.tablelist (TableStatus) TO [pda_sync];';

-- order.check_merge and order.items_move: the lines and their line info change check.
IF COL_LENGTH(N'dbo.tmpTrn', N'CheckNo') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.tmpTrn (CheckNo) TO [pda_sync];';
IF COL_LENGTH(N'dbo.tmpTrnInfo', N'CheckNo') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.tmpTrnInfo (CheckNo) TO [pda_sync];';

-- order.check_merge closes the emptied source (legacy SqlCancelOrder): it removes the open header, its info and line info
-- (the tmpTrn delete is 070's), marks the open-check log closed and archives the header to the cancel audit when the build
-- has it.
IF OBJECT_ID(N'dbo.tmpInvoice', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT DELETE ON OBJECT::dbo.tmpInvoice TO [pda_sync];';
IF OBJECT_ID(N'dbo.tmpInvoiceInfo', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT DELETE ON OBJECT::dbo.tmpInvoiceInfo TO [pda_sync];';
IF OBJECT_ID(N'dbo.tmpTrnInfo', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT DELETE ON OBJECT::dbo.tmpTrnInfo TO [pda_sync];';
IF COL_LENGTH(N'dbo.checkopenlog', N'isOpen') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.checkopenlog (isOpen) TO [pda_sync];';
IF COL_LENGTH(N'dbo.checkopenlog', N'CloseTime') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.checkopenlog (CloseTime) TO [pda_sync];';
IF COL_LENGTH(N'dbo.checkopenlog', N'LastAction') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.checkopenlog (LastAction) TO [pda_sync];';
IF OBJECT_ID(N'dbo.InvoiceCxl', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.InvoiceCxl TO [pda_sync];';
IF COL_LENGTH(N'dbo.InvoiceCxl', N'PaymentMode') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.InvoiceCxl (PaymentMode) TO [pda_sync];';
IF COL_LENGTH(N'dbo.InvoiceCxl', N'CloseBy') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.InvoiceCxl (CloseBy) TO [pda_sync];';

-- order.items_move: the legacy MOVE audit row per moved line (best-effort, as the legacy service had it).
IF OBJECT_ID(N'dbo.TrnActivities', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.TrnActivities TO [pda_sync];';
