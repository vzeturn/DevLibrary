-- Copied verbatim from SML-PDAOrder apps/sql-sync-agent/rust/e2e/fixture/schema.sql (the side-by-side E1 fixture);
-- tests/live_support builds the catalog shape between the two files like e2etool does.
-- E1 side-by-side fixture, part 1: the vendor tables the agent reads with vendor-like types. The order-verb tables, seed
-- and routines are tests/MM.PdaOrder.SyncAgent.Tests/Commands/OrderVerbsSchema.cs (RVC 1); the rest follows
-- tests/MM.PdaOrder.SyncAgent.Tests/Capture/SqlServerCaptureIntegrationTests.cs and Staff/SqlStaffSourceTests.cs.
-- After this file, e2etool adds every other catalog table and every missing catalog column (generated, NULLable), then
-- runs seed.sql. Batches are separated by GO lines.
DECLARE @rvc int = 1;
CREATE TABLE dbo.Para (RVC int NOT NULL, ParaName varchar(50) NOT NULL, ParaStr varchar(200) NULL,
    PRIMARY KEY (RVC, ParaName));
CREATE TABLE dbo.GlobPara (RVC int NOT NULL, ParaName varchar(50) NOT NULL, ParaStr varchar(200) NULL);
CREATE TABLE dbo.MealPeriod (MealNo int NOT NULL, RVCNo int NOT NULL, FromTime int NOT NULL, ToTime int NOT NULL);
CREATE TABLE dbo.CurrencyDef (CrCode int NOT NULL PRIMARY KEY, DecDiv int NULL);
CREATE TABLE dbo.ItemDef (ICode varchar(30) NOT NULL PRIMARY KEY, IName varchar(50) NULL, OpenItem bit NULL,
    OpenPrice bit NULL, IsPkg bit NULL, ICurrency int NULL, Price money NULL);
CREATE TABLE dbo.PkgDef (PkgCode varchar(30) NOT NULL, ItemCode varchar(30) NOT NULL, iQty numeric(18,2) NULL,
    UseItemPrice bit NULL, IPrice money NULL, DisplayOrder int NULL);
CREATE TABLE dbo.ItemMenu (MenuNo varchar(30) NOT NULL, RVC int NOT NULL, MenuActive bit NULL, PNo int NULL,
    MenuOrder int NULL);
CREATE TABLE dbo.RVCItemMenu (RVC int NOT NULL, ItemCode varchar(30) NOT NULL, ItemMenu varchar(30) NOT NULL);
CREATE TABLE dbo.tmpInvoice (CheckNo numeric(11,0) NOT NULL PRIMARY KEY, IYear int NULL, IMonth int NULL,
    IDay int NULL, OpenTime datetime NULL, OpenBy varchar(20) NULL, RVC int NULL, TableNo varchar(10) NULL,
    SubTableNo int NULL, NGuest int NULL, NChild int NULL, ExRate numeric(18,5) NULL, ClientCode int NULL,
    WSID varchar(20) NULL, MealPeriod int NULL, ShiftPeriod tinyint NULL, RoomCode nvarchar(20) NULL,
    FolioNum int NULL, MemberID varchar(20) NULL, MemberSubID int NULL, PostedToInterface int NULL,
    Status int NULL, InvStatus int NULL, SubTotal money NULL, Discount money NULL, SurCharge money NULL,
    BeforeTax money NULL, TaxTotal money NULL, STaxTotal money NULL, TotalBase money NULL, TotalPref money NULL,
    TotalPaid money NULL, OrderTime datetime NULL, StsCode1 nvarchar(50) NULL, StsCode2 nvarchar(50) NULL,
    StsCode3 nvarchar(50) NULL, StsCode4 nvarchar(50) NULL, LastChgCashier varchar(20) NULL,
    LastChgTime datetime NULL);
CREATE TABLE dbo.tmpInvoiceInfo (CheckNo numeric(11,0) NOT NULL PRIMARY KEY, CurPage int NULL, CurLine int NULL,
    Printed int NULL, Ordered int NULL, PrintType int NULL);
CREATE TABLE dbo.checkopenlog (nSeq numeric(18,0) IDENTITY(1,1) NOT NULL PRIMARY KEY, RVC int NULL,
    CheckNo numeric(18,0) NULL, OpenBy varchar(50) NULL, OpenPC varchar(50) NULL, OpenTime datetime NULL,
    isOpen int NULL, LastAction datetime NULL);
