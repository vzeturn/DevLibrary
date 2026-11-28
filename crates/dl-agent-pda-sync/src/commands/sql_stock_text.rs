//! The SQL text of the SMILE_BO stock unit (RT-1427), generated from the C# raw strings of
//! `SqlTxnStockUnit.cs`, `SqlTxnStockUnit.Issue.cs` and `SqlTxnStockUnit.Receipt.cs` so the
//! two agents send the same statements; `{0}` is the bracketed BO database, `{1}`/`{2}` the
//! generated rows and the numbering batch. Every line is checked against the C# source by the
//! tests below. The database name is the only interpolated text (validated first).

/// `HAS_DBACCESS(@Name)`: whether `pda_sync` may use the database.
pub(super) const DATABASE_ACCESS_SQL: &str = r#"SELECT HAS_DBACCESS(@Name);"#;

/// The store map row of one cloud store (`Pda_TxnStockMap`, script 170).
pub(super) const MAP_SQL: &str = r#"SELECT StoreCode, BoDatabase, StoreRvc, WithRvc, DeptCode, JType FROM dbo.Pda_TxnStockMap WHERE StoreCode = @StoreCode;"#;

pub(super) const STORES_SQL: &str =
    r#"SELECT COUNT(DISTINCT RVCNo) FROM {0}.dbo.DefRVCList WHERE RVCNo IN ({1});"#;

pub(super) const READY_SQL: &str = r#"SELECT CASE WHEN OBJECT_ID(N'{0}.dbo.tmpINVVoucher', N'U') IS NULL OR OBJECT_ID(N'{0}.dbo.tmpINVTrn', N'U') IS NULL
              OR OBJECT_ID(N'{0}.dbo.INVTrn', N'U') IS NULL OR OBJECT_ID(N'{0}.dbo.INVVoucher', N'U') IS NULL
              OR OBJECT_ID(N'{0}.dbo.GlobPara', N'U') IS NULL OR OBJECT_ID(N'{0}.dbo.JType', N'U') IS NULL
              OR OBJECT_ID(N'{0}.dbo.ItemDef', N'U') IS NULL OR OBJECT_ID(N'{0}.dbo.DefRVCList', N'U') IS NULL
              OR OBJECT_ID(N'{0}.dbo.INVUpdateGLAvg', N'P') IS NULL THEN 0 ELSE 1 END,
       CASE WHEN OBJECT_ID(N'{0}.dbo.IVoucherType', N'U') IS NULL THEN 0 ELSE 1 END;"#;

pub(super) const VOUCHER_TYPES_SQL: &str = r#"SELECT COUNT(*) FROM {0}.dbo.IVoucherType;"#;

pub(super) const SX_PROFILE_SQL: &str = r#"SELECT ITrnCode, ITrnSub, COUNT_BIG(*) FROM {0}.dbo.INVTrn WHERE PApp = 'SX' GROUP BY ITrnCode, ITrnSub ORDER BY ITrnCode, ITrnSub;"#;

pub(super) const SETTINGS_SQL: &str = r#"SELECT (SELECT TOP 1 TRY_CONVERT(int, LTRIM(RTRIM(CONVERT(varchar(255), p.ParaStr)))) FROM {0}.dbo.GlobPara p
         WHERE LTRIM(RTRIM(p.ParaName)) = 'INVCostMethod' ORDER BY DATALENGTH(p.ParaName)),
       (SELECT TOP 1 TRY_CONVERT(int, LTRIM(RTRIM(CONVERT(varchar(255), p.ParaStr)))) FROM {0}.dbo.GlobPara p
         WHERE LTRIM(RTRIM(p.ParaName)) = 'INVPeriod' ORDER BY DATALENGTH(p.ParaName)),
       (SELECT TOP 1 TRY_CONVERT(int, LTRIM(RTRIM(CONVERT(varchar(255), p.ParaStr)))) FROM {0}.dbo.GlobPara p
         WHERE LTRIM(RTRIM(p.ParaName)) = 'GLPeriod' ORDER BY DATALENGTH(p.ParaName));"#;

