# Brief M1: `membership` module in dl-agent — hub link + read side (RT-0164 task 557, step 6)

## Goal

The Azolo Membership site agent (`~/Projects/MM-MEMBERSHIP/src/MM.Membership.Agent`, .NET 10,
~19k lines, Windows service, live at two Victory hotels) moves onto the shared `dl-agent` as module
`membership`, following the pattern of `crates/dl-agent-einvoice` and `crates/dl-agent-pms` (read
their READMEs and `module/`). It is too large and too close to money for one slice:

- **M1 (this brief):** config/identity, hub client, heartbeat, sync-config, job pull/ack plumbing,
  and every **read-only** scanner that reports to the hub.
- **M2 (next):** the write side (`Jobs/JobDispatcher`, `Smile/FolioWriter*`, `ProfileWriter`,
  `PmsWriteGate`, walk-in earn writes). In M1 a pulled job is **not executed**: the module must not
  ack it either, so the hub keeps it for the .NET agent / M2. M1 is therefore only ever run where
  the .NET agent still owns writes, or on the demo tenant.

Do not change MM-MEMBERSHIP, release, or deploy. Read `~/Projects/MM-MEMBERSHIP/CLAUDE.md` first
(note its Gotchas, the `e2e-checkout-earn` gate and the SMILE test fixtures).

## Keep in place

- `%ProgramData%\MM.Membership.Agent\config.dpapi`: DPAPI **LocalMachine + entropy**
  `MM.Membership.Agent.v1` (`Configuration/AgentSecureConfig.cs`, `AgentSecretStore.cs`,
  `PortableAgentConfigStore.cs` for the non-Windows form). Read it in place so a site switches with
  no re-provisioning. Reuse `dl-winsvc` `dpapi.rs` (add entropy support if missing) and the
  off-Windows test seam the other crates use.
- Cursor and state files under `%ProgramData%\MM.Membership.Agent\` (folio-observation cursor,
  walk-in backfill state, PMS identity fingerprint): read and keep using them where they are, so
  hub-confirmed cursors are not reset. Module config `modules.membership`: `enabled`,
  `program_data_dir` (optional override).

## Port (M1 scope)

- `Hub/HubClient` (Bearer per hotel, all `api/agent/*` routes the read side uses: heartbeat,
  sync-config, jobs pull/ack plumbing, bookings/pending-checkout, pms-cancelled, earn report
  routes **only as data posting of what scanners observe**, folio-observations, availability,
  room-inventory, room-types, pms/extra-bed-terms, folio-revenue, checkout-scan-summary,
  pms/special-services) with the older-hub fallbacks and the 401 gate.
- `Worker/`: `SyncWorker` loop shape, `CheckoutScanner` (observation/report part only — anything
  that writes SMILE or earns points is M2; split cleanly and say where the seam is),
  `AvailabilityScanner` (+ ExtraBedUsage, GroupBlocks, PhysicalRooms), `RoomInventoryScanner`,
  `FolioObservationScanner` + cursor store + poster, `PmsIdentityTracker`, `SmileDiagnostics*`,
  `HousekeepingDerivation`, `GatedPush`, `WalkInBackfill*` read side.
- `Smile/` readers: `FolioObservationReader*`, `RoomTypeCatalogReader`, `TravelAgentReader`,
  `ExtraBedUsageReader`, `SmileDbIdentityProbe`, `SmileSchemaProbe`, `SmileDiagnosticsProbe`,
  `FolioRevenueScan`, occupancy fingerprint, `Tcvn3` (use `dl-vn`, it already matches PDAOrder's
  rules — verify against the .NET cases). `Sync/` readers (extra-bed terms, special services, earn
  pair reader).
- Whatever pieces of `MM.Membership.Application` / `Infrastructure` these depend on: port only the
  minimum, as plain functions in the crate, and list them in the report (drift risk).
- SQL on `dl-tds` (SQL 2005–2022, old TLS, SSPI; see `dl-agent-pms` / `ctx.sql`). Parameterized only.
- **Byte compatibility is the bar:** every JSON body sent to the hub must match .NET for the same
  SMILE data. Produce golden fixtures with a throwaway exporter in your scratch dir (not committed
  to MM-MEMBERSHIP) that runs the .NET readers/serializers on fixed inputs, and test against them.
- **Drop what dl-agent owns:** `Update/`, `Observability/` (Serilog), `LivenessFile`, service host,
  `Cli/` except what an enroll needs.

## Module, CLI, reports

- New crate `crates/dl-agent-membership`, `membership_module() -> Factory`, registered in
  `crates/dl-agent-bin/src/main.rs` (other slices edit that file in other branches; keep your change
  to one line + one `use`). `Disabled` unless `modules.membership.enabled`; also a
  `modules.membership.read_only` flag that defaults to **true** in M1 (M2 will add the writer).
- Sub-command `dl-agent membership-enroll --hub-url <https origin>` with hotel token from env
  `DL_AGENT_MEMBERSHIP_TOKEN` or stdin and the SMILE connection string from
  `DL_AGENT_MEMBERSHIP_SMILE` (never argv), writing `config.dpapi` exactly as the .NET installer
  does; skip when it exists unless `--force`.
- `Module::run`: not configured → `set_degraded`, wait on `ctx.stop()`. `set_running` after the
  first heartbeat; degraded with reason on hub unreachable past one back-off, 401, SMILE down, or
  a DB identity fingerprint change (wrong database → stop scanning, report).
- Metrics: per-scanner last run/rows/errors, cursor positions, jobs seen-but-left (M1), heartbeat.
  Reports with stable codes, rate-limited, no guest PII, no SQL text, no token:
  `membership_hub_failed`, `membership_auth_failed`, `membership_smile_failed`,
  `membership_db_identity_changed`, `membership_scan_failed`.

## Tests (mirror source structure, hand-written fakes, no mocking crates)

- Port the `tests/MM.Membership.Agent.Tests` cases that cover what you ported; golden JSON from .NET.
- Fake hub on a local socket: heartbeat, sync-config, scanners posting, cursor confirmed only after
  hub 2xx, 401 gate, older-hub fallback; a pulled job is neither executed nor acked in read-only.
- SQL integration, env-gated (`MEMBERSHIP_AGENT_TEST_SQL`), once against a local SQL Server 2022
  container seeded the way `tests/MM.Membership.E2E.Tests/E2EFixture.cs` seeds SMILE; remove the
  container afterwards; `DOCKER_HOST=unix://$HOME/.orbstack/run/docker.sock`; never touch
  containers you did not create.

## Checks before commit

- `~/.cargo/bin/cargo fmt --all`
- `~/.cargo/bin/cargo test -p dl-agent-membership -p dl-agent-bin`
- `~/.cargo/bin/cargo clippy -p dl-agent-membership -p dl-agent-bin --all-targets -- -D warnings`,
  and the same with `--target x86_64-pc-windows-gnu`.
- Every file ≤ 800 lines (200–400 typical). No swallowed errors. Heavy builds via
  `~/.claude/scripts/heavy.sh`.

## Rules

- Work only in worktree `~/Projects/DevLibrary-mb`, branch `feat/dl-agent-membership`.
  Conventional Commits in English, trailer:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01DAn3zia3PHZ2jf2uZQSp3k`.
  **Do not push, merge or deploy.** Do not modify MM-MEMBERSHIP. Never print secrets, connection
  strings or guest data.
- Report to `docs/plans/2026-11-19-membership-module-report.md`: what was ported, the M1/M2 seam,
  Application/Infrastructure pieces copied, differences from .NET, gaps, test counts.
