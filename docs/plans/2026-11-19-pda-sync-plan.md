# Plan: `pda-sync` module (PDAOrder SQL sync agent on dl-agent)

Owner rule (2026-11-19): every site uninstalls ALL old agents (the C# and the Rust
`PdaSyncAgent`) and enrols dl-agent fresh. There is no in-place takeover. PDAOrder must
be deployable on dl-agent urgently.

Source of truth:
- the Rust port `SML-PDAOrder/apps/sql-sync-agent/rust`, about 63k lines;
- for behaviour, the C# agent `apps/sql-sync-agent`.

Scope (SMILE rule):
- **IN:** everything that reads or writes `SMILE_POS`, plus the site-ship needs (enrol, heartbeat, selftest, reports). This includes the print relay (see §4).
- **OUT:** speaker, `legacy-archive`, Azure, systemd, the config UI, door-lock.

## 1. Done in phase A (branch `feat/smile100-pda`)

**The crate.** `crates/dl-agent-pda-sync` is the port as a workspace crate:
- workspace dependencies, no git pins, workspace lints, no `unsafe`;
- `missing_docs` on.

Dropped from the port:
- `update/`, which dl-agent's core self-update replaces;
- `diag`, which the core diagnostics bundle replaces;
- the `PdaSyncAgent` binary and its service-recovery hook. That hook would have changed the OLD service while it is being removed.

**The module.** `pda_sync_module()` is registered in `dl-agent-bin` and is opt-in through `modules.pda-sync`:
- Its data folder is `<data>/modules/pda-sync`, with the C# layout: `agent.json`, `secrets`, `outbox.db`, `capture.db`, `blobs.db`, `status.json`, `logs`.
- Settings come from the shipped `appsettings.json`. The hub's `modules.pda-sync` section overrides them, with the same keys as the C# `Agent` section (`HeartbeatSeconds`, `PrintRelay`, `SiteTimeZone`, ...). `AutoUpdate` is forced off.
- Until `agent.json` exists, the module stays degraded with the next step, and it still answers commands.
- Once `agent.json` exists, it runs the C# host's loops on a thread:
  - capture;
  - upload, with 424 parent-wait;
  - blobs;
  - heartbeat and runtime config;
  - command channel with push and long-poll;
  - staff sync;
  - grants monitor;
  - print relay;
  - site events.

  It mirrors `status.json` into the module's state, metrics (queue depths) and reports. A report goes out once per change into `degraded` (Warning) or `reenroll_required` (Critical).
- Tools:
  - `dl-agent pda-enroll --code --url [--dry-run] [--sql-instance] [--database] [--sql-admin-user]`;
  - `dl-agent pda-grants`. It is the C# `grants` verb.

  The SQL admin password comes from `PDA_SQL_ADMIN_PASSWORD` or a hidden prompt, never from argv.
- Selftest rows: settings, enrolled, state, cloud ready, `pda_sync` signs in and reads, order grants, data folder, print relay settings.
- Hub commands:
  - `status`;
  - `selftest`;
  - `capture_dry_run`: one snapshot plus open-check pass into memory, giving per-stream batches, rows, tombstones and sha256, plus the blob count and bytes. Nothing is queued or uploaded. It runs on its own thread, one at a time.

  Kinds may carry the `pda-sync.` prefix.
- The agent version reported to the PDA cloud is `dl-agent/<ver>`.
- Operator hints now name `dl-agent pda-enroll` and `dl-agent pda-grants`, never the PDA cloud's `irm …/agent | iex`, which would reinstall the old agent.

**The live acceptance test.** `tests/live_smile_pos.rs` is gated on `PDA_SYNC_TEST_SQL` and runs on SQL Server 2017 and 2022. It uses the SMILE_POS fixture of the PDAOrder side-by-side e2e: `testdata/sql/smile_pos_{schema,seed}.sql`, copied verbatim, plus the e2etool catalog-shape generation, ported to `tests/live_support`. The test:
1. runs the real bootstrap `010`–`090` as an administrator;
2. runs the rest as the `pda_sync` login those scripts create;
3. checks that a `capture_dry_run` pass is complete and deterministic;
4. checks that the production capture pipeline fills a SQLite outbox;
5. sends one guarded `kds.line_state` twice, and checks:
   - one `tmpTrnInfo` write;
   - one `Pda_SyncCommandLog` row;
   - the same answer both times;
   - the change captured back into the outbox.