pub(super) const UNKNOWN_ITEMS_SQL: &str = r#"SELECT v.ICode FROM (VALUES {1}) v (ICode) WHERE NOT EXISTS (SELECT 1 FROM {0}.dbo.ItemDef x WHERE x.ICode = v.ICode);"#;

pub(super) const HELD_SQL: &str = r#"SELECT ItemCode, StoreCode, Qty, BusinessDate FROM dbo.Pda_TxnStockHeld WITH (UPDLOCK, HOLDLOCK) WHERE AggregateKey = @Key ORDER BY ItemCode, StoreCode;"#;

pub(super) const SAVE_HELD_SQL: &str =
    r#"DELETE FROM dbo.Pda_TxnStockHeld WHERE AggregateKey = @Key;"#;

pub(super) const INSERT_HELD_SQL: &str = r#"INSERT INTO dbo.Pda_TxnStockHeld (AggregateKey, ItemCode, StoreCode, Qty, BusinessDate) VALUES (@Key, @Item, @Store, @Qty, @Day);"#;

/// The voucher number batch shared by the issue voucher and the count (`{0}` is the bracketed database).
pub(super) const VOUCHER_NUMBERING_SQL: &str = r#"DECLARE @J varchar(20) = NULLIF(LTRIM(RTRIM(@JType)), ''), @Init varchar(30), @Len int, @Last bigint, @Year bit, @Month bit, @Lock int;
IF @J IS NULL
    SELECT TOP (1) @J = LTRIM(RTRIM(j.JType)) FROM {0}.dbo.JType j WHERE LTRIM(RTRIM(j.VoucherType)) = 'SX' ORDER BY j.JType DESC;
SELECT TOP (1) @Init = COALESCE(LTRIM(RTRIM(j.VoucherInit)), ''), @Len = j.VoucherLen, @Last = j.LastVoucher,
       @Year = j.VoucherYearReset, @Month = j.VoucherMonthReset
  FROM {0}.dbo.JType j WHERE LTRIM(RTRIM(j.JType)) = @J AND LTRIM(RTRIM(j.VoucherType)) = 'SX';
IF @Init IS NULL
BEGIN
    SELECT CAST(NULL AS bigint);
    RETURN;
END;
DECLARE @Res nvarchar(255) = @LockPrefix + @J;
EXEC @Lock = sp_getapplock @Resource = @Res, @LockMode = 'Exclusive', @LockOwner = 'Transaction', @LockTimeout = 10000;
IF @Lock < 0 THROW 51310, 'Another command holds the SMILE_BO stock voucher counter of this JType; it is retried.', 1;
-- Re-read the counter under the applock and a row lock held to the commit, so the vendor app can not move it meanwhile.
SELECT TOP (1) @Last = j.LastVoucher FROM {0}.dbo.JType j WITH (UPDLOCK, HOLDLOCK)
 WHERE LTRIM(RTRIM(j.JType)) = @J AND LTRIM(RTRIM(j.VoucherType)) = 'SX';
DECLARE @From int = CASE WHEN @Month = 1 THEN @P WHEN @Year = 1 THEN (@P / 100) * 100 + 1 ELSE 0 END,
        @To int = CASE WHEN @Month = 1 THEN @P WHEN @Year = 1 THEN (@P / 100) * 100 + 12 ELSE 999912 END, @Max bigint;
SELECT @Max = MAX(TRY_CONVERT(bigint, s.Tail))
  FROM (SELECT LTRIM(RTRIM(v.VNo)) AS No, v.VPeriod FROM {0}.dbo.INVVoucher v
        UNION ALL
        SELECT LTRIM(RTRIM(v.VNo)), v.VPeriod FROM {0}.dbo.tmpINVVoucher v) u
 CROSS APPLY (SELECT SUBSTRING(u.No, LEN(@Init) + 1, 40) AS Tail) s
 WHERE u.VPeriod BETWEEN @From AND @To
   AND LEFT(u.No, LEN(@Init)) = @Init AND LEN(s.Tail) BETWEEN 1 AND 18 AND s.Tail NOT LIKE '%[^0-9]%';
