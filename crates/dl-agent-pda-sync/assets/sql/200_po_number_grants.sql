-- order.po_number (RT-1413): the guest of an open check ("<room>-<guest>") goes to the check header's PONumber, the column POS
-- WIN copies into the closed bill. A narrow column grant to [pda_sync]; no schema change and no vendor object is altered
-- (docs/internal/pos-compat.md); idempotent (GRANT is repeatable). The batch that uses it is OrderCommandSql.SetPoNumber
-- (Commands/OrderCommandSql.PoNumber.cs). Guarded by the column, so a build without it still installs; the verb is then
-- refused with schema_unsupported. The older scripts are applied at stores and are never edited.
IF COL_LENGTH(N'dbo.tmpInvoice', N'PONumber') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.tmpInvoice (PONumber) TO [pda_sync];';
