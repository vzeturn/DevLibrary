# dl-agent-membership

The Azolo Membership module of `dl-agent`: the read side of the .NET `MM.Membership.Agent`
(MM-MEMBERSHIP `src/MM.Membership.Agent`). It reads SMILE (SQL Server 2008 R2 and later)
read-only and reports to the Membership hub:

- **heartbeat** (`POST /api/agent/heartbeat`) with the SMILE identity (`DbIdentity`) and
  whether SMILE answers;
- **sync-config** (`GET /api/agent/sync-config`): which syncs the hub wants, the baseline
  identity, the extra-bed special code;
- **availability** (`POST /api/agent/availability`): rooms sold / total per room type and
  night, extra-bed usage, the cheap probe that skips a scan when nothing changed;
- **room inventory and room types** (`POST /api/agent/room-inventory`, `/room-types`);
- **folio observations** (`POST /api/agent/folio-observations`) from a cursor that moves
  only after the hub answered 2xx;
- **job pull plumbing** (`POST /api/agent/jobs/pull`, or `GET /api/agent/jobs` on an older
  hub): with `read_only: true` (the default) jobs are counted, never executed, never acked.

**Writes (M2).** With `read_only: false` and `pull_jobs: true` the module runs the hub's
jobs against SMILE and acks them like the .NET agent: create / modify / cancel folios
(applock, `GetFolioNumber` / `GetConfirmNum` / `GetCancelNum`, replay lookup, hand-entered
duplicate guard), member profile (`GetProfileNumber`), link folio, voucher and reward
discounts (`PostOneTransaction`), travel agent read, behind the write gate (DB identity),
and `EarnPoints` jobs (revenue read from SMILE, `POST /api/agent/earn`).
Extra beds follow `extra_bed_write_mode`: `SMILE_SPECIAL` runs the .NET preflight (terms
read from `GlobalParameter` / `SpecialService` / `TransactionCode`, the hub's verified
fingerprint, the special the hub picked in sync-config or in the job) and writes one
agent-marked `Folio_Special` row per price band inside the folio transaction; any failed
check folds the bed into the room rate with the .NET reason. With writes on, the module
also reports the extra-bed terms and the `SpecialService` catalogue to the hub every
300 s (`pms/extra-bed-terms`, `pms/special-services`). Only one writer may run per hotel:
stop the .NET `MM.Membership.Agent` service before turning writes on.

**Earn (checkout scan).** Every `checkout_interval_seconds` the scan thread asks the hub for
the bookings still waiting to earn (`GET /api/agent/bookings/pending-checkout`), reports earn
for those whose live folios are all checked out (`POST /api/agent/earn`, plus the
`folio-revenue` audit), reports walk-in stays by the first guest's phone (`POST
/api/agent/earn/walk-in`, remembered in memory so a group is asked once per process), reports
bookings cancelled in SMILE (`bookings/pms-cancelled`; an older hub answering 401/404/405
turns that off) and the daily summary (`checkout-scan-summary`). There is no cursor: the hub
names what is pending. It only reads SMILE. A wrong `guest_phone_column` turns the walk-in
half off (heartbeat `WalkInEarnOk: false`) without stopping booking earn.

## Turning it on

The module is opt-in: it stays `disabled` until the remote config has

```json
{"modules": {"membership": {"enabled": true}}}
```

Optional keys mirror the .NET `appsettings.json` with the same defaults:

| Key | Default | Meaning |
|---|---|---|
| `program_data_dir` | `%ProgramData%\MM.Membership.Agent` (the module data dir off Windows) | where `config.dpapi` and the folio cursor live |
| `read_only` | `true` | `false` runs and acks hub jobs against SMILE (needs `pull_jobs: true`) |
| `pull_jobs` | `false` | pull jobs to count them; off by default because an un-acked lease goes back to the queue and four such deliveries turn a job MANUAL |
| `max_jobs_per_pull` | `20` | |
| `heartbeat_seconds` | `30` | |
| `report_interval_minutes` | `15` | the minimum gap between two problem reports with the same code |
| `folio_scan_enabled`, `folio_interval_seconds`, `folio_max_per_scan`, `folio_overlap_minutes`, `folio_cold_start_lookback_days` | `true`, `60`, `500`, `15`, `7` | `FolioObservationScan` |
| `availability_probe_seconds`, `availability_heartbeat_minutes`, `availability_horizon_days`, `availability_full_push_every`, `availability_max_folios` | `5`, `5`, `365`, `24`, `20000` | `AvailabilityScan` |
| `rooms_interval_seconds`, `rooms_full_push_every` | `300`, `12` | room inventory |
| `extra_bed_special_code`, `guest_phone_column`, `earn_charge_codes`, `member_discount_code` | `EB`, `MobileNum`, SMILE's catalog, none | `Smile` / `Earn` |
| `write_lock_timeout_ms`, `default_payment_code`, `default_breakfast_code`, `extra_bed_write_mode` | `10000`, `1`, `BFF`, `SMILE_SPECIAL` | `SmileWriteOptions`; an empty code is not written; an unknown mode falls back to `FOLD_LEGACY` with a warning |
| `checkout_scan_enabled` | `true` when `read_only` is `false`, else `false` | the checkout scan (earn); the .NET agent always runs it, so a read-only module beside the .NET service leaves it off |
| `checkout_interval_seconds`, `checkout_max_per_scan`, `checkout_grace_days` | `60`, `100`, `1` | `CheckoutScan` |
| `walk_in_earn_enabled`, `walk_in_lookback_days`, `walk_in_max_per_scan` | `true`, `3`, `100` | `CheckoutScan` walk-in earn (phone column: `guest_phone_column`) |
| `smile_hotel_id` | none | `Smile:SmileHotelId`: the `GlobalParameter.HotelID` whose extra-bed terms are read; needed only when SMILE has several |
| `capabilities` | the .NET `FullCapabilities` | sent in the heartbeat |

No secret is in the remote config: the hotel token and the SMILE connection string stay in
`config.dpapi`.

## Enrolling a site

```
set DL_AGENT_MEMBERSHIP_TOKEN=<enrollment code>
set DL_AGENT_MEMBERSHIP_SMILE=<SMILE connection string>
dl-agent membership-enroll --hub-url https://hub.example
```

The code may also come on stdin. Neither value is ever an argument, and neither is printed.
The tool calls `POST /api/agent/enroll` (no token), checks that the answer's `HubBaseUrl` is
on the origin that was called, and writes `config.dpapi` the way the .NET installer does
(DPAPI LocalMachine on Windows). An existing file is kept unless `--force`; with `--force`
the previous file is kept beside it. Without `DL_AGENT_MEMBERSHIP_SMILE` a forced re-enroll
keeps the SMILE connection string of the previous config.

A site already enrolled by the .NET installer needs nothing: the module reads the same
`config.dpapi`.

## Walk-in backfill

```
dl-agent membership-backfill-walkin --from 2026-01-01 [--to 2026-09-19] [--page-size 200]
    [--phone-column MobileNum] [--extra-bed-code EB] [--member-discount-code MD]
