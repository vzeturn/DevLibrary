# dl-agent-pms

The SMILE PMS site agent as a [dl-agent](../dl-agent/README.md) module (`pms-site`). It does
the work of `pms-agent` (Windows service `SmilePmsAgent`; Go `apps/site-agent`, ported to Rust
in `apps/site-agent/rust`) inside the shared agent:

- enrolls with a one-time code, keeps the site key (`agent.key`, DPAPI machine scope on
  Windows, 0600 elsewhere) and `identity.json`, and exchanges a signed request for a stream
  token;
- holds the WebSocket stream to the PMS cloud (`/agent/v1/stream`: `hello`, ping/pong, ops,
  at most 8 in flight, close codes, back-off 1 → 30 s);
- runs allow-listed ops against the hotel's SMILE databases only: procedures from the
  embedded ops manifest, binds, deadlock retry, result caps, each op on the database its
  frame names;
- answers replays of keyed writes from a local op journal.

### The SMILE-only contract

SMILE-PMS decided on 2026-11-19 that a site holds only the vendor databases. `MM_PMS` lives
in the cloud, the cloud's ledger is authoritative, and the agent never creates an object on a
customer database (no `MM_OpLog`, no migrations, no retention job).

- **Databases**: `SMILE_FO`, `SMILE_POS`, `SMILE_BO`. Each comes from dl-agent's `sql.json`
  when it declares that name (`dl-agent sql-setup` / `sql-add`: auth `windows` or `sql`,
  sealed password, the saved TLS rung), else from `databases.json` in the state directory
  (`SmilePmsAgent`'s file). An `MM_PMS` entry is ignored (`pms.status` lists it under
  `ignored`). Pools are opened lazily; the module starts without any database.
- **Routing**: an op frame carries `target_db` (`SMILE_FO` / `SMILE_POS` / `SMILE_BO`; any
  other value is a bad frame, close 4400). The executor refuses an op whose manifest
  `target_db` differs (`rejected`, `target_db_mismatch`) and one whose database is not
  configured (`failed`, `DB_UNAVAILABLE`).
- **`hello`**: `schema_version: 0`, `contract_max` and `ops: ["fo.hotelDate.get@1", …]`,
  the ops of the embedded manifest that have agent calls (what this build runs). The cloud
  gates on that list; the agent still answers a `contract_ver` it does not run `rejected`
  `CONTRACT_UNSUPPORTED`, and an op it does not know or that runs in the cloud only
  `rejected` `OP_REJECTED` (`unknown_op` / `in_process_only`), as SMILE-PMS's shared
  `rejections.json` fixture says.
- **Op journal** (`<state_dir>/op-journal`, one file per request key, named by its SHA-256;
  the key itself is never stored): a keyed write that `applied` is recorded after its commit
  (atomic write: temporary file, fsync, rename, fsync of the directory). Entries are kept 8
  days; at 100 000 entries or 256 MiB new keyed writes are refused before they run
  (`failed` `JOURNAL_FULL`) with a critical `pms_journal_failed` report. Reads carry no key
  and are never journaled.
- **Replay**: an op with `replay: true` is answered from the journal (`replayed: true`) or
  `rejected` `REQUEST_KEY_UNKNOWN`; it never runs SQL. A keyed write already in the journal
  is answered from it as well; the same key for another op is `request_key_conflict`.
- `migrate` frames from a cloud on the old contract are logged (`agent.frame.retired`) and
  ignored; `dl-agent pms-migrate` only prints that it is retired.

### Ops