DECLARE @Seq bigint = COALESCE(@Max, 0);
IF COALESCE(@Month, 0) = 0 AND COALESCE(@Year, 0) = 0 AND COALESCE(@Last, 0) > @Seq SET @Seq = @Last;
SET @Seq += 1;
-- A counter that never resets is the vendor's floor too: move it forward like the POS stock link does (never back).
IF COALESCE(@Month, 0) = 0 AND COALESCE(@Year, 0) = 0
    UPDATE {0}.dbo.JType SET LastVoucher = @Seq
     WHERE LTRIM(RTRIM(JType)) = @J AND LTRIM(RTRIM(VoucherType)) = 'SX' AND (LastVoucher IS NULL OR LastVoucher < @Seq);
DECLARE @Digits varchar(20) = CONVERT(varchar(20), @Seq),
        @Width int = CASE WHEN COALESCE(@Len, 0) - LEN(@Init) > 1 THEN @Len - LEN(@Init) ELSE 1 END, @No varchar(60);
SET @No = CONCAT(@Init, CASE WHEN LEN(@Digits) < @Width THEN REPLICATE('0', @Width - LEN(@Digits)) ELSE '' END, @Digits);
IF LEN(@No) > 20
BEGIN
    SELECT CAST(NULL AS bigint);
    RETURN;
END;
DECLARE @VNo varchar(20) = @No;"#;

pub(super) const COUNT_SQL: &str = r#"DECLARE @SRes nvarchar(255) = @LockPrefix + N'count:' + CONVERT(nvarchar(20), @StoreRvc), @SLock int;
EXEC @SLock = sp_getapplock @Resource = @SRes, @LockMode = 'Exclusive', @LockOwner = 'Transaction', @LockTimeout = 10000;
IF @SLock < 0 THROW 51312, 'Another command is counting this SMILE_BO store; it is retried.', 1;
IF EXISTS (SELECT 1 FROM {0}.dbo.InvSession WITH (UPDLOCK, HOLDLOCK) WHERE RVC = @StoreRvc AND COALESCE(InvStatus, 0) IN (0, 1, 2))
BEGIN
    SELECT CAST(-1 AS bigint);
    RETURN;
END;
DECLARE @D TABLE (Ord int NOT NULL PRIMARY KEY, ICode varchar(20) NOT NULL, SystemQty numeric(18, 5) NOT NULL, CountedQty numeric(18, 5) NOT NULL);
INSERT INTO @D (Ord, ICode, SystemQty, CountedQty)
SELECT l.Ord, l.ICode, CONVERT(numeric(18, 5), COALESCE(s.VEnd, 0)), l.Qty
  FROM (VALUES {1}) l (Ord, ICode, Qty)
 OUTER APPLY (SELECT TOP 1 v.VEnd FROM {0}.dbo.Inventory v WHERE v.VPeriod = @P AND v.RVC = @StoreRvc AND v.VICode = l.ICode) s;
DECLARE @SOut TABLE (SSeq bigint NOT NULL);
INSERT INTO {0}.dbo.InvSession (VPeriod, RVC, InvDate, InvDesc, InvBy, PostLoc, PostDept)
OUTPUT inserted.vSeq INTO @SOut (SSeq)
VALUES (@P, @StoreRvc, @VDate, LEFT(@VDesc, 50), LEFT(@User, 50), @WithRvc, @Dept);
DECLARE @SSeq bigint = (SELECT SSeq FROM @SOut);
INSERT INTO {0}.dbo.InvSSDtl (vSeq, ICode, ICount, ManCount, CountBy, StockCount, IName, IUnit, UnitName)
SELECT @SSeq, d.ICode, d.CountedQty, 1, LEFT(@User, 20), d.SystemQty, LEFT(i.IName, 50), i.IUnit, LEFT(RTRIM(u.UnitName), 50)
  FROM @D d
 OUTER APPLY (SELECT TOP 1 x.IName, x.IUnit FROM {0}.dbo.ItemDef x WHERE x.ICode = d.ICode ORDER BY x.IClass) i
 OUTER APPLY (SELECT TOP 1 y.UnitName FROM {0}.dbo.IUnitDef y WHERE y.UnitCode = i.IUnit) u
 ORDER BY d.Ord;