```

The .NET `--backfill-from` mode: reports every checked-out walk-in stay of the range (at most
400 days) to the hub and exits non-zero unless the whole range went through without a failed
group. It reads `config.dpapi`, only reads SMILE, and saves its cursor in
`backfill-walkin.json` next to `config.dpapi` after each clean page, so re-running the same
command resumes. Re-running never double-earns (the hub dedups by `CONF:{ConfirmNum}`).

## Behaviour

- Not enrolled: `degraded` ("not enrolled: …"), rechecked every 30 s, no problem report.
- `running` after the first heartbeat; `degraded` while SMILE is down, the hub is unreachable
  past one back-off (5 s, 10 s, … 300 s), the hub rejected the token, or SMILE is another
  database than the one pinned.
- A 401 carrying `X-MM-Agent-Auth-Error: AGENT_TOKEN_REJECTED` closes a token gate: no hub
  call for 15 minutes, then one probe. A plain 401 is a hub failure.
- Another database (identity fingerprint change): scans stop and
  `membership_db_identity_changed` is reported; heartbeats go on so the hub sees it.
- Commands: `membership.status` (whether the site is enrolled, the status detail and the
  metrics below; no token, no connection string).
- Metrics: `heartbeat.count/failures/last`, `hub.failures_in_a_row`, `smile.ok`,
  `scans.paused`, `jobs.seen/left`, per scanner `{availability,rooms,folio,checkout}.runs/rows/errors/
  last/last_run`, `folio.cursor`, `folio.lag_seconds`, `identity.pinned`.
- Problem reports (rate-limited per code; no guest data, SQL text or token):
  `membership_hub_failed`, `membership_auth_failed`, `membership_smile_failed`,
  `membership_db_identity_changed`, `membership_scan_failed`.

## Not ported yet

The TCVN3 decoding of
legacy text, `SmileDiagnostics`, the SMILE
schema probe, the folio revenue scan, the earn-pair drift check and the inventory fallback.
Off Windows `config.dpapi` cannot be opened (no DPAPI); tests use a plain sealer.

## Tests

```
cargo test -p dl-agent-membership
```

Unit tests sit beside the code; `tests/` holds golden JSON for every wire body, a fake hub on
a local socket (`fake_hub.rs`, `enroll.rs`, `module_lifecycle.rs`) and an opt-in live run
against SQL Server:

```
MEMBERSHIP_AGENT_TEST_SQL='Data Source=127.0.0.1,14333;User ID=sa;Password=…;Encrypt=disable' \
  cargo test -p dl-agent-membership --test sql_live --test sql_live_writes --test sql_live_earn --test sql_live_extra_bed
```

`sql_live_earn` seeds folios and `FolioTransaction` rows and runs the `EarnPoints` job through
the real `SmileWriter`, the checkout scan and the walk-in backfill against the fake hub.

It creates a throw-away database from the MM-MEMBERSHIP SMILE fixture schema
(`tests/fixtures/smile_schema.sql`), seeds it, runs every SMILE read and the scan workers
against the fake hub, and drops the database. macOS has no TLS backend, so use
`Encrypt=disable` there.
