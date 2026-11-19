# dl-agent-membership M2 (write side): report

Brief: `docs/plans/2026-11-19-membership-writes-brief.md`.
Branch: `feat/dl-agent-membership-writes`, worktree `~/Projects/DevLibrary-mb2`.
Nothing was pushed, merged, released or deployed.

Status: **DONE_WITH_CONCERNS**. The core write path is ported and tested, including against a real SQL Server. Some write-side features are not ported yet (see Gaps). Two of those gaps block a Victory cutover today:

- earn reporting;
- SMILE_SPECIAL extra-bed rows.

## Commits

| SHA | What |
|---|---|
| `c2dd98a` | Job executor (`jobs/*`), DB-identity write gate, `POST jobs/ack`, sync pump, TCVN3, counters |
| `3cc1b91` | SMILE folio writer: applock, create/update/cancel, AdditionalName, duplicate guard, lookup, discount post, golden SQL fixtures |
| `deb14cc` | Link folio, member profile (`ProfileWriter` / `ProfileMemberKey`), travel agent reader |
| `7448602` | `read_only: false` accepted; module wires `SmileWriter` + `WriteGate` + `Dispatcher` into the sync worker; `SmileWriteOptions` keys |
| `6b600b9` | Fake-hub job-flow tests and the live SQL writes test |
| `1787334` | README, CHANGELOG, hub doc line |
| (this report) | `docs: membership writes report` |

## What was ported (from `MM.Membership.Agent`)

**Job pipeline**
- `JobDispatcher` → `jobs/dispatch.rs`. Actions:
  - Create, Modify, Cancel, LinkFolio, RedeemVoucher, GrantReward;
  - RedeemVoucher with the `vouchers` array (MM-MEMBERSHIP 58a4524, `HandleStackedRedeemAsync`, RT-0260):
    `jobs/redeem.rs`; one discount line per voucher in `sequence` order for the hub's `appliedAmount`,
    idempotent per code, the whole job to the desk when any voucher lacks a positive amount; golden
    `tests/fixtures/golden/sql/redeem_two_vouchers.json`, live in `tests/sql_live_writes.rs`;
  - Availability is a no-op ack;
  - EarnPoints → `report_earn`, which is not ported.
- `RoomDiff`, `JobPayload` parsing.
- Ack building, including `ErrorCode` / `ErrorClass` / `Attempts`.
- `PmsWriteGate.EvaluateAsync`: 60 s sync-config cache, gated actions, `Denied` ack.
- `PumpCoreAsync`: one failed ack never stops the batch; if every ack fails, the worker backs off.

**SMILE writer (`writer/*`, over the shared `TdsSmile` session)**

`FolioWriter.Reservation`
- runs under the `SMILE_FolioNumber` applock in a READ COMMITTED transaction, with `SET LOCK_TIMEOUT`;
- calls `GetFolioNumber` and `GetConfirmNum`;
- uses the replay lookup (`ExternalConfirmNumber` + `UserInfo1`), so a replayed room updates its folio instead of inserting a new one;
- writes Folio INSERT/UPDATE through the schema-probed column whitelist (`ColumnList`), plus FolioRate nights and the AdditionalName placeholders and reconcile;
- encodes names in TCVN3 and writes the first-night rate;
- writes PaymentCode / BreakfastCode / IncludeBF defaults;
- applies the note gating.

The rest of the writer:
- **`DuplicateGuard`:** a hand-entered twin is refused (`POSSIBLE_DUPLICATE`) and rolled back; `force` skips the check.
- **`Cancel`:** runs under the `SMILE_CancelNumber` applock with `GetCancelNum`, and is idempotent.
- **`PostDiscount`:** calls `PostOneTransaction` with the same 20 arguments, in the same order and with the same types. The `has_posted_ref` guard uses varchar parameters and cuts the reference to 30 characters.
- **`FolioWriter.Link`:**
  - row read with UPDLOCK/ROWLOCK, then the mismatch checks; each reason gives the same text as .NET (`LINK_TARGET_CHANGED`);
  - a folio that is already stamped is skipped.
- **`ProfileWriter` / `ProfileMemberKey`:**
  - looks up by member code, then by the legacy key (first 12 characters of the member id), and adopts a profile found under the legacy key;
  - fills blank contact fields (a SQL error there is logged and the existing number is kept, as in .NET);
  - a new profile is created under the `SMILE_ProfileNumber` applock with a re-check for races, then `GetProfileNumber`.
