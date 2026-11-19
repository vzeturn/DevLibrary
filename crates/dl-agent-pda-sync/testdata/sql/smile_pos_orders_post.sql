-- B9 (RT-0257), after the generated catalog shape: the cancelled-invoice archive mirrors the open-check header column for
-- column, because the inlined pda.spCancelOrder (and POS WIN's frmCheck.HideCheck) copies it with
-- INSERT INTO dbo.InvoiceCxl SELECT * FROM dbo.tmpInvoice. The catalog generation alone would give InvoiceCxl its own
-- column order, so it is rebuilt from tmpInvoice here (SqlTableCommandsIntegrationTests.cs gives both the same columns).
IF OBJECT_ID(N'dbo.InvoiceCxl', N'U') IS NOT NULL DROP TABLE dbo.InvoiceCxl;
SELECT TOP 0 * INTO dbo.InvoiceCxl FROM dbo.tmpInvoice;
IF COL_LENGTH(N'dbo.InvoiceCxl', N'PaymentMode') IS NULL OR COL_LENGTH(N'dbo.InvoiceCxl', N'CloseBy') IS NULL
    RAISERROR('the catalog shape lost tmpInvoice.PaymentMode or CloseBy', 16, 1);