CREATE TABLE dbo.tmpTrn (NYear int NULL, NMonth int NULL, NDay int NULL, TrnSeq numeric(18,0) NOT NULL,
    CheckNo numeric(11,0) NOT NULL, TrnTime datetime NULL, Cashier varchar(10) NULL, TrnCode int NULL,
    TrnSub int NULL, ItemCode varchar(30) NULL, TrnDesc varchar(30) NULL, ItemPrice money NULL,
    BaseSub money NULL, BaseTax money NULL, BaseTTL money NULL, TaxNo int NULL, TrnOrg money NULL,
    TrnBase money NULL, TrnPref money NULL, TrnQty float NULL, RVC int NULL, OrgCrCode int NULL, NewTrn int NULL,
    Refund int NULL, RefundRef numeric(18,0) NULL, AuthorCashier varchar(10) NULL, Status int NULL,
    ClassCode int NULL, GroupCode int NULL, StsCode1 varchar(50) NULL, StsCode2 varchar(50) NULL,
    StsCode3 varchar(50) NULL, StsCode4 varchar(50) NULL, BaseSrc int NULL, SrcNo int NULL, MealPeriod int NULL,
    ShiftPeriod int NULL, Split int NULL, FromRVC int NULL, BaseSTax money NULL, STaxNo int NULL,
    OrderNo int NULL, OrgCheck numeric(11,0) NULL, OrgPrice money NULL, DscRef numeric(18,0) NULL,
    Rsv1 varchar(50) NULL, Rsv2 varchar(30) NULL, Rsv3 varchar(50) NULL, Rsv4 varchar(50) NULL,
    PRIMARY KEY (CheckNo, TrnSeq));
SELECT TOP 0 * INTO dbo.VoidTrn FROM dbo.tmpTrn;
SELECT TOP 0 * INTO dbo.Trn FROM dbo.tmpTrn;
SELECT TOP 0 * INTO dbo.Invoice FROM dbo.tmpInvoice;
ALTER TABLE dbo.Invoice ADD CloseTime datetime NULL, CloseBy varchar(20) NULL, PONumber varchar(50) NULL,
    PaymentInfo varchar(100) NULL;
CREATE TABLE dbo.tmpTrnInfo (CheckNo numeric(11,0) NOT NULL, TrnNo numeric(18,0) NOT NULL, PToOrder bit NULL,
    POrderTime datetime NULL, PToCheck bit NULL, Void bit NULL, OrderQty float NULL, IsDone bit NULL,
    DoneTime datetime NULL, PRIMARY KEY (CheckNo, TrnNo));
CREATE TABLE dbo.CGTrn (TrnNo numeric(18,0) NOT NULL, ItemCode varchar(30) NULL, CGCode int NOT NULL,
    CGName varchar(100) NULL);
CREATE TABLE dbo.PDASendOrder (nSeq int IDENTITY(1,1) NOT NULL PRIMARY KEY, RVC int NULL,
    CheckNo numeric(18,0) NULL, SendBy varchar(50) NULL, SendTime datetime NULL);
CREATE TABLE dbo.TableReservation (nID int IDENTITY(1,1) PRIMARY KEY, GuestInfo nvarchar(200), NumAdult int,
    NumChild int, FromDateTime datetime, ToDateTime datetime, TableNum nvarchar(20), TelNum nvarchar(50),
    Booker nvarchar(50), CompanyName nvarchar(200), VATCode nvarchar(50), Address nvarchar(200),
    RVC nvarchar(10));
CREATE TABLE dbo.TableList (TableNum varchar(10) NOT NULL, RVC int NOT NULL, TableStatus int NULL,
    LastChangedTime datetime NULL, PRIMARY KEY (TableNum, RVC));
-- Staff sync (ADR 0101): FullName varchar (TCVN3), the optional IsDisable and Security columns present.
CREATE TABLE dbo.CashierDef (Initial varchar(10) NOT NULL PRIMARY KEY, FullName varchar(50) NULL,
    PassWord varchar(50) NULL, TouchID varchar(20) NULL, CardID varchar(50) NULL, IsDisable bit NULL, Security int NULL);
CREATE TABLE dbo.CashierRVC (Cashier varchar(10) NULL, RVC int NULL);
CREATE TABLE dbo.ClerkPermission (ClerkID varchar(10) NULL, FunctionID varchar(50) NULL, Allowed bit NULL);
CREATE TABLE dbo.DefRVCList (RVCNo int NOT NULL PRIMARY KEY, RVCName nvarchar(50) NULL, Picture image NULL);

