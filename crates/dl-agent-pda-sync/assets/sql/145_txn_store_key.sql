-- Shift verbs (ADR 0160, RT-0935): the identity key the store gave an aggregate on its first write (CashierShift.ShiftNo, which the
-- cloud cannot choose and the agent cannot set without ALTER on the vendor table) is kept next to the aggregate's version in the
-- agent-owned dbo.Pda_TxnApplied, as a nullable column (NULL for a bill). Additive and idempotent; only the agent's own table is
-- altered, never a vendor object (docs/internal/pos-compat.md). The table is created by 130; its pda_sync grant (SELECT, INSERT,
-- UPDATE) is table-wide, so the new column needs none.
IF OBJECT_ID(N'dbo.Pda_TxnApplied', N'U') IS NOT NULL AND COL_LENGTH(N'dbo.Pda_TxnApplied', N'StoreKey') IS NULL
    ALTER TABLE dbo.Pda_TxnApplied ADD StoreKey BIGINT NULL;
