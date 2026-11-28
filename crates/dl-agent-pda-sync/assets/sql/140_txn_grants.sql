-- txn.snapshot (ADR 0159, RT-0849): the cloud-authoritative bill write-down replaces one bill's rows table by table, so
-- [pda_sync] may insert and delete rows of the six bill tables. Only the txn.snapshot executor (Commands/SqlTxnCommandUnit.cs)
-- uses these grants, and the cloud enqueues it only for txn_authority=cloud tenants. Reading is db_datareader (050). No schema
-- change, no vendor object is altered (docs/internal/pos-compat.md); idempotent (GRANT is repeatable). A build without one of
-- the tables grants nothing for it. Dynamic so a missing table cannot fail the batch at compile time.
IF OBJECT_ID(N'dbo.TmpInvoice', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT, DELETE ON OBJECT::dbo.TmpInvoice TO [pda_sync];';
IF OBJECT_ID(N'dbo.TmpTrn', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT, DELETE ON OBJECT::dbo.TmpTrn TO [pda_sync];';
IF OBJECT_ID(N'dbo.TmpPayment', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT, DELETE ON OBJECT::dbo.TmpPayment TO [pda_sync];';
IF OBJECT_ID(N'dbo.Invoice', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT, DELETE ON OBJECT::dbo.Invoice TO [pda_sync];';
IF OBJECT_ID(N'dbo.Trn', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT, DELETE ON OBJECT::dbo.Trn TO [pda_sync];';
IF OBJECT_ID(N'dbo.Payment', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT, DELETE ON OBJECT::dbo.Payment TO [pda_sync];';