DECLARE @ResVSeq bigint = NULL, @ResVNo varchar(20) = NULL;
IF EXISTS (SELECT 1 FROM @D WHERE CountedQty <> SystemQty)
BEGIN
{2}
    DECLARE @Out TABLE (VSeq bigint NOT NULL);
    INSERT INTO {0}.dbo.tmpINVVoucher (VNo, VDesc, VDate, VPeriod, PDate, PUser, VRef, VType, sApp)
    OUTPUT inserted.VSeq INTO @Out (VSeq)
    VALUES (@VNo, @VDesc, @VDate, @P, GETDATE(), LEFT(@User, 10), CONVERT(varchar(30), @SSeq), @J, 'SX');
    SET @ResVSeq = (SELECT VSeq FROM @Out);
    SET @ResVNo = @VNo;
    INSERT INTO {0}.dbo.tmpINVTrn (ITrnDate, VoucherNo, VSeq, IPeriod, ICode, TrnQty, RVC, WithRVC, IUnit, Cashier, ITrnCode,
           ITrnSub, IRefCode, DeptCode, PApp)
    SELECT @VDate, @VNo, @ResVSeq, @P, d.ICode, d.CountedQty - d.SystemQty, @StoreRvc, @WithRvc, CONVERT(varchar(10), i.IUnit),
           LEFT(@User, 5), 200, 60, '', @Dept, 'SI'
      FROM @D d
     OUTER APPLY (SELECT TOP 1 x.IUnit FROM {0}.dbo.ItemDef x WHERE x.ICode = d.ICode ORDER BY x.IClass) i
     WHERE d.CountedQty <> d.SystemQty ORDER BY d.Ord;
END;
UPDATE {0}.dbo.InvSession SET InvStatus = 9, VNo = @ResVNo WHERE vSeq = @SSeq;
SELECT @SSeq AS SessionSeq, @ResVSeq AS VSeq, @ResVNo AS VNo;
SELECT ICode, SystemQty, CountedQty FROM @D ORDER BY Ord;"#;

pub(super) const COUNT_READ_SQL: &str = r#"SELECT s.VNo, d.ICode, d.ICount, d.StockCount, s.RVC, s.InvDesc
  FROM {0}.dbo.InvSession s LEFT JOIN {0}.dbo.InvSSDtl d ON d.vSeq = s.vSeq
 WHERE s.vSeq = @Session ORDER BY d.TrnSeq;"#;

pub(super) const COUNT_READY_SQL: &str = r#"SELECT CASE WHEN OBJECT_ID(N'{0}.dbo.InvSession', N'U') IS NULL OR OBJECT_ID(N'{0}.dbo.InvSSDtl', N'U') IS NULL
              OR OBJECT_ID(N'{0}.dbo.Inventory', N'U') IS NULL OR OBJECT_ID(N'{0}.dbo.Department', N'U') IS NULL
              OR OBJECT_ID(N'{0}.dbo.IUnitDef', N'U') IS NULL THEN 0 ELSE 1 END;"#;

pub(super) const COUNT_TARGET_SQL: &str = r#"SELECT CASE WHEN EXISTS (SELECT 1 FROM {0}.dbo.DefRVCList WHERE RVCNo = @Store AND RVCType = 0)
             AND EXISTS (SELECT 1 FROM {0}.dbo.DefRVCList WHERE RVCNo = @Loc AND RVCType = 1)
             AND EXISTS (SELECT 1 FROM {0}.dbo.Department WHERE DepartmentCode = @Dept) THEN 1 ELSE 0 END;"#;