**Live results (2026-11-19).** `crates/dl-agent-pda-sync/scripts/live-sql.sh` runs every `PDA_SYNC_TEST_SQL`-gated test on throwaway containers. All 10 tests pass on both servers:

| Server | `live_smile_pos` | `bootstrap_sql_live` | `capture_runner_sql` | `commands_sql` | `staff_sql` |
|---|---|---|---|---|---|
| SQL Server 2022 RTM-CU27 (16.0.4295.3) | 1/1 (22 batches, 18 streams) | 1/1 | 3/3 | 4/4 | 1/1 |
| SQL Server 2017 RTM-CU31-GDR (14.0.3570.2) | 1/1 (22 batches, 18 streams) | 1/1 | 3/3 | 4/4 | 1/1 |

Slice B1 (2026-11-19) adds `verify_live` to the script: 1/1 on SQL Server 2022 (16) and 2017 (14), with the other live tests still green on both.

The tests run in a Linux container of the repo's build image (`scripts/xbuild/Dockerfile`), not on the macOS host. The agent connects with `Encrypt=Required`, and `dl-tls` has no macOS backend, so on macOS every agent-login connection fails with "error 0". Tests that only use the `encrypt=disable` admin URL pass on macOS too. Windows uses SChannel; it is covered only by the cross-target clippy (B6).

`bootstrap_sql_live` used to assert `major >= 16`. It now asserts the preflight minimum (`MIN_MAJOR`, 11), so that it runs on the 2017 container as well.

### Deviations from the port

- **The port's loop host is kept.** The module runs the C# host's service loops on one thread inside `Module::run`, instead of one core job per loop. This keeps the port's tested ordering and back-off. Moving loops onto core jobs is not planned.
- **Module-owned SQL login.** The module signs in as its own least-privilege `pda_sync` login created by bootstrap `010`–`090`, not through the core `sql.json` pools. Core hook H3 covers the selftest and survey side.
- **No in-place state.** `%ProgramData%\MM\SyncAgent` is never read (owner rule). Audit row 9 is replaced by the fresh enrol.

## 1b. Audit status after phase A

Status: ✅ ready (code, unit tests and, where it touches SQL, a live test), 🟡 partly, ❌ not started. Coverage counts ✅ as 1 and 🟡 as 0.5 over the 31 IN rows.

