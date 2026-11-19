CREATE TABLE Folio (
    ID                    numeric(9,0)    IDENTITY(1,1) NOT NULL,
    FolioNum              decimal(18,0)   NOT NULL,
    FolioSubNum           numeric(5,0)    NOT NULL DEFAULT 0,
    ConfirmNum            decimal(18,0)   NOT NULL,
    ExternalConfirmNumber varchar(50)     NULL,
    ExternalFolioNumber   varchar(50)     NULL,
    UserInfo1             varchar(200)    NULL,
    UserInfo2             varchar(200)    NULL,
    UserInfo3             varchar(200)    NULL,
    RoomTypeBooked        varchar(20)     NULL,
    RmChgTrn              int             NULL,  -- mã tiền phòng riêng của folio: TrnCode*100 + Sub (ADR-0105)
    ArrivalDate           datetime        NULL,
    DepartureDate         datetime        NULL,
    CheckInTime           datetime        NULL,
    CheckOutTime          datetime        NULL,
    CheckOutClerkID       varchar(3)      NULL,
    LastChangeTime        datetime        NULL,
    LastChangeClerkID     varchar(50)     NULL,
    CancelTime            datetime        NULL,
    NoShowFlag            bit             NULL,
    BookStatus            tinyint         NULL,
    PermFolioType         int             NULL,
    NumAdult              int             NULL,
    NumChild              int             NULL,
    RateAmount            decimal(18,2)   NULL,
    -- FirstName giữ LÝ DO của folio OOO (FolioStatus=5): LastName của folio đó luôn là hằng
    -- "** OUT-OF-ORDER **" và không mang thông tin, còn FirstName là chỗ lễ tân gõ lý do bảo trì.
    FirstName             varchar(500)    NULL,
    LastName              varchar(500)    NULL,
    OriginCode            varchar(10)     NULL,
    ProfileNumber         decimal(18,0)   NULL,
    MemberID              varchar(30)     NULL,
    FolioStatus           int             NOT NULL DEFAULT 1,
    CancelNum             decimal(18,0)   NOT NULL DEFAULT 0,
    RoomCode              varchar(20)     NULL,
    -- LOẠI PHÒNG THỰC TẾ khách đã ở (PMS thật: varchar(10)), đi cặp với RoomTypeBooked ở trên.
    -- Hai cột lệch nhau trên 17,9% folio đã trả phòng của dữ liệu thật — đổi/nâng phòng lúc
    -- nhận phòng là chuyện thường ngày, không phải ngoại lệ.
    RoomTypeCode          varchar(10)     NULL,
    -- Nhóm cột TA + phân loại thị trường (ADR-0041) — kiểu đúng như SMILE_FO thật, khoá ở
    -- SchemaContractTests. Hẹp hơn nhiều so với cột nguồn varchar(20) bên ClientSts.
    TravelAgent1Code      bigint          NULL,
    TravelAgent2Code      bigint          NULL,
    ContactName           nvarchar(30)    NULL,
    MarketSegmentCode     varchar(20)     NULL,
    -- Mã giá — khoá phân loại cho luật loại trừ tích điểm (ADR-0055).
    RoomRateCode          varchar(6)      NULL,
    MarketBusinessCode    varchar(3)      NULL,
    MarketChannelCode     varchar(3)      NULL,
    Geographic1Code       varchar(3)      NULL,
    Geographic2Code       varchar(3)      NULL,
    SourceGeographic1Code varchar(3)      NULL,
    SourceGeographic2Code varchar(3)      NULL,
    UserDefineCode1       varchar(3)      NULL,
    UserDefineCode2       varchar(3)      NULL,
    UserDefineCode3       varchar(3)      NULL,
    UserDefineCode4       varchar(3)      NULL,
    UserDefineCode5       varchar(3)      NULL,
    UserDefineCode6       varchar(3)      NULL,
    UserDefineCode7       varchar(3)      NULL,
    UserDefineCode8       varchar(3)      NULL,
    -- Nhóm cột "booking đầy đủ" mà Agent ghi khi tạo folio — kiểu đo trên SMILE_FO thật 2026-10-01.
    -- Đều là cột TUỲ CHỌN (SmileSchemaProbe.OptionalColumns): bản cài thiếu cột thì Agent bỏ qua.
    TelNumM               varchar(255)    NULL,
    MailM                 varchar(255)    NULL,
    PaymentCode           varchar(3)      NULL,
    RateOverideFlag       bit             NULL,
    FolioCurrencyCode     varchar(3)      NULL,
    FolioExRate           numeric(18,2)   NULL,
    UpdateExRateAtCI      bit             NULL,
    NumRoom               int             NULL,
    WalkInFlag            bit             NULL,
    -- Dấu vết tạo/huỷ + cờ mặc định folio lễ tân — kiểu đo trên SMILE_FO thật 2026-10-02, đều NULL
    -- không DEFAULT, đều là cột TUỲ CHỌN.
    ReservationTime       datetime        NULL,
    ReservationClerkID    varchar(3)      NULL,
    CancelClerkID         varchar(50)     NULL,
    NetRate               bit             NULL,
    Booker                nvarchar(50)    NULL,
    IncludeBF             bit             NULL,
    BreakfastCode         varchar(3)      NULL,
    ConfirmPrintFlag      bit             NULL,
    DscPcnt               numeric(18,2)   NULL,
    DscAmount             numeric(18,2)   NULL,
    ExtraBed              int             NULL,
    NumEnf                int             NULL,
    -- Folio thuộc đoàn: GroupCode = GroupBlockDetail.AllocationID (varchar(20) cả hai bên trên bản thật).
    GroupCode             varchar(20)     NULL,
    SubGroupCode          varchar(20)     NULL,
    CONSTRAINT PK_Folio PRIMARY KEY (FolioNum, FolioSubNum)
);