| Op | Database | Procedure | Result |
|---|---|---|---|
| `fo.hotelDate.get@1` (read) | `SMILE_FO` | vendor `GetHotelDate1` (`Select HotelDate From HotelParameter`) | one set, first row `HotelDate` (datetime, wall clock as stored); no row or `NULL` → `failed` `NO_DATA`. The cloud returns `{hotelDate: 'YYYY-MM-DD'}`, the date part with no timezone shift. |
| `fo.clerk.login@1` (write, keyless, not idempotent) | `SMILE_FO` | vendor `LogIn` (`ClerkID`, `Password` = `passwordHash`, `TodayPassword`, `ComputerName` = manifest constant `Azolo PMS`) | the procedure's sets and `ReturnValue`. Tried once, never journaled or replayed. `passwordHash` and `todayPassword` are secrets: no log line, telemetry report or bind error carries an arg value. |
| `fo.clerk.rights.get@1` (read) | `SMILE_FO` | vendor `UGetUserPermission` (`UserID` = `clerkId`) | the clerk's rights rows. |
| `fo.availability.chart@1` (read) | `SMILE_FO` | vendor `AvailabilityChartNew` (`roomType`, `startDate`, `numDays`) | the chart rows. |
| `fo.availability.source@1` (read) | `SMILE_FO` | no procedure: ten `select` calls (ADR 0007) on `HotelParameter`, `RoomType`, `Folio`, `GroupFolio`, `GroupBlockDetail`, `AllotmentContract`, `AllotmentContractWeekday` (`startDate`, `endDate`), only the manifest's declared columns, values bound | one set per call; the cloud computes availability and treats a set that reaches `max_rows` as possibly truncated (error). Replaces the chart on SR, where `AvailabilityChartNew` fails with SQL 8152 (RT-0474, RT-0614). |
| `fo.availability.sourceV2@1` (read) | `SMILE_FO` | the same `select` calls as `source@1`, but `AllotmentContract` is read without `ALMStatus` (absent on the SR SMILE_FO) | one set per call; same truncation rule as `source@1`. The cloud uses it when `source@1` fails on a missing column (RT-0474). |
| `fo.availability.contractStatus@1` (read) | `SMILE_FO` | one `select` on `AllotmentContract` (`TANo`, `ContractNo`, `ALMStatus`) for contracts overlapping `startDate`..`endDate` | the contract status rows; fails on a SMILE_FO without `ALMStatus`, which the cloud treats as "no status" (RT-0474). |
| `fo.reservation.list@1` (read) | `SMILE_FO` | vendor `rptReservMasterList` (`fromDate`, `toDate`) | reservation rows (guest names). |
| `fo.reservation.listFigures@1` (read) | `SMILE_FO` | three `select` calls (ADR 0007) on `Folio`: reservations made on `date`, cancellations on `date`, no-shows of the previous day (`date`, `nextDate`, `prevDate`) | `RoomTypeCode` (and `NoShowFlag`) per row; the cloud counts. |
| `fo.dashboard.today@1` (read) | `SMILE_FO` | three `select` calls (ADR 0007) on `Folio`: arrivals, departures and check-outs of `date` (`date`, `nextDate`) | folio status, cancel, room type, flags and guest counts; the cloud counts rooms and guests (no names). |
| `fo.dashboard.houseMix@1` (read) | `SMILE_FO` | three `select` calls: in-house `Folio` rows of `date`, those staying past it, and the `Profile` rows with a VIP level | counts only after the cloud's tally. |
| `fo.dashboard.rooms@1` (read) | `SMILE_FO` | seven `select` calls: `Room` (housekeeping bits) and in-house, out-of-inventory (4) and out-of-order (5) `Folio` blocks of `date` | the cloud derives the GetCurrentStatus counts and the room board. |
| `fo.departures.onDate@1` (read) | `SMILE_FO` | two `select` calls (ADR 0007): `AdditionalName` guests departing on `date` and the main `Folio` rows of that day | guest and folio columns the manifest declares; the cloud joins them. |
| `fo.arrivals.expected@1`, `fo.departures.expected@1`, `fo.inhouse.list@1` (read) | `SMILE_FO` | vendor `HSKPArrivalExpected`, `HSKPDepartureExpected`, `HSKPCurrentOccupied` (`date`) | room/guest rows. |

Result rows (guest names) are sent to the cloud only; the agent logs op name, id, status, code
and duration, never rows or args.

#### Select calls ([ADR 0007](../../docs/decisions/0007-pms-select-calls.md))

A read op may read a table directly instead of calling a procedure, but only a table and
columns the manifest declares in its top-level `tables` list:

```json
"tables": [{"name": "Room", "columns": ["RoomNo", "RoomType", "Status"]}],
"calls": [{"select": {
  "table": "Room",
  "columns": ["RoomNo", "RoomType", "Status"],
  "where": [{"column": "RoomType", "op": "=",
             "param": {"name": "roomType", "type": "varchar", "arg": "roomType", "size": 10, "nullable": false}}],
  "order_by": ["RoomNo"],
  "max_rows": 2000}}]
```

- A call has exactly one of `proc` (with `params`) or `select`; a select only in an op of kind `read`.
- Table and column names are bare identifiers, matched case-insensitively against `tables`;
  `columns` is non-empty without duplicates. Table names matching `^sys`, `^#` or
  `^INFORMATION_SCHEMA$` are refused (the table floor, next to the procedure floor). The schema is always `dbo`.
