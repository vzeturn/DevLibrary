-- dbo.MMEI_SourceHash(@Source, @ID) -> varbinary(32)
--
-- The single source-fingerprint used by BOTH the select SPs (to decide whether
-- a bill changed since last push) and sp_MarkCollected (to record what was
-- pushed). Defining it once guarantees the two sides compute an identical hash
-- from identical inputs — the whole change-detection contract depends on that.
--
-- It must fingerprint every source column that flows into the pushed payload:
-- the header money/buyer fields AND a stable checksum over the child charge /
-- item rows. If the DBA extends a fetch SP with more source columns, mirror the
-- addition here so an edit to those columns triggers a re-collect.
--
-- A scalar UDF may NOT run EXEC / sp_executesql (SQL Server error 557), so the
-- FO branch — whose revenue predicate + 3 probe columns vary by schema — is made
-- STATIC by having the Go installer (InstallCollectSQLString) substitute the
-- schema-dependent fragments at install time. It probes the SAME COL_LENGTH /
-- OBJECT_ID conditions sp_FetchFO uses and replaces the /*{{…}}*/ placeholder
-- tokens below before CREATE OR ALTER, so the installed body statically
-- references only columns that exist on THIS install. Never hand-run this file
-- with the tokens unresolved — install it through the Go path.
--
-- Deploy AFTER 00_marker_table.sql and BEFORE the SPs.
CREATE OR ALTER FUNCTION dbo.MMEI_SourceHash (@Source varchar(4), @ID varchar(64))
RETURNS varbinary(32)
AS
BEGIN
    DECLARE @sig nvarchar(max) = N'';

    IF @Source = 'FO'
    BEGIN
        DECLARE @BillID numeric = TRY_CONVERT(numeric, @ID);

        -- ĐỢT 2 FO revenue predicate — the fenced block below is byte-identical
        -- (after whitespace normalisation) to sp_FetchFO's fenced block, so the
        -- Go drift guard's cross-file parity check passes. sp_FetchFO picks its
        -- predicate at RUN time via sp_executesql (legal in an SP); this scalar
        -- UDF cannot, so the installer resolves the SAME branch at INSTALL time
        -- and substitutes the static WHERE at /*{{FO_STATIC_PRED}}*/ below. The
        -- @pred builder here is kept verbatim (for fence parity) and its value is
        -- intentionally unreferenced — the static predicate carries the filter.
        /*MMEI_FO_PREDICATE*/
        DECLARE @pred nvarchar(max);
        IF COL_LENGTH('dbo.TransactionCode', 'TransactionGroup') IS NOT NULL
           AND COL_LENGTH('dbo.FolioTransaction', 'FolioBalanceCode') IS NOT NULL
            SET @pred = N'EXISTS (SELECT 1 FROM dbo.TransactionCode tcp WHERE tcp.TransactionCode = ft.TransactionCode AND tcp.TransactionGroup = 1) AND ft.FolioBalanceCode NOT IN (''P'', ''V'', ''C'')';
        ELSE
            SET @pred = N'ft.TransactionCode IN (''110'', ''150'', ''160'', ''170'', ''200'', ''201'', ''210'', ''211'', ''220'', ''221'', ''230'', ''240'')';
        /*END_MMEI_FO_PREDICATE*/

        -- Header fields that flow into the payload (money + buyer + content).
        -- CONCAT (not CONCAT_WS, which is SQL Server 2017+ — unavailable on the
        -- customer's 2016 RTM box) with '|' spliced between every argument.
        -- CONCAT null-coalesces each arg to '', matching CONCAT_WS's NULL
        -- handling closely enough here since every arg is already ISNULL/CAST-wrapped.
        SELECT @sig = CONCAT(
            ISNULL(b.CompanyName, ''), '|', ISNULL(b.VATCode, ''), '|', ISNULL(b.Address1, ''), '|',
            -- Buyer email flows from the folio's first guest profile, not b.EMail
            -- (see sp_FetchFO CustomerEmail). Hash the SAME resolved value so an
            -- edit to the guest's Profile.Email re-collects the bill; fall back to
            -- b.EMail exactly as the fetch does, so the fingerprint tracks the
            -- payload byte-for-byte.
            ISNULL(NULLIF((
                SELECT TOP (1) p.Email
                FROM dbo.GuestEveryDay g
                JOIN dbo.Profile p ON p.ProfileNumber = g.ProfileID
                WHERE g.FolioNum = b.FolioNum
                  AND g.IsAdditionalName = 1
                  AND g.ProfileID > 0
                  AND p.Email IS NOT NULL AND p.Email <> ''
                ORDER BY g.FolioSubNum, g.GuestID
            ), ''), ISNULL(b.EMail, '')), '|', ISNULL(b.BillSeri1, ''), '|',
            ISNULL(CAST(b.BillNumber1From AS varchar(30)), ''), '|',
            CONVERT(varchar(19), b.InvDate, 120), '|',
            CAST(ROUND(ISNULL(b.SubAmount, 0), 0) AS varchar(30)), '|',
            CAST(ROUND(ISNULL(b.ServiceCharge, 0), 0) AS varchar(30)), '|',
            CAST(ROUND(ISNULL(b.TaxAmount, 0), 0) AS varchar(30)), '|',
            CAST(ROUND(ISNULL(b.TotalAmount, 0), 0) AS varchar(30)), '|',
            ISNULL(b.GuestName, ''), '|', ISNULL(b.Address2, ''), '|', ISNULL(b.BillNote, ''), '|',
            -- Cancel/print state: a cancel or re-print must re-collect.
            CAST(ISNULL(b.Cancel, 0) AS varchar(4)), '|', CAST(ISNULL(b.Printed, 0) AS varchar(4)))
        FROM dbo.BillInfor b WHERE b.BillID = @BillID;

        -- Child charge lines (order-independent aggregate). Any change to a
        -- folio charge's identity or NET amount alters the checksum. Each
        -- per-row hash is reduced modulo a large prime BEFORE summing — a bill
        -- can carry enough charge rows that summing raw MD5-derived bigints
        -- (each up to ~2^63 in magnitude) overflows bigint (verified live on
        -- SMILE_FO). The modulo keeps every addend small while remaining a
        -- deterministic, order-independent function of each row's content —
        -- matching the POS branch's #TRN aggregate below. STATIC: the revenue
        -- predicate and the 3 probe columns (TrnGroup/IsHKPost/TaxRate) are
        -- resolved at INSTALL time via the /*{{…}}*/ tokens, so no dynamic SQL.
        SELECT @sig = CONCAT(@sig, N'#FT:',
            ISNULL(CAST(SUM(CAST(HASHBYTES('MD5', CONCAT(
                CAST(ft.TransactionID AS varchar(30)), '|', ISNULL(ft.TransactionCode, ''), '|',
                ISNULL(CAST(ft.TransactionSubCode AS varchar(10)), ''), '|',
                ISNULL(tc.Description, ''), '|',
                ISNULL(ft.Description, ''), '|',
                CAST(ISNULL(ft.Quantity, 1) AS varchar(30)), '|',
                CAST(ROUND(ISNULL(ft.SubAmount, 0), 0) AS varchar(30)), '|',
                CAST(ROUND(ISNULL(ft.TaxAmount, 0), 0) AS varchar(30)), '|',
                ISNULL(ft.STaxCode, ''), '|',
                CAST(ROUND(ISNULL(ft.STaxAmount, 0), 0) AS varchar(30)), '|',
                CAST(ROUND(ISNULL(ft.ServiceCharge, 0), 0) AS varchar(30)), '|',
                -- ĐỢT 2 raw classification columns, fingerprinted so an edit
                -- to the catalog group, the HK flag, or the TaxDef rate recollects.
                -- Each expression below is substituted by the installer: the real
                -- column ref when present, or the literal fallback ('' / '0') when
                -- absent — matching sp_FetchFO's own probe fallbacks so the hash
                -- never drifts from what the fetch SP emits.
                /*{{FO_GRP_EXPR}}*/'', '|',
                /*{{FO_HK_EXPR}}*/'0', '|',
                /*{{FO_RATE_EXPR}}*/'')) AS bigint) % 999999999989) AS varchar(40)), '0'),
            N':', CAST(COUNT(*) AS varchar(20)))
        FROM dbo.FolioTransaction ft
        OUTER APPLY dbo.SA_TrnCode_Resolve(ft.TransactionCode, ft.TransactionSubCode, 0, 0) trn
        LEFT JOIN dbo.TransactionCode tc
            ON tc.TransactionCode = trn.TransactionCode
           AND tc.TransactionSubCode = trn.TransactionSubCode
        /*{{FO_RATE_JOIN}}*/
        WHERE ft.BillID = @BillID
          AND (/*{{FO_STATIC_PRED}}*/1 = 1);

        -- Detail items (order-independent aggregate). Same modulo-before-sum
        -- guard as the folio aggregate above (bill 10384's detail alone spans
        -- multiple charges' rows; raw bigint sums overflow live). Its parent
        -- subquery reuses the SAME static predicate so it fingerprints exactly
        -- the detail rows sp_FetchFO's result set 3 emits.
        SELECT @sig = CONCAT(@sig, N'#IP:',
            ISNULL(CAST(SUM(CAST(HASHBYTES('MD5', CONCAT(
                CAST(ip.TrnID AS varchar(30)), '|', ISNULL(ip.ICode, ''), '|',
                ISNULL(ip.IName, ''), '|',
                ISNULL(ig.GroupName, ''), '|',
                CAST(ROUND(ISNULL(ip.Price, 0), 0) AS varchar(30)), '|',
                CAST(ISNULL(ip.Qty, 1) AS varchar(30)), '|',
                CAST(ROUND(ISNULL(ip.Amount, 0), 0) AS varchar(30)))) AS bigint) % 999999999989) AS varchar(40)), '0'),
            N':', CAST(COUNT(*) AS varchar(20)))
        FROM dbo.ItemPosted ip
        LEFT JOIN dbo.ItemList il ON RTRIM(il.ICode) = RTRIM(ip.ICode)
        LEFT JOIN dbo.ItemGroup ig ON RTRIM(ig.GroupCode) = RTRIM(il.IGroup)
        WHERE ip.TrnID IN (
            SELECT ft.TransactionID FROM dbo.FolioTransaction ft
            WHERE ft.BillID = @BillID
              AND (/*{{FO_STATIC_PRED}}*/1 = 1));
    END
    ELSE IF @Source = 'POS'
    BEGIN
        DECLARE @CheckNo numeric = TRY_CONVERT(numeric, @ID);

        -- Header fields that flow into the payload: check-level money + status
        -- + TaxOmit (VAT-omission flag → KCT) + the FolioNum dedup gate, plus
        -- the TaxTable buyer identity sp_FetchPOS v2 now reads (a buyer-info
        -- edit must trigger a recollect). LEFT JOIN so a check with no TaxTable
        -- row still fingerprints (matching the fetch SP's join).
        -- CONCAT (not CONCAT_WS, 2017+ only) with '|' spliced between args;
        -- CONCAT null-coalesces each arg to '', so a NULL CloseTime/OpenTime
        -- pair no longer nulls out the whole signature the way `+` would.
        SELECT @sig = CONCAT(
            CAST(i.CheckNo AS varchar(30)), '|',
            CONVERT(varchar(19), ISNULL(i.CloseTime, i.OpenTime), 120), '|',
            -- The payload InvDate takes its CALENDAR date from IYear/IMonth/IDay
            -- (CloseTime supplies only the time-of-day; sp_FetchPOS builds it that
            -- way because CloseTime lacks a stable calendar date on this schema).
            -- The CloseTime component above therefore does NOT protect the invoice
            -- date — fingerprint the integer date parts here or a ngày-lập edit is
            -- silently missed.
            CAST(ISNULL(i.IYear, 0) AS varchar(10)), '|',
            CAST(ISNULL(i.IMonth, 0) AS varchar(10)), '|',
            CAST(ISNULL(i.IDay, 0) AS varchar(10)), '|',
            CAST(ROUND(ISNULL(i.SubTotal, 0), 0) AS varchar(30)), '|',
            CAST(ROUND(ISNULL(i.TaxTotal, 0), 0) AS varchar(30)), '|',
            -- STaxTotal (thuế TTĐB) rides its own header field into the payload and
            -- projects a "Thuế đặc biệt" wire line; a special-tax edit must recollect.
            CAST(ROUND(ISNULL(i.STaxTotal, 0), 0) AS varchar(30)), '|',
            CAST(ROUND(ISNULL(i.TotalBase, 0), 0) AS varchar(30)), '|',
            CAST(ROUND(ISNULL(i.Discount, 0), 0) AS varchar(30)), '|',
            CAST(ROUND(ISNULL(i.SurCharge, 0), 0) AS varchar(30)), '|',
            CAST(ISNULL(i.InvStatus, 0) AS varchar(10)), '|',
            CAST(ISNULL(i.Status, 0) AS varchar(10)), '|',
            CAST(ISNULL(i.TaxOmit, 0) AS varchar(10)), '|',
            ISNULL(CAST(i.RVC AS varchar(10)), ''), '|',
            ISNULL(CAST(i.FolioNum AS varchar(30)), ''), '|',
            ISNULL(tt.ClientVAT, ''), '|', ISNULL(tt.Company, ''), '|',
            ISNULL(tt.ClientName, ''), '|', ISNULL(tt.Address, ''), '|',
            -- InvContent flows into the collected header (nội dung hóa đơn), so an
            -- edit to it on an un-issued check must re-collect; fingerprint it here
            -- or the change is silently missed.
            ISNULL(tt.InvContent, ''))
        FROM dbo.Invoice i
        LEFT JOIN dbo.TaxTable tt ON tt.TCheckNo = i.CheckNo
        WHERE i.CheckNo = @CheckNo;

        -- Line rows (order-independent aggregate). Both revenue (TrnCode=100)
        -- and discount (110) rows are fingerprinted over their identity + the
        -- EXACT per-line money (BaseSub/BaseTax/TaxNo + BaseSTax/STaxNo) that
        -- sp_FetchPOS v2 reads — an edit to a line's net/VAT/rate bucket, its
        -- special-tax (TTĐB) amount/rate, or a discount row change alters the
        -- checksum. Adding/removing a row changes the COUNT too.
        --
        -- NOTE: unlike the FO branch's SUM(CAST(HASHBYTES(...) AS bigint)),
        -- here each per-row hash is reduced modulo a large prime BEFORE
        -- summing. A POS check can carry 30+ lines (verified: check 200021306
        -- has 35), and summing raw MD5-derived bigints (each up to ~2^63 in
        -- magnitude) overflows bigint well before the FO side's typical row
        -- counts would — verified live via arithmetic overflow on SMILE_POS.
        -- The modulo keeps every addend small while remaining a deterministic,
        -- order-independent function of each row's content.
        SELECT @sig = CONCAT(@sig, N'#TRN:',
            ISNULL(CAST(SUM(CAST(HASHBYTES('MD5', CONCAT(
                CAST(t.TrnSeq AS varchar(30)), '|', CAST(ISNULL(t.TrnCode, 0) AS varchar(10)), '|',
                ISNULL(t.ItemCode, ''), '|',
                -- Per-line t.RVC (distinct from the header i.RVC hashed above): it
                -- flows to Origin.RVC and is exposed as the {RVC} line-name template
                -- var, so a template using {RVC} renders it into the pushed wire line.
                -- Editing one line's Trn.RVC changes the payload; hash it so a lone
                -- RVC edit (no other co-hashed column touched) still re-collects.
                CAST(ISNULL(t.RVC, 0) AS varchar(10)), '|',
                -- TrnDesc (wire line name via sp_FetchPOS Description → payload Name)
                -- and ItemPrice (wire UnitPrice, an independent column NOT re-derived
                -- from BaseSub/Qty) both flow into the pushed line — mirror the FO #IP
                -- branch (IName/Price) so a name fix, or a unit-price edit at constant
                -- BaseSub/Qty, still triggers a re-collect.
                ISNULL(t.TrnDesc, ''), '|',
                CAST(ROUND(ISNULL(t.ItemPrice, 0), 0) AS varchar(30)), '|',
                CAST(ISNULL(t.TrnQty, 1) AS varchar(30)), '|',
                CAST(ROUND(ISNULL(t.BaseSub, 0), 0) AS varchar(30)), '|',
                CAST(ROUND(ISNULL(t.BaseTax, 0), 0) AS varchar(30)), '|',
                CAST(ISNULL(t.TaxNo, 0) AS varchar(10)), '|',
                CAST(ROUND(ISNULL(t.BaseSTax, 0), 0) AS varchar(30)), '|',
                CAST(ISNULL(t.STaxNo, 0) AS varchar(10)))) AS bigint) % 999999999989) AS varchar(40)), '0'),
            N':', CAST(COUNT(*) AS varchar(20)))
        FROM dbo.Trn t
        WHERE t.CheckNo = @CheckNo
          AND t.TrnCode IN (100, 110);
    END

    RETURN CASE WHEN @sig IS NULL THEN NULL ELSE HASHBYTES('SHA2_256', @sig) END;
END;
GO
