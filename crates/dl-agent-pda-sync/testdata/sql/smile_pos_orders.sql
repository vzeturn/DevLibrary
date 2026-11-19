-- B9 (RT-0257): the order-verb, combo and table-verb shapes of the C# command tests that the E1 fixture does not carry,
-- applied after smile_pos_schema.sql and before tests/live_support generates the rest of the catalog shape.
-- Sources (SML-PDAOrder tests/MM.PdaOrder.SyncAgent.Tests/Commands):
--   OrderVerbsSchema.cs  tables and routines are already in smile_pos_schema.sql for RVC 1; here the same seed for RVC 25
--                        (25 * 100000000 overflows int, so the batches must widen first);
--   ComboStoreSchema.cs  the numeric combos 10/20/30 and package B, RVC 3 (plain) and RVC 4 (OrderByCourse), with
--                        GroupChkTrn on; ItemDef.ICurrency stays the vendor int (CrCode 1), not the stand-in's 'VND';
--   SqlTableCommandsIntegrationTests.cs  tmpTrnInfo.TableNo, checkopenlog.CloseTime and the TrnActivities audit table,
--                        typed like the vendor columns; InvoiceCxl is smile_pos_orders_post.sql.
-- The stand-in routines stay those of smile_pos_schema.sql (both batches only rely on what they do).
-- The cancel archive's two stamped columns with their vendor types (the catalog generation would type PaymentMode int).
ALTER TABLE dbo.tmpInvoice ADD CloseBy varchar(20) NULL, PaymentMode varchar(10) NULL;
ALTER TABLE dbo.tmpTrnInfo ADD TableNo varchar(10) NULL;
ALTER TABLE dbo.checkopenlog ADD CloseTime datetime NULL;
CREATE TABLE dbo.TrnActivities (nSeq numeric(18,0) IDENTITY(1,1) NOT NULL PRIMARY KEY, tTime datetime NULL, RVC int NULL,
    Cashier varchar(30) NULL, CheckNo numeric(18,0) NULL, ToCheckNo numeric(18,0) NULL, TrnNo numeric(18,0) NULL,
    ActCode int NULL, ActName varchar(10) NULL, TrnAmount money NULL);

-- OrderVerbsSchema.Tables for @rvc = 25.
INSERT dbo.Para (RVC, ParaName, ParaStr) VALUES (25, 'CurYear', '2026'), (25, 'CurMonth', '11'), (25, 'CurDay', '14'),
    (25, 'LastCheck', '0'), (25, 'LastTrn', '0'), (25, 'OrderPrintByMenu', '1');
INSERT dbo.MealPeriod (MealNo, RVCNo, FromTime, ToTime) VALUES (2, 25, 0, 2359);
INSERT dbo.ItemMenu (MenuNo, RVC, MenuActive, PNo, MenuOrder) VALUES ('M01', 25, 1, 1, 1);
INSERT dbo.RVCItemMenu (RVC, ItemCode, ItemMenu) VALUES (25, 'CF01', 'M01');

-- ComboStoreSchema.Create: items, packages, the two combo RVCs and their menu.
INSERT dbo.ItemDef (ICode, IName, OpenItem, OpenPrice, IsPkg, ICurrency, Price) VALUES
    ('A', 'Pho bo', 0, 0, 0, 1, 50000), ('B', 'Set trang mieng', 0, 0, 1, 1, 30000), ('B1', 'Che', 0, 0, 0, 1, 10000),
    ('B2', 'Kem', 0, 0, 0, 1, 12000), ('C', 'Tra da', 0, 0, 0, 1, 8000), ('77', 'Mon le', 0, 0, 0, 1, 99000);
INSERT dbo.PkgDef (PkgCode, ItemCode, iQty, IPrice, UseItemPrice, DisplayOrder) VALUES
    -- combo 10: A at its package price, then package B at its item price, twice.
    ('10', 'A', 1, 40000, 0, 1), ('10', 'B', 2, 0, 1, 2),
    -- combo 20: 77, then A (A may merge into an unsent A line as the last component).
    ('20', '77', 1, 20000, 0, 1), ('20', 'A', 1, 40000, 0, 2),
    -- combo 30: A, then C.
    ('30', 'A', 1, 40000, 0, 1), ('30', 'C', 1, 6000, 0, 2),
    -- package B's own children.
    ('B', 'B1', 1, 0, 0, 1), ('B', 'B2', 1, 5000, 0, 2);
INSERT dbo.Para (RVC, ParaName, ParaStr) VALUES
    (3, 'CurYear', '2026'), (3, 'CurMonth', '11'), (3, 'CurDay', '14'), (3, 'LastCheck', '0'), (3, 'LastTrn', '0'),
    (3, 'GroupChkTrn', '1'), (3, 'OrderPrintByMenu', '1'), (3, 'OrderByCourse', '0'),
    (4, 'CurYear', '2026'), (4, 'CurMonth', '11'), (4, 'CurDay', '14'), (4, 'LastCheck', '0'), (4, 'LastTrn', '0'),
    (4, 'GroupChkTrn', '1'), (4, 'OrderPrintByMenu', '1'), (4, 'OrderByCourse', '1');
INSERT dbo.MealPeriod (MealNo, RVCNo, FromTime, ToTime) VALUES (2, 3, 0, 2359), (2, 4, 0, 2359);
INSERT dbo.ItemMenu (MenuNo, RVC, MenuActive, PNo, MenuOrder) VALUES ('M1', 3, 1, 1, 1), ('M1', 4, 1, 1, 1);
INSERT dbo.RVCItemMenu (RVC, ItemCode, ItemMenu) VALUES (3, 'A', 'M1'), (3, 'B', 'M1'), (3, 'C', 'M1'), (4, 'A', 'M1'),
    (4, 'B', 'M1'), (4, 'C', 'M1');
INSERT dbo.TableList (TableNum, RVC, TableStatus, LastChangedTime) VALUES ('T4', 3, 1, NULL), ('T5', 3, 1, NULL),
    ('T12', 3, 0, NULL);