- **`TravelAgentReader.FindAsync`:** `ClientSts JOIN CLIENT`, with trailing blanks trimmed.

All SQL is parameterized. Identifiers come only from the column whitelist checked against `INFORMATION_SCHEMA`.

**Settings**

| Key | Default |
|---|---|
| `write_lock_timeout_ms` | 10000; a non-positive value falls back to the default |
| `default_payment_code` | "1"; present but blank means the column is not written |
| `default_breakfast_code` | "BFF"; same blank rule |
| `extra_bed_write_mode` | SMILE_SPECIAL; an unknown value becomes FOLD_LEGACY plus a warning, matching `ExtraBedModeProblem` |

`read_only` still defaults to `true`, and with `true` the M1 behaviour is unchanged. When it is `false`, the dispatcher is built only if the SMILE connection string parses. If `pull_jobs` is false, a warning says nothing will be written.

`crates/dl-agent-bin/src/main.rs` is not touched by these commits. It shows in a diff against `origin/main` only because `origin/main` moved after the branch point.

## Parity evidence

1. **Golden SQL fixtures** (`tests/fixtures/golden/sql/`):
   - Files: `create_booking.json`, `cancel_folio.json`, `post_discount.json`.
   - Each one records every statement in order: text, parameter names, DbType, size and value.
   - **How they were made:** the Rust recorder generated them (`GOLDEN_SQL_WRITE=1`). I then compared them by hand, statement by statement, with:
     - `Smile/FolioWriter.Reservation.cs`
     - `Smile/DuplicateGuard.cs`
     - `Smile/FolioWriter.Cancel.cs`
     - `FolioWriter.PostDiscount` in `Smile/FolioWriter.cs`
   - **Caveat:** they are not captured from a .NET run (there was no SQL trace of the .NET agent), so they lock Rust to a hand-verified reading of the .NET source, not to recorded .NET traffic.
2. **Recorder unit tests** (116 lib tests in total, including M1). They pin:
   - statement order;
   - BEGIN/COMMIT/ROLLBACK and `sp_getapplock` arguments;
   - the refusal paths;
   - the link mismatch reasons, character for character;
   - profile key rules, legacy adoption and the fill-blank SQL;
   - the travel agent parameter type (Int64).
3. **Live SQL Server 2022** (`tests/sql_live_writes.rs`):
   - **Setup:** a throw-away database built from `tests/fixtures/smile_schema.sql`, which has the same stored procedures as the .NET `SmileFixture`.
   - **What it runs:** Create → folio 7001 / ConfirmNum 8001 with UserInfo1/UserInfo2 tags, 2 FolioRate rows, 2 AdditionalName rows (one per adult), 1 Profile under the member code.
   - **Replayed Create:** same folio, still one Folio and one Profile.
   - **Modify:** keeps the folio.
   - **`PostOneTransaction`:** posts once; the ref guard then finds it.
   - **Travel agent read:** runs over the real join.
   - **Cancel:** sets CancelNum. A replayed Cancel takes no second cancel number.

## Differences from .NET (intentional or small)

- **Member code too long:** a member code over 12 characters is refused with `Coded{MEMBER_CODE_TOO_LONG}`; the job fails with a code. .NET throws `ArgumentException`, which ends up as an unclassified failure.
- **`@Ext` size:** the profile lookup's `@Ext` varchar parameter is sent without an explicit size of 12. SQL Server still compares against the varchar(12) column. Only the plan cache key differs.
- **Extra-bed special code:** the configured `extra_bed_special_code` is passed to the writer. The hub's sync-config `ExtraBedSpecialCode` override is not applied on the write side. It only matters for SMILE_SPECIAL rows, which are not ported.

## Gaps (not ported)

1. **Earn reporting.** `EarnPoints` jobs return `NotPorted`, are left un-acked, and the hub re-leases them. After 4 deliveries the hub marks them MANUAL. **This blocks cutover.**
2. **SMILE_SPECIAL extra-bed rows.** `Folio_Special` writing and its preflight are not ported. A booking that needs special rows in SMILE_SPECIAL mode (the default) is left un-acked. The FOLD_LEGACY fold works. **This blocks cutover** unless the hotel runs `extra_bed_write_mode: FOLD_LEGACY`.
3. **Not ported, no blocking effect found:**
   - the checkout scanner, the walk-in scanner and the pms-cancelled report;
   - the EarnPair drift check;
   - the extra-bed terms sync (`pms/extra-bed-terms`, `pms/special-services`).