GO

-- Danh mục khách hàng/TA của SMILE (rút gọn) và phân loại thị trường đi kèm — nguồn hồ sơ TA.
CREATE TABLE CLIENT (
    ClientFolioNum numeric(18,0)  NOT NULL PRIMARY KEY,
    ClientName     nvarchar(100)  NULL,
    Deleted        bit            NULL
);

GO

CREATE TABLE ClientSts (
    ClientCode bigint       NOT NULL,
    Origin     varchar(20)  NULL,
    MSegment   varchar(20)  NULL,
    MBusiness  varchar(20)  NULL,
    MChanel    varchar(20)  NULL,
    GEO1       varchar(20)  NULL,
    GEO2       varchar(20)  NULL,
    UDef1      varchar(20)  NULL,
    UDef2      varchar(20)  NULL,
    UDef3      varchar(20)  NULL,
    UDef4      varchar(20)  NULL,
    UDef5      varchar(20)  NULL,
    UDef6      varchar(20)  NULL,
    UDef7      varchar(20)  NULL,
    UDef8      varchar(20)  NULL
);

GO

CREATE TABLE FolioRate (
    FolioRateId  int identity(1,1) PRIMARY KEY,
    FolioNum     decimal(18,0)  NOT NULL,
    hDate        datetime       NOT NULL,
    ToDate       datetime       NOT NULL,
    RateAmount   decimal(18,2)  NOT NULL,
    isCancel     bit            NOT NULL DEFAULT 0,
    RateOverride int            NULL,
    CreateBy     varchar(30)    NULL,
    CreateTime   datetime       NULL,
    DscPcnt      numeric(18,2)  NULL,
    DscAmount    numeric(18,2)  NULL
);

GO

CREATE TABLE AdditionalName (
    IdAddition       numeric(9,0)   IDENTITY(1,1) NOT NULL PRIMARY KEY,
    FolioNum         decimal(18,0)  NOT NULL,
    FolioSubNum      numeric(5,0)   NULL,  -- nullable, no default: matches SMILE_FO; ~29% of real guest rows are NULL
    ConfirmNum       numeric(18,0)  NULL,
    AdtStatus        int            NULL,
    IsChild          bit            NULL,
    VIPLevel         int            NULL,
    AOriginCode      varchar(10)    NULL,
    FirstName        varchar(500)   NULL,
    LastName         varchar(500)   NULL,
    MobileNum        varchar(255)   NULL,
    TeleNum          varchar(255)   NULL,
    ArrivalTime      datetime       NULL,
    DepartureTime    datetime       NULL,
    Email            varchar(255)   NULL,
    BirthDate        datetime       NULL,
    Gender           bit            NULL,
    NationalityCode  varchar(3)     NULL,
    ACancelNum       numeric(19,0)  NOT NULL DEFAULT 0
);