INSERT dbo.Para (RVC, ParaName, ParaStr) VALUES (@rvc, 'CurYear', '2026'), (@rvc, 'CurMonth', '11'),
    (@rvc, 'CurDay', '14'), (@rvc, 'LastCheck', '0'), (@rvc, 'LastTrn', '0'), (@rvc, 'OrderPrintByMenu', '1');
INSERT dbo.MealPeriod (MealNo, RVCNo, FromTime, ToTime) VALUES (2, @rvc, 0, 2359);
INSERT dbo.CurrencyDef (CrCode, DecDiv) VALUES (1, 1);
INSERT dbo.ItemDef (ICode, IName, OpenItem, OpenPrice, IsPkg, ICurrency, Price) VALUES
    ('CF01', 'Coffee', 0, 0, 0, 1, 25000), ('CK01', 'Cake', 0, 0, 0, 1, 10000), ('CB01', 'Combo', 0, 0, 1, 1, 60000);
INSERT dbo.PkgDef (PkgCode, ItemCode, iQty, UseItemPrice, IPrice, DisplayOrder) VALUES
    ('CB01', 'CF01', 1, 0, 0, 1), ('CB01', 'CK01', 2, 1, 0, 2);
INSERT dbo.ItemMenu (MenuNo, RVC, MenuActive, PNo, MenuOrder) VALUES ('M01', @rvc, 1, 1, 1);
INSERT dbo.RVCItemMenu (RVC, ItemCode, ItemMenu) VALUES (@rvc, 'CF01', 'M01');
GO
CREATE OR ALTER PROCEDURE dbo.GetRVCSeqNumber @SeqName varchar(50), @RVC int, @GetBy nvarchar(50) = N'', @PC varchar(50) = ''
AS
BEGIN
    SET NOCOUNT ON;
    DECLARE @Next bigint;
    UPDATE dbo.Para SET @Next = CAST(ParaStr AS bigint) + 1, ParaStr = CAST(CAST(ParaStr AS bigint) + 1 AS varchar(200))
    WHERE RVC = @RVC AND ParaName = @SeqName;
    SELECT @Next;
END
GO
CREATE OR ALTER PROCEDURE dbo.PostITrn @CheckNo numeric(11,0), @TrnSeq numeric(18,0), @Cashier varchar(10),
    @ItemCode varchar(30), @TrnDesc varchar(30), @Price money, @Qty numeric(18,2), @OrderNo int,
    @ParentTrn varchar(50) = '', @isPkgBal int = 0, @TrnQty numeric(18,2) = NULL, @GuestNo int = 0,
    @PPolicy varchar(10) = '', @ItemDiscount money = 0, @ISubCode varchar(50) = ''
AS
BEGIN
    SET NOCOUNT ON;
    DECLARE @LineQty numeric(18,2) = ISNULL(@TrnQty, @Qty);
    INSERT dbo.tmpTrn (NYear, NMonth, NDay, TrnSeq, CheckNo, TrnTime, Cashier, TrnCode, ItemCode, TrnDesc, ItemPrice,
        BaseTTL, TrnQty, RVC, Status, StsCode4, MealPeriod, OrderNo, OrgPrice, Rsv1)
    SELECT h.IYear, h.IMonth, h.IDay, @TrnSeq, @CheckNo, GETDATE(), @Cashier, 100, @ItemCode,
        COALESCE(NULLIF(@TrnDesc, ''), (SELECT IName FROM dbo.ItemDef WHERE ICode = @ItemCode)), @Price,
        CASE WHEN @isPkgBal = 1 THEN 0 ELSE @Price * @LineQty END, @LineQty, h.RVC, 0, '', @GuestNo, @OrderNo, @Price,
        NULLIF(@ParentTrn, '')
    FROM dbo.tmpInvoice h WHERE h.CheckNo = @CheckNo;
END
GO
CREATE OR ALTER PROCEDURE dbo.UpdateTmpInvoice @CheckNo numeric(11,0)
AS
BEGIN
    SET NOCOUNT ON;
    UPDATE dbo.tmpInvoice
       SET SubTotal = ISNULL((SELECT SUM(BaseTTL) FROM dbo.tmpTrn WHERE CheckNo = @CheckNo AND TrnCode = 100), 0)
     WHERE CheckNo = @CheckNo;
END
GO
CREATE OR ALTER FUNCTION dbo.Unicode2TCVN (@Text nvarchar(4000)) RETURNS varchar(4000)
AS
BEGIN
    RETURN CAST(@Text AS varchar(4000));
END
GO
CREATE OR ALTER PROCEDURE dbo.CloseCheck @CheckNo numeric(11,0)
AS
    UPDATE dbo.tmpInvoice SET Status = 2 WHERE CheckNo = @CheckNo;
