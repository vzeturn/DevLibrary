# dl-agent-pda-sync

PDAOrder site sync module for `dl-agent` (module name `pda-sync`). A port of the
`PdaSyncAgent` service (`apps/sql-sync-agent` in SML-PDAOrder, C# and its Rust port) onto
the dl-agent runtime: dl-agent hosts the Windows service, self-update, problem reports
and diagnostics; this crate brings everything that reads or writes `SMILE_POS`.

- **Capture** (`capture`): SELECT-only reads of `SMILE_POS` as the `pda_sync` login: open
  checks every 2 s, masters every 120 s, the closed ledger by keyset, shadow hashes in
  `capture.db`, batches into `outbox.db` (ADR 0016). Open checks are read in two phases
  (PDAOrder ADR 0153, RT-0759): one `COUNT_BIG`/`CHECKSUM_AGG`/`SUM(BINARY_CHECKSUM)` row
  per check and table, then only the checks whose signature moved; every 30th cycle
  (`open_full_read_every`, 1 disables it), after a reset and on a source without
  signatures the whole open set is read.
- **Upload** (`outbox`): signed batches, blobs and the 15 s heartbeat to the PDA cloud,
  parent-pending parking (HTTP 424) and resnapshots (ADR 0102/0109).
- **Commands** (`commands`): the cloud's site commands (KDS, reservations, order verbs,
  covers, combo, table moves) in one SQL transaction each, journalled in
  `Pda_SyncCommandLog` so a redelivered command is never applied twice (ADR 0029/0100).
- **Legacy export** (`legacy_export`): the cloud's `legacy.export` command does what
  `migrate-legacy.ps1` does by hand: it acks at once, then exports the on-prem store
  (`pda.db`, or the SQL Server tables of a build from before it, SELECT only) into a bundle
  under `<data dir>\legacy-export`, uploads it signed as a dry run, with `commit` commits it
  and counts again, deletes the bundle whatever happened, and reports site events of sources
  `legacy-export` / `legacy-upload` (RT-0390, ADR 0136).
- **Verify** (`commands::verify`, `capture::{shadow_manifest, key_repair}`): the cloud's
  `sync.verify` compares the shadow with `GET /sync/v1/manifest` per bucket, narrows a
  mismatch to row keys (`/sync/v1/manifest/keys`) queued in `outbox.db`'s `key_repair`, and
  the capture's `key-repair` job resends or tombstones them (ADR 0126/0128). This heals
  rows deleted while no agent ran (fresh enrol). Open-check tables and tables that tolerate
  duplicate rows (`CashierRVC`) are not repaired by key, as in C#.
- **Bootstrap** (`bootstrap`): the `pda_sync` login, the `Pda_Sync*` tables and the
  grants `010`–`090`, run by `dl-agent pda-enroll` under the operator's Windows login or a
  SQL admin (`PDA_SQL_ADMIN_PASSWORD`). The running service applies missing order grants
  itself, as the Windows identity and then the SMILE default login, every 15 minutes while
  they are missing; it only touches `pda_sync`'s objects and grants (ADR 0010).
- **Staff** (`staff`): `CashierDef` to the cloud with hashed PINs (ADR 0101/0114).
- **Print relay** (`printrelay`): kitchen prints from the cloud's `/hubs/order` to raw TCP
  or Windows spooler printers (ADR 0098/0105). With `PrinterSubnets` empty it dials any
  private LAN address (`10/8`, `172.16/12`, `192.168/16`, `fc00::/7`) on the allowed ports
  (default `9100`); a list narrows it. Public, loopback and link-local addresses are always
  refused, so it is never an open TCP proxy (ADR 0010).

Every wire body, row hash and SQL text matches the C# agent byte for byte: the golden
tests read the shared catalog (`assets/catalog`), the bootstrap scripts (`assets/sql`)
and copies of the C# sources (`testdata/cs`).

```text
dl-agent pda-enroll --code <code> --url <https cloud url> [--dry-run]
                    [--sql-instance <instance>] [--database SMILE_POS]
                    [--sql-admin-user <login> | --sql-admin-saved]
```

Without an explicit administrator the tool tries, in order (RT-0365): (1) the Windows account
running it (LocalSystem as a service); (2) the SMILE default SQL login `smile`
(`DL_AGENT_PDA_SQL_DEFAULT_USER` renames it; its password is `DL_AGENT_PDA_SQL_DEFAULT_PASSWORD`,
else the default built into the release; with neither, that attempt is recorded as skipped).
Only an access refusal (`SQL_NO_ACCESS`, `SQL_LOGIN_FAILED`, `SQL_DATABASE_NOT_FOUND`) moves on
to the next identity; the default login only creates `pda_sync` and is never altered. Each
attempt is a console line and an `ENROLL_SQL_ADMIN_ATTEMPT` site event (never the password);
when both are refused the error lists them. To name the administrator yourself (no fallback):
pass `--sql-admin-user <login>` with the password in
`DL_AGENT_PDA_SQL_ADMIN_PASSWORD` (or `PDA_SQL_ADMIN_PASSWORD`), or `--sql-admin-saved` to
use the SQL login dl-agent keeps in `sql.json` (the POS database's, else the first SQL login;
its server becomes `--sql-instance` unless one is given).

The code may come from the environment variable `DL_AGENT_PDA_CODE` instead of `--code`
(the argument wins), so an installer such as `install.ps1 -Demo` keeps it out of the
process list.

Live SQL tests run when `PDA_SYNC_TEST_SQL` holds a dl-tds URL of a login that may
create databases; each test creates and drops its own `pda_sync_test_*` database.
`tests/live_orders*.rs` run every order, combo, covers, table, KDS and reservation command
as `pda_sync` against the SMILE_POS fixture (`testdata/sql/smile_pos_*.sql`), each one
delivered twice; `scripts/live-sql.sh` runs all live tests on SQL Server 2022 and 2017.

The module declares the PDA cloud to the dl-agent watchdog from the first heartbeat the
cloud accepted (`reporting/cloud_link.rs`), and every Error site event also goes to the
hub as a problem report with its code (`reporting/module_reports.rs`).