pub(super) const POST_PARAMETERS_SQL: &str = r#"SELECT p.name, TYPE_NAME(p.system_type_id) FROM {0}.sys.parameters p
 WHERE p.object_id = OBJECT_ID(N'{0}.dbo.INVUpdateGLAvg', N'P') ORDER BY p.parameter_id;"#;

pub(super) const POST_SQL_TEMPLATE: &str = r#"DECLARE @rc int;
EXEC @rc = {0}.dbo.INVUpdateGLAvg {1};
SELECT 'Pda_TxnStockCheck' AS PdaCheck, @rc AS Rc, (SELECT COUNT(*) FROM {0}.dbo.tmpINVVoucher WHERE VSeq = @VSeq) AS Left_;"#;

pub(super) const UNPOSTABLE_SQL: &str = r#"SELECT COUNT(*) FROM {0}.dbo.tmpINVTrn n
  LEFT JOIN {0}.dbo.ItemDef d ON n.ICode = d.ICode
  LEFT JOIN {0}.dbo.StockInterface s ON s.RVCNo IN (n.RVC, n.WithRVC) AND d.IClass = s.IClass
 WHERE n.VSeq = @VSeq AND s.StockAccount NOT IN (SELECT a.AccNo FROM {0}.dbo.GLCOA a WHERE a.AccType = 'D');"#;

pub(super) const ISSUE_READ_SQL: &str = r#"SELECT v.VNo, v.VRef, t.ICode, t.TrnQty, t.RVC, t.WithRVC
  FROM {0}.dbo.INVVoucher v
  LEFT JOIN {0}.dbo.INVTrn t ON t.VSeq = v.VSeq AND t.ITrnCode = 200 AND t.ITrnSub = 50 AND t.PApp = 'SX'
 WHERE v.VSeq = @VSeq AND v.sApp = 'SX'
 ORDER BY t.ITrnSeq;"#;

pub(super) const STORE_TYPE_SQL: &str =
    r#"SELECT TOP (1) RVCType FROM {0}.dbo.DefRVCList WHERE RVCNo = @Rvc;"#;

pub(super) const RECEIPT_READY_SQL: &str = r#"SELECT CASE WHEN OBJECT_ID(N'{0}.dbo.tmpReceivingHeader', N'U') IS NULL OR OBJECT_ID(N'{0}.dbo.tmpReceivingDetail', N'U') IS NULL
              OR OBJECT_ID(N'{0}.dbo.ReceivingHeader', N'U') IS NULL OR OBJECT_ID(N'{0}.dbo.ReceivingDetail', N'U') IS NULL
              OR OBJECT_ID(N'{0}.dbo.CLIENT', N'U') IS NULL OR OBJECT_ID(N'{0}.dbo.APAccGroup', N'U') IS NULL
              OR OBJECT_ID(N'{0}.dbo.StockInterface', N'U') IS NULL OR OBJECT_ID(N'{0}.dbo.GLCOA', N'U') IS NULL
              OR OBJECT_ID(N'{0}.dbo.IVoucherType', N'U') IS NULL OR OBJECT_ID(N'{0}.dbo.JVoucher', N'U') IS NULL
              OR OBJECT_ID(N'{0}.dbo.tmpJVoucher', N'U') IS NULL OR OBJECT_ID(N'{0}.dbo.INVTrn', N'U') IS NULL
              OR OBJECT_ID(N'{0}.dbo.INVVoucher', N'U') IS NULL OR OBJECT_ID(N'{0}.dbo.tmpINVVoucher', N'U') IS NULL
              OR OBJECT_ID(N'{0}.dbo.ItemDef', N'U') IS NULL OR OBJECT_ID(N'{0}.dbo.DefRVCList', N'U') IS NULL
              OR OBJECT_ID(N'{0}.dbo.RCVUpdateGL', N'P') IS NULL THEN 0 ELSE 1 END;"#;