| # | Function | Status | Evidence or what is left |
|---|---|---|---|
| 1 | Service host, SCM recovery | ✅ | dl-agent core; the port's service code is dropped |
| 2 | Self-update | ✅ | dl-agent core; `AutoUpdate` forced off in the module |
| 3 | Problem reports, site events | 🟡 | State changes report through `ctx.report`; per-code mapping of site events is B5 |
| 4 | Diagnostic bundle | 🟡 | Core bundle with `status.json` and `runtime-config.json`; the log file is B5 |
| 5 | Installer takeover | ❌ | Replaced by uninstall + fresh enrol (owner rule); the hub step is B3 |
| 6 | Hub product gate | ❌ | B3 |
| 7 | Module skeleton, config, opt-in | ✅ | `src/module/{mod,settings}.rs`, unit tests |
| 8 | Enrolment, `pda-enroll` | ✅ | `src/module/tool.rs`, port enrol tests, `--dry-run` |
| 9 | Read old state in place | out | Replaced by the fresh enrol (owner rule) |
| 10 | SQL bootstrap 010–090 | ✅ | Live on 2017 and 2022 (`live_smile_pos`, `bootstrap_sql_live`) |
| 11 | `grants` verb, grants monitor | ✅ | `pda-grants` tool, port tests, selftest row |
| 12 | SQL discovery and preflight | 🟡 | Preflight live; local instance discovery is Windows-only (B6) |
| 13 | TDS access, TLS, lock timeout | ✅ | Live with `Encrypt=Required` on Linux; SQL 2008 R2/2012 and SSPI are bench-only (B6) |
| 14 | TCVN3 text normalisation | ✅ | Port unit and golden tests |
| 15 | Contracts byte-identical to C# | ✅ | Port golden tests (`contracts/tests/dotnet.rs`) |
| 16 | Capture engine | ✅ | Live capture as `pda_sync` on 2017 and 2022 |
| 17 | Snapshot, resnapshot, 424 parent wait | ✅ | Snapshot live; key repair live (B1, `verify_live`) |
| 18 | Outbox, uploader | ✅ | Port tests; live capture fills a SQLite outbox |
| 19 | Blob queue | ✅ | Port tests (`outbox_uploaders`) |
| 20 | Heartbeat, runtime config | ✅ | Port tests |
| 21 | Command channel | ✅ | Port tests (`commands_channel`) |
| 22 | Exactly-once `kds.line_state`, reservations | ✅ | Live redelivery test on 2017 and 2022 |
| 23 | Order verbs `open/post/send/void` | 🟡 | SQL text pinned byte for byte to C#; not yet executed live (B9) |
| 24 | Combo, table change/transfer/merge | 🟡 | Same as 23 (B9) |
| 25 | `sync.verify` and key repair | ✅ | B1: `commands/verify*.rs`, `capture/{shadow_manifest,key_repair}.rs`, `outbox/key_repairs.rs`; C# tests ported (`verify_handler`, `verify_contract`, `verify_repair`); wire JSON checked against .NET 10; `verify_live` on 2017 and 2022 |
| 26 | Staff sync from `CashierDef` | ✅ | Live `staff_sql` on 2017 and 2022 |
| 27 | Print relay engine | 🟡 | Wired and unit-tested; no real printer yet (B7) |
| 28 | Print queue discovery | 🟡 | Windows-only; bench (B6, B7) |
| 29 | `legacy-export --from-sql` | ❌ | B4 |
| 30 | `legacy-archive` | out | Not SMILE_POS |
| 31 | `legacy-upload` | out | Not SMILE_POS |
| 32 | `status`, `status.json` | ✅ | Hub `status` command, module state |
| 33 | Selftest and Windows bench | 🟡 | Selftest rows done; bench run is B6 |
| 34–38 | speaker | out | Speaker is out of this programme |
| 39 | Print relay duplication | 🟡 | Decision in §4; needs the speaker's guard when the speaker returns |

**Coverage:** 17 ✅ + 10 🟡 = 22/31 = **71.0%** (strict ✅ only: 17/31 = 54.8%). The audit baseline was 15.4%.

After slice B1 (rows 17 and 25 ✅): 19 ✅ + 9 🟡 = 23.5/31 = **75.8%** (strict 19/31 = 61.3%).

## 2. Fresh-enrol analysis

The owner asked whether a fresh enrol of a live site is safe, and what it costs. These are the findings from the PDA cloud code and ADRs 0029, 0100, 0102, 0109, 0126 and 0128, plus the agent code.

| Question | Finding | Safe? |
|---|---|---|
| Duplicate orders or commands | The exactly-once journal is `dbo.Pda_SyncCommandLog`, keyed by `CommandId`. It lives **in SMILE_POS**, not in the agent folder. A command the old agent applied answers from that row when the new agent gets it again. `sync.commands` are tenant-level with a lease, so a command leased by the old agent becomes available again when the lease expires, and the journal makes the re-run a read. The live test proves the redelivery path. | **Yes** |
| Duplicate rows in the cloud | `sync.row_state` upserts per tenant by natural key, guarded by `src_ver`. A resnapshot rewrites the same keys with the same content. | **Yes** |
| Cursors | Upload cursors and stream sequences are per `agent_id`. A new agent starts with no cursors and an empty `capture.db`, so it sends a **full snapshot of every stream**, including the closed-ledger history window (`historyMonths`, 24 by default). | Costly |
| Cost | The catalog's sample site has about 1.05M rows in captured tables: about 0.97M closed ledger (`Trn` 172k, `CheckActivities` 140k, `KitchenOrderDetail` 135k, ...), 48k masters and 30k open-check rows. The upload rate is 8 batches per cycle. Expect hours of upload, during which the PDA reads lag. `capture_dry_run` gives the exact count per site before the cutover. | Plan it |
| Deletions while no agent runs | The new `capture.db` has no shadow hashes. A row deleted or closed in SMILE_POS between the old agent's last cycle and the new agent's snapshot gets **no tombstone** from the snapshot. Since slice B1, one `sync.verify` heals it: the manifest mismatch is narrowed to keys and the capture's key-repair job tombstones them (`verify_repair`, `verify_live`). Two C# limits remain: open-check tables are never compared, so the open-check row of a check closed in the gap stays in the cloud's open set; and tables that tolerate duplicate rows (`CashierRVC`) are dropped by the key repair, so their deleted rows stay and are queued again by every verify. | Healed by B1, except open checks and `CashierRVC` |
| Two agents at once | Both would capture, relay prints and lease commands. The journal keeps commands exactly-once, but prints could double. | Uninstall first |