- `op` is one of `=`, `<>`, `<`, `<=`, `>`, `>=`; conditions are joined with `AND`. A `param`
  follows the procedure-parameter rules (bindable type, exactly one of `arg` or `value`), never
  `output`, with unique names in the call. `max_rows` is 1..=10 000.
- All of it is checked when the manifest is parsed, so a bad manifest stops the agent at boot.
- The statement is built from the manifest only, e.g.
  `SELECT TOP (2000) [RoomNo], [RoomType], [Status] FROM [dbo].[Room] WHERE [RoomType] = @roomType ORDER BY [RoomNo]`;
  values are bound through `sp_executesql`, never spliced. It runs like a procedure call (same
  transaction, lock timeout, deadline, retries, caps and encoding) and returns one result set
  without `ReturnValue`. `allowlist_hash` still covers `procs` only. The site's SQL login needs
  `SELECT` on the declared tables.

`assets/ops-manifest.json` is a byte copy of SMILE-PMS `packages/shared/src/ops/ops-manifest.json`
(manifest v2). `tests/ops_manifest_drift.rs` fails when it differs from the file named by
`PMS_OPS_MANIFEST`.

## Turning it on

The `dl-agent` executable (package `dl-agent-bin`) registers the module. It is opt-in: it stays
`disabled` until the hub's remote config enables it for the site.

```json
{"modules": {"pms-site": {
    "enabled": true,
    "config_path": "C:\\ProgramData\\SmilePMS\\Agent\\config.json",
    "state_dir": "C:\\ProgramData\\SmilePMS\\Agent\\state"
}}}
```

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Run the module on this site. |
| `config_path` | `$PMS_AGENT_CONFIG`, else `SmilePmsAgent`'s: `%ProgramData%\SmilePMS\Agent\config.json` (off Windows the relative `pms-agent.json`, so set it there) | The PMS agent config file (`cloud_url`, `state_dir`, `enrollment_code`, `log_level`, …). |
| `state_dir` | the config file's `state_dir`, else `SmilePmsAgent`'s (`…\Agent\state`) | Where `agent.key`, `identity.json`, `databases.json` and `op-journal` live. |

The config file and the state directory are read and written exactly as `SmilePmsAgent` does,
including its `PMS_AGENT_*` environment overrides. The module adds nothing under its own data
directory (`<data>/modules/pms-site`).

## Enrolling a new site

`dl-agent pms-enroll --cloud-url URL --code CODE [--config PATH] [--state-dir DIR] [--force]`
writes `cloud_url` and `enrollment_code` into the config file (created 0600 / with the state
ACL, other keys kept, written atomically). The module enrolls with the code within 30 s, or at
once on `pms.reconnect`, and removes the code from the file afterwards.

- **Already enrolled** (`identity.json` in the state dir): prints one line and changes nothing.
  `--force` removes `identity.json` so the next attempt enrolls with the new code.
- The code is never printed or logged. It may come from `DL_AGENT_PMS_CODE` instead of `--code`
  (the argument wins), which keeps it off the command line; `install.ps1` removes that variable
  after its setup step.
- No service step: dl-agent installs its own service.

## Migrations

None on a site. `dl-agent pms-migrate` is kept so an old script gets a clear answer: it
accepts the old options, changes nothing and exits with dl-agent's config exit code.

## Behaviour

- **Not enrolled** (no `identity.json`, no code): `degraded` with `not enrolled: …`, looks again
  every 30 s; not a failure, no restart, no report.
- **Config invalid** (unreadable file, bad `cloud_url`, …): `degraded` with `config invalid: …`
  and one `pms_config_invalid` report; values from the file are not repeated.
- **Enrolled**: the agent runs on a worker thread. The module is `running` once the cloud's
  first frame arrives, and `degraded` with the reason once the stream has been reconnecting for
  longer than one back-off cycle (30 s).
- **Terminal stop from the cloud** (4409 superseded, 4426 contract, the token exchange refusing
  a revoked agent, a refused enrollment code): the agent stops, the module stays `degraded` with the next step and files a
  critical report; it starts again when the config file or `identity.json` changes, or on
  `pms.reconnect`.
- **Stop**: in-flight ops get up to 5 s, then are cancelled; the stream is closed with 1000.
- `update` frames from the cloud are logged and ignored: dl-agent updates itself.

Metrics: `connected`, `ops_inflight`, `ops_ok`, `ops_rejected`, `ops_failed`, `last_op_unix`,
`reconnects`.

