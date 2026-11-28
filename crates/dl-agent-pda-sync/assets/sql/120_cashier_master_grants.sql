-- master.upsert entity cashier (ADR 0150, RT-0834): the store-side write of a cloud-owned POS clerk on a master_authority=cloud
-- tenant, as narrow grants to [pda_sync]. Only the statements of Commands/SqlCashierCommandUnit.cs use them (a test pins the
-- pairing): insert/update of dbo.CashierDef (name, PIN column TouchID, disabled flag, security level), the clerk's RVC rows,
-- and the Allowed flag of the mapped dbo.ClerkPermission rows. SQL Server has no column-scoped INSERT, so the CashierDef and
-- ClerkPermission inserts are table grants; the agent's insert names only Initial, FullName, TouchID, IsDisable, Security and
-- ClerkID, FunctionID, Allowed. Reading (and UPDLOCK) comes from db_datareader (050); FullName on a varchar build goes through
-- dbo.Unicode2TCVN, granted in 070. No schema change, no vendor object is altered (docs/internal/pos-compat.md); idempotent.
-- A build without a table or an optional column grants nothing for it; the command is then refused, never the bootstrap.
IF OBJECT_ID(N'dbo.CashierDef', N'U') IS NOT NULL
BEGIN
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.CashierDef TO [pda_sync];';
    IF COL_LENGTH(N'dbo.CashierDef', N'FullName') IS NOT NULL
        EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.CashierDef (FullName) TO [pda_sync];';
    IF COL_LENGTH(N'dbo.CashierDef', N'TouchID') IS NOT NULL
        EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.CashierDef (TouchID) TO [pda_sync];';
    IF COL_LENGTH(N'dbo.CashierDef', N'IsDisable') IS NOT NULL
        EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.CashierDef (IsDisable) TO [pda_sync];';
    IF COL_LENGTH(N'dbo.CashierDef', N'Security') IS NOT NULL
        EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.CashierDef (Security) TO [pda_sync];';
END
IF OBJECT_ID(N'dbo.CashierRVC', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT, DELETE ON OBJECT::dbo.CashierRVC TO [pda_sync];';
IF OBJECT_ID(N'dbo.ClerkPermission', N'U') IS NOT NULL
BEGIN
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.ClerkPermission TO [pda_sync];';
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.ClerkPermission (Allowed) TO [pda_sync];';
END
