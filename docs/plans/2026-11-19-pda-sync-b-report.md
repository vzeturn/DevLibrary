# pda-sync phase B + speaker (RT-0257): report

Request: RT-0257 (part of RT-0164). Branch `cloud/rt-0257`, built in three parallel streams (`-verify`, `-orders`,
`-speaker`) merged into it. Plan and audit: `2026-11-19-pda-sync-plan.md` (coverage after B1+B5+B9: 25/31 = 80.6%,
strict 22/31 = 71.0%). Nothing was deployed; PDAOrder was not changed. B8 (`capture.db` carry-over) was not done
(owner decision).

## What was done

| Part | Where | Source |
|---|---|---|
| B1 `sync.verify` per bucket + repair by key (ADR 0126/0128) | `src/commands/verify*.rs`, `src/capture/{shadow_manifest,key_repair}.rs`, `src/outbox/key_repairs.rs` | C# `Commands/VerifyCommandHandler.cs`, `CloudManifestSource.cs`, `Outbox/SqliteOutbox.KeyRepairs.cs`, C# tests ported (`verify_handler`, `verify_contract`, `verify_repair`) |
| B9 order/kds/reservation/sync commands live, exactly once | `testdata/sql/smile_pos_orders.sql`, `tests/live_orders{,_combo,_tables}.rs` | C# `OrderVerbsSchema.cs`, `ComboStoreSchema.cs`, `SqlOrderCommandUnit.cs`, `SqlTableCommandUnit.cs` (no executor bug found) |
| B5 watchdog link + reports | `src/reporting/{cloud_link,module_reports}.rs`, `src/module/mod.rs` | `ctx.set_cloud_link` from the last accepted heartbeat; Error site events → `ctx.report` (once per code per 15 min) |
| Speaker module | new crate `crates/dl-agent-speaker`, module `speaker` (opt-in) | Go `apps/speaker-agent`, its Rust port |

Speaker details:
- SignalR over `dl-signalr` (negotiate + WS), `JoinRvc` per configured RVC, backoff, reconnect.
- 11 WAV tones embedded; SAPI vi/en through COM on an STA thread (Windows only; elsewhere the module logs).
- Sound mode per RVC from `GET /api/v1/speaker/sound-mode/{rvc}`, cached and refreshed.
- **Identity:** the speaker no longer uses a raw 365-day JWT. It reuses the pda-sync enrolment: the Ed25519 seed plus the agent token, exchanged for a 15-minute access token. It does not relay prints: pda-sync is the relay (ADR 0026 / 0098).

## Acceptance criteria → evidence

| Criterion | Evidence |
|---|---|
| `cargo test -p dl-agent-pda-sync` (+ speaker) green | `cargo test -p dl-agent-pda-sync -p dl-agent-speaker -p dl-agent-bin`: 786 passed, 0 failed (merged branch) |
| clippy -D warnings host + windows-gnu, fmt | `cargo fmt --all --check` ok; `cargo clippy --workspace --all-targets -- -D warnings` ok; `cargo clippy -p dl-agent-pda-sync -p dl-agent-speaker -p dl-agent-bin --all-targets --target x86_64-pc-windows-gnu -- -D warnings` ok |
| sync.verify heals rows deleted/closed while no agent ran, in one verify cycle | `verify_repair` (fake cloud) and `verify_live` (SQL Server 2022 and 2017): fresh `capture.db` with no shadow, rows deleted in SMILE_POS beforehand, then one verify. The rows are tombstoned and the keys repaired. See the limits below. |
| order.* replayed with the same request key does not write twice | `live_orders`, `live_orders_combo`, `live_orders_tables`: each command is delivered twice and gives one write, one `Pda_SyncCommandLog` row and the same answer. Commands are refused after a POS WIN change, and a lost grant rolls back both the write and its journal row. The unit/fake redelivery tests are in `commands_executor`. |
| Speaker: per-RVC playback unit tests + SignalR to a fake hub | `dl-agent-speaker` `decision.rs` tests (event → tone/voice per RVC mode) and `tests/hub_fake.rs` (negotiate + WS, JoinRvc sent for each RVC, an order event plays through a fake player) |