## Victory cutover steps (when the gaps above are closed)

1. Stop and disable the .NET `MM.Membership.Agent` Windows service. Only one writer may run per hotel, because both would take the same jobs and applocks.
2. Confirm `config.dpapi` is in `%ProgramData%\MM.Membership.Agent` and that the module is enrolled (`membership.status`).
3. Set the remote config: `modules.membership = {enabled: true, read_only: false, pull_jobs: true}`.
   - Keep `extra_bed_write_mode` as the .NET `Smile:ExtraBedWriteMode` was.
   - Copy `default_payment_code`, `default_breakfast_code` and `member_discount_code` from the .NET `appsettings.json`.
4. Check the hub:
   - heartbeat shows the agent version `dl-agent/…`;
   - the job queue drains;
   - acks show `Success`;
   - no `PMS_IDENTITY_MISMATCH`.
5. Check SMILE: a test booking creates exactly one folio, and its replay does not add one.
6. **Rollback:** set `read_only: true`, then start the .NET service again.

## Tests and gates (exact commands)

| Command | Result |
|---|---|
| `HEAVY_LOCK=/tmp/heavy-dl-mb2 ~/.claude/scripts/heavy.sh ~/.cargo/bin/cargo fmt --all --check` | exit 0 |
| `HEAVY_LOCK=/tmp/heavy-dl-mb2 ~/.claude/scripts/heavy.sh ~/.cargo/bin/cargo test -p dl-agent-membership -p dl-agent-bin` | exit 0 (details below) |
| `HEAVY_LOCK=/tmp/heavy-dl-mb2 ~/.claude/scripts/heavy.sh ~/.cargo/bin/cargo clippy -p dl-agent-membership --all-targets -- -D warnings` | exit 0 |
| same with `--target x86_64-pc-windows-gnu` | exit 0 |
| `MEMBERSHIP_AGENT_TEST_SQL='Data Source=127.0.0.1,14339;User ID=sa;Password=…;Encrypt=disable' … cargo test -p dl-agent-membership --test sql_live --test sql_live_writes` | both passed against my SQL Server 2022 container |

Details of the `cargo test` run:
- 151 passed, 0 failed:
  - lib 116;
  - test files: enroll 5, fake_hub 9, golden 9, jobs_hub 5, module_lifecycle 5, sql_live 1, sql_live_writes 1;
  - `dl-agent-bin` 0.
- In that run, `sql_live` and `sql_live_writes` returned early (skipped) because `MEMBERSHIP_AGENT_TEST_SQL` was unset; their live results are in the last row of the table.

The live run used a container I created (`dl-mb2-sql`, `mcr.microsoft.com/mssql/server:2022-latest`, a random password kept in a scratch file, never printed). I removed the container and its volume afterwards and deleted the password file. No other Docker resource was touched.

`tests/jobs_hub.rs` uses the real `HubClient` against the fake hub. It covers:
- pull → execute → ack, with the folio map and the lease attempts;
- a refused write acked with `POSSIBLE_DUPLICATE`;
- a closed gate (another baseline DB): no write, ack `PMS_IDENTITY_MISMATCH` / `PMS_ENVIRONMENT`;
- a replayed Create (one folio, same map);
- a replayed Modify (no write).

## Concerns

- **Gaps 1 and 2 above.** Do not set `read_only: false` at Victory until earn is ported, or until the hub stops sending EarnPoints to this agent.
- **The golden fixtures are verified by hand against the .NET source, not captured from .NET traffic** (see Parity evidence).
- **`module_lifecycle::enrolled_with_smile_down_heartbeats_and_degrades_on_smile` failed once.** It was the first run after the module change, run alongside the lib tests. It passed on 3 later runs, including the full gate run. It is a timing test (5 s tick window), so this looks like load flakiness; I am noting it rather than claiming it is fixed.
- **The `dl-agent-bin` integration is unchanged by design** (`main.rs` not edited). The module picks up `read_only: false` from the remote config.