pub(super) const RECEIPT_POST_PARAMETERS_SQL: &str = r#"SELECT p.name FROM {0}.sys.parameters p WHERE p.object_id = OBJECT_ID(N'{0}.dbo.RCVUpdateGL', N'P') ORDER BY p.parameter_id;"#;

pub(super) const RECEIPT_VOUCHER_TYPE_SQL: &str = r#"SELECT TOP (1) LTRIM(RTRIM(VoucherType)) FROM {0}.dbo.IVoucherType
 WHERE Receiving = 1 AND ITrnCode = 100 AND ITrnSubCode = 10 AND LEN(LTRIM(RTRIM(VoucherType))) BETWEEN 1 AND 5 ORDER BY VoucherType;"#;

pub(super) const RECEIPT_PROFILE_SQL: &str = r#"SELECT ITrnCode, ITrnSub, COUNT_BIG(*) FROM {0}.dbo.INVTrn WHERE PApp = 'SR' GROUP BY ITrnCode, ITrnSub ORDER BY ITrnCode, ITrnSub;"#;

pub(super) const RECEIPT_TARGET_SQL: &str = r#"SELECT CASE WHEN EXISTS (SELECT 1 FROM {0}.dbo.DefRVCList WHERE RVCNo = @Store AND RVCType = 0)
             AND EXISTS (SELECT 1 FROM {0}.dbo.CLIENT cl JOIN {0}.dbo.APAccGroup g ON g.GrpCode = cl.APAccGroup
                          JOIN {0}.dbo.GLCOA a ON a.AccNo = g.AccNo AND a.AccType = 'D'
                         WHERE cl.ClientFolioNum = @Vendor AND cl.isAP = 1 AND COALESCE(cl.Deleted, 0) = 0)
             AND NOT EXISTS (SELECT 1 FROM (VALUES {1}) v (ICode)
                              WHERE NOT EXISTS (SELECT 1 FROM {0}.dbo.ItemDef d
                                                  JOIN {0}.dbo.StockInterface si ON si.IClass = d.IClass AND si.RVCNo = @Store AND si.Enable = 1
                                                  JOIN {0}.dbo.GLCOA a ON a.AccNo = CASE WHEN COALESCE(si.isStockSub, 0) = 1 THEN si.StockAccount ELSE si.SubAccount END
                                                                      AND a.AccType = 'D'
                                                 WHERE d.ICode = v.ICode))
            THEN 1 ELSE 0 END;"#;

pub(super) const RECEIPT_SQL: &str = r#"DECLARE @VType varchar(5), @Code int, @Sub int;
SELECT TOP (1) @VType = LTRIM(RTRIM(VoucherType)), @Code = ITrnCode, @Sub = ITrnSubCode FROM {0}.dbo.IVoucherType
 WHERE Receiving = 1 AND ITrnCode = 100 AND ITrnSubCode = 10 AND LEN(LTRIM(RTRIM(VoucherType))) BETWEEN 1 AND 5 ORDER BY VoucherType;
IF @VType IS NULL
BEGIN
    SELECT CAST(NULL AS bigint) AS NoVoucherType;
    RETURN;