**Verdict.** A fresh enrol is safe for money and orders. It costs one full resnapshot, and it has a stale-row gap for anything deleted between the uninstall and the new snapshot.

Minimum procedure that makes it safe without carrying any state:
1. Cut over in a quiet window: after end of day, all checks closed, no POS activity.
2. Run `capture_dry_run` first, to size the upload.
3. Uninstall the old agent, then run `dl-agent pda-enroll` in the same session, so the gap is minutes with no POS writes.
4. Revoke the old agent in the PDA cloud.
5. Send `sync.verify` once after the first snapshot (cloud request 5); stale ledger and master rows heal by themselves. Doing the cutover with all checks closed (step 1) keeps the open-check limit out of the way.

The alternative, only if the owner wants no resnapshot, is to carry over `capture.db` alone (shadow hashes and cursors, slice B8). It is the minimum state. It is not built, because cloud cursors are per `agent_id` and reusing them under a new agent needs a PDA cloud change. Do not build it without an owner decision.

## 3. PDA cloud requests (exact list)

1. **Enrol codes.** One code per site for `dl-agent pda-enroll`. This is the existing admin flow and needs no code change. The bench needs a demo tenant and a code.
2. **Revoke the old agent** after the cutover (the existing `DELETE /agents/{id}` admin action). The PDA admin should show which agent is current, so that an operator does not revoke the new one.
3. **Readiness rule `AgentBuild`.** Accept `dl-agent/<semver>` as an agent build. Today it is non-blocking, but it reads as unknown or outdated.
4. **Install hint for migrated tenants.** The cloud's `irm <cloud>/agent | iex` install path and the "re-run the installer" hints must not be shown to dl-agent sites, because they reinstall `PdaSyncAgent`. Point to the dl-agent installer instead. Do not serve `PdaSyncAgent` update manifests to tenants marked as migrated.
5. **Optional, after slice B1:** a "verify now" action that sends `sync.verify` to the new agent after the first snapshot completes.

## 4. Print relay decision

**IN.** ADR 0098 makes the SQL sync agent the default-on print relay at sites without a speaker. Uninstalling it without a replacement would stop kitchen printing.

The module runs the port's relay:
- over `/hubs/order`;
- TCP 9100 or spooler RAW;
- with the crash-safe journal;
- fail-closed;
- with the private-CIDR target policy.

It is wired through the host hooks. It is **not yet verified on a real printer** (slice B7).

Where the speaker also relays (speaker is OUT of this programme), keep the one-relay-per-branch rule of ADR 0026. The speaker module must not relay while `pda-sync` does.

## 5. Phase-B slices

Each slice can run in parallel with the others. The owned files are disjoint.

