# Membership module M1: report

Brief: `docs/plans/2026-11-19-membership-module-brief.md`. Branch `feat/dl-agent-membership`
(worktree `~/Projects/DevLibrary-mb`). Not pushed, merged or deployed.

The repo has no root `CLAUDE.md`, so `CONTRIBUTING.md` was followed (workspace lints, no
third-party crates, docs on every public item, a CHANGELOG entry, crate README).

Status: **DONE_WITH_CONCERNS**. The concerns are listed at the end and are about deployment, not
about the code.

## Commits

| SHA | What |
|---|---|
| `6c74004` | `feat(dl-agent-membership): read side of the Azolo Membership agent (M1)`. The crate, its tests and fixtures, the wiring, the docs and the brief. |
| (next) | `docs(dl-agent-membership): M1 report` (this file) |

## What was ported

The new crate `crates/dl-agent-membership` (about 9,000 lines of source including unit tests, plus 2,100 lines of integration tests, with no file over 629 lines) covers:

- **Module:** `membership_module() -> Factory`, an opt-in module.
  - It is Disabled unless `modules.membership.enabled`. `read_only` defaults to `true`, and `false` is refused (Degraded) until M2 exists.
  - Not enrolled: Degraded with `not enrolled: no config.dpapi in <dir>; run dl-agent membership-enroll`. It rechecks every 30 s, answers `membership.status`, and stops promptly on `ctx.stop()`.
  - It moves to Running after the first heartbeat.
  - It goes Degraded on any of these:
    - SMILE is down;
    - the hub is unreachable past one back-off;
    - the hub answers 401 with `AGENT_TOKEN_REJECTED`;
    - the DB identity changes (scans pause and the change is reported).
  - One thread runs the module, one runs sync, one runs scans. All of them stop on `ctx.stop()`.
- **Enroll:** `dl-agent membership-enroll --hub-url <https origin> [--force]`.
  - The code comes from `DL_AGENT_MEMBERSHIP_TOKEN` or stdin, with any BOM and CRLF removed. The SMILE connection string comes from `DL_AGENT_MEMBERSHIP_SMILE`; a forced re-enroll keeps the previous one. Neither value is ever in argv or printed.
  - It writes `config.dpapi` in the .NET installer's format: STJ JSON sealed with DPAPI LocalMachine on Windows.
  - A second run skips unless `--force`, which keeps the previous file beside the new one.
- **Hub client** (`/api/agent/*`): heartbeat, sync-config, jobs/pull with an older-hub fallback to `GET jobs`, availability, room-inventory, room-types and folio-observations.
  - The token gate opens only for a 401 that carries `X-MM-Agent-Auth-Error: AGENT_TOKEN_REJECTED`. It then holds every call for 15 minutes, followed by one probe.
- **Heartbeat + sync-config** (`SyncWorker`, read side):
  - The heartbeat carries `SmileOk`, `DbIdentity` and the capabilities.
  - Sync-config is fetched every 60 s and shared with the scanners.
  - Failures back off 5 s, 10 s, … up to 300 s.
- **Job pull plumbing:** off by default (`pull_jobs: false`). When on, jobs are counted (`jobs.seen` and `jobs.left`). They are never executed and never acked; no ack, result, complete or fail call exists in M1.
- **Scanners:**
  - Availability: the cheap probe; sold, blocked and group rooms per type and night; physical-room raise; extra-bed usage.
  - Physical rooms: housekeeping derivation and full push every N scans.
  - Room types.
  - Folio observations: cursor and watermark, earn pairs, revenue fingerprint, chunked poster. The cursor is saved only after a hub 2xx.
- **Metrics:**
  - `heartbeat.count/failures/last`, `hub.failures_in_a_row`, `smile.ok`, `scans.paused`, `jobs.seen/left`;
  - `{availability,rooms,folio}.runs/rows/errors/last/last_run`;
  - `folio.cursor`, `folio.lag_seconds`, `identity.pinned`.
- **Reports:** rate-limited per code, by default 15 minutes. They carry no guest PII, SQL text or token.
  - Codes: `membership_hub_failed`, `membership_auth_failed`, `membership_smile_failed`, `membership_db_identity_changed`, `membership_scan_failed`.
- **Wiring:**
  - root `Cargo.toml` workspace dependency;
  - `dl-agent-bin` dependency, plus in `main.rs` one `use` and the registration;
  - the crate is excluded from the `cargo wasm` alias;
  - a CHANGELOG `[Unreleased]` entry, a module row in `crates/dl-agent/README.md`, and the crate README.

## The M1/M2 seam

- **Jobs:** `HubApi::pull_jobs` returns parsed `Job`s.
  - `SyncWorker` only counts them in `Shared::stats` (`jobs_seen`, `jobs_left`).
  - M2 adds a job executor behind the same pull, plus the ack, result, complete and fail routes on `HubApi`.
