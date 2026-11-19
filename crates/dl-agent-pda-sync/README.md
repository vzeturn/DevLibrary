# dl-agent-pda-sync

PDAOrder site sync module for `dl-agent` (module name `pda-sync`). A port of the
`PdaSyncAgent` service (`apps/sql-sync-agent` in SML-PDAOrder, C# and its Rust port) onto
the dl-agent runtime: dl-agent hosts the Windows service, self-update, problem reports
and diagnostics; this crate brings everything that reads or writes `SMILE_POS`.

- **Capture** (`capture`): SELECT-only reads of `SMILE_POS` as the `pda_sync` login: open
  checks every 2 s, masters every 120 s, the closed ledger by keyset, shadow hashes in
  `capture.db`, batches into `outbox.db` (ADR 0016).
- **Upload** (`outbox`): signed batches, blobs and the 15 s heartbeat to the PDA cloud,
  parent-pending parking (HTTP 424) and resnapshots (ADR 0102/0109).
- **Commands** (`commands`): the cloud's site commands (KDS, reservations, order verbs,
  covers, combo, table moves) in one SQL transaction each, journalled in
  `Pda_SyncCommandLog` so a redelivered command is never applied twice (ADR 0029/0100).
- **Verify** (`commands::verify`, `capture::{shadow_manifest, key_repair}`): the cloud's
  `sync.verify` compares the shadow with `GET /sync/v1/manifest` per bucket, narrows a
  mismatch to row keys (`/sync/v1/manifest/keys`) queued in `outbox.db`'s `key_repair`, and
  the capture's `key-repair` job resends or tombstones them (ADR 0126/0128). This heals
  rows deleted while no agent ran (fresh enrol). Open-check tables and tables that tolerate
  duplicate rows (`CashierRVC`) are not repaired by key, as in C#.
- **Bootstrap** (`bootstrap`): the `pda_sync` login, the `Pda_Sync*` tables and the
  grants `010`–`090`, run by `dl-agent pda-enroll` under the operator's Windows login or a
  SQL admin (`PDA_SQL_ADMIN_PASSWORD`).
- **Staff** (`staff`): `CashierDef` to the cloud with hashed PINs (ADR 0101/0114).
- **Print relay** (`printrelay`): kitchen prints from the cloud's `/hubs/order` to raw TCP
  or Windows spooler printers (ADR 0098/0105).

Every wire body, row hash and SQL text matches the C# agent byte for byte: the golden
tests read the shared catalog (`assets/catalog`), the bootstrap scripts (`assets/sql`)
and copies of the C# sources (`testdata/cs`).

```text
dl-agent pda-enroll --code <code> --url <https cloud url> [--dry-run]
                    [--sql-instance <instance>] [--database SMILE_POS] [--sql-admin-user <login>]
```

Live SQL tests run when `PDA_SYNC_TEST_SQL` holds a dl-tds URL of a login that may
create databases; each test creates and drops its own `pda_sync_test_*` database.
`tests/live_orders*.rs` run every order, combo, covers, table, KDS and reservation command
as `pda_sync` against the SMILE_POS fixture (`testdata/sql/smile_pos_*.sql`), each one
delivered twice; `scripts/live-sql.sh` runs all live tests on SQL Server 2022 and 2017.

The module declares the PDA cloud to the dl-agent watchdog from the first heartbeat the
cloud accepted (`reporting/cloud_link.rs`), and every Error site event also goes to the
hub as a problem report with its code (`reporting/module_reports.rs`).