GO

-- Nhật ký thay đổi folio của SMILE — kiểu đo trên SMILE_FO thật 2026-10-02 (mọi cột NULL). Do trigger
-- triggerFolioUpdate ghi, Agent KHÔNG BAO GIỜ ghi thẳng.
CREATE TABLE FolioLog (
    FolioID      bigint         NULL,
    FolioNum     numeric(18,0)  NULL,
    FolioSubNum  int            NULL,
    FieldName    varchar(50)    NULL,
    FieldType    varchar(50)    NULL,
    OldValue     nvarchar(500)  NULL,
    NewValue     nvarchar(500)  NULL,
    ChangedClerk varchar(10)    NULL,
    ChangedTime  datetime       NULL,
    HotelID      varchar(30)    NULL
);

GO

CREATE TABLE FolioLogDef (
    FieldName        varchar(30)  NULL,
    FieldDescription varchar(50)  NULL,
    LogIfChanged     bit          NULL,
    HotelID          varchar(30)  NULL
);

GO

-- Bản thật có ~165 trường, tất cả LogIfChanged = 1; fixture giữ tập con Agent chạm tới.
INSERT INTO FolioLogDef (FieldName, FieldDescription, LogIfChanged) VALUES
    ('ArrivalDate', 'Arrival Date', 1), ('DepartureDate', 'Departure Date', 1),
    ('NumAdult', 'Adult', 1), ('NumChild', 'Child', 1), ('RateAmount', 'Rate Amount', 1),
    ('LastName', 'Last Name', 1), ('RoomTypeBooked', 'Room Type Booked', 1),
    ('CancelNum', 'Cancel Number', 1), ('CancelTime', 'Cancel Time', 1),
    ('CancelClerkID', 'Cancel Clerk', 1), ('LastChangeTime', 'Last Change Time', 1),
    ('LastChangeClerkID', 'Last Change Clerk', 1);

GO

-- BẢN SAO NGUYÊN VĂN phần ghi FolioLog của trigger thật (OBJECT_DEFINITION trên SMILE_FO, 2026-10-02);
-- bỏ phần cập nhật Room.HSKPOccupied và AdditionalName.ArrivalTime/DepartureTime. Giữ đúng hành vi
-- ghép chuỗi: LastChangeClerkID NULL ⇒ cả @SQL thành NULL ⇒ EXEC không làm gì ⇒ KHÔNG có dòng log nào.
-- Không SET NOCOUNT như bản thật: số dòng UPDATE Folio trả về gồm cả dòng trigger ghi.
CREATE TRIGGER [dbo].[triggerFolioUpdate] ON [dbo].[Folio]
FOR  UPDATE
AS
Begin
Select * into #TmpI from Inserted
Select * into #TmpD from Deleted

Declare @SQL varchar(2000)
Declare @Field varchar(100)
Declare @FDesc varchar(100)
Declare @FolioNum_New Varchar(30),@FolioSubNum_New Varchar(30),@LastChangeClerkID varchar(6) ,@FolioID Varchar(30)

SELECT  @FolioNum_New=FolioNum,@FolioSubNum_New=Coalesce(FolioSubNum,0),@LastChangeClerkID=LastChangeClerkID,
     @FolioID=[ID] From Inserted