| Slice | Work | Owned files | Size |
|---|---|---|---|
| B1 `sync.verify` + key repair (**done**, RT-0257) | Port C# `VerifyCommandHandler` (bucket manifests vs shadow) and `SqliteOutbox.KeyRepairs` (ADR 0126/0128), with tests from the C# suite. This closes the fresh-enrol deletion gap, except the two C# limits in §2. | `src/commands/verify*.rs`, `src/outbox/key_repairs*.rs`, `src/capture/{shadow_manifest,key_repair}.rs`, `tests/verify_*.rs` | M |
| B2 file splits | 15 files are over 800 lines: `util/stj.rs` 1517, `commands/batches.rs` 1168, `hosting/host.rs` 1066, `contracts/time.rs` 1027, `reporting/format.rs` 976, `commands/executor.rs` 898, `config/providers.rs` 863, `capture/runner.rs` 839, `contracts/legacy/sql_store.rs` 823, plus the tests `commands_executor.rs` 1516, `outbox_uploaders.rs` 1117, `capture/strategy_tests.rs` 1040, `site_events.rs` 996, `contracts/tests/dotnet.rs` 810, `commands/tests/sql.rs` 806. Split them by responsibility and keep the public paths. Run the split after B1 for `commands/`. | the files listed | M |
| B3 hub: uninstall the old agent + product gate | `install/pdaLegacy.ts`: stop and delete the `PdaSyncAgent` service and any `PdaSyncAgent*` scheduled task. Rename `%ProgramData%\MM\SyncAgent` to `SyncAgent.retired-<date>` for rollback; never delete it. Then run `dl-agent pda-enroll`. Also a `pda-sync` product requirement: SMILE_POS tables, the `pda_sync` login, `Pda_Sync*` tables, order grants. | `services/agent-hub/src/install/pda*.ts`, its tests | M |
| B4 `pda-legacy-export --from-sql` | Port `legacy/export` as a Tool. It is IN only for the `--from-sql` path, which reads `dbo.Pda_*` in SMILE_POS (ADR 0094). | `src/legacy/export/**`, `src/module/tool.rs` (one tool), one `dl-agent-bin` line | M |
| B5 reports, logs, diagnostics, cloud link | Map the port's site events and error codes onto `ctx.report`. Add the current log file to the bundle (`status.json` and `runtime-config.json` are already named). Call `ctx.set_cloud_link(cloud_url, ok_at)` from the heartbeat's last success, so the core watchdog sees the PDA link; a wrong `ok_at` restarts the service, so it needs a test. | `src/reporting/**`, `src/module/mod.rs` | S |
| B6 bench run | Windows bench with SMILE_POS and a PDA demo tenant: uninstall the old agent, `pda-enroll`, then: capture to the cloud; a KDS done from the PDA UI; one order post; one reservation; staff sync of `CashierDef`; `selftest`; `capture_dry_run`. | none (report only) | M |
| B7 print relay on hardware | One ticket to a 9100 printer and one to a spooler queue, then an unknown outcome with no reprint, then a cloud redeploy with a quiet reconnect. | `src/printrelay/**` (fixes only) | S |
| B8 `capture.db` carry-over (only on an owner decision) | `pda-import-state --from %ProgramData%\MM\SyncAgent`: copy `capture.db` only. Needs PDA cloud support for adopting cursors. | `src/module/import_state.rs` | S |
| B9 live order verbs | Extend the fixture with the C# `OrderVerbsSchema.cs` and `ComboStoreSchema.cs` shapes, kept faithful to SMILE_POS. Then run `open/post/send/void`, covers, combo post, table change/transfer, check merge and move lines live, each one exactly-once and refused after a POS WIN change. Add them to `scripts/live-sql.sh`. | `testdata/sql/**`, `tests/live_orders*.rs`, `tests/live_support/**` | M |

## 6. Core hooks needed (for the core slice)

- **H1. Long-running module commands.** `capture_dry_run` can take minutes on a large site. It needs a way to keep the command leased or to report progress (`ctx.progress(id, …)`), or an assurance that a late `complete` is accepted.
- **H2. Diagnostics include list per module.** Done on main as `ModuleContext::add_diagnostic_file`; the module names `status.json` and `runtime-config.json`. Still missing: a way to name a rolling log folder (the core takes single files), for B5.
- **H3. Module-owned SQL login.** `pda-sync` uses its own least-privilege `pda_sync` login with the bootstrap grants, not the core `sql.json` pools. The core SQL selftest and the survey should be able to skip `SMILE_POS` for this module, or take a per-module credential. The new core `sql/setup.rs` could later host the server-level login scripts (010/020).
- **H4. Installer step** (hub install script, core-owned). Call the B3 uninstall before the service install, on sites where `PdaSyncAgent` exists.

## 7. Verification still owed

- A Windows compile is covered by clippy with `--target x86_64-pc-windows-gnu`. A Windows run is not: no service run, DPAPI secret store, spooler or SSPI admin yet (B6).
- SQL Server 2008 R2 and 2012 with the TLS 1.0 ladder are untested; docker images exist only from 2017 on.
- The port's side-by-side "0 differences" against C# was not re-run here.
- The order verbs, combo and table commands write to SMILE_POS but have no live test yet (B9).
- The live tests need Linux (or Windows); on macOS the `Encrypt=Required` connections fail because `dl-tls` has no macOS backend.
