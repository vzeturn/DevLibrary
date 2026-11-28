-- txn.shift_open, txn.shift_close, txn.cash_in, txn.cash_out (ADR 0160, RT-0935): the cloud-authoritative shift and cash write-down
-- as narrow grants to [pda_sync]. Only Commands/SqlTxnCommandUnit.cs and the shift executor use them. CashierShift: INSERT (the
-- shift's first command; ShiftNo is an identity the agent reads back, never sets) and UPDATE of the eight mutable columns only
-- (SQL Server has no column-scoped INSERT or DELETE), so the key columns RVCNo, Cashier, StartTime, OpenDate can never be changed
-- by a later command. ShiftMoney: INSERT, DELETE (the shift's counted lines are replaced by ShiftNo). DailyCashLog: INSERT only
-- (a cash movement is appended once, never edited or deleted by the agent). Reading is db_datareader (050); the TCVN3 text columns
-- go through dbo.Unicode2TCVN (granted in 070). No schema change, no vendor object is altered (docs/internal/pos-compat.md);
-- idempotent (GRANT is repeatable). A build without a table or an optional column grants nothing for it; the command is then
-- refused, never the bootstrap. Dynamic so a missing object cannot fail the batch at compile time.
IF OBJECT_ID(N'dbo.CashierShift', N'U') IS NOT NULL
BEGIN
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.CashierShift TO [pda_sync];';
    IF COL_LENGTH(N'dbo.CashierShift', N'ShiftStatus') IS NOT NULL
        EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.CashierShift (ShiftStatus) TO [pda_sync];';
    IF COL_LENGTH(N'dbo.CashierShift', N'EndTime') IS NOT NULL
        EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.CashierShift (EndTime) TO [pda_sync];';
    IF COL_LENGTH(N'dbo.CashierShift', N'CloseDate') IS NOT NULL
        EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.CashierShift (CloseDate) TO [pda_sync];';
    IF COL_LENGTH(N'dbo.CashierShift', N'CashOpen') IS NOT NULL
        EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.CashierShift (CashOpen) TO [pda_sync];';
    IF COL_LENGTH(N'dbo.CashierShift', N'CashIn') IS NOT NULL
        EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.CashierShift (CashIn) TO [pda_sync];';
    IF COL_LENGTH(N'dbo.CashierShift', N'CashOut') IS NOT NULL
        EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.CashierShift (CashOut) TO [pda_sync];';
    IF COL_LENGTH(N'dbo.CashierShift', N'CashEnd') IS NOT NULL
        EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.CashierShift (CashEnd) TO [pda_sync];';
    IF COL_LENGTH(N'dbo.CashierShift', N'ShiftNotes') IS NOT NULL
        EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.CashierShift (ShiftNotes) TO [pda_sync];';
END
IF OBJECT_ID(N'dbo.ShiftMoney', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT, DELETE ON OBJECT::dbo.ShiftMoney TO [pda_sync];';
IF OBJECT_ID(N'dbo.DailyCashLog', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.DailyCashLog TO [pda_sync];';