- **Writes:** `read_only` is checked once, at module start.
  - M2 replaces the refusal with the writer set.
  - The write gate already exists for pushes (`identity::gate::PmsWriteGate`, `GatedPush`) and can guard writes the same way.
- **SMILE:** the reads sit behind traits in `smile/mod.rs` (`SmileHealth`, `RoomSource`, `AvailabilitySource`, `FolioSource`, `IdentityProbe`).
  - M2 adds a `FolioWriter`-style trait on `TdsSmile`.
  - The schema fixture (`tests/fixtures/smile_schema.sql`) already has the stored procedures (`GetFolioNumber`, `PostOneTransaction`, …) that the writers call.

## Application / Infrastructure pieces copied (.NET → Rust)

| .NET (MM-MEMBERSHIP `src/MM.Membership.Agent*`) | Rust |
|---|---|
| `AvailabilityScanner`, `GroupBlocks`, `PhysicalRooms` | `availability/{mod,count}.rs` |
| `RoomInventoryScanner`, `HousekeepingDerivation` | `rooms.rs` |
| `FolioObservationScanner`, `FolioObservationCursor`, `FolioObservationWatermark` | `folio/{scanner,cursor}.rs` |
| `FolioObservationPoster` | `folio/poster.rs` |
| `FolioObservationReader`, `EarnPairReader`, `EarnPair`, `EarnPairSource` | `folio/{read,earn}.rs`, `smile/folio_sql.rs` |
| `FolioWriter.ScanOccupancy/RoomBlocks/GroupBlocks/…`, `ExtraBedUsageReader`, room reads | `smile/occupancy_sql.rs` |
| `PmsDbIdentityPolicy`, the identity probe, `PmsWriteGate.Decide`, `GatedPush` | `identity/{mod,gate}.rs`, `smile/tds.rs` |
| `HubClient` (read routes), `HubTokenRejectionGate` | `hub/{mod,gate}.rs` |
| `SyncWorker` (heartbeat and sync-config, job pull without execution) | `sync.rs` |
| `AgentConfig`, `ConfigStore`, the DPAPI sealer, `EnrollCommand` (install part) | `config.rs`, `enroll/{mod,tool}.rs` |
| Request and response records, `System.Text.Json` default format | `wire/{mod,parse}.rs`, `stj.rs` |
| `appsettings.json` sections (`Worker`, `AvailabilityScan`, `FolioObservationScan`, `Smile`, `Earn`) | `settings.rs` (same defaults) |

## Differences from .NET

1. Sync-config is fetched by the sync thread every 60 s and shared, instead of separately by each scanner.
2. A failed heartbeat counts as a hub failure, so it drives the back-off and the `membership_hub_failed` report.
3. `pull_jobs` defaults to `false`. A pulled but unacked lease returns to the queue, and after four such deliveries the hub marks the job MANUAL. Pulling only to count jobs would therefore harm real jobs while the .NET agent is still in use.
4. The heartbeat sends `AgentVersion` as `dl-agent/0.2.1`, which the hub may show as an unknown agent version.
5. `PmsNow` uses +07:00 (the hotel offset). .NET uses the machine's local time.
6. When not enrolled, the module rechecks every 30 s, so a site enrolled later needs no service restart.
7. A diagnostics request from the hub is only logged; `SmileDiagnostics` is not ported.
8. Enroll is stricter.
   - It requires `https`, and the hub's `HubBaseUrl` must be on the origin that was called.
   - It refuses a missing code.
   - It never takes secrets from argv.
9. Only a 401 carrying `AGENT_TOKEN_REJECTED` closes the token gate. A plain 401 is a hub failure.
10. An invalid `EnrollmentId` in the enroll answer is treated as absent instead of failing.
11. `rooms_full_push_every: 0` now means a full push on every scan; before, it would have divided by zero.

## Gaps (not in M1)

- The writers and the job executor (M2), and `CheckoutScanner`.
- The extra-bed terms and special-service sync.
- TCVN3 decoding of legacy text.
- `SmileDiagnostics`, the walk-in backfill, `TravelAgentReader`, `SmileSchemaProbe` and the folio revenue scan.
- The `EarnPair` drift check and the inventory fallback.
- DPAPI works only on Windows. Elsewhere `config.dpapi` cannot be opened, and tests use a plain sealer.
  - The Windows build passes clippy (`x86_64-pc-windows-gnu`) but was **not run on Windows**. Neither the DPAPI round trip with a file written by the .NET installer nor service start under dl-winsvc was exercised.
- The live SQL run used `Encrypt=disable`, so TLS to SMILE was not exercised. dl-tls has no TLS backend on macOS. Production SMILE (2008 R2, TLS 1.0) goes through the normalizer's defaults: encrypt required, trust the server certificate, TLS 1.0 minimum.

## Tests

All commands were run from the worktree root. The heavy ones went through `~/.claude/scripts/heavy.sh` with `HEAVY_LOCK=/tmp/claude-501/heavy-dl-mb.lock`.

