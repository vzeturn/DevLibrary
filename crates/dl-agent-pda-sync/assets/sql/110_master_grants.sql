-- master.upsert (ADR 0147, RT-0844, ADR 0151): the store-side write of a cloud-owned master row, as narrow column grants to [pda_sync].
-- item_class: the name and change stamp of dbo.IClassDef. RT-0832 adds PaymentMode.PDesc, TaxClassDef.TaxRate, STaxDef.STaxRate and
-- CurrencyDef.ExRate (nothing else of those tables), each only when the build has that column. Reading the row (and its UPDLOCK) needs no
-- extra grant: SELECT comes from db_datareader (050). No schema change, no vendor object is altered
-- (docs/internal/pos-compat.md); idempotent (GRANT is repeatable). The statement that uses this grant is the UpdateSql of
-- the entities of MasterEntities (Commands/IMasterCommandUnit.cs; a test pins the pairing). A build without the table grants
-- nothing, and the command is then refused by SQL Server as a missing row/permission instead of failing the bootstrap.
IF OBJECT_ID(N'dbo.IClassDef', N'U') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.IClassDef (ClsName, LastChangeTime) TO [pda_sync];';

IF COL_LENGTH(N'dbo.PaymentMode', N'PDesc') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.PaymentMode (PDesc) TO [pda_sync];';

IF COL_LENGTH(N'dbo.TaxClassDef', N'TaxRate') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.TaxClassDef (TaxRate) TO [pda_sync];';

IF COL_LENGTH(N'dbo.STaxDef', N'STaxRate') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.STaxDef (STaxRate) TO [pda_sync];';

IF COL_LENGTH(N'dbo.CurrencyDef', N'ExRate') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT UPDATE ON OBJECT::dbo.CurrencyDef (ExRate) TO [pda_sync];';

-- RT-0833 (ADR 0154): areas, tables/beds and outlet parameters. A create is a whole-row INSERT (SQL Server has no column-scoped
-- INSERT); the statement names its columns (InsertSql of MasterEntities) and the agent checks the key is absent under UPDLOCK,HOLDLOCK
-- in the same transaction. Updates stay column-scoped; TableStatus and every other vendor column are never written by master.upsert. The same Para grant
-- serves txn.counters (ADR 0161, Commands/SqlTxnCounterUnit.cs), which only ever raises a GetRVCSeqNumber counter row.
IF OBJECT_ID(N'dbo.TableArea', N'U') IS NOT NULL AND COL_LENGTH(N'dbo.TableArea', N'TableAreaName') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.TableArea TO [pda_sync]; GRANT UPDATE ON OBJECT::dbo.TableArea (TableAreaName) TO [pda_sync];';

IF OBJECT_ID(N'dbo.TableList', N'U') IS NOT NULL AND COL_LENGTH(N'dbo.TableList', N'IsActive') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.TableList TO [pda_sync]; GRANT UPDATE ON OBJECT::dbo.TableList (Description, TableArea, IsActive, LastChangedBy, LastChangedTime) TO [pda_sync];';

IF OBJECT_ID(N'dbo.Para', N'U') IS NOT NULL AND COL_LENGTH(N'dbo.Para', N'ParaStr') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.Para TO [pda_sync]; GRANT UPDATE ON OBJECT::dbo.Para (ParaStr) TO [pda_sync];';

-- RT-0831 (ADR 0156, 0157): the menu. Updates: item name/price/group/class/flags and its change stamp, group name, menu category
-- name/flag/order, the shown flag of an item in a category, discount code fields and its void stamp, meal period name/hours/status.
-- INSERT (no column-scoped form exists) for the cloud's creates, which copy the vendor columns they do not edit from a template row.
-- No DELETE: hiding is a field. An update never writes PrintName, stock, cost, recipe, report group or any other vendor column.
IF OBJECT_ID(N'dbo.ItemDef', N'U') IS NOT NULL AND COL_LENGTH(N'dbo.ItemDef', N'LastChangeBy') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.ItemDef TO [pda_sync]; GRANT UPDATE ON OBJECT::dbo.ItemDef (IName, Price, IGroup, IClass, IActive, OpenPrice, isPkg, LastChangeBy, LastChangeTime) TO [pda_sync];';

-- RT-0979 (ADR 0160): combos. The item flag isPkg above marks one; its components are PkgDef rows (quantity, own price, use-item-price
-- flag, order). INSERT for a new component; no DELETE, a component is never removed by the cloud.
IF OBJECT_ID(N'dbo.PkgDef', N'U') IS NOT NULL AND COL_LENGTH(N'dbo.PkgDef', N'DisplayOrder') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.PkgDef TO [pda_sync]; GRANT UPDATE ON OBJECT::dbo.PkgDef (IQty, IPrice, UseItemPrice, DisplayOrder) TO [pda_sync];';

IF COL_LENGTH(N'dbo.IGroupDef', N'GName') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.IGroupDef TO [pda_sync]; GRANT UPDATE ON OBJECT::dbo.IGroupDef (GName) TO [pda_sync];';

IF OBJECT_ID(N'dbo.ItemMenu', N'U') IS NOT NULL AND COL_LENGTH(N'dbo.ItemMenu', N'MenuOrder') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.ItemMenu TO [pda_sync]; GRANT UPDATE ON OBJECT::dbo.ItemMenu (MenuName, MenuActive, MenuOrder) TO [pda_sync];';

IF COL_LENGTH(N'dbo.RVCItemMenu', N'ItemShow') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.RVCItemMenu TO [pda_sync]; GRANT UPDATE ON OBJECT::dbo.RVCItemMenu (ItemShow) TO [pda_sync];';

IF OBJECT_ID(N'dbo.DiscountCode', N'U') IS NOT NULL AND COL_LENGTH(N'dbo.DiscountCode', N'VoidedAt') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.DiscountCode TO [pda_sync]; GRANT UPDATE ON OBJECT::dbo.DiscountCode (DscName, DscByAmount, DscAmount, isOpen, IsVoided, VoidedBy, VoidedAt) TO [pda_sync];';

IF OBJECT_ID(N'dbo.MealPeriod', N'U') IS NOT NULL AND COL_LENGTH(N'dbo.MealPeriod', N'Status') IS NOT NULL
    EXECUTE sys.sp_executesql N'GRANT INSERT ON OBJECT::dbo.MealPeriod TO [pda_sync]; GRANT UPDATE ON OBJECT::dbo.MealPeriod (MealName, FromTime, ToTime, Status) TO [pda_sync];';