Live run on the merged branch: SQL Server 2022 container, `TZ=Asia/Ho_Chi_Minh`, `PDA_SYNC_TEST_SQL=…`.
`live_smile_pos`, `bootstrap_sql_live`, `capture_runner_sql`, `commands_sql`, `staff_sql`, `verify_live`,
`live_orders`, `live_orders_combo` and `live_orders_tables` gave 19/19 passed. The streams also ran them on SQL Server 2017, all passed. The tests ran
directly on Linux; `scripts/live-sql.sh` (xbuild image, for macOS) now lists the new tests but was not run itself.

## Known limits (owner decision needed)

1. **A check closed while no agent ran keeps its open-check rows in the cloud.** Verify does not compare open-check
   tables (ADR 0126 §3, same in C#). The closed ledger rows do arrive from the fresh snapshot. Today's mitigation:
   cut over with every check closed (plan §2 step 1). A real fix changes the open-stream snapshot semantics on the agent
   and the PDA cloud.
2. **Tables that allow duplicate rows (`CashierRVC`, `duplicate_rows_tolerated`) are never tombstoned.** Every verify
   queues their keys again. C# behaves the same.
3. **The speaker cannot play live until the PDA cloud changes** (PDAOrder not touched):
   - `OrderHub.JoinRvc` refuses `IsSyncAgent()` (`OrderHub.cs:146-148`). It must let a SyncAgent join the RVC groups of
     its own tenant.
   - `GET /api/v1/speaker/sound-mode/{rvc}` must accept the SyncAgent role (read-only, scoped to its tenant).
   - Until then the module is degraded with one `SPEAKER_JOIN_REFUSED` report, or uses the default mode per RVC.

## Only verifiable on Windows / real SMILE_POS / hardware (B6, B7)

- A Windows service run: DPAPI secret store, SChannel, SSPI admin. Only the cross-target clippy covers these here.
- SAPI speech and WAV playback on a real speaker PC.
- The watchdog's scheduled-task probe on Windows.
- Real SMILE_POS volumes for verify: time budget on large shadows.
- The real PDA cloud manifest endpoints (`/sync/v1/manifest`, `/manifest/keys`). Only a fake cloud and golden parsing covered them.
- SQL Server 2008 R2 / 2012.
- The print relay on a real printer (B7).

---

# Stream reports (verbatim)

## RT-0257 slice B1 — `sync.verify` per bucket + repair by key

Status: DONE_WITH_CONCERNS. Branch `cloud/rt-0257-verify`, worktree `/home/user/wt/verify`, commit `4bd0004` (one commit, not pushed).

## What was ported (C# -> Rust)

| C# source (SML-PDAOrder) | Here |
|---|---|
| `apps/sql-sync-agent/Commands/VerifyCommandHandler.cs` (whole file: Resolve, Rotate, Compare, Narrow, Differing, Repair, SkipReasons, Budget 15 s, TableLimit 25 s, RepairCooldown 7 d, 64 buckets / 500 keys, cursor `sync_verify.next_table`) | `src/commands/verify.rs` (`VerifyCommandHandler`, `VerifyParts`, `skip_reasons`, `differing`) |
| `apps/sql-sync-agent/Commands/CloudManifestSource.cs` (`CloudManifest`, `CloudManifestKeys`, `ICloudManifestSource`, `HttpCloudManifestSource` with one 401/403 retry, `Parse`, `ParseKeys`, `Uri.EscapeDataString`) | `src/commands/verify_manifest.rs` |
| `apps/sql-sync-agent/Outbox/SqliteOutbox.KeyRepairs.cs` (schema, Queue/Pending/Complete/KeyRepairsPending, `OutboxKeyRepairList`) + `StreamUnsettled` from `SqliteOutbox.Resnapshots.cs:180` | `src/outbox/key_repairs.rs`; schema added to `SqliteOutbox::open` (`outbox/batches.rs`) |
| `src/MM.PdaOrder.Sync.Capture/State/ShadowManifest.cs` (Build in passes, BucketRows, Mismatched, digest in UTF-8 byte order) | `src/capture/shadow_manifest.rs` |
| `src/MM.PdaOrder.Sync.Capture/Strategies/KeyRepairCapture.cs` (+ `KeyRepairTarget`, `IKeyRepairList`, cursor `repair:{table}` = `maxId|rejected|after`) | `src/capture/key_repair.rs` |
| `ICaptureState.CountShadowAsync/ScanShadowAsync` | `CaptureState::count_shadow/scan_shadow` (trait defaults; streaming override in `sqlite_state.rs`) |
| `CaptureEngine.RunKeyRepairsAsync`, `CaptureScheduler` job `key-repair` (master interval) | `capture/engine.rs` (`with_key_repairs`, `run_key_repairs`), `capture/scheduler.rs` |
| `IBatchStore.KeyRepairs`, `OutboxBatchStore.KeyRepairs`, `SqlCaptureRunner` passing `queue.KeyRepairs` | `capture/sinks_bridge.rs` (`BatchStore::key_repairs`, default `None`), `capture/runner.rs` (`.with_key_repairs(queue.key_repairs())`) |
| `SiteCommandExecutor.cs:77` route of `sync.verify` | `commands/executor.rs` (`with_verify`, dispatch `SYNC_VERIFY`) |
| `AgentHost.cs:122-129` registration (own SqliteCaptureState per run, HttpCloudManifestSource, runtime config) | `commands/service.rs` |
| `Wire/SiteCommands.cs` (`SyncVerify` type, spec Durable/3, `SyncVerifyPayload`, `SyncVerifyRepair`, `SyncVerifyTableResult`, `SyncVerifySkippedTable`, `SyncVerifyCommandResult`) | `contracts/wire/sync_verify.rs`, `site_commands.rs` (type + spec), `wire/mod.rs` |
| `Hashing/RowKey.cs` `TryParse` | `contracts/hashing.rs` `row_key::try_parse` |

The Rust port (`apps/sql-sync-agent/rust`) had none of this, so everything above is ported from C#.

## Tests (ported C# suites + acceptance)

- `tests/verify_handler.rs` (17): all of `VerifyCommandHandlerTests.cs` (match/skip, bucket report + resnapshot, narrowing to keys, truncated fallback, >64 buckets, Differing, recovery cooldown + deferred, 7-day repair cooldown, request spelling/unknown/not_mirrored/open_check, allowlist not_captured, stream busy before/during, resnapshot pending, duplicate keys, manifest refusal/outage logging, time budget + rotation cursor, invalid payloads, executor routing).
- `tests/verify_contract.rs` (15): `ShadowManifestTests.cs` (golden digest `b849393b…` on memory and SQLite state, multi-pass equality, duplicates, empty, Mismatched), `KeyRepairQueueTests.cs`, contract tests from `SiteCommandContractTests.cs`, `Parse`/`ParseKeys`, paths/escaping, `RowKey.TryParse`. Wire JSON of payload and result compared byte for byte with strings printed by .NET 10.0.404 over a copy of the real `MM.PdaOrder.Sync.Contracts` project (copied to scratch, the SML-PDAOrder repo was not touched).
- `tests/verify_repair.rs` (6): `KeyRepairTests.cs` (resend + tombstone, nothing queued, open/unknown table dropped without reading, no queue) + a paged/cursor test + **acceptance on a fake cloud**: an old agent mirrors the store, check 3 and outlet 2 are deleted and check 100 closed, a new agent starts with an empty SQLite capture.db through the production runner + `OutboxBatchStore`, one verify queues the keys, the runner's key-repair job tombstones them, a second verify is clean.
- `tests/verify_live.rs` (1, gated on `PDA_SYNC_TEST_SQL`, reuses `tests/live_support` + bootstrap 010–090, capture as `pda_sync`): same scenario on the SMILE_POS fixture; deletes closed check 903 and cashier BINH (with its CashierRVC rows); one verify mismatches Invoice, CashierDef, CashierRVC with `keys_queued`; key repair tombstones Invoice 903 and CashierDef BINH. Added to `scripts/live-sql.sh`.
- `src/capture/key_repair.rs` unit test of the cursor format/parse.
- Existing `outbox_store::the_file_has_the_csharp_schema` updated for the new `key_repair`/`ux_key_repair` (same as C# outbox.db).

Commands and results (CARGO_TARGET_DIR=/home/user/wt/target-verify):
- `cargo fmt --all --check`: ok.
- `cargo clippy -p dl-agent-pda-sync --all-targets -- -D warnings`: ok; `cargo clippy -p dl-agent-bin --all-targets -- -D warnings`: ok.
- `cargo clippy -p dl-agent-pda-sync -p dl-agent-bin --all-targets --target x86_64-pc-windows-gnu -- -D warnings`: ok.
- `cargo test -p dl-agent-pda-sync`: 739 passed, 0 failed (live tests skip without SQL). `verify_repair` repeated 10x: all green.
- Live, own container `rt257-verify` (port 14351, TZ Asia/Ho_Chi_Minh, random SA password in a private scratch file, removed with its volume afterwards), all `live-sql.sh` tests with `--test-threads=1`:
  - SQL Server 2022 (16): bootstrap_sql_live 1/1, capture_runner_sql 3/3, commands_sql 4/4, live_smile_pos 1/1, staff_sql 1/1, verify_live 1/1 (also 20+ repeated runs of verify_live green after the settle fix).
  - SQL Server 2017 (14): same, all green (live_smile_pos 22 batches / 18 streams; verify_live 48 tables compared).
  - One early verify_live flake (old-agent phase stopped before CashierRVC was mirrored) was fixed by waiting for the capture to be quiet before stopping/verifying.
  - Note: the tests ran directly on this Linux host, not through `scripts/live-sql.sh` (the script itself was only edited, not executed).

## Deviations / notes

- New files outside the listed owned paths: `src/capture/shadow_manifest.rs`, `src/capture/key_repair.rs`, `src/contracts/wire/sync_verify.rs` (new files, so no conflict). Small hooks in shared files: `capture/{engine,scheduler,sinks_bridge,runner,state,sqlite_state,mod}.rs`, `commands/{executor,mod,service}.rs`, `contracts/{hashing.rs,wire/mod.rs,wire/site_commands.rs}`, `outbox/{batches,mod}.rs`. `executor.rs` (898 -> 910) and `runner.rs` (839 -> 842) were already over 800 lines (B2 list).
- `CaptureEngine::new` signature unchanged; the queue is passed via `.with_key_repairs(..)` (C#: optional ctor arg).
- The run budget uses the injected clock's wall time (C#: `TimeProvider` timestamp); the 25 s table limit is a `Ctx` deadline in real time.
- Key-repair tombstones of keys never met are emitted in queue order (C#: dictionary enumeration order); content identical.
- Log text of the cooldown/limit messages prints the C# TimeSpan forms (`00:00:25`, `7.00:00:00`).

## Concerns (C# parity limits, found by the live test)

1. **Closed open checks are not healed.** Verify skips open-check tables (`open_check`, ADR 0126 §3). A check closed while no agent ran gets its closed ledger rows from the fresh snapshot, but its open-check rows (`TmpInvoice` etc.) stay in the cloud's open set. Asserted as a parity limit in `verify_repair`. Mitigation: cut over with all checks closed (plan §2 step 1). A real fix needs a design change (e.g. open stream snapshot semantics), not done.
2. **Tables that tolerate duplicate rows are not repaired by key.** `CashierRVC` (logical-keys.json `duplicate_rows_tolerated`) is compared by verify and its keys are queued, but `KeyRepairCapture` drops them with a note, so deleted `CashierRVC` rows stay in the cloud and every verify queues them again (C# behaves the same). Asserted in `verify_live`.
3. Plan doc §2 now says this; cloud request 5 ("verify now" after the first snapshot) becomes the way to heal a fresh enrol.

## Only verifiable on Windows / real systems
- Windows run (service, DPAPI, SChannel) only covered by the cross-target clippy.
- The real PDA cloud manifest endpoints (`/sync/v1/manifest`, `/manifest/keys`) were exercised only through `Parse`/`ParseKeys` and a fake cloud; the digest parity with PostgreSQL rests on the shared golden vector.
- Real SMILE_POS volumes (time budget/limits on large shadows) untested.

## Docs
- `docs/plans/2026-11-19-pda-sync-plan.md`: audit rows 17 and 25 ✅ (coverage after B1 75.8%), §2 fresh-enrol row and procedure step 5, slice B1 marked done, live results note.
- `crates/dl-agent-pda-sync/README.md`: Verify bullet. `CHANGELOG.md`: one Added line.

## Stream ORDERS (B9 + B5 cloud link) — report

Status: DONE. Branch `cloud/rt-0257-orders`, worktree `/home/user/wt/orders`.
Commits: `4abfe07` test (B9), `92800d8` feat (B5). Final SHA `92800d8`. Nothing pushed.

## B9: order/kds/reservation/sync commands live, exactly once

### Fixture (`testdata/sql/**`, `tests/live_support/**`)
- `testdata/sql/smile_pos_orders.sql` (runs after `smile_pos_schema.sql`, before the catalog shape):
  - OrderVerbsSchema.cs: tables/routines were already in the verbatim E1 schema (RVC 1); added the same seed for
    RVC 25 (Para, MealPeriod, ItemMenu, RVCItemMenu) — RVC 25 makes `Rvc*100000000` overflow int as in C#.
  - ComboStoreSchema.cs: items A/B/B1/B2/C/77, PkgDef combos 10/20/30 + package B, Para for RVC 3 (GroupChkTrn 1,
    OrderByCourse 0) and RVC 4 (OrderByCourse 1), ItemMenu/RVCItemMenu M1. Kept vendor types (ICurrency int, CrCode 1)
    instead of the stand-in's `'VND'`; stand-in routines stay the E1 ones (GetRVCSeqNumber via Para, PostITrn, ...).
  - SqlTableCommandsIntegrationTests.cs shapes: `tmpTrnInfo.TableNo`, `checkopenlog.CloseTime`, `TrnActivities`
    (vendor-typed), `tmpInvoice.CloseBy varchar(20)` / `PaymentMode varchar(10)` (the catalog generator would type
    PaymentMode `int`, which breaks `PaymentMode = 'Z_CANCEL'`), TableList T4/T5/T12 for RVC 3.
- `testdata/sql/smile_pos_orders_post.sql` (after the catalog shape): `InvoiceCxl` rebuilt as
  `SELECT TOP 0 * INTO ... FROM tmpInvoice` because the inlined spCancelOrder does `INSERT InvoiceCxl SELECT * FROM tmpInvoice`.
- `tests/live_support/mod.rs` runs both; `tests/live_support/orders.rs` is the harness: real bootstrap 010–090 as
  admin, then `SiteCommandExecutor` + `SqlSiteCommandStore` as `pda_sync` (Encrypt=Required); `Live::once` delivers a
  command twice and asserts same answer, +1 `Pda_SyncCommandLog` row, and `find_recorded` == answer.
  `Live::seen` computes the check fingerprint as admin (used after POS WIN edits and to cross-check
  `fingerprint_after`).

### Tests (ported from C# `SqlOrderVerbsIntegrationTests`, `SqlOrderCoversIntegrationTests`,
`SqlOrderComboIntegrationTests`, `SqlTableCommandsIntegrationTests`, KDS/reservation cases of
`SqlSiteCommandStoreIntegrationTests`)
- `tests/live_orders.rs` (3): check_open (header/info/open log, LastCheck once), item_post (line+seat+note+menu, CGTrn,
  package with components and balance line, LastTrn +5 once), send (PDASendOrder once, info rows), sent void (VoidTrn
  once, info flagged), unsent removal (package + 4 rows archived, refused for a sent line); POS WIN close →
  bill_not_open, removal → check_not_found (post/send/void/covers/combo), line edit → check_changed (post/send/void/
  covers/combo) then applies with the re-seen fingerprint; covers once; REVOKE INSERT on VoidTrn → SQL_GRANTS_MISSING,
  nothing written, not journalled, replay after 070 applies once; REVOKE INSERT on Pda_SyncCommandLog → the posted line
  and LastTrn roll back with the journal row (proves one transaction), replay applies once.
- `tests/live_orders_combo.rs` (2): combo 10 qty 2 (A, B, B1, B2, balance; sub_total 260000; LastTrn once), note rows,
  selection ["B"], selection outside → item_not_found logged + nothing written, numeric code without components (77),
  non-numeric code (C), by-course merges (20, 30: the legacy loop bug does not fire), missing check → check_not_found;
  KDS accept/done/undone/unaccept on seed check 1001 (+ not_done/not_accepted/check_not_found refusals);
  reservation.save once (same id), cancel only within RVC, once.
- `tests/live_orders_tables.rs` (3): table_change once (TableList busy), transfer to trimmed table (+ info rows),
  same_table, closed check → bill_not_open; merge once (lines+info moved, source removed, InvoiceCxl Z_CANCEL,
  checkopenlog closed); items_move once with dedup, info rows and one TrnActivities MOVE row per line (no second audit
  on redelivery); paid / tendered / discounted / stale / unknown-line refusals write nothing; move onto itself rejected
  before the store (not journalled).
- `tests/commands_redelivery.rs` (fake, 2): item_post/send/both voids delivered twice → one store write each, journal
  answer; `sync.resnapshot` redelivered → idempotent (already_pending, no SQL transaction) — same as C# (not journalled).
- `scripts/live-sql.sh`: added `--test live_orders --test live_orders_combo --test live_orders_tables`.
- `sync.verify` untouched (owned by the verify worker).

### Bugs found
None in the executor/store/batches: every scenario passed against the faithful fixture on the first run. Only the
fixture needed vendor-faithful columns (PaymentMode type, InvoiceCxl mirror) that the catalog generator gets wrong.

## B5
- `src/config/health.rs`: `AgentHealth::last_heartbeat_ok()` — unix seconds (system clock, `fetch_max`, shared by
  clones) of the last 2xx heartbeat; set in `record` (only HeartbeatSender calls it). Unit test: 0 before first 2xx,
  failures/5xx don't count, value is seconds, never backwards.
- `src/reporting/cloud_link.rs`: `CloudLinkPublisher` → `LinkUpdate::{Keep, Set{url, ok_at}, Clear}`: nothing declared
  before the first accepted heartbeat, ok_at monotonic, `reenroll_required` withdraws the link (a restart cannot fix a
  revoked agent), empty URL never declared. Tests run `dl_agent::watchdog::decide`: no link → healthy forever; declared
  link healthy at 19:59 silent, restart at 20:00.
- `src/reporting/module_reports.rs`: `ModuleReports` log sink — every Error-level site event (marker `site_event=true`,
  its `code`) → `ctx.report(Severity::Error, code, message, {source, event_id, suppressed})`, at most once per code per
  15 min; `TeeSink` puts it beside the site-event bridge.
- `src/module/mod.rs`: wires both (publish loop every 15 s; link cleared at stop). Log folder in the diagnostics
  bundle still needs core hook H2 (documented in the plan).

## Docs
Plan `docs/plans/2026-11-19-pda-sync-plan.md`: rows 3, 23, 24 → ✅ (coverage 75.8%, strict 64.5%), slices B5/B9 marked,
B9 live results table, §7 updated. README and CHANGELOG (Unreleased/Added) entries.

## Commands and results
- `cargo fmt --all --check`: ok.
- `cargo clippy -p dl-agent-pda-sync -p dl-agent-bin --all-targets -- -D warnings`: ok (host and
  `--target x86_64-pc-windows-gnu`). `CARGO_TARGET_DIR=/home/user/wt/target-orders`.
- `cargo test -p dl-agent-pda-sync -p dl-agent-bin`: 719 passed, 0 failed (live tests skip without the env var).
- Live (`PDA_SYNC_TEST_SQL=sqlserver://sa:***@127.0.0.1:14352?encrypt=disable`, container `rt257-orders`,
  `-e TZ=Asia/Ho_Chi_Minh`, `--test-threads=1`), all 8 live test binaries (18 tests):
  - SQL Server 2022 RTM-CU28 (16.0.4325.4): 18/18 (live_smile_pos 22 batches, 18 streams).
  - SQL Server 2017 RTM-CU31-GDR (14.0.3570.2): 18/18.
  Container and volume removed (`docker rm -fv`), password scratch file deleted.
  `scripts/live-sql.sh` itself was not run (needs the xbuild image); the same test list ran directly on the host.

## Deviations / only verifiable elsewhere
- The C# combo parity half (legacy batch run first and rolled back) is not ported; batch text is already pinned byte for
  byte to C#. Vendor routines are the fixture stand-ins; real SMILE_POS procedure bodies (PostITrn, GetRVCSeqNumber,
  UpdateTmpInvoice) only on the B6 bench/real SMILE_POS.
- `sync.resnapshot` is not journalled (same as C#); idempotent instead.
- An Error site event and a status change can both report the same problem (different severities); acceptable.
- Watchdog link behaviour on a real Windows service (scheduled-task watchdog probing the PDA cloud) only on Windows/B6.
- Shared files touched (small, additive): `src/config/health.rs`, `src/module/mod.rs`, `src/reporting/mod.rs`,
  `tests/live_support/mod.rs`, `scripts/live-sql.sh`, README, CHANGELOG, plan doc.

## RT-0257 stream SPEAKER: report

Status: DONE_WITH_CONCERNS (live use needs a PDA cloud change; real audio unverified off Windows)
Branch: cloud/rt-0257-speaker (worktree /home/user/wt/speaker), commit d2f4f3e. Not pushed.

## What was built
New crate `crates/dl-agent-speaker`, registered in `crates/dl-agent-bin/src/main.rs` as module `speaker`
(`enabled_by_default = false`, opt-in through `modules.speaker`):

```
{"modules": {"speaker": {"enabled": true, "rvcList": [1, 2], "soundRefreshSeconds": 120}}}
```

Only workspace crates are used (dl-agent, dl-agent-pda-sync, dl-signalr, dl-ws, dl-http, dl-json, dl-log,
dl-winsvc, dl-winos). No third-party crates, `#![forbid(unsafe_code)]`.

| Go / Rust port source | Here |
|---|---|
| Go `main.go` run/announce/parseGuestArgs/parseServiceArgs; port `agent.rs:41-102` | `src/decision.rs` (parse_event, service_kind, decide), `src/output.rs` (Announcer) |
| Go `internal/soundconfig/soundconfig.go`; port `soundconfig.rs` | `src/mode.rs` (Mode, parse_mode, mode_from_envelope, Cache), `src/fetch.rs` (FetchMode + 2-min refresh) |
| Go `internal/tts/tts.go` BuildPhrase/NormalizeLang; port `tts.rs` | `src/phrase.rs` |
| Go `internal/tts` Speak (PowerShell System.Speech), `internal/player` (winmm) ; port `tts.rs`, `player.rs` | `src/sapi.rs` (#[cfg(windows)], SAPI.SpVoice.Speak + SAPI.SpFileStream/SpeakStream via dl-winos COM on an STA thread), `src/output.rs` SystemOutput (non-Windows: logs only) |
| Go `internal/player/tones/*.wav` | `assets/tones/*.wav` (11 files, embedded) , `src/tones.rs` |
| Go `internal/signalrclient` Run/runOnce/NextBackoff; port `signalr.rs` | `src/hub.rs` on dl-signalr (negotiate + WS, Authorization header only, JoinRvc awaited per RVC, web-client backoff, 5-min retry when every RVC is refused, 401 -> token invalidated) |
| Go `config.json` apiUrl/token/rvcList | `src/module/settings.rs` (`rvcList`, `soundRefreshSeconds`); URL and credential from pda-sync enrolment, `src/auth.rs` |
| - | `src/module/mod.rs` (Module impl, degraded until pda-sync enrolled, metrics, `SPEAKER_JOIN_REFUSED` report), `src/module/commands.rs` (`status`, `test`) |

## Authentication decision
No 365-day speaker-device JWT file. `src/auth.rs` reads the pda-sync enrolment in `<data>/modules/pda-sync`
(`agent.json` for the cloud URL, the secret store's Ed25519 seed + agent token; never creates a key), and builds
pda-sync's own `TokenProvider` (signed `POST /sync/v1/token`, 15-minute access tokens, refresh before expiry,
invalidate on 401). Token only in the `Authorization` header (never in URL/logs/command output).
Side effect inherited from TokenProvider: if the cloud rejects the agent token it writes `reenroll_required`
into pda-sync's `status.json` (same fact pda-sync itself would record).

### Cloud change needed (documented in crate README, PDAOrder not touched)
1. `OrderHub.JoinRvc` throws `FORBIDDEN_RVC` for `IsSyncAgent()` (OrderHub.cs:146-148). Needs: allow SyncAgent
   to join RVC groups of its own tenant's branches (the `PrintRelayBranches` set), keep refusing JoinCheck/JoinKdsDashboard.
2. `GET /api/v1/speaker/sound-mode/{rvc}` is `ClerkOrSpeakerDevice` (SpeakerEndpoints.cs:30). Needs: accept the
   SyncAgent role scoped to its tenant's RVCs, read-only, the one key.
Until (1): module degraded with the reason + one `SPEAKER_JOIN_REFUSED` report, plays nothing.
Until (2): default mode per RVC (guest tone, vi voice), `status` lists the 403 per RVC.

## Print relay
Not ported: no JoinPrintRelay, `PrintJob` ignored (ADR 0026 one relay per branch; pda-sync relays per ADR 0098).
`ForceUninstall` logged and ignored (dl-agent owns uninstall). Documented in README and plan.

## Docs
- `crates/dl-agent-speaker/README.md` (behaviour, auth choice, cloud change, not ported, verified where).
- `CHANGELOG.md` one Added line; `crates/dl-agent/README.md` module table row.
- `docs/plans/2026-11-19-pda-sync-plan.md`: rows 34-38 -> "IN (RT-0257)" 🟡, row 39 -> ✅, new §4b,
  §4 relay sentence and the scope OUT list updated. The "Coverage:" line was NOT recomputed (other streams edit
  this file; recompute at merge).

## Tests (commands run with CARGO_TARGET_DIR=/home/user/wt/target-speaker)
- `cargo fmt --all --check`: clean.
- `cargo clippy -p dl-agent-speaker -p dl-agent-bin --all-targets -- -D warnings`: clean.
- `cargo clippy -p dl-agent-speaker -p dl-agent-bin --all-targets --target x86_64-pc-windows-gnu -- -D warnings`: clean (sapi.rs compiled).
- `cargo test -p dl-agent-speaker -p dl-agent-bin`: 24 unit + 3 integration passed, 0 failed (integration run 3x, stable, ~0.3 s).
  - Decision unit tests (`decision.rs`): event -> kind mapping incl. case-insensitive targets and garbled args; default
    mode = guest tone + vi phrase; tone null keeps voice (en); blank tone + voice off = nothing; voice off = tone only;
    per-RVC modes from the cache incl. unfetched RVC default; service kinds with lang auto.
  - mode/envelope/phrase/lang tables ported from the Go and Rust-port tests; settings bind/refuse; fetch against a
    local HTTP server (cache kept on 401/bad body, 401 invalidates token, URL + bearer header); Announcer order,
    counters, unknown tone, stop; commands status/test/unknown.
  - `tests/hub_fake.rs` (fake hub: negotiate + WS upgrade + handshake): JoinRvc sent for 1,2,3 (3 refused
    FORBIDDEN_RVC), GuestOrderRequested(1,"T05") -> `play guest` + `speak vi Bàn T05 đặt món`,
    GuestServiceRequested(2,"7",..,"REQUEST_BILL") with tone-off/en mode -> `speak en Table 7 requests the bill`,
    PrintJob ignored, Authorization header on negotiate and upgrade, no token in URL; all-refused -> closed and
    retried, nothing connected; negotiate 401 -> token invalidated, error recorded without the token.
- Ignored manual test `sapi::tests::plays_a_tone_and_speaks_both_languages` (Windows with speakers, `-- --ignored`).
- No SQL involved; no docker container used.

## Only verifiable on Windows / live
- Real audio: SAPI SpFileStream WAV playback, voice selection by LCID 042A/0409/0809 (Microsoft An/OneCore voices may
  not be visible to SAPI 5; then the default voice speaks, as the Go agent).
- Live PDA cloud: only after the cloud change above; the token exchange path is pda-sync's (already tested there).

## Deviations from Go
- JoinRvc awaited (invoke) instead of fire-and-forget, to see refusals.
- Announcements play on a worker thread with a 32-entry queue (overflow dropped and counted) instead of in the read loop.
- Speech in-process via SAPI COM instead of PowerShell; tones via SAPI file stream instead of winmm (no `unsafe`).
- Sound-mode refresh interval configurable (default 120 s, min 30 s).