END;
DECLARE @Res nvarchar(255) = @LockPrefix + N'receipt', @Lock int;
EXEC @Lock = sp_getapplock @Resource = @Res, @LockMode = 'Exclusive', @LockOwner = 'Transaction', @LockTimeout = 10000;
IF @Lock < 0 THROW 51313, 'Another command holds the SMILE_BO receiving counter; it is retried.', 1;
DECLARE @Max bigint;
SELECT @Max = MAX(TRY_CONVERT(bigint, s.Tail))
  FROM (SELECT LTRIM(RTRIM(h.VNo)) AS No FROM {0}.dbo.tmpReceivingHeader h WITH (UPDLOCK, HOLDLOCK) WHERE h.VPeriod = @P
        UNION ALL SELECT LTRIM(RTRIM(h.VNo)) FROM {0}.dbo.ReceivingHeader h WITH (UPDLOCK, HOLDLOCK) WHERE h.VPeriod = @P
        UNION ALL SELECT LTRIM(RTRIM(v.VNo)) FROM {0}.dbo.tmpINVVoucher v WHERE v.VPeriod = @P
        UNION ALL SELECT LTRIM(RTRIM(v.VNo)) FROM {0}.dbo.INVVoucher v WHERE v.VPeriod = @P
        UNION ALL SELECT LTRIM(RTRIM(j.JVNo)) FROM {0}.dbo.tmpJVoucher j WHERE j.JVPeriod = @P
        UNION ALL SELECT LTRIM(RTRIM(j.JVNo)) FROM {0}.dbo.JVoucher j WHERE j.JVPeriod = @P) u
 CROSS APPLY (SELECT SUBSTRING(u.No, LEN(@Init) + 1, 40) AS Tail) s
 WHERE LEFT(u.No, LEN(@Init)) = @Init AND LEN(s.Tail) BETWEEN 1 AND 18 AND s.Tail NOT LIKE '%[^0-9]%';
DECLARE @Digits varchar(20) = CONVERT(varchar(20), COALESCE(@Max, 0) + 1);
DECLARE @VNo varchar(20) = CONCAT(@Init, CASE WHEN LEN(@Digits) < @Width THEN REPLICATE('0', @Width - LEN(@Digits)) ELSE '' END, @Digits);
DECLARE @L TABLE (Ord int NOT NULL PRIMARY KEY, ICode varchar(20) NOT NULL, Qty numeric(18, 5) NOT NULL, Price numeric(18, 5) NOT NULL, Sub numeric(18, 2) NOT NULL);
INSERT INTO @L (Ord, ICode, Qty, Price, Sub)
SELECT l.Ord, l.ICode, l.Qty, l.Price, CONVERT(numeric(18, 2), ROUND(l.Qty * l.Price, 2)) FROM (VALUES {1}) l (Ord, ICode, Qty, Price);
DECLARE @SumSub numeric(18, 2) = (SELECT SUM(Sub) FROM @L);
DECLARE @Out TABLE (VSeq bigint NOT NULL);
INSERT INTO {0}.dbo.tmpReceivingHeader (VNo, InvoiceSeri, InvoiceNumber, VDesc, VDate, VPeriod, PDate, PUser, VStatus, Status, VendorCode,
       VendorName, ReceiveBy, VRef, VType, subAmount, Discount, Fee1, Fee2, Fee3, Tax1Amount, Tax2Amount, Tax3Amount, Tax4Amount, Tax5Amount,
       TotalAmount, ReceivedNonePO, SubAmountVN, Tax1AmountVN, Tax2AmountVN, Tax3AmountVN, Tax4AmountVN, Tax5AmountVN, TotalAmountVN, NonAP,
       PAccNo, PDeptCode, Sts1, Sts2, Sts3, Sts4, InvoiceType, InvoiceForm)
OUTPUT inserted.VSeq INTO @Out (VSeq)
SELECT @VNo, '', '', @VDesc, @VDate, @P, GETDATE(), LEFT(@User, 3), 0, 1, @Vendor,
       (SELECT TOP (1) LEFT(c.ClientName, 200) FROM {0}.dbo.CLIENT c WHERE c.ClientFolioNum = @Vendor), N'', @VRef, @VType, @SumSub, 0, 0, 0, 0, 0, 0, 0, 0, 0,
       @SumSub, 1, 0, 0, 0, 0, 0, 0, 0, 0,
       '', '', '', '', '', '', '', '';