| Command | Result |
|---|---|
| `~/.cargo/bin/cargo fmt --all -- --check` | clean |
| `~/.cargo/bin/cargo test -p dl-agent-membership -p dl-agent-bin` | **70 passed**, 0 failed: 41 unit, 9 golden, 9 fake_hub, 5 enroll, 5 module_lifecycle, 1 sql_live (returns at once without the env var) |
| `~/.cargo/bin/cargo clippy -p dl-agent-membership -p dl-agent-bin --all-targets -- -D warnings` | clean |
| same with `--target x86_64-pc-windows-gnu` | clean |
| `MEMBERSHIP_AGENT_TEST_SQL=… cargo test -p dl-agent-membership --test sql_live` | **passed once** against a SQL Server 2022 container (see below) |

What the tests cover:

- **fake_hub** (fake hub on a local socket, with the real `HubClient`):
  - heartbeat and sync-config carry the token and the hotel;
  - an older hub (404 on pull) gets a job count with no ack, result, complete or fail;
  - a rejected token closes the gate and is reported once per probe;
  - a plain 401 is a hub failure;
  - the hub down past one back-off is reported without the token;
  - SMILE down still heartbeats with `SmileOk:false`;
  - every scanner posts;
  - the folio cursor moves only after a 2xx (on a 500 no cursor file is written and the next pass reposts the same body);
  - another database pauses scans and is reported.
- **module_lifecycle** (real supervisor):
  - opt-in;
  - not enrolled: Degraded, answers status, stops in under 2 s;
  - `read_only:false` refused;
  - enrolled with SMILE down;
  - 401 gate.
- **enroll:**
  - no secret printed or sent beyond the code;
  - skip and `--force`;
  - code on stdin;
  - another origin in the answer saves nothing;
  - a refused code or an `http://` hub saves nothing.
- **golden:** byte-for-byte STJ JSON for every wire body and `config.dpapi`; identity, revenue and housekeeping fingerprints.
- **sql_live:**
  - It creates a throw-away database from the MM-MEMBERSHIP `SmileFixture.SchemaScript`. The script was copied read-only to `tests/fixtures/smile_schema.sql`, split on `GO`, 29 batches including the `triggerFolioUpdate` trigger and the stored procedures.
  - It seeds synthetic rooms, room types, an out-of-order room, a group block, two folios with room and minibar revenue, guests and an extra-bed special.
  - It runs every `TdsSmile` read: ping, identity, rooms, room types, probe, occupancy, room and group blocks, extra beds, earn catalog, `RmChgTrn`, the column probe, changed folios, revenues (room revenue only, and any-sub), guests and extra-bed specials.
  - It then runs the sync worker and scan loop over real SMILE against the fake hub. All four POST routes get one call each, the folio post has 2 observations, and there are no reports.
  - Finally it drops the database.
- **SQL container run:**
  - I created and used `dl-mb-sql2022` from `mcr.microsoft.com/mssql/server:2022-latest`. The image was already on the machine, so nothing was pulled.
  - The SA password was random, kept in a 0600 scratch file and never printed.
  - The port was bound to 127.0.0.1 only.
  - The first try with `TrustServerCertificate=True` failed at TLS (no TLS backend on macOS, as dl-agent's HANDOFF says). The second try with `Encrypt=disable` passed in 0.6 s.
  - Afterwards I removed the container and its volume with `docker rm -f -v dl-mb-sql2022` and deleted the scratch password file and log. No other container was touched.

Changes made while running the gates:

- clippy flagged `manual_is_multiple_of` in `rooms.rs`. It now uses `is_multiple_of(max(1))`, which also removes a divide-by-zero panic for `rooms_full_push_every: 0`.
- The enroll console writer now logs a failed write instead of discarding it.

## Concerns

1. **One agent per hotel.** If both the .NET service and dl-agent run against the same `config.dpapi`, both heartbeat for the hotel. They would also race on the shared folio cursor file (`program_data_dir`). The cutover should stop and disable the .NET service before setting `modules.membership.enabled`.
2. **Re-enrolling replaces the .NET token.** `membership-enroll --force` writes a new token into the `config.dpapi` that the .NET agent also reads. The .NET service picks up the new token on its next restart, so a forced re-enroll during cutover affects both agents.
3. **Observations can be posted twice.** If the cursor cannot be saved after a 2xx, or a restart rescans the 15-minute overlap, the same observations are posted again. This is the same as .NET: the hub skips unchanged observations by fingerprint, and the poster skips what the hub already took during the process lifetime.
4. **`main.rs` diff.** rustfmt splits `.module(membership_module()).tool(membership_enroll_tool())` into two lines. The change is therefore one `use` (inside `main`) plus two adjacent lines after the einvoice registration. Merging with the other slices should be a trivial adjacent-line conflict.
5. **Hub version display.** The hub may show `AgentVersion` `dl-agent/0.2.1` as an unknown or older agent version. Its update banner logic was not checked.
6. **Not run on Windows.** The Windows build was not executed, so the DPAPI round trip with an installer-written file is unproven.
