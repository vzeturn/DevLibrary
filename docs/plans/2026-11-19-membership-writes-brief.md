# Brief M2: `membership` module write side (RT-0164 task 557, step 6)

## Goal

M1 (merged, `crates/dl-agent-membership`, report `docs/plans/2026-11-19-membership-module-report.md`)
ported the hub link and every read-only scanner of the Azolo Membership site agent
(`~/Projects/azolo-victory-membership/src/MM.Membership.Agent`). M2 ports the **write side** so dl-agent can
fully replace the .NET service: job executor + every SMILE writer. This touches folios, reservations
and member earn at two live Victory hotels: **byte/semantic parity with .NET is the bar**, and
nothing may write to SMILE unless the existing write gate allows it.

Read first: `~/Projects/azolo-victory-membership/CLAUDE.md` (Gotchas, `e2e-checkout-earn` gate, SMILE test
fixtures), the M1 brief + report (section "The M1/M2 seam"), and `crates/dl-agent-membership`.

## Port (from `src/MM.Membership.Agent`)

- `Jobs/`: `JobDispatcher`, `JobPayload`, `JobResult`, `GuestDataResolver`, `RoomDiffCalculator`,
  `PmsWriteGate` (M1 has `identity::gate::PmsWriteGate` for pushes: extend it, don't duplicate),
  `ProfileUnavailableException`. Hub routes ack / result / complete / fail on `HubApi`, with the same
  lease semantics as .NET (a pulled job is acked or failed exactly once; retries and MANUAL marking
  stay hub-side).
- `Smile/`: every `FolioWriter.*` partial (Atomic, AdditionalName, AvailabilityProbe, Cancel,
  CheckoutTime, DuplicateGuard, ExtraBed, ExtraBedSpecial, GroupBlocks, GuestCounts, Link, Lookup,
  Note, Reservation, Revenue, RoomBlocks, RoomInventory, StayClassification, UserInfo2),
  `ProfileWriter`, `AppLock`, `SmileClerk`, `FolioRefNumbers`, `ExtraBedPreflight`,
  `WalkInCheckout`, `CheckedOutFolio`, `EarnPairFilter`, `Tcvn3` (use `dl-vn`, don't re-port).
  Also the M1 gaps that writes depend on: `CheckoutScanner`, walk-in backfill, `FolioRevenueScan`,
  `TravelAgentReader`, `SmileSchemaProbe`, `EarnPair` drift check, extra-bed terms / special-service
  sync. List anything you leave out in the report with the reason.
- Transactions: every multi-statement write runs in one SQL transaction exactly where .NET uses one
  (`FolioWriter.Atomic`, `AppLock`); parameterized SQL only; same stored procedures
  (`GetFolioNumber`, `PostOneTransaction`, …) with the same argument order/types.
- `read_only: false` is now accepted; `read_only` stays the default `true` until a site is cut
  over. `pull_jobs` executes jobs only when `read_only` is false; with `read_only` true it keeps M1
  behaviour (count only, no ack).

## Tests (mirror source structure, hand-written fakes, no mocking crates)

- Port the .NET unit tests covering the ported code (`~/Projects/azolo-victory-membership/tests`), at least
  the job dispatcher, write gate, duplicate guard, room diff, walk-in checkout, earn pair.
- **Golden SQL parity:** for each writer, a test that records the exact statements + parameters the
  Rust writer issues against a fake TDS session and compares with fixtures produced from the .NET
  writer (throwaway exporter in your scratch dir, never committed to MM-MEMBERSHIP; commit only the
  JSON fixtures), the way the einvoice port used golden cases.
- Live SQL, env-gated (`MEMBERSHIP_AGENT_TEST_SQL`): run the main write jobs (reservation create,
  cancel, check-out earn, room block) against a local SQL Server 2022 container loaded with
  `tests/fixtures/smile_schema.sql`; assert rows; remove the container afterwards
  (`DOCKER_HOST=unix://$HOME/.orbstack/run/docker.sock`; never touch containers you did not create).
- Fake hub on a local socket: pull → execute → result/complete; failure → fail with code; gate
  closed → no SQL issued, job failed with the .NET refusal code; replayed job id → no double write.

## Checks before commit

- `~/.cargo/bin/cargo fmt --all`
- `~/.cargo/bin/cargo test -p dl-agent-membership -p dl-agent-bin`
- `~/.cargo/bin/cargo clippy -p dl-agent-membership --all-targets -- -D warnings`, and the same with
  `--target x86_64-pc-windows-gnu`. Heavy builds via `~/.claude/scripts/heavy.sh`
  (`HEAVY_LOCK=/tmp/heavy-dl-mb2`).
- Every file ≤ 800 lines (200–400 typical). No swallowed errors. CHANGELOG entry.

## Rules

- Work only in worktree `~/Projects/DevLibrary-mb2`, branch `feat/dl-agent-membership-writes`.
  Do not edit `crates/dl-agent-bin/src/main.rs` (registration already exists). Conventional Commits
  in English, trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01DAn3zia3PHZ2jf2uZQSp3k`.
- **Do not push, merge, release or deploy.** Do not modify MM-MEMBERSHIP. Never print secrets,
  connection strings or guest data. Never connect to a real hotel's SQL Server or the prod hub.
- Commit in reviewable steps (executor + gate, then writer groups). Report to
  `docs/plans/2026-11-19-membership-writes-report.md`: what was ported, parity evidence, differences
  from .NET and why, gaps, cutover steps for a Victory hotel (stop .NET service first), test counts.