DECLARE @VSeq bigint = (SELECT VSeq FROM @Out);
INSERT INTO {0}.dbo.tmpReceivingDetail (ITrnDate, Cashier, ITrnCode, ITrnSub, IVCode, TrnQty, UPrice, CheckNo, subAmount, UDiscount,
       ImTaxRate, SpecTaxRate, Tax3Rate, VATRate, Tax5Rate, ImTaxCode, SpecTaxCode, Tax3Code, VATCode, Tax5Code, Ufee1, Ufee2, Ufee3,
       Location, IPRNo, IPONo, Department, Status, VType, podtlID, DisCount, Fee1, Fee2, Fee3, Tax1Amount, Tax2Amount, Tax3Amount,
       Tax4Amount, Tax5Amount, TaTolAmount, OrPrice, SubAmountVN, Tax1AmountVN, Tax2AmountVN, Tax3AmountVN, Tax4AmountVN, Tax5AmountVN,
       TotalAmountVN, ISubCode)
SELECT @VDate, LEFT(@User, 5), @Code, @Sub, l.ICode, l.Qty, l.Price, @VSeq, l.Sub, 0,
       0, 0, 0, 0, 0, '', '', '', '', '', 0, 0, 0,
       @StoreRvc, 0, 0, '', 1, @VType, 0, 0, 0, 0, 0, 0, 0, 0,
       0, 0, l.Sub, 0, 0, 0, 0, 0, 0, 0,
       0, ''
  FROM @L l ORDER BY l.Ord;
DECLARE @rc int;
EXEC @rc = {0}.dbo.RCVUpdateGL @VNo = @VNo, @VPeriod = @P, @UpdateBy = @User;
SELECT 'Pda_TxnReceiptCheck' AS PdaCheck, @rc AS Rc, @VSeq AS VSeq,
       (SELECT COUNT(*) FROM {0}.dbo.tmpReceivingHeader WHERE VSeq = @VSeq) AS Left_,
       (SELECT COUNT(*) FROM {0}.dbo.ReceivingHeader WHERE VSeq = @VSeq) AS Posted,
       (SELECT COUNT(*) FROM {0}.dbo.INVTrn WHERE CheckNo = @VSeq AND PApp = 'SR' AND ITrnCode = @Code AND ITrnSub = @Sub) AS Trn;"#;

pub(super) const RECEIPT_READ_SQL: &str = r#"SELECT h.VNo, h.VRef, h.VendorCode, d.IVCode, d.TrnQty, d.subAmount, d.Location
  FROM {0}.dbo.ReceivingHeader h LEFT JOIN {0}.dbo.ReceivingDetail d ON d.CheckNo = h.VSeq
 WHERE h.VSeq = @Seq ORDER BY d.ITrnSeq;"#;

/// `VoucherSql` after [`VOUCHER_NUMBERING_SQL`]: the un-posted voucher (`tmpINVVoucher`/`tmpINVTrn`) with `{1}` the `(ord, @C, @Q)` rows.
pub(super) const VOUCHER_INSERT_SQL: &str = r#"DECLARE @Out TABLE (VSeq bigint NOT NULL);
INSERT INTO {0}.dbo.tmpINVVoucher (VNo, VDesc, VDate, VPeriod, PDate, PUser, VRef, VType, sApp)
OUTPUT inserted.VSeq INTO @Out (VSeq)
VALUES (@VNo, @VDesc, @VDate, @P, GETDATE(), LEFT(@User, 10), LEFT(@VRef, 30), @J, 'SX');
DECLARE @VSeq bigint = (SELECT VSeq FROM @Out);
INSERT INTO {0}.dbo.tmpINVTrn (ITrnDate, VoucherNo, VSeq, IPeriod, ICode, TrnQty, RVC, WithRVC, IUnit, Cashier, ITrnCode,
       ITrnSub, IRefCode, DeptCode, PApp)
SELECT @VDate, @VNo, @VSeq, @P, l.ICode, l.Qty, @StoreRvc, @WithRvc, CONVERT(varchar(10), i.IUnit), LEFT(@User, 5), 200, @TrnSub,
       '', @Dept, 'SX'
  FROM (VALUES {1}) l (Ord, ICode, Qty)
 OUTER APPLY (SELECT TOP 1 x.IUnit FROM {0}.dbo.ItemDef x WHERE x.ICode = l.ICode ORDER BY x.IClass) i
 ORDER BY l.Ord;
SELECT @VSeq;"#;
