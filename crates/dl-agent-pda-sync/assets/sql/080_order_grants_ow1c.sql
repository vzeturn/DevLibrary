-- OW1c order verbs (ADR 0107): the store-side write of order.covers, as a narrow column grant to [pda_sync]. No schema
-- change and no vendor object is altered (docs/internal/pos-compat.md); idempotent (GRANT is repeatable). The batch that uses
-- it is OrderCommandSql.SetCovers (Commands/OrderCommandSql.BatchesOw1c.cs), ported verbatim from the legacy PDA order
-- repository (a test pins the pairing). Reads are db_datareader (050_grants.sql). 070_order_grants.sql is applied and stays
-- as it is; the verbs ADR 0107 keeps guarded (combo, transfer/merge, split, member, course) get no grant.

-- order.covers: the guest and child count of an open check, and who changed it when. The open check already inserts these
-- columns (070), so every supported build has them.
GRANT UPDATE ON OBJECT::dbo.tmpInvoice (NGuest, NChild, LastChgCashier, LastChgTime) TO [pda_sync];
