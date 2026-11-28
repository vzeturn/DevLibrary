-- txn.stock_deplete, txn.stock_restore (ADR 0164, RT-1022): two agent-owned tables in the POS database. Additive and idempotent; no
-- vendor object is touched (docs/internal/pos-compat.md).
-- dbo.Pda_TxnStockMap: where a cloud stock store (TxnStockLine.StoreCode, the cloud RVC) is booked in SMILE_BO. The operator fills one
--   row per cloud store (as a SQL administrator; the agent only reads it): BoDatabase (NULL = SMILE_BO), StoreRvc (the BO store the
--   goods leave, tmpINVTrn.RVC), WithRvc (the consumption location, WithRVC), DeptCode and JType (NULL = the vendor default, the last
--   JType of VoucherType 'SX'). A command naming an unmapped store is refused with txn_stock_store_unmapped and writes nothing.
-- dbo.Pda_TxnStockHeld: what each bill aggregate holds out of SMILE_BO stock after its last applied command, so a newer version
--   writes only the difference and a restore puts back exactly what was taken, on the day it was taken.
IF OBJECT_ID(N'dbo.Pda_TxnStockMap', N'U') IS NULL
    CREATE TABLE dbo.Pda_TxnStockMap (
        StoreCode  INT          NOT NULL CONSTRAINT PK_Pda_TxnStockMap PRIMARY KEY,
        BoDatabase SYSNAME      NULL,
        StoreRvc   INT          NOT NULL,
        WithRvc    INT          NOT NULL,
        DeptCode   VARCHAR(10)  NULL,
        JType      VARCHAR(20)  NULL,
        AtUtc      DATETIME2(3) NOT NULL CONSTRAINT DF_Pda_TxnStockMap_AtUtc DEFAULT SYSUTCDATETIME()
    );
IF OBJECT_ID(N'dbo.Pda_TxnStockHeld', N'U') IS NULL
    CREATE TABLE dbo.Pda_TxnStockHeld (
        AggregateKey NVARCHAR(100) NOT NULL,
        ItemCode     VARCHAR(20)   NOT NULL,
        StoreCode    INT           NOT NULL,
        Qty          DECIMAL(18,4) NOT NULL,
        BusinessDate DATE          NOT NULL,
        CONSTRAINT PK_Pda_TxnStockHeld PRIMARY KEY (AggregateKey, ItemCode, StoreCode)
    );
GRANT SELECT ON OBJECT::dbo.Pda_TxnStockMap TO [pda_sync];
GRANT SELECT, INSERT, DELETE ON OBJECT::dbo.Pda_TxnStockHeld TO [pda_sync];