Problem reports (each code at most once per 10 minutes, the next one counts what was held back;
op names, error codes, SQL error numbers and ids only, never SQL text, bind values, results or
request keys): `pms_stream_failed`, `pms_auth_failed`, `pms_sql_failed`, `pms_journal_failed`,
`pms_op_rejected`, `pms_config_invalid`.

Commands:

| Command | Answer |
|---|---|
| `pms.status` | the counters, `connected`, `detail`, `ops`, `contract_max`, `databases` (`configured`: name, source `sql.json`/`databases.json`, auth; `ignored`) and `journal` (dir, entries, bytes, caps, `full`); nothing connects. |
| `pms.reconnect` | closes the stream with 1000 and connects again; also wakes a waiting or parked module. |
| `pms.selftest` | the module's self-test rows as JSON (`checks`): config, cloud, op journal, databases configured, `SELECT 1` per database with its source, auth and TLS rung, and the `fo.hotelDate.get` probe; not ok when a row fails. |
| `pms.op_dry_run` | runs one read (`args.op`, default `fo.hotelDate.get`; `args.args`) locally through the SQL executor, without the cloud or the journal: `{op, target_db, status, took_ms, hotelDate, result}`. Writes are refused. |

The same self-test rows appear in `dl-agent selftest` on the site.

## Differences from the Go agent

| Area | `SmilePmsAgent` | This module |
|---|---|---|
| Process | its own Windows service, `install` / `uninstall` | a module of `dl-agent` |
| Updates | self-update from cloud `update` frames | none; dl-agent updates itself, `update` frames are ignored |
| `hello` `agent_version` | the agent's version | `dl-agent-<version>` (at most 32 characters) |
| Logs | rotated service log file | dl-agent's log (target `pms-site`, same event names) |
| Terminal stop | the service exits 0 | the module parks `degraded` with the next step |
| Migrations | `MM_PMS` bundles, `MM_OpLog` ledger and retention | none (SMILE-only contract) |
| Replay | `MM_Op_GetLog` / `MM_Op_CreateLog` on `MM_PMS` | the local op journal |
| SQL pools | own pools over `databases.json` | dl-agent's `sql.json` first, `databases.json` as fallback |

## Switching a live site over

1. Install or update `dl-agent` on the machine (the hub enrolment is separate from PMS's).
2. Stop and uninstall the old service: `pms-agent uninstall` (or `sc stop SmilePmsAgent`,
   `sc delete SmilePmsAgent`). Keep `%ProgramData%\SmilePMS\Agent`: it holds the identity.
3. Enable the module for the site: `modules.pms-site.enabled = true` (paths only when the old
   agent used a non-default config).
4. Check `pms.status` (`connected: true`). If the old service still runs, the cloud closes one
   stream with 4409; the module parks with "stop or uninstall the other agent", then send
   `pms.reconnect`.

No re-enroll is needed: the module uses the same `agent.key`, `identity.json` and
`databases.json`. To move the connections into dl-agent's sealed store, run
`dl-agent sql-setup` for `SMILE_FO`, `SMILE_POS` and `SMILE_BO`; `sql.json` wins once it
names a database.

## Tests

```text
cargo test -p dl-agent-pms
```

- Unit tests per source file (ported, plus telemetry, settings, prepare, the journal and the
  tools).
- `wire_fixtures`, `ops_contract`: the golden frames (hello with `ops`, op with `target_db`,
  replay) and the op fixtures (`fo.hotelDate.get`, routing and argument rejections).
- `fake_cloud`: the module against a fake PMS cloud on a local socket (enroll, token, hello with
  `ops`, ops, replay hit and miss from the journal, a retired `migrate` frame, a bad
  `target_db`, reconnect after a close, refused code, superseded close, `pms.reconnect`).
- `module_lifecycle`: opt-in, not enrolled, invalid config, `pms.selftest` and
  `pms.op_dry_run` answers, prompt stop with an op in flight, a hung op cancelled after the
  grace.
- `sql_integration`: set `PMS_AGENT_TEST_SQL=sqlserver://sa:<pw>@127.0.0.1:14333?encrypt=disable`
  (SQL Server 2017 and 2022 containers); skipped without it. It creates `pms_agent_it_fo` and
  `pms_agent_it_pos` with the vendor's `HotelParameter` and `GetHotelDate1`.
- `ops_manifest_drift`: set `PMS_OPS_MANIFEST` to SMILE-PMS's manifest; skipped without it.
