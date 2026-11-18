-- @ID is the POS CheckNo (see sp_SelectPendingPOS — the checks and their real
-- money live on dbo.Invoice/dbo.Trn; TaxTable/VATBillInfor are too sparse to
-- key off and here carry buyer identity only).
--
-- sp_FetchPOS v2 — the agent no longer synthesizes per-line VAT by
-- apportionment. dbo.Trn carries the EXACT per-line money and rate bucket, so
-- this SP hands the raw Trn rows + Invoice header straight to the Go
-- normalizer (internal/agent/pos_normalize.go), which apportions discount,
-- derives+verifies the legal VAT rate, and reconciles against the header.
--
-- VERIFIED LIVE on SMILE_POS, 2026:
--  - dbo.Invoice header money reconciles EXACTLY:
--    BeforeTax = SubTotal + Discount + SurCharge; TotalBase = BeforeTax +
--    TaxTotal. Discount is stored NEGATIVE. SurCharge is a check-level service
--    charge applied post-discount (SC% = SurCharge/(SubTotal+Discount)).
--  - dbo.Trn.TrnCode discriminates row purpose: 100 = revenue item line,
--    110 = discount line (negative BaseSub/BaseTax). 200/300 = payment,
--    400/570 = other non-revenue postings — excluded here.
--  - dbo.Trn carries the exact per-line money: BaseSub (net), BaseTax (VAT),
--    TaxNo (rate bucket: 8 -> 8%, 1 -> 10%), BaseSTax/STaxNo (service-tax,
--    unused on this install — 0). Trn.TotalR/TaxR are RATE-SCALED, not VND, so
--    they are NOT read. Proven: check 200021466 Σ BaseSub=535000=SubTotal,
--    Σ BaseTax=44940=TaxTotal (exact); discounted check 200021431 reconciles
--    exactly incl. the negative 110-row (52164 = 57960 - 5796).
--  - dbo.TaxTable (1 row/check, keyed TCheckNo=CheckNo) carries buyer identity
--    (ClientVAT/ClientName/Company/Address + InvContent) on this
--    install; its money buckets are NULL and are NOT read here. LEFT JOIN so a
--    check with no TaxTable row still fetches (buyer fields default to '').
CREATE OR ALTER PROCEDURE dbo.sp_FetchPOS @ID varchar(64) AS
BEGIN
    SET NOCOUNT ON;
    DECLARE @CheckNo numeric = TRY_CONVERT(numeric, @ID);

    -- Result set 1: header (one row) — check-level money truth off dbo.Invoice
    -- + buyer identity off dbo.TaxTable. Scan order fixed in fetchPOSRaw.
    -- InvDate is rebuilt from the integer date parts (IYear/IMonth/IDay) plus
    -- CloseTime's time-of-day so a source-issued date survives even when
    -- OpenTime/CloseTime lack a stable calendar date on this schema.
    SELECT
        SubTotal   = CAST(ROUND(ISNULL(i.SubTotal, 0), 0) AS bigint),
        Discount   = CAST(ROUND(ISNULL(i.Discount, 0), 0) AS bigint),
        SurCharge  = CAST(ROUND(ISNULL(i.SurCharge, 0), 0) AS bigint),
        BeforeTax  = CAST(ROUND(ISNULL(i.BeforeTax, 0), 0) AS bigint),
        TaxTotal   = CAST(ROUND(ISNULL(i.TaxTotal, 0), 0) AS bigint),
        STaxTotal  = CAST(ROUND(ISNULL(i.STaxTotal, 0), 0) AS bigint),
        TotalBase  = CAST(ROUND(ISNULL(i.TotalBase, 0), 0) AS bigint),
        InvStatus  = CAST(ISNULL(i.InvStatus, 0) AS int),
        TaxOmit    = CAST(ISNULL(i.TaxOmit, 0) AS int),
        RVC        = CAST(ISNULL(i.RVC, 0) AS int),
        FolioNum   = ISNULL(CAST(i.FolioNum AS varchar(30)), ''),
        InvDate    = DATEADD(day, ISNULL(i.IDay, 1) - 1,
                     DATEADD(month, ISNULL(i.IMonth, 1) - 1,
                     DATEADD(year, ISNULL(i.IYear, 1900) - 1900,
                     CAST(CAST(i.CloseTime AS time) AS datetime)))),
        ExRate     = CAST(ISNULL(i.ExRate, 1) AS float),
        ClientVAT  = ISNULL(tt.ClientVAT, ''),
        ClientName = ISNULL(tt.ClientName, ''),
        Company    = ISNULL(tt.Company, ''),
        Address    = ISNULL(tt.Address, ''),
        InvContent = ISNULL(tt.InvContent, '')
    FROM dbo.Invoice i
    LEFT JOIN dbo.TaxTable tt ON tt.TCheckNo = i.CheckNo
    WHERE i.CheckNo = @CheckNo;

    -- Result set 2: revenue + discount lines off dbo.Trn (TrnCode IN (100,110)),
    -- 14 cols. Money columns are the EXACT per-line net/VAT/service-charge/
    -- service-tax; the Go normalizer apportions the 110-row discount across the
    -- 100-rows, derives the legal rate from (BaseSub, BaseTax) and reconciles to
    -- the header.
    --
    -- đơn vị tính resolves ItemCode -> ItemDef.IUnit -> IUnitDef.UnitName.
    -- VERIFIED LIVE 2026-07-15: resolves on 16235/16235 (100%) of the 2026
    -- revenue rows (PART, CUP, GLASS, CAN, BOTTLE, SET, PHONG, LY, CHAI, ...).
    -- The join is on ICode ALONE and deliberately not RVC-scoped: ItemDef.RVC is
    -- NULL on all 1731 rows of this install, so any RVC predicate matches
    -- nothing and silently blanks every unit.
    --
    -- Nhóm hàng hóa (item group) is NOT emitted: it does not exist on this
    -- install. VERIFIED LIVE 2026-07-15: Trn.GroupCode = 0 on all 16235 2026
    -- revenue rows and ItemDef.IGroup = '0' on every row, so a Group column
    -- could only ever be a constant ''. Adding one would put an always-empty
    -- field in the payload and the source hash for no data. A DBA on an install
    -- that populates IGroup can join IGroupDef here and append the column AFTER
    -- UnitName, then extend the Go scan in the same order.
    SELECT
        TrnSeq    = CAST(t.TrnSeq AS int),
        TrnCode   = CAST(t.TrnCode AS int),
        ItemCode  = ISNULL(t.ItemCode, ''),
        TrnDesc   = ISNULL(t.TrnDesc, ''),
        UnitName  = ISNULL(ud.UnitName, ''),
        Qty       = CAST(ISNULL(t.TrnQty, 1) AS float),
        UnitPrice = CAST(ROUND(ISNULL(t.ItemPrice, 0), 0) AS bigint),
        BaseSub   = CAST(ROUND(ISNULL(t.BaseSub, 0), 0) AS bigint),
        -- BaseSrc is the EXACT per-line service charge (phí phục vụ). BaseSub
        -- EXCLUDES it and BaseTax is levied on (BaseSub + BaseSrc), which is why
        -- the Go rate band tolerates a ratio up to r x 1.15.
        -- VERIFIED LIVE 2026-07-15: SUM(BaseSrc) over TrnCode IN (100,110)
        -- equals Invoice.SurCharge on 3139/3139 of the 2026 checks. It must be
        -- summed over BOTH codes: 94 of 95 discount rows carry a non-zero
        -- BaseSrc (the SC reduction that rides the discount), and counting only
        -- the 100-rows reconciles on just 3101/3139.
        BaseSrc   = CAST(ROUND(ISNULL(t.BaseSrc, 0), 0) AS bigint),
        BaseTax   = CAST(ROUND(ISNULL(t.BaseTax, 0), 0) AS bigint),
        TaxNo     = CAST(ISNULL(t.TaxNo, 0) AS int),
        BaseSTax  = CAST(ROUND(ISNULL(t.BaseSTax, 0), 0) AS bigint),
        STaxNo    = CAST(ISNULL(t.STaxNo, 0) AS int),
        RVC       = CAST(ISNULL(t.RVC, 0) AS int)
    FROM dbo.Trn t
    LEFT JOIN dbo.ItemDef idf ON RTRIM(idf.ICode) = RTRIM(t.ItemCode)
    LEFT JOIN dbo.IUnitDef ud ON RTRIM(ud.UnitCode) = RTRIM(idf.IUnit)
    WHERE t.CheckNo = @CheckNo
      AND t.TrnCode IN (100, 110)
    ORDER BY t.TrnSeq;
END;
GO
