-- Copied verbatim from SML-PDAOrder apps/sql-sync-agent/rust/e2e/fixture/seed.sql (the side-by-side E1 fixture);
-- tests/live_support builds the catalog shape between the two files like e2etool does.
-- E1 side-by-side fixture, part 2 (after the generated catalog tables and columns). @base is a fixed local day passed
-- by run.ps1 (ten days back), so both agents read the same datetimes and none of them looks like "now".
-- TCVN3 bytes: 0x4B68A86E206CB96E68206D69D46E207068DD = "Khăn lạnh miễn phí", 0x4E677579D46E2056A86E20416E = "Nguyễn Văn An".
-- Open checks (snapshot-diff), with their lines and line info.
INSERT dbo.tmpInvoice (CheckNo, IYear, IMonth, IDay, OpenTime, OpenBy, RVC, TableNo, NGuest, NChild, ExRate, Status,
    SubTotal, Discount, TotalPaid, MealPeriod, StsCode1, LastChgTime)
VALUES (1001, 2026, 11, 14, DATEADD(MINUTE, 9 * 60 + 5, @base), 'AN', 1, 'T01', 2, 0, 1.0, 1, 60000.5, 0, 0, 2,
        N'Khách quen – bàn cửa sổ', DATEADD(MINUTE, 9 * 60 + 20, @base)),
       (1002, 2026, 11, 14, DATEADD(SECOND, 10 * 3600 + 7, @base), 'BINH', 1, 'T02', 4, 1, 1.0, 1, 25000, 0, 0, 2,
        NULL, NULL);
INSERT dbo.tmpInvoiceInfo (CheckNo, CurPage, CurLine, Printed, Ordered, PrintType) VALUES (1001, 1, 2, 0, 1, 0), (1002, 1, 1, 0, 0, 0);
INSERT dbo.tmpTrn (NYear, NMonth, NDay, TrnSeq, CheckNo, TrnTime, Cashier, TrnCode, ItemCode, TrnDesc, ItemPrice, BaseTTL,
    TrnQty, RVC, Status, OrderNo, Rsv1, Rsv2)
VALUES (2026, 11, 14, 5001, 1001, DATEADD(MINUTE, 9 * 60 + 6, @base), 'AN', 100, 'CF01', 'Coffee', 25000, 50000, 2, 1, 0, 1, NULL, 'M01'),
       (2026, 11, 14, 5002, 1001, DATEADD(MINUTE, 9 * 60 + 7, @base), 'AN', 100, 'CK01',
        CAST(0x4B68A86E206CB96E68206D69D46E207068DD AS varchar(30)), 10000.5, 10000.5, 1, 1, 0, 1, NULL, ''),
       (2026, 11, 14, 5003, 1002, DATEADD(MINUTE, 10 * 60 + 1, @base), 'BINH', 100, 'CF01', 'Coffee', 25000, 25000, 1, 1, 0, 1, NULL, 'M01');
INSERT dbo.tmpTrnInfo (CheckNo, TrnNo, PToOrder, POrderTime, PToCheck, Void, OrderQty, IsDone, DoneTime)
VALUES (1001, 5001, 1, DATEADD(MINUTE, 9 * 60 + 8, @base), 0, 0, 2, 0, NULL),
       (1001, 5002, 0, NULL, 0, 0, 0, 0, NULL);
INSERT dbo.CGTrn (SeqNo, TrnNo, ItemCode, CGCode, CGName) VALUES (1, 5001, 'CF01', 0, CAST(0x4B68A86E206CB96E68206D69D46E207068DD AS varchar(100)));
INSERT dbo.checkopenlog (RVC, CheckNo, OpenBy, OpenPC, OpenTime, isOpen, LastAction)
VALUES (1, 1001, 'AN', 'POS1', DATEADD(MINUTE, 9 * 60 + 5, @base), 1, DATEADD(MINUTE, 9 * 60 + 5, @base));

-- Closed checks (watermark+rescan) and their lines.
INSERT dbo.Invoice (CheckNo, IYear, IMonth, IDay, OpenTime, CloseTime, OpenBy, CloseBy, RVC, TableNo, NGuest, NChild, ExRate,
    Status, SubTotal, Discount, TaxTotal, TotalPaid, PaymentInfo, LastChgTime)