DECLARE CField CURSOR FOR Select FieldName,FieldDescription FROM FolioLogDef  Where LogIfChanged =1
OPEN CField
FETCH NEXT FROM CField Into @Field,@FDesc
WHILE @@FETCH_STATUS = 0
BEGIN
    Set @SQL='
    Declare @ValI Varchar(500)
    Declare @ValD Varchar(500)
       SELECT @ValI = ' + @Field + ' FROM #TmpI WHERE [ID] = ''' + @FolioID + '''
       SELECT @ValD = ' + @Field + ' FROM #TmpD WHERE [ID] = ''' + @FolioID + '''

    If Coalesce(@ValI,'''')<>Coalesce(@ValD,'''')
        Insert InTo FolioLog(FolioID,FolioNum,FolioSubNum,FieldName,OldValue,NewValue,ChangedClerk,ChangedTime)
              Values(' + @FolioID + ',' + @FolioNum_New + ',' + @FolioSubNum_New + ',''' +  @Field + ''',@ValD,@ValI,''' + @LastChangeClerkID + ''',GetDate())
    '
    Exec(@SQL)
    FETCH NEXT FROM CField Into @Field,@FDesc
END
CLOSE CField
DEALLOCATE CField

Drop Table #TmpI
Drop Table #TmpD
End

GO

CREATE TABLE FolioTransaction (
    TransactionID      numeric(9,0)   IDENTITY(1,1) NOT NULL PRIMARY KEY,
    FolioNum           varchar(10)    NOT NULL,
    TransactionCode    varchar(5)     NULL,
    TransactionSubCode numeric(7,0)   NULL,
    TransactionAmount  numeric(19,2)  NOT NULL,
    FolioBalanceCode   varchar(2)     NULL,
    RefNumber          varchar(30)    NULL,
    Discount           numeric(19,2)  NULL,
    Comment            nvarchar(200)  NULL,
    PostingClerkID     varchar(3)     NULL,
    SystemTime         datetime       NULL,
    TransactionDate    datetime       NULL,
    VoidTransactionFlag bit           NULL
);

GO

-- Bảng danh mục mã giao dịch — ScanFolioTransactionsAsync JOIN vào đây lấy Description để Hub
-- audit/hiển thị. Composite key (TransactionCode, TransactionSubCode) khớp đúng cách FolioWriter
-- JOIN (tránh nhân bản dòng nếu 1 TransactionCode có nhiều subcode).
CREATE TABLE TransactionCode (
    TransactionCode    varchar(20)  NOT NULL,
    TransactionSubCode varchar(3)   NOT NULL,
    Description        nvarchar(50) NULL,
    -- Thuế/phí phục vụ + AutoPost (1 = mã tiền phòng): ExtraBedTermsReader so mã EB với mã phòng (§9.4).
    TaxCode            varchar(5)   NULL,
    ServiceChargeCode  varchar(5)   NULL,
    STaxCode           varchar(5)   NULL,
    AutoPost           int          NULL,  -- 1 = mã tiền phòng night audit tự ghi (ADR-0105)
    PRIMARY KEY (TransactionCode, TransactionSubCode)
);

GO

-- Cấu hình SMILE mà EarnPairReader suy cặp mã tích điểm từ đó (spec §9.8.1). Kiểu theo sys.columns thật.
-- SpecialService/GlobalParameter khai một lần ở dưới (dùng chung với ExtraBedTermsReader).
CREATE TABLE PackageCode (
    PackageCode varchar(20) NOT NULL,
    TrnCode     varchar(6)  NULL,
    TrnSubCode  varchar(6)  NULL
);
GO

CREATE TABLE FolioNumberCounter (Value decimal(18,0) NOT NULL);
INSERT INTO FolioNumberCounter VALUES (0);

CREATE TABLE ConfirmNumberCounter (Value decimal(18,0) NOT NULL);
INSERT INTO ConfirmNumberCounter VALUES (0);

CREATE TABLE CancelNumberCounter (Value decimal(18,0) NOT NULL);
INSERT INTO CancelNumberCounter VALUES (0);

CREATE TABLE ProfileNumberCounter (Value decimal(18,0) NOT NULL);
INSERT INTO ProfileNumberCounter VALUES (0);

CREATE TABLE Profile (
    ProfileNumber          decimal(18,0) NOT NULL,
    ExternalProfileNumber  varchar(12) NULL,
    LastName               nvarchar(100) NULL,
    FirstVisitDate         datetime NULL,
    NumVisit               int NULL,
    NumNight               int NULL,
    TotalRevenue           decimal(18,2) NULL,
    FirstName              nvarchar(50) NULL,
    PhoneNum               varchar(20) NULL,
    Email                  nvarchar(50) NULL,
    BirthDate              datetime NULL,
    Gender                 bit NULL,
    CountryCode            nvarchar(20) NULL
);

GO

-- Danh mục LOẠI PHÒNG của PMS. KHÔNG bao phủ hết thực tế: trên dữ liệu thật mã 'DLX QD' nằm
-- trên 46 folio mà không có dòng nào ở đây — nên Hub còn ghi nhận cả mã nhìn thấy trên folio.
CREATE TABLE RoomType (
    RoomTypeCode  varchar(10)   NOT NULL PRIMARY KEY,
    Description   varchar(70)   NULL,
    NumRoom       int           NULL,
    DisplayOrder  int           NULL,
    TrnCode       int           NULL,  -- mã tiền phòng night audit của loại phòng (0 = theo AutoPost)
    TrnSubCode    int           NULL
);

GO

-- Kiểu/độ rộng lấy từ sys.columns của SMILE_FO thật. RoomCode của Room là varchar(10) còn của
-- RoomOOS là varchar(30) — lệch nhau ngay trong PMS thật, giữ nguyên để không giấu mất chuyện đó.
-- KHÔNG có PRIMARY KEY và cũng không có index nào: đo trên SMILE_FO thật (2026-09-20) thì `Room`
-- là một HEAP, `RoomCode` nullable. Đặt PK ở đây cho "sạch" là giấu mất hai sự thật cùng lúc —
-- SQL Server không cho PK trên cột nullable, nên fixture sẽ phải sửa cột thành NOT NULL và câu
-- SQL sản xuất mất luôn lý do tồn tại của mệnh đề `r.RoomCode IS NOT NULL`.
CREATE TABLE Room (
    RoomCode      varchar(10)  NULL,
    RoomName      varchar(50)  NULL,
    RoomTypeCode  varchar(10)  NULL,
    Floor         smallint     NULL,
    RStatus       int          NULL,
    HotelID       varchar(30)  NULL,
    -- Housekeeping THẬT của bản cài này (RStatus NULL trên 262/262 phòng thật, đo 2026-09-20):
    -- HSKPClean = đã dọn, Inspected = đã kiểm tra sau dọn, TouchedUp = số lần dọn nhẹ (touch-up).
    HSKPClean     bit          NULL,
    Inspected     bit          NULL,
    TouchedUp     int          NULL
);

GO

-- BẢNG TRA, không phải trạng thái từng phòng. Đã seed sẵn đúng 5 mã của bản cài thật.
CREATE TABLE RoomStatus (
    StsCode   int          NOT NULL PRIMARY KEY,
    StsDesc   varchar(50)  NULL,
    stsColor  int          NULL,
    HotelID   varchar(30)  NULL
);

GO

INSERT INTO RoomStatus (StsCode, StsDesc) VALUES
    (0, 'Inspected'), (1, 'Clean'), (2, 'Touch'), (3, 'Dirty'), (4, 'StopSale');

GO

CREATE TABLE RoomOOS (
    nSeq        bigint identity(1,1) PRIMARY KEY,
    RoomCode    varchar(30)   NULL,
    SetTime     datetime      NULL,
    SetBy       varchar(30)   NULL,
    ClearTime   datetime      NULL,
    ClearBy     varchar(30)   NULL,
    isCleared   int           NULL,
    HSKPNotes   varchar(200)  NULL,
    ENGNotes    varchar(200)  NULL,
    OOSCode     varchar(30)   NULL,
    HotelID     varchar(30)   NULL,
    FromDate    datetime      NULL,
    ToDate      datetime      NULL
);

GO

-- Đoàn: bảng chủ GroupFolio + dòng giữ phòng GroupBlockDetail. Kiểu/độ rộng từ sys.columns của
-- SMILE_FO thật (2026-10-03), chỉ dựng các cột Agent đọc. Cả hai là HEAP không index, mọi cột trừ ID
-- đều nullable — giữ y như thật. Huỷ đoàn chỉ bật GroupFolio.IsCanceled; dòng block vẫn Cancelled = 0.
CREATE TABLE GroupFolio (
    ID               numeric(18,0) identity(1,1) NOT NULL,
    GroupCode        varchar(20)   NULL,
    GroupStatusCode  char(1)       NULL,
    IsCanceled       bit           NULL,
    CancelTime       datetime      NULL,
    ConfirmStatus    int           NULL,
    ReleaseDate      datetime      NULL,
    ReleaseDays      int           NULL,
    HotelID          varchar(30)   NULL
);

GO

CREATE TABLE GroupBlockDetail (
    ID                   int          identity(1,1) NOT NULL,
    InventoryChangeType  char(1)      NULL,
    AllocationType       char(1)      NULL,
    AllocationID         varchar(20)  NULL,
    SubGroupCode         varchar(20)  NULL,
    RoomTypeCode         varchar(10)  NULL,
    FromDate             datetime     NULL,
    ToDate               datetime     NULL,
    NumRoom              int          NULL,
    NumPickup            int          NULL,
    Cancelled            bit          NULL,
    DGroupStatus         int          NULL,
    DReleaseDate         datetime     NULL,
    DReleaseDays         int          NULL,
    HotelID              varchar(30)  NULL
);

GO

-- Một dòng duy nhất trên bản thật; fixture để TRỐNG (câu quét rơi về hôm nay của Agent) — test cần
-- HotelDate cụ thể tự chèn rồi tự xoá.
CREATE TABLE HotelParameter (
    HotelID    varchar(30)  NULL,
    HotelDate  datetime     NULL
);

GO

-- Tham số SMILE dạng (HotelID, ParaName, ParaString) — Agent CHỈ ĐỌC các dòng giường phụ (§9.7).
-- Fixture để TRỐNG; test tự chèn bằng SeedExtraBedTermsAsync trong database riêng của nó.
CREATE TABLE GlobalParameter (
    HotelID    varchar(30)   NULL,
    ParaName   varchar(30)   NULL,   -- kiểu theo SMILE_FO thật (SchemaContractTests)
    ParaString varchar(250)  NULL,
    Status     int           NULL
);

GO

-- Tồn giường phụ theo HotelID (SUM(NumBed)) — CHỈ ĐỌC.
CREATE TABLE ExtraBedInv (
    CruiseCode varchar(10) NULL,
    NumBed     int         NULL,
    HotelID    varchar(30) NULL
);

GO

-- Danh mục SpecialService — CHỈ ĐỌC (dòng của Smile:ExtraBedSpecialCode, §9.7). Fixture để TRỐNG; test tự
-- chèn bằng SeedExtraBedSpecialAsync.
CREATE TABLE SpecialService (
    SpecialServiceCode  varchar(2)    NOT NULL PRIMARY KEY,  -- SMILE_FO thật: varchar(2)
    Description         nvarchar(50)  NULL,
    AmountCharge        numeric(18,2) NULL,
    IncludeInRoomCharge bit           NULL,
    OpenPrice           bit           NULL,
    TrnCode             varchar(5)    NULL,
    TrnSubCode          varchar(5)    NULL
);

GO

-- SpecialService ghi theo folio (NAPostSpecial_Service): SMILE đếm giường phụ = SUM(Qty) của mã EB trên các
-- đêm có DayNo (đêm đến = 1) trong FromDay..ToDay — CHỈ ĐỌC (đặc tả occupancy pricing §9.3 rev 21b). Tên cột
-- theo đặc tả; schema SMILE thật chưa đối chiếu (bài VM §9.4.2).
CREATE TABLE Folio_Special (
    FolioNum    decimal(18,0) NOT NULL,
    nSeq        int           IDENTITY(1,1) NOT NULL,
    SpecialCode varchar(10)   NULL,
    Qty         int           NULL,
    Price       numeric(18,2) NULL,
    FromDay     int           NULL,
    ToDay       int           NULL,
    SPNotes     nvarchar(255) NULL,
    SalesPerson varchar(20)   NULL,
    -- Huỷ theo dòng (ExtrabedSumary lọc CancelNum = 0, đặc tả §9.1). Kiểu theo Folio.CancelNum; chưa đối chiếu.
    CancelNum   decimal(18,0) NULL,
    CONSTRAINT PK_Folio_Special PRIMARY KEY (FolioNum, nSeq)
);

GO

CREATE PROCEDURE GetFolioNumber
AS
BEGIN
    UPDATE FolioNumberCounter SET Value = Value + 1;
    SELECT Value AS FolioNumber, 0 AS FolioSubNumber FROM FolioNumberCounter;
END

GO

CREATE PROCEDURE GetConfirmNum
AS
BEGIN
    UPDATE ConfirmNumberCounter SET Value = Value + 1;
    SELECT Value AS ConfNumber FROM ConfirmNumberCounter;
END

GO

CREATE PROCEDURE GetCancelNum
AS
BEGIN
    UPDATE CancelNumberCounter SET Value = Value + 1;
    SELECT Value AS CancelNumber FROM CancelNumberCounter;
END

GO

CREATE PROCEDURE GetProfileNumber
AS
BEGIN
    UPDATE ProfileNumberCounter SET Value = Value + 1;
    SELECT Value AS ProfileNumber FROM ProfileNumberCounter;
END

GO

-- Chữ ký lấy đúng theo OBJECT_DEFINITION của SP thật trên SMILE_FO (đọc read-only 2026-09-20).
-- SP thật có 38 tham số, nhưng từ tham số 21 trở đi đều CÓ giá trị mặc định nên lời gọi 20 tham
-- số của FolioWriter chạy được; fixture chỉ dựng 20 tham số đầu, đúng phần FolioWriter truyền.
-- Độ rộng ở đây HẸP HƠN bản cũ vì bản cũ tự nới ra: @TransactionCode thật là varchar(10) chứ
-- không phải varchar(20), @PostingClerkID varchar(10) chứ không phải varchar(20), @RefNumber
-- varchar(30) chứ không phải varchar(50). Khai rộng hơn thật làm test nuốt mất đúng loại lỗi
-- truncate mà nó có nhiệm vụ bắt.
CREATE PROCEDURE PostOneTransaction
    @FolioNum            varchar(10),
    @BillAmount          numeric(18,2),
    @BillExRate          numeric(18,2),
    @TransactionCode     varchar(10),
    @TransactionSubCode  varchar(10),
    @sTransactionDate    varchar(20)   = NULL,
    @RoomCode            varchar(10)   = '',
    @RefNumber           varchar(30)   = '',
    @TransactionAmount   numeric(18,2),
    @FolioBalanceCode    varchar(5)    = 'A',
    @MealPeriodNumber    int           = 0,
    @Comment             nvarchar(200) = '',
    @sSystemTime         varchar(50)   = NULL,
    @OriginAmount        numeric(18,2) = 0,
    @OriginCurrency      varchar(20)   = '',
    @OriginFolioNumber   varchar(10)   = '',
    @PostingClerkID      varchar(10)   = '',
    @NumberCover         int           = 0,
    @OriginRoom          varchar(10)   = '',
    @Discount            numeric(18,2) = 0
AS
BEGIN
    -- VoidTransactionFlag = 0: bút toán vừa post chưa bị huỷ. Ghi tường minh thay vì để NULL
    -- để câu lọc doanh thu (ISNULL(VoidTransactionFlag, 0) = 0) được chạy trên dữ liệu
    -- giống bản thật, chứ không chỉ trên nhánh NULL.
    INSERT INTO FolioTransaction
        (FolioNum, TransactionCode, TransactionAmount, FolioBalanceCode, RefNumber, Discount,
         Comment, PostingClerkID, SystemTime, VoidTransactionFlag)
    VALUES
        (@FolioNum, @TransactionCode, @TransactionAmount, @FolioBalanceCode,
         @RefNumber, @Discount, @Comment, @PostingClerkID,
         ISNULL(TRY_CONVERT(datetime, @sSystemTime), GETDATE()), 0);
END
