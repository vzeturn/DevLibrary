CREATE OR ALTER PROCEDURE dbo.sp_FetchFO @ID varchar(64) AS
BEGIN
    SET NOCOUNT ON;
    DECLARE @BillID numeric = TRY_CONVERT(numeric, @ID);

    -- Two-tier FO revenue filter (ĐỢT 2). The revenue-line predicate is built
    -- ONCE here and reused for result set 2 and result set 3's subquery so both
    -- select the SAME row set. It PROBES the install's schema:
    --   * both dbo.TransactionCode.TransactionGroup AND
    --     dbo.FolioTransaction.FolioBalanceCode present → the optimal predicate
    --     (TransactionGroup=1 revenue class, minus void/cancel/payment balances),
    --   * otherwise → the legacy 12-code IN-list, so a schema variant that lacks
    --     the new columns NEVER loses revenue (falls back to today's behaviour).
    -- The predicate travels as text through sp_executesql because the WHERE
    -- itself changes with the probe; the fenced block below is byte-identical
    -- (after whitespace normalisation) to the copies in sp_FetchFO result set 3
    -- and in 01_source_hash.sql (#FT/#IP), which the Go drift guard compares.
    /*MMEI_FO_PREDICATE*/
    DECLARE @pred nvarchar(max);
    IF COL_LENGTH('dbo.TransactionCode', 'TransactionGroup') IS NOT NULL
       AND COL_LENGTH('dbo.FolioTransaction', 'FolioBalanceCode') IS NOT NULL
        SET @pred = N'EXISTS (SELECT 1 FROM dbo.TransactionCode tcp WHERE tcp.TransactionCode = ft.TransactionCode AND tcp.TransactionGroup = 1) AND ft.FolioBalanceCode NOT IN (''P'', ''V'', ''C'')';
    ELSE
        SET @pred = N'ft.TransactionCode IN (''110'', ''150'', ''160'', ''170'', ''200'', ''201'', ''210'', ''211'', ''220'', ''221'', ''230'', ''240'')';
    /*END_MMEI_FO_PREDICATE*/

    -- ItemPosted's tax grain varies PER ITEM GROUP, not per install: earlier
    -- revisions probed COL_LENGTH(TaxAmount/ServiceChargeAmount) as a proxy for
    -- gross-vs-net, but VERIFIED LIVE 2026-08-30 (local SMILE_FO, 3330-row full
    -- scan) those columns exist on ItemPosted regardless of convention — the
    -- real switch is dbo.ItemGroup.IsPriceNet, reached via
    -- ItemPosted.ICode -> ItemList.ICode -> ItemList.IGroup = ItemGroup.GroupCode
    -- (Price*Qty == Amount held for all 3330 rows where IsPriceNet=1; 0
    -- mismatches). @ipHasTax now gates only whether the tax/STax/SC columns are
    -- PRESENT on this install (schema-variant fallback, unrelated to net/gross);
    -- @ipIsNet (declared with the join in result set 3, since it needs ig) picks
    -- the net/gross formula per that item's own group setting.
    DECLARE @ipHasTax bit =
        CASE WHEN COL_LENGTH('dbo.ItemPosted', 'TaxAmount') IS NOT NULL
              AND COL_LENGTH('dbo.ItemPosted', 'ServiceChargeAmount') IS NOT NULL
             THEN 1 ELSE 0 END;

    -- Column-probe expressions for the 3 raw classification columns result set 2
    -- appends (TrnGroup/IsHKPost/TaxRate). Each is '' / 0 when its column or
    -- table is absent, so an install missing them never zeroes a folio line.
    DECLARE @grpExpr  nvarchar(200) =
        CASE WHEN COL_LENGTH('dbo.TransactionCode', 'TransactionGroup') IS NOT NULL
             THEN N'ISNULL(RTRIM(CAST(tc.TransactionGroup AS varchar(20))), '''')'
             ELSE N'''''' END;
    DECLARE @hkExpr   nvarchar(200) =
        CASE WHEN COL_LENGTH('dbo.FolioTransaction', 'isHKPost') IS NOT NULL
             THEN N'CAST(ISNULL(ft.isHKPost, 0) AS bit)'
             ELSE N'CAST(0 AS bit)' END;
    DECLARE @rateExpr nvarchar(400) =
        CASE WHEN OBJECT_ID('dbo.TaxDef') IS NOT NULL
              AND COL_LENGTH('dbo.TaxDef', 'TaxRate') IS NOT NULL
             THEN N'ISNULL(RTRIM(CAST(td.TaxRate AS varchar(20))), '''')'
             ELSE N'''''' END;
    DECLARE @rateJoin nvarchar(400) =
        CASE WHEN OBJECT_ID('dbo.TaxDef') IS NOT NULL
              AND COL_LENGTH('dbo.TaxDef', 'TaxRate') IS NOT NULL
             THEN N'LEFT JOIN dbo.TaxDef td ON td.TaxCode = ft.TaxCode'
             ELSE N'' END;

    -- Room-night + lossless coverage column probes (result set 2 appends these
    -- LAST). Each degrades to '' / 0 when its column is absent, so a schema
    -- variant that lacks a column never zeroes a folio line.
    DECLARE @roomExpr nvarchar(200) =
        CASE WHEN COL_LENGTH('dbo.FolioTransaction', 'OriginRoom') IS NOT NULL
             THEN N'ISNULL(RTRIM(CAST(ft.OriginRoom AS varchar(20))), '''')' ELSE N'''''' END;
    DECLARE @roomCodeExpr nvarchar(200) =
        CASE WHEN COL_LENGTH('dbo.FolioTransaction', 'RoomCode') IS NOT NULL
             THEN N'ISNULL(RTRIM(CAST(ft.RoomCode AS varchar(20))), '''')' ELSE N'''''' END;
    DECLARE @trnDateExpr nvarchar(200) =
        CASE WHEN COL_LENGTH('dbo.FolioTransaction', 'TransactionDate') IS NOT NULL
             THEN N'ISNULL(CONVERT(varchar(10), ft.TransactionDate, 23), '''')' ELSE N'''''' END;
    DECLARE @postAccExpr nvarchar(200) =
        CASE WHEN COL_LENGTH('dbo.FolioTransaction', 'PostToACC') IS NOT NULL
             THEN N'CAST(ISNULL(ft.PostToACC, 0) AS bit)' ELSE N'CAST(0 AS bit)' END;
    DECLARE @coverExpr nvarchar(200) =
        CASE WHEN COL_LENGTH('dbo.FolioTransaction', 'NumberCover') IS NOT NULL
             THEN N'CAST(ROUND(ISNULL(ft.NumberCover, 0), 0) AS int)' ELSE N'CAST(0 AS int)' END;
    DECLARE @xrefExpr nvarchar(200) =
        CASE WHEN COL_LENGTH('dbo.FolioTransaction', 'XRef') IS NOT NULL
             THEN N'ISNULL(RTRIM(CAST(ft.XRef AS varchar(30))), '''')' ELSE N'''''' END;
    DECLARE @curExpr nvarchar(200) =
        CASE WHEN COL_LENGTH('dbo.FolioTransaction', 'OriginCurrency') IS NOT NULL
             THEN N'ISNULL(RTRIM(CAST(ft.OriginCurrency AS varchar(10))), '''')' ELSE N'''''' END;
    DECLARE @oamtExpr nvarchar(200) =
        CASE WHEN COL_LENGTH('dbo.FolioTransaction', 'OriginAmount') IS NOT NULL
             THEN N'CAST(ROUND(ISNULL(ft.OriginAmount, 0), 0) AS bigint)' ELSE N'CAST(0 AS bigint)' END;
    DECLARE @rptRateExpr nvarchar(200) =
        CASE WHEN COL_LENGTH('dbo.FolioTransaction', 'RptExRate') IS NOT NULL
             THEN N'ISNULL(RTRIM(CAST(ft.RptExRate AS varchar(30))), '''')' ELSE N'''''' END;

    -- Result set 1: header (22 columns, scan order in FetchCollected).
    -- GuestName/Address2/InvContent are shipped-SP superset extensions, and
    -- ConfirmNo/Arrival/Departure/RoomNo are the reservation-detail extensions;
    -- all are omitempty on the Go side, so an install whose BillInfor/Folio lacks
    -- a column can leave the SELECT expression '' and nothing changes for
    -- existing invoices. New columns are appended AFTER the stable ones so the
    -- Go scan offsets never move.
    SELECT TOP (1)
        SellerTaxCode = '' /* set to the property MST */,
        CompanyName   = ISNULL(b.CompanyName, ''),
        CustomerTax   = ISNULL(b.VATCode, ''),
        CustomerAddr  = ISNULL(b.Address1, ''),
        -- Buyer email: SMILE stores the folio's booking email on the GUEST
        -- profile, not on BillInfor (b.EMail is NULL on virtually every folio
        -- here). Resolve it from the folio's FIRST guest: BillInfor.FolioNum →
        -- GuestEveryDay (the per-night guest snapshot) → Profile.Email, taking
        -- the guest with the smallest (FolioSubNum, GuestID) = the primary guest
        -- attached earliest. IsAdditionalName=1 selects real profile-linked
        -- guests (the additional-name rows carry the booking parties; ProfileID>0
        -- excludes the walk-in/no-profile lines whose ProfileID is 0). Falls back
        -- to b.EMail when no guest email exists, so nothing regresses. The gefirst
        -- subquery is guarded: GuestEveryDay/Profile are core SMILE_FO tables, but
        -- the ISNULL keeps a bare install (no match) at today's b.EMail value.
        -- Outer ISNULL(...,'') is required: when BOTH the guest-profile lookup
        -- and b.EMail are empty/NULL, the inner ISNULL/NULLIF chain evaluates
        -- to NULL (not ''), which the Go scan (a plain string, not
        -- sql.NullString) rejects with "converting NULL to string is
        -- unsupported" — silently dropping the bill from every collect cycle.
        CustomerEmail = ISNULL(ISNULL(NULLIF((
            SELECT TOP (1) p.Email
            FROM dbo.GuestEveryDay g
            JOIN dbo.Profile p ON p.ProfileNumber = g.ProfileID
            WHERE g.FolioNum = b.FolioNum
              AND g.IsAdditionalName = 1
              AND g.ProfileID > 0
              AND p.Email IS NOT NULL AND p.Email <> ''
            ORDER BY g.FolioSubNum, g.GuestID
        ), ''), NULLIF(b.EMail, '')), ''),
        CustomerTel   = '',
        InvoiceType   = 'GTGT',
        CodeMode      = 'CO_MA',
        KyHieu        = ISNULL(b.BillSeri1, ''),
        MauSo         = '',
        InvDate       = b.InvDate,
        -- Canonical Subtotal ≡ tổng tiền chưa thuế, derived as Total − Tax so the
        -- hub invariant Subtotal+Tax==Total holds BY CONSTRUCTION and the value is
        -- SERVICE-CHARGE-INCLUSIVE. BillInfor.SubAmount EXCLUDES the service charge
        -- (TotalAmount includes it), so using SubAmount here would break invariant 1
        -- on every SC-bearing folio. ServiceCharge is kept below for information.
        Subtotal      = CAST(ROUND(ISNULL(b.TotalAmount, 0), 0) AS bigint)
                        - CAST(ROUND(ISNULL(b.TaxAmount, 0), 0) AS bigint),
        ServiceCharge = CAST(ROUND(ISNULL(b.ServiceCharge, 0), 0) AS bigint),
        TaxAmount     = CAST(ROUND(ISNULL(b.TaxAmount, 0), 0) AS bigint),
        TotalAmount   = CAST(ROUND(ISNULL(b.TotalAmount, 0), 0) AS bigint),
        -- Superset (verified live on SMILE_FO.BillInfor): the guest name, the
        -- second address line, and the free-text invoice content note.
        GuestName     = ISNULL(b.GuestName, ''),
        BuyerAddress2 = ISNULL(b.Address2, ''),
        InvContent    = ISNULL(b.BillNote, ''),
        -- Booking confirmation number (verified LIVE 2026-07-30 on SMILE_FO):
        -- BillInfor.FolioNum → Folio.ConfirmNum (numeric, always populated for a
        -- real folio). This is the reservation's confirmation no. the Hub threads
        -- into MISA CustomField1. LEFT JOIN + ISNULL so a bill with no matching
        -- folio (or a bare install without dbo.Folio) leaves it '' and nothing
        -- regresses. Cast the numeric to a trimmed string; strip any decimal.
        ConfirmNo     = ISNULL(NULLIF(RTRIM(CONVERT(varchar(30), CONVERT(bigint, f.ConfirmNum))), '0'), ''),
        -- Reservation stay dates + room (verified LIVE 2026-07-30 on SMILE_FO):
        -- BillInfor.FolioNum → Folio.ArrivalDate/DepartureDate (datetime) and
        -- Folio.RoomCode (varchar). These feed MISA CheckIn/CheckOut/RoomNo. Same
        -- LEFT JOIN dbo.Folio as ConfirmNo above, so a bill with no matching folio
        -- (or a bare install without dbo.Folio) leaves them '' — nothing regresses.
        -- Dates emitted ISO yyyy-MM-dd; the Hub reformats per provider.
        Arrival       = ISNULL(CONVERT(varchar(10), f.ArrivalDate, 23), ''),
        Departure     = ISNULL(CONVERT(varchar(10), f.DepartureDate, 23), ''),
        RoomNo        = ISNULL(RTRIM(CONVERT(varchar(30), f.RoomCode)), '')
    FROM dbo.BillInfor b
    LEFT JOIN dbo.Folio f ON f.FolioNum = b.FolioNum
    -- A BillID can appear on MULTIPLE BillInfor rows: reprints/partial cancels
    -- add a Cancel=1 row whose totals are NULL. VERIFIED LIVE 2026-07-22
    -- (bills 10399/10401/10404): without this filter the Go scan reads an
    -- arbitrary first row and a NULL-totals cancel row zeroes the header.
    -- Same active-bill predicate as sp_SelectPendingFO.
    WHERE b.BillID = @BillID
      AND (b.Cancel = 0 OR b.Cancel IS NULL)
    -- TOP(1) + newest print: a rare bill still has >1 active row (verified
    -- live: 1 such BillID); the header must be exactly one row for the Go
    -- scan, and the latest print is the billing truth.
    ORDER BY b.PrintDate DESC, b.ID DESC;

    -- Result set 2: merged folio lines (revenue codes only), 33 cols. The first
    -- 21 are unchanged (stable scan positions in FetchCollected); cols 22-24 —
    -- TrnGroup / IsHKPost / TaxRate — are the ĐỢT 2 raw classification columns;
    -- cols 25-33 — OriginRoom / RoomCode / TransactionDate / PostToACC /
    -- NumberCover / XRef / OriginCurrency / OriginAmount / RptExRate — are the
    -- room-night + lossless coverage columns. All appended so existing scan
    -- offsets never move. Dynamic because the WHERE predicate and the probe
    -- expressions are chosen at run time.
    --
    -- ft.SubAmount is ALWAYS net on this install, independent of @ipHasTax.
    -- VERIFIED LIVE 2026-08-28 against the real DXQN SMILE_FO database, bill
    -- 22582: for the two water-bottle lines (TrnCode 540/14), where
    -- ft.SubAmount can be cross-checked against a NET ItemPosted total,
    -- ft.TransactionAmount − ft.SubAmount == ft.TaxAmount + ft.STaxAmount +
    -- ft.ServiceCharge EXACTLY (16000−14110=1890=1185+0+705;
    -- 48000−42328=5672=3556+0+2116), and SubAmount/TransactionAmount is the
    -- SAME ratio (0.8818…) as the System Charge line
    -- (1366843/1550000=0.8818…) — proving SubAmount is already net for every
    -- line on this install, including System Charge, which has no ItemPosted
    -- children to self-check. An earlier attempt to also subtract
    -- Tax+STax+ServiceCharge out of SubAmount here (reusing @ipHasTax as a
    -- proxy) was WRONG — it double-subtracted an already-net figure. Do not
    -- reintroduce that without a fresh live cross-check on the affected
    -- install; TransactionAmount, not SubAmount, is the gross column.
    DECLARE @sql2 nvarchar(max) = N'
    SELECT
        LineKey      = CAST(ft.TransactionID AS varchar(30)),
        -- The source document behind this charge. For POS transfers
        -- (TransactionCode 200/201/210/211) RefNumber IS the POS CheckNo
        -- (VERIFIED LIVE 2026-07-22: 6009/6022 billed charge rows resolve in
        -- SMILE_POS.dbo.Invoice), which the Go collector uses to swap the
        -- opaque transfer summary for the check''s real items (fo_enrich.go).
        -- Other codes carry room numbers / misc refs — Go ignores those.
        SourceRef    = ISNULL(RTRIM(ft.RefNumber), ''''),
        TrnCode      = ISNULL(ft.TransactionCode, ''''),
        TrnSubCode   = ISNULL(CAST(ft.TransactionSubCode AS varchar(10)), ''''),
        DescPrimary  = ISNULL(tc.Description, ISNULL(ft.Description, '''')),
        UnitName     = '''',
        Qty          = CAST(ISNULL(ft.Quantity, 1) AS float),
        UnitPrice    = CAST(ROUND(ISNULL(ft.SubAmount, 0), 0) AS bigint),
        Amount       = CAST(ROUND(ISNULL(ft.SubAmount, 0), 0) AS bigint),
        TaxCode      = ISNULL(ft.TaxCode, ''''),
        TaxAmount    = CAST(ROUND(ISNULL(ft.TaxAmount, 0), 0) AS bigint),
        STaxCode     = ISNULL(ft.STaxCode, ''''),
        STaxAmount   = CAST(ROUND(ISNULL(ft.STaxAmount, 0), 0) AS bigint),
        SrvChgCode   = '''',
        SrvChgAmount = CAST(ROUND(ISNULL(ft.ServiceCharge, 0), 0) AS bigint),
        RVC          = '''',
        Outlet       = '''',
        FolioLink    = CAST(@BillID AS varchar(30)),
        OriginFolioNumber = ISNULL(RTRIM(CAST(ft.OriginFolioNumber AS varchar(30))), ''''),
        TrnType      = ISNULL(RTRIM(tc.TransactionType), ''''),
        DiscountAmount = CAST(ROUND(ISNULL(ft.Discount, 0), 0) AS bigint),
        -- ĐỢT 2 raw classification columns (probe-guarded, appended last):
        --   TrnGroup = dbo.TransactionCode.TransactionGroup catalog label
        --   IsHKPost = FolioTransaction.isHKPost housekeeping/minibar flag
        --   TaxRate  = TaxDef.TaxRate legal-band label (JOIN on ft.TaxCode)
        TrnGroup     = ' + @grpExpr + N',
        IsHKPost     = ' + @hkExpr + N',
        TaxRate      = ' + @rateExpr + N',
        -- Room-night + lossless coverage columns (probe-guarded, appended LAST so
        -- the ĐỢT-2 scan offsets above never move). OriginRoom counts {Rooms},
        -- TransactionDate counts {Nights}; the rest are lossless-only.
        OriginRoom      = ' + @roomExpr + N',
        RoomCode        = ' + @roomCodeExpr + N',
        TransactionDate = ' + @trnDateExpr + N',
        PostToACC       = ' + @postAccExpr + N',
        NumberCover     = ' + @coverExpr + N',
        XRef            = ' + @xrefExpr + N',
        OriginCurrency  = ' + @curExpr + N',
        OriginAmount    = ' + @oamtExpr + N',
        RptExRate       = ' + @rptRateExpr + N'
    FROM dbo.FolioTransaction ft
    OUTER APPLY dbo.SA_TrnCode_Resolve(ft.TransactionCode, ft.TransactionSubCode, 0, 0) trn
    LEFT JOIN dbo.TransactionCode tc
        ON tc.TransactionCode = trn.TransactionCode
       AND tc.TransactionSubCode = trn.TransactionSubCode
    ' + @rateJoin + N'
    WHERE ft.BillID = @BillID
      AND (' + @pred + N');';
    EXEC sp_executesql @sql2, N'@BillID numeric', @BillID = @BillID;

    -- Result set 3: detail items (ItemPosted), 15 cols. Its parent-line subquery
    -- reuses the SAME @pred so detail only attaches to kept revenue lines.
    -- Net-vs-gross is decided PER ITEM GROUP via dbo.ItemGroup.IsPriceNet
    -- (reached ItemPosted.ICode -> ItemList.ICode -> ItemList.IGroup =
    -- ItemGroup.GroupCode), the property's own configured setting for that
    -- group. @ipGroupHasNet probes whether IsPriceNet exists at all (older
    -- SMILE schema variants may lack it); when absent, or when @ipHasTax=0
    -- (no Tax/STax/SC columns to a convention over), @ipNetExpr falls back to
    -- @ipHasTax's own value (0), preserving the original always-net behaviour.
    -- NOTE: IsPriceNet is a per-group SETTING, not ground truth — it has been
    -- observed wrong on real data for a given group/version (VERIFIED LIVE
    -- 2026-08-30, local SMILE_FO, ICode 1110001 "Minibar" group: IsPriceNet=1
    -- but the real menu price is gross). A pure row-arithmetic check
    -- (Price*Qty vs Amount) CANNOT correct this: Price and Amount are always
    -- mutually consistent on the same row regardless of convention, so that
    -- comparison trivially matches "net" every time and can never discriminate
    -- — do not reintroduce it. The reliable correction (comparing actual
    -- ServiceChargeAmount/TaxAmount against the property's configured standard
    -- rate × Amount) needs a per-tenant rate constant the SQL layer does not
    -- have; it runs in the Go agent (AgentConfig.StandardServiceChargeRate)
    -- AFTER this SP returns, before the item is pushed to the Hub — see
    -- internal/agentcollector/collectsource.go correctFODetailNetGross and
    -- spec 2026-08-30-fo-detail-net-gross-correction. This SQL signal is the
    -- agent's starting hypothesis, not the final answer.
    DECLARE @ipGroupHasNet bit =
        CASE WHEN COL_LENGTH('dbo.ItemGroup', 'IsPriceNet') IS NOT NULL THEN 1 ELSE 0 END;
    DECLARE @ipNetExpr nvarchar(200) = CASE WHEN @ipHasTax = 1 AND @ipGroupHasNet = 1
        THEN N'CASE WHEN ISNULL(ig.IsPriceNet, 1) = 1 THEN 1 ELSE 0 END'
        WHEN @ipHasTax = 1 THEN N'1'
        ELSE N'0' END;
    DECLARE @ipAmount nvarchar(1200) = N'
        CASE WHEN ' + @ipNetExpr + N' = 0
             THEN CAST(ROUND(ISNULL(ip.Amount,0) - ISNULL(ip.TaxAmount,0) - ISNULL(ip.STaxAmount,0) - ISNULL(ip.ServiceChargeAmount,0), 0) AS bigint)
             ELSE CAST(ROUND(ISNULL(ip.Amount, 0), 0) AS bigint) END';
    -- ip.Price follows the same net/gross convention as ip.Amount (VERIFIED LIVE
    -- 2026-07-22, bill 22066, gross-reconciled row: qty=1 water-bottle ip.Price =
    -- ip.Amount = 16000 = 14110 net + 1185 tax + 705 SC). When the group is
    -- gross, back the per-unit tax+STax+SC share out of Price so UnitPrice
    -- lands on the same net basis as the NET Amount above (UnitPrice × Qty ≈
    -- Amount, up to rounding). When net, Price is already net like the
    -- untouched Amount branch.
    DECLARE @ipUnitPrice nvarchar(1400) = N'
        CASE WHEN ' + @ipNetExpr + N' = 0
             THEN CAST(ROUND(ISNULL(ip.Price,0) - (ISNULL(ip.TaxAmount,0) + ISNULL(ip.STaxAmount,0) + ISNULL(ip.ServiceChargeAmount,0)) / CASE WHEN ISNULL(ip.Qty,0) = 0 THEN 1 ELSE ip.Qty END, 0) AS bigint)
             ELSE CAST(ROUND(ISNULL(ip.Price, 0), 0) AS bigint) END';
    DECLARE @ipTaxCode  nvarchar(200) = CASE WHEN @ipHasTax = 1 THEN N'ISNULL(ip.TaxCode, '''')'          ELSE N'CAST('''' AS varchar(20))' END;
    DECLARE @ipTax      nvarchar(200) = CASE WHEN @ipHasTax = 1 THEN N'CAST(ROUND(ISNULL(ip.TaxAmount,0),0) AS bigint)'  ELSE N'CAST(0 AS bigint)' END;
    DECLARE @ipSTaxCode nvarchar(200) = CASE WHEN @ipHasTax = 1 THEN N'ISNULL(ip.STaxCode, '''')'         ELSE N'CAST('''' AS varchar(20))' END;
    DECLARE @ipSTax     nvarchar(200) = CASE WHEN @ipHasTax = 1 THEN N'CAST(ROUND(ISNULL(ip.STaxAmount,0),0) AS bigint)' ELSE N'CAST(0 AS bigint)' END;
    DECLARE @ipScCode   nvarchar(200) = CASE WHEN @ipHasTax = 1 THEN N'ISNULL(ip.ServiceChargeCode, '''')' ELSE N'CAST('''' AS varchar(20))' END;
    DECLARE @ipSc       nvarchar(200) = CASE WHEN @ipHasTax = 1 THEN N'CAST(ROUND(ISNULL(ip.ServiceChargeAmount,0),0) AS bigint)' ELSE N'CAST(0 AS bigint)' END;
    DECLARE @sql3 nvarchar(max) = N'
    SELECT
        LineKey      = CAST(ip.TrnID AS varchar(30)),
        DetailKey    = CAST(ip.nSeq AS varchar(30)),
        ICode        = ISNULL(ip.ICode, ''''),
        NamePrimary  = ISNULL(ip.IName, ''''),
        ItemGroup    = ISNULL(ig.GroupName, ''''),
        UnitName     = ISNULL(iu.UnitName, ''''),
        Qty          = CAST(ISNULL(ip.Qty, 1) AS float),
        UnitPrice    = ' + @ipUnitPrice + N',
        Amount       = ' + @ipAmount + N',
        TaxCode      = ' + @ipTaxCode + N',
        TaxAmount    = ' + @ipTax + N',
        STaxCode     = ' + @ipSTaxCode + N',
        STaxAmount   = ' + @ipSTax + N',
        SrvChgCode   = ' + @ipScCode + N',
        SrvChgAmount = ' + @ipSc + N'
    FROM dbo.ItemPosted ip
    LEFT JOIN dbo.ItemList il ON RTRIM(il.ICode) = RTRIM(ip.ICode)
    LEFT JOIN dbo.ItemGroup ig ON RTRIM(ig.GroupCode) = RTRIM(il.IGroup)
    LEFT JOIN dbo.ItemUnit iu ON RTRIM(iu.UnitCode) = RTRIM(il.IUnit)
    WHERE ip.TrnID IN (
        SELECT ft.TransactionID FROM dbo.FolioTransaction ft
        WHERE ft.BillID = @BillID
          AND (' + @pred + N'));';
    EXEC sp_executesql @sql3, N'@BillID numeric', @BillID = @BillID;
END;
GO