VALUES (901, 2026, 11, 13, DATEADD(MINUTE, -14 * 60, @base), DATEADD(MINUTE, -13 * 60, @base), 'AN', 'AN', 1, 'T03', 2, 0, 1.0,
        2, 120000, 0, 9600, 129600, CAST(0x4B68A86E206CB96E68206D69D46E207068DD AS varchar(100)), DATEADD(MINUTE, -13 * 60, @base)),
       (902, 2026, 11, 13, DATEADD(MINUTE, -12 * 60, @base), DATEADD(MILLISECOND, -11 * 3600000 + 123, @base), 'BINH', 'BINH', 1, 'T01', 1, 0, 1.0,
        2, 25000, 2500, 1800, 24300, NULL, NULL),
       (903, 2026, 11, 14, DATEADD(MINUTE, 8 * 60, @base), DATEADD(MINUTE, 8 * 60 + 45, @base), 'AN', 'AN', 1, 'T02', 3, 1, 1.0,
        2, 35000.25, 0, 0, 35000.25, NULL, NULL);
INSERT dbo.Trn (NYear, NMonth, NDay, TrnSeq, CheckNo, TrnTime, Cashier, TrnCode, ItemCode, TrnDesc, ItemPrice, BaseTTL, TrnQty, RVC, Status, OrderNo)
VALUES (2026, 11, 13, 4001, 901, DATEADD(MINUTE, -14 * 60 + 5, @base), 'AN', 100, 'CF01', 'Coffee', 25000, 100000, 4, 1, 0, 1),
       (2026, 11, 13, 4002, 901, DATEADD(MINUTE, -14 * 60 + 6, @base), 'AN', 100, 'CK01',
        CAST(0x4B68A86E206CB96E68206D69D46E207068DD AS varchar(30)), 10000, 20000, 2, 1, 0, 1),
       (2026, 11, 13, 4003, 902, DATEADD(MINUTE, -12 * 60 + 1, @base), 'BINH', 100, 'CF01', 'Coffee', 25000, 25000, 1, 1, 0, 1),
       (2026, 11, 14, 4004, 903, DATEADD(MINUTE, 8 * 60 + 2, @base), 'AN', 100, 'CB01', 'Combo', 35000.25, 35000.25, 1, 1, 0, 1);

-- Masters (scan-diff) with Vietnamese text: TCVN3 in varchar, Unicode in nvarchar, and an image blob.
UPDATE dbo.ItemDef SET IName = CAST(0x4B68A86E206CB96E68206D69D46E207068DD AS varchar(50)) WHERE ICode = 'CK01';
INSERT dbo.CashierDef (Initial, FullName, PassWord, TouchID, CardID, IsDisable, Security)
VALUES ('AN', CAST(0x4E677579D46E2056A86E20416E AS varchar(50)), 'secret-pw', '1234', 'CARD-1', 0, 3),
       ('BINH', 'Binh Tran', 'secret-pw2', NULL, NULL, 0, 1),
       ('CUONG', 'Cuong', NULL, '12ab', NULL, 1, NULL);
INSERT dbo.CashierRVC (Cashier, RVC) VALUES ('AN', 1), ('BINH', 1), ('BINH', 2);
INSERT dbo.ClerkPermission (ClerkID, FunctionID, Allowed)
VALUES ('AN', 'PT_Payment', 1), ('AN', 'Cashier', 1), ('BINH', 'SS_Kitchen', 1), ('BINH', 'PT_Payment', 0);
INSERT dbo.DefRVCList (RVCNo, RVCName, Picture)
VALUES (1, N'Nhà hàng Sông Hương', 0x89504E470D0A1A0A0000000D4948445200000001000000010806000000),
       (2, N'Quầy bar', NULL);
INSERT dbo.TableList (TableNum, RVC, TableStatus, LastChangedTime)
VALUES ('T01', 1, 1, DATEADD(MINUTE, 9 * 60, @base)), ('T02', 1, 1, NULL), ('T03', 1, 0, NULL);
INSERT dbo.GlobPara (RVC, ParaName, ParaStr) VALUES (1, 'qrCodeNeedConfirmOrder', '0');
INSERT dbo.TableReservation (GuestInfo, NumAdult, NumChild, FromDateTime, ToDateTime, TableNum, TelNum, Booker, RVC)
VALUES (N'Chị Lan – sinh nhật', 6, 2, DATEADD(MINUTE, 19 * 60, @base), DATEADD(MINUTE, 21 * 60, @base), N'T03', N'0901234567', N'Lan', N'1');
