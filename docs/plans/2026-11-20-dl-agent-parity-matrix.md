# dl-agent parity matrix: every old site agent vs dl-agent (RT-0260)

Task 644 of RT-0164. For each of the 9 old agents in `crates/dl-agent/INVENTORY.md` §1, every function, command, endpoint,
SQL object and local state was read in the old agent's repo and matched against dl-agent (core + module). State as of
2026-11-20, after RT-0256..0259 (PRs #63–#66) and the small fixes of this review (PR #68).

Columns: function | old agent (repo/path:line) | dl-agent (DevLibrary/path:line) | trạng thái (có / thiếu / khác) |
rủi ro tiền-dữ liệu | việc cần làm. Paths are prefixed with the repo name; the old SMILE-PMS site agent was removed in
`1297a55`, so its rows cite the tree at `1297a55^`.

Totals: 501 rows (E-Invoice 74, PDAOrder SyncAgent 81, speaker 29, PMS site 48, PMS device 41, Check-In 53,
Membership 64, HotelLink 53, SRBackup 58).

## 0. Large gaps in dependency order (customer projects first)

| # | Gap | Risk | Work | Depends on |
|---|---|---|---|---|
| 1 | Membership: REDEEM jobs ignored the `vouchers` array (.NET `58a4524`: one discount line per voucher, exactly the hub's `appliedAmount`) | **cao (tiền)** | **fixed in PR #69** (golden + live SQL 2022 with replay). Victory / Mũi Né stay `read_only: true` until a release with it is on stable | — |
| 2 | Membership: `SmileDiagnostics` (periodic, after failed jobs, on hub request) not ported | trung | port, 4–6 h | — |
| 3 | Membership: heartbeat shares the job-pull loop (a failed pull skips it; a slow batch delays it → hub may mark the agent offline) | trung | own heartbeat thread, ~2 h | — |
| 4 | Check-In: no `export-key` to print the tenant master key for offline storage (only cloud escrow + fingerprint) | trung | owner decision, then port | owner |
| 5 | pda-sync: old `appsettings.site.json` (SiteTimeZone, PrintRelay) not read at switch → wrong time zone shifts every captured time | **cao (dữ liệu)** | read it in `pda-enroll` / switch, or make it a required remote setting before cutover | — |
| 6 | pda-sync: order/combo/table money commands run only against fixture procs, not real SMILE_POS | **cao (tiền)** | B6 bench on a real SMILE_POS | SR bench |
| 7 | speaker: PDA cloud refuses SyncAgent in `JoinRvc` and `sound-mode` → silent after switch; speaker-only PCs (no SQL, no pda-sync enrol) cannot run it | cao (vận hành) | PDA cloud change + a speaker-only enrolment | PDA cloud (owner) |
| 8 | backup: encrypted `.bak.dlbk` copies need the agent to restore (no standalone decrypt tool) | cao (dữ liệu) | ship a `backup-decrypt` tool before the SRBackup hub enables keys; owner decision | SRBackup hub route (RT-0259) |
| 9 | backup: a self-update cancels a running BACKUP | trung | core "busy" hint that defers updates | core |
| 10 | backup: Go job history not imported; DST time zones refused; adoption does not look for `agent.json` next to the Go exe | thấp–trung | port each | — |
| 11 | pms-site: live bench against the real cloud on Windows; SQL 2008 site; first write op | trung | bench | SR bench, RT-0252 |
| 12 | pms-device: enrolment instead of a hand-placed token; hardware bench; OPOS now opens the cash drawer where .NET refused (owner decision) | trung | port + decision | owner, bench |
| 13 | E-Invoice: check An Lâm's `collect_profile` on prod uses `iname1` (Go always took `IName1`) | **cao (hoá đơn)** | read the prod tenant config in MM-EINVOICE before switching An Lâm | MM-EINVOICE prod |
| 14 | E-Invoice: Go `no-update` opt-out not carried over; the Go service must be removed at switch (else double push — already in `-Replace`/`remove_legacy`, keep on the checklist) | trung | installer step in the hub | hub |
| 15 | HotelLink: local config UI (127.0.0.1:8088) not ported; `update.window` not carried over; HL hub still offers Go builds; Windows checks (DPAPI `enc:` fields, SSPI, kill -9) | trung | hub-side screen over `hl.*` commands or a local UI (owner choice); cutover checklist; SMILE-HLAPI hub change; bench | owner, SMILE-HLAPI, SR bench |
| 16 | Check-In: HTTP/1.1 without PING (silent NAT drop seen after 60 s, not ~30 s); `{today}` uses a fixed +07:00 | thấp | later | — |

## 1. E-Invoice (MM-EINVOICE Go `cmd/agent` → `dl-agent-einvoice`)

Old agent read: `MM-EINVOICE/cmd/agent/*.go`, `internal/agentcollector/*.go` (6.5 k lines, HEAD `c5d193d`). New: `DevLibrary/crates/dl-agent-einvoice` + dl-agent core. Customer: An Lâm (DXQN is MISA, not on the agent).

| function | old agent (path:line) | dl-agent (path:line) | status | rủi ro tiền-dữ liệu | việc cần làm |
|---|---|---|---|---|---|
| Process entry, flag parsing (`-config -selftest -once -enroll -selfcheck`) | MM-EINVOICE/cmd/agent/main.go:27 | DevLibrary/crates/dl-agent/src/cli.rs:190; DevLibrary/crates/dl-agent-einvoice/src/enroll/tool.rs:35 | khác | thấp: verbs moved to `einvoice-enroll` + hub commands | none |
| Windows service host (SCM detect, Execute, stop) | MM-EINVOICE/cmd/agent/service_windows.go:33 | DevLibrary/crates/dl-agent/src/cli.rs:420 (`dl_winsvc::run_service_with_exit_code`) | có | thấp | none |
| Service recovery actions (restart on failure) | MM-EINVOICE/internal/agentcollector/service_windows.go:121 | DevLibrary/crates/dl-agent/src/service_watch.rs:131 | có | thấp | none |
| Install + start service, persist exe to Program Files | MM-EINVOICE/internal/agentcollector/service_windows.go:37, :168 | DevLibrary/crates/dl-agent/src/cli_install.rs:256 (install.ps1) | khác | thấp: dl-agent's own service | none |
| Remove stale `MM-EInvoice*` services at enroll | MM-EINVOICE/internal/agentcollector/service_windows.go:220 | install.ps1 `-Replace '*MM-EInvoice*','MMEI-Agent'`; DevLibrary/services/agent-hub/src/install/benchLegacy.ts:223 | khác | trung: two agents would double-push/relay if Go not removed | keep `-Replace` on every einvoice install |
| Config dir default `%ProgramData%\MM-EInvoice` | MM-EINVOICE/cmd/agent/main.go:137 | DevLibrary/crates/dl-agent-einvoice/src/module/settings.rs:131 | có | thấp | none |
| `config.enc` load, DPAPI LocalMachine open | MM-EINVOICE/internal/agentcollector/config.go:185 | DevLibrary/crates/dl-agent-einvoice/src/agent_config.rs:328, :446 | có | thấp | none |
| Env seeding `MMEI_AGENT_*` when no `config.enc` (and save) | MM-EINVOICE/internal/agentcollector/config.go:216 | — (`load` returns None → degraded "not enrolled") | thiếu | thấp: dev/unenrolled path only | none (document) |
| Defaults for zero fields (SP names, batch 50, poll 30 s, cycle 2 min) | MM-EINVOICE/internal/agentcollector/config.go:123, :270 | DevLibrary/crates/dl-agent-einvoice/src/agent_config.rs:67, :268 | có | thấp | none |
| Redacted config log | MM-EINVOICE/internal/agentcollector/config.go:334 | DevLibrary/crates/dl-agent-einvoice/src/agent_config.rs:311; module/mod.rs:184 | có | thấp | none |
| Outbox path from sealed config (`outbox.db`) | MM-EINVOICE/internal/agentcollector/config.go:264 | DevLibrary/crates/dl-agent-einvoice/src/outbox.rs:51 (`<data>/modules/einvoice/outbox.json`) | khác | thấp: Go queue not imported; rows unmarked → re-collected, platform dedups | none |
| `MMEI_AGENT_MOCK` mock SQL sources | MM-EINVOICE/cmd/agent/main.go:175 | — | thiếu | thấp: test only | none |
| `-enroll`: token exchange `POST /agent/enroll` | MM-EINVOICE/internal/agentcollector/enroll.go:31 | DevLibrary/crates/dl-agent-einvoice/src/enroll/exchange.rs:16 | khác | thấp: `agent_version` in `X-Agent-Version` header, not body | none |
| SQL creds default (SMILE service account, build-time password) | MM-EINVOICE/internal/agentcollector/enroll.go:88 | DevLibrary/crates/dl-agent-einvoice/src/enroll/mod.rs:79 (`option_env!` + `MMEI_SQL_PASSWORD`) | có | thấp | none |
| `-sql-prompt` interactive SQL form | MM-EINVOICE/internal/agentcollector/enroll_prompt.go:178 | DevLibrary/crates/dl-agent-einvoice/src/enroll/prompt.rs:145 | có | thấp | none |
| Hidden password input on console | MM-EINVOICE/cmd/agent/console_windows.go:14 | DevLibrary/crates/dl-agent-einvoice/src/enroll/tool.rs:126 | có | thấp | none |
| Enroll TLS ladder (TLS 1.0, then `encrypt=disable`) | MM-EINVOICE/internal/agentcollector/enroll_transport.go:107 | DevLibrary/crates/dl-agent-einvoice/src/enroll/ping.rs:56 | có | thấp | none |
| `-collect-from` / default today VN time | MM-EINVOICE/internal/agentcollector/enroll.go:175, :183 | DevLibrary/crates/dl-agent-einvoice/src/enroll/mod.rs:55, :68 | có | thấp | none |
| Seal config + read hub config at enroll | MM-EINVOICE/internal/agentcollector/enroll.go:194, :249 | DevLibrary/crates/dl-agent-einvoice/src/enroll/mod.rs:130 | có | thấp | none |
| Switch from Go keeping device/SQL (new) | — (Go re-run of `-enroll`) | DevLibrary/crates/dl-agent-einvoice/src/enroll/switch.rs:33, :62 | khác | thấp: Go key revoked by the exchange | none |
| Install collect procedures (FO/POS) | MM-EINVOICE/internal/agentcollector/installsql.go:156, :211 | DevLibrary/crates/dl-agent-einvoice/src/install.rs:244, :261 | có | thấp | none |
| FO schema tokens in proc bodies | MM-EINVOICE/internal/agentcollector/installsql_fo.go:106 | DevLibrary/crates/dl-agent-einvoice/src/install.rs:86 | có | thấp | none |
| SQL Server 2008 rewrite (CONCAT, IIF, …) | MM-EINVOICE/internal/agentcollector/installsql_legacy.go:35, :87 | DevLibrary/crates/dl-agent-einvoice/src/legacy.rs:29, :260 | có | thấp: legacy golden tests | none |
| Write-back proc self-heal on error 2812 | MM-EINVOICE/internal/agentcollector/sqlsource.go:89 | DevLibrary/crates/dl-agent-einvoice/src/sql.rs:508, :573 | khác | thấp: also installed proactively before first cycle | none |
| SQL connection per source, transport | MM-EINVOICE/internal/agentcollector/collectsource.go:95; sqlsource.go:53 | DevLibrary/crates/dl-agent-einvoice/src/sql/conn.rs:251 (run-time ladder), module/settings.rs:100 (`sql_source: site`) | khác | thấp: better (runtime step-down, reported) | none |
| Platform client retry (5 tries, 200 ms→5 s, 429/5xx) | MM-EINVOICE/internal/agentcollector/client.go:23, :140 | DevLibrary/crates/dl-agent-einvoice/src/platform.rs:123, :220 | có | thấp | none |
| Proxy (Go `ProxyFromEnvironment`) | MM-EINVOICE/internal/agentcollector/client.go:56 | DevLibrary/crates/dl-agent-einvoice/src/platform.rs:173 (`Proxy::FromEnv`) | có | thấp | none |
| Device replaced (401 `device_replaced`) → stop | MM-EINVOICE/internal/agentcollector/client.go:122; runner.go:556 | DevLibrary/crates/dl-agent-einvoice/src/platform.rs:133; module/mod.rs:316 | có | thấp | none |
| `GET /config` | MM-EINVOICE/internal/agentcollector/client.go:210 | DevLibrary/crates/dl-agent-einvoice/src/platform.rs:279 | có | thấp | none |
| `POST /invoices:push` | MM-EINVOICE/internal/agentcollector/client.go:232 | DevLibrary/crates/dl-agent-einvoice/src/platform.rs:294, :300 | có | thấp: 800 golden push items | none |
| `POST /trn-types` | MM-EINVOICE/internal/agentcollector/client.go:256 | DevLibrary/crates/dl-agent-einvoice/src/platform.rs:326 | có | thấp | none |
| `POST /poll` (write-back commands) | MM-EINVOICE/internal/agentcollector/client.go:270 | DevLibrary/crates/dl-agent-einvoice/src/platform.rs:347 | có | thấp | none |
| `POST /writeback:ack` | MM-EINVOICE/internal/agentcollector/client.go:288 | DevLibrary/crates/dl-agent-einvoice/src/platform.rs:374 | có | thấp | none |
| `POST /heartbeat` (10 s, no retry) | MM-EINVOICE/internal/agentcollector/client.go:328 | DevLibrary/crates/dl-agent-einvoice/src/platform.rs:388 | khác | thấp: `agent_version` = `dl-agent-<v>` instead of `b<N>-<sha8>` | none |
| Heartbeat log ring (WARN/ERROR + cycle lines) | MM-EINVOICE/internal/agentcollector/loghandler.go:130 | DevLibrary/crates/dl-agent-einvoice/src/log.rs:53, :138 | có | thấp | none |
| Log tee `agentcollector.log` (truncated per start) | MM-EINVOICE/cmd/agent/main.go:229 | DevLibrary/crates/dl-agent/src/cli.rs:379 (dl-log, 10 MB × 5) | khác | thấp | none |
| Run loop: first cycle at once, ticker | MM-EINVOICE/internal/agentcollector/runner.go:521 | DevLibrary/crates/dl-agent-einvoice/src/module/collector.rs:148 | có | thấp | none |
| `runCycle`: refresh config, cycle, log, heartbeat | MM-EINVOICE/internal/agentcollector/runner.go:556 | DevLibrary/crates/dl-agent-einvoice/src/runner.rs:602 | có | thấp | none |
| `refreshHubConfig` (collect-from, sources, recalc; stale streak 5) | MM-EINVOICE/internal/agentcollector/runner.go:421 | DevLibrary/crates/dl-agent-einvoice/src/runner.rs:502 | có | thấp | none |
| `RunOnce` with per-phase `CycleTimeout` | MM-EINVOICE/internal/agentcollector/runner.go:397 | DevLibrary/crates/dl-agent-einvoice/src/runner.rs:492 | có | thấp | none |
| `sourcesFor` (FO / POS / both, fail-open) | MM-EINVOICE/internal/agentcollector/runner.go:37 | DevLibrary/crates/dl-agent-einvoice/src/runner.rs:85 | có | thấp | none |
| ReadCycle (parallel FO/POS, de-dup, enqueue) | MM-EINVOICE/internal/agentcollector/runner.go:106 | DevLibrary/crates/dl-agent-einvoice/src/runner.rs:245 | có | thấp | none |
| FO charge-code drift guard | MM-EINVOICE/internal/agentcollector/driftcheck.go:238 | DevLibrary/crates/dl-agent-einvoice/src/sql.rs:671; sql/drift.rs:175 | có | thấp | none |
| `ListTrnTypes` (FO) | MM-EINVOICE/internal/agentcollector/collectsource_select.go:94 | DevLibrary/crates/dl-agent-einvoice/src/sql.rs:588 | có | thấp | none |
| `ProbeFOSchema` info line | MM-EINVOICE/internal/agentcollector/collectsource_select.go:125 | DevLibrary/crates/dl-agent-einvoice/src/sql.rs:639 | có | thấp | none |
| `SelectPendingCollect` (`sp_SelectPending*`) | MM-EINVOICE/internal/agentcollector/collectsource_select.go:20 | DevLibrary/crates/dl-agent-einvoice/src/sql.rs:237 | có | thấp | none |
| `FetchCollected` (FO / POS) | MM-EINVOICE/internal/agentcollector/collectsource_fetch.go:16 | DevLibrary/crates/dl-agent-einvoice/src/fetch.rs:18 | có | thấp | none |
| POS raw fetch | MM-EINVOICE/internal/agentcollector/collectsource_raw.go:73 | DevLibrary/crates/dl-agent-einvoice/src/sql.rs:274 | có | thấp | none |
| POS line names from `ItemDef.IName1` | MM-EINVOICE/internal/agentcollector/collectsource_raw.go:131; pos_item_names.go:48 | DevLibrary/crates/dl-agent-einvoice/src/sql.rs:348; profile.rs:31 | khác | trung: Go names every site from IName1; dl-agent only when hub `collect_profile.pos_item_name = iname1` (ADR 0021, An Lâm set by migration 0025) | verify An Lâm's `agent_config.collect_profile` is `iname1` on prod before switch |
| POS normalize (folio num, legal rate, apportion) | MM-EINVOICE/internal/agentcollector/pos_normalize.go:54, :329, :365 | DevLibrary/crates/dl-agent-einvoice/src/pos.rs:126; money.rs:16, :61 | có | thấp: golden | none |
| Tax recalc into SC (FO/POS) | MM-EINVOICE/internal/agentcollector/recalc_tax.go:28, :80, :113 | DevLibrary/crates/dl-agent-einvoice/src/money.rs:96; fo.rs:149; pos.rs:310 | có | thấp: golden | none |
| FO line VAT derivation | MM-EINVOICE/internal/agentcollector/collectsource_select.go:65 | DevLibrary/crates/dl-agent-einvoice/src/fo.rs:130 | có | thấp | none |
| FO detail snap / net-gross / tax apportion | MM-EINVOICE/internal/agentcollector/collectsource_folio.go:45, :54, :58 | DevLibrary/crates/dl-agent-einvoice/src/fo.rs:197, :173, :230 | có | thấp: golden | none |
| FO void-pair elimination | MM-EINVOICE/internal/agentcollector/fo_enrich.go:34 | DevLibrary/crates/dl-agent-einvoice/src/fo.rs:288 | có | thấp | none |
| FO POS-charge enrichment | MM-EINVOICE/internal/agentcollector/fo_enrich.go:103 | DevLibrary/crates/dl-agent-einvoice/src/fo.rs:339 | có | thấp | none |
| TCVN3 → Unicode | MM-EINVOICE/internal/agentcollector/tcvn3.go:75; fo_enrich.go:245 | DevLibrary/crates/dl-agent-einvoice/src/tcvn3.rs:99, :116 | có | thấp: golden 800 cases | none |
| Canonical payload hash / push item | MM-EINVOICE/internal/agentcollector/collect_canonical.go:14, :50 | DevLibrary/crates/dl-agent-einvoice/src/domain.rs:298, :469 | có | thấp: byte-identical | none |
| Durable outbox (enqueue/drain/remove/applied) | MM-EINVOICE/internal/agentcollector/outbox.go:38–120 | DevLibrary/crates/dl-agent-einvoice/src/outbox.rs:56–191 | khác | thấp: JSON file instead of bbolt | none |
| PushCycle (mark processed true/false, remove) | MM-EINVOICE/internal/agentcollector/runner.go:234 | DevLibrary/crates/dl-agent-einvoice/src/runner.rs:355 | có | thấp | none |
| `MarkCollected` | MM-EINVOICE/internal/agentcollector/collectsource_folio.go:141 | DevLibrary/crates/dl-agent-einvoice/src/sql.rs:479 | có | thấp | none |
| WritebackCycle (applied set, ack) | MM-EINVOICE/internal/agentcollector/runner.go:300 | DevLibrary/crates/dl-agent-einvoice/src/runner.rs:425 | có | thấp: applied set lost at switch, write-back idempotent | none |
| Write-back command mapping (unknown type refused) | MM-EINVOICE/internal/agentcollector/runner.go:369 | DevLibrary/crates/dl-agent-einvoice/src/runner.rs:642 | có | thấp | none |
| `ApplyWriteback` EXEC `sp_ApplyInvoiceWriteback` | MM-EINVOICE/internal/agentcollector/sqlsource.go:89, :120 | DevLibrary/crates/dl-agent-einvoice/src/sql.rs:508, :531 | có | thấp | none |
| Relay worker loop (204, 404/405 → 5 min, backoff 5→60 s) | MM-EINVOICE/internal/agentcollector/relay.go:116 | DevLibrary/crates/dl-agent-einvoice/src/relay/mod.rs:103 | có | thấp | none |
| Relay allow-list + perform (VNPT, 443, 3 headers, 60 s cap, 8 MiB) | MM-EINVOICE/internal/agentcollector/relay.go:156 | DevLibrary/crates/dl-agent-einvoice/src/relay/mod.rs:187; relay/allow.rs:52 | có | thấp | none |
| `/relay/next`, `/relay/{id}/result` | MM-EINVOICE/internal/agentcollector/relay.go:40, :65 | DevLibrary/crates/dl-agent-einvoice/src/platform.rs:445, :459 | có | thấp | none |
| `-selftest` (platform + SQL ping) | MM-EINVOICE/cmd/agent/main.go:297 | DevLibrary/crates/dl-agent-einvoice/src/module/selftest.rs:30; module/commands.rs:46 (`einvoice.selftest`) | khác | thấp: richer, hub command | none |
| `-once` | MM-EINVOICE/cmd/agent/main.go:160 | DevLibrary/crates/dl-agent-einvoice/src/module/commands.rs:46 (`einvoice.collect_now`) | khác | thấp | none |
| `-selfcheck` before swap | MM-EINVOICE/cmd/agent/main.go:44, :366 | DevLibrary/crates/dl-agent/src/cli_install.rs:515 (`verify_build`) | có | thấp | none |
| Self-update from platform (`/update/manifest`, `/update/binary`, SHA-256) | MM-EINVOICE/internal/agentcollector/selfupdate.go:196; selfupdate_client.go:61, :81 | DevLibrary/crates/dl-agent/src/agent_update.rs:50; update.rs:412 (signed manifest) | khác | thấp: platform update hints ignored by design | none |
| Rollback guard at start (bad SHA list) | MM-EINVOICE/internal/agentcollector/selfupdate.go:141 | DevLibrary/crates/dl-agent/src/update.rs:462 | có | thấp | none |
| `no-update` opt-out file | MM-EINVOICE/internal/agentcollector/selfupdate.go:42 (`<ProgramData>\MM-EInvoice\no-update`) | DevLibrary/crates/dl-agent/src/optout.rs:41 (`<dl-agent data>\no-update`) | khác | thấp: an old opt-out in the MM-EInvoice folder is not carried over | switch: copy the file if present (install.ps1) |
| Re-sealed `config.enc` picked up without restart (new) | — | DevLibrary/crates/dl-agent-einvoice/src/module/mod.rs:132 | khác | thấp | none |
| Dry run / write-back probe commands (new) | — | DevLibrary/crates/dl-agent-einvoice/src/module/commands.rs:197, :218 | khác | thấp | none |

### Lỗ hổng (E-Invoice)

1. **POS item names depend on the hub profile** — trung. dl-agent names POS lines from `ItemDef.IName1` only when the hub's `collect_profile` says `iname1`; the Go agent always did. ADR 0021 sets An Lâm to `iname1` (migration 0025). Work: read An Lâm's prod `agent_config.collect_profile` through the ops MCP before the switch; ≤ 15 min; needs MM-EINVOICE prod access (not DevLibrary).
2. **`no-update` opt-out not carried** — thấp. Work: install.ps1 copies `%ProgramData%\MM-EInvoice\no-update` into the dl-agent data dir at switch; 30 min; install.ps1 lives in the hub (`services/agent-hub`), so it touches the installer contract → not fixed here.
3. **Go service must be gone** — trung (double push, relay fight). Already handled by `-Replace` / bench `remove_legacy`; check on the canary.
4. Not a gap, a verification item: Windows SCM + Schannel + `config.enc` DPAPI of a real Go install on the SR bench (already in INVENTORY §4b).

## 2. PDAOrder SyncAgent (`SML-PDAOrder/apps/sql-sync-agent`, C#) → `dl-agent-pda-sync`

Read on both sides: the C# host, verbs, options, endpoints and SQL texts; the Rust module glue
(`module/*`), the host hooks (`hosting/host.rs`) and the ported parts named below. The crate is a
line-by-line port (golden tests against .NET 10 output, live SQL 2017/2022 tests, see
`DevLibrary/docs/plans/2026-11-19-pda-sync-plan.md` §1b and `-pda-sync-b-report.md`). Line numbers are
from branch `cloud/rt-0260-pb` after the fixes below.

| function | old agent (path:line) | dl-agent (path:line) | status | rủi ro tiền-dữ liệu | việc cần làm |
|---|---|---|---|---|---|
| **Service / platform** | | | | | |
| Windows service host (`AddWindowsService`, name `PdaSyncAgent`) | `SML-PDAOrder/apps/sql-sync-agent/Hosting/Cli.cs:157` | `DevLibrary/crates/dl-agent/src/host.rs` (core service) + `DevLibrary/crates/dl-agent-pda-sync/src/module/mod.rs:66` (module `pda-sync`, opt-in) | khác | thấp: one `dl-agent` service hosts the module | none (opt-in through hub `modules.pda-sync.enabled`) |
| SCM recovery (restart 5 s/15 s/60 s, reset 1 day) | `SML-PDAOrder/apps/sql-sync-agent/Bootstrap/EnrollWorkflow.cs:214`, `SML-PDAOrder/src/MM.PdaOrder.Api/wwwroot/downloads/install-sync-agent.ps1:292` | `DevLibrary/crates/dl-agent/src/host.rs` (dl-winsvc recovery, re-applied each start); port hook dropped (plan §1) | khác | thấp | none |
| Loop host: resilient loops, restart with capped backoff | `SML-PDAOrder/apps/sql-sync-agent/Hosting/AgentServices.cs:13` | `DevLibrary/crates/dl-agent-pda-sync/src/hosting/services.rs:54` (`run_resilient`), `DevLibrary/crates/dl-agent-pda-sync/src/hosting/host.rs:597` (one thread per loop) inside the module thread `module/mod.rs:118` | có | thấp | none |
| Loop stop timeout reported | (host shutdown timeout, .NET default) | `DevLibrary/crates/dl-agent-pda-sync/src/module/mod.rs:193` (`PDA_STOP_TIMEOUT` report) | khác (better) | thấp | none |
| Self-update loop (manifest `/downloads/manifest.json`, SHA-256, `apply-update`, rollback) | `SML-PDAOrder/apps/sql-sync-agent/Hosting/AgentServices.cs:126`, `Update/SelfUpdater.cs:115`, `Update/UpdateSource.cs:22`, `Update/ApplyUpdateCommand.cs:19` | `DevLibrary/crates/dl-agent/src/update.rs` (signed manifest, probation, rollback); module forces `AutoUpdate=false` at `DevLibrary/crates/dl-agent-pda-sync/src/module/settings.rs:50` | khác | thấp | none (PDA cloud must stop offering `PdaSyncAgent` manifests to migrated tenants: plan §3.4) |
| Local update opt-out `Agent:AutoUpdate=false` | `SML-PDAOrder/apps/sql-sync-agent/Config/AgentOptions.cs:49` | `DevLibrary/crates/dl-agent/src/optout.rs` (`<data>/no-update`) | khác | thấp | operator note: the opt-out file replaces the setting |
| Not-enrolled refusal (`status.json` `not_enrolled`, exit 1) | `SML-PDAOrder/apps/sql-sync-agent/Hosting/Cli.cs:146` | `DevLibrary/crates/dl-agent-pda-sync/src/module/mod.rs:87` (degraded "not enrolled", rechecks every 30 s, still answers commands) | khác (better) | thấp | none |
| Data dir `%ProgramData%\MM\SyncAgent` (`agent.json`, `secrets`, `outbox.db`, `capture.db`, `blobs.db`, `events.db`, `status.json`, `logs`) | `SML-PDAOrder/apps/sql-sync-agent/Config/AgentPaths.cs:4`, `Config/AgentOptions.cs:63` | `DevLibrary/crates/dl-agent-pda-sync/src/config/paths.rs:9` under `<data>/modules/pda-sync` (`module/settings.rs:49`) | khác | trung: old queues and shadow are not carried over (owner rule: fresh enrol, full resnapshot) | B8 only on owner decision |
| Static options `Agent:*` (HeartbeatSeconds, OutboxCapBytes, SiteTimeZone, UploadBatchesPerCycle, ParentPendingEscalateMinutes, BlobQueueCapBytes, UploadBlobsPerCycle, UploadBacklogAlertBatches, WebSocketFailuresBeforePoll, PrintRelay.*) | `SML-PDAOrder/apps/sql-sync-agent/Config/AgentOptions.cs:4` | `DevLibrary/crates/dl-agent-pda-sync/src/config/options.rs:73`, bound from hub `modules.pda-sync` at `DevLibrary/crates/dl-agent-pda-sync/src/module/settings.rs:32` | có | thấp | none |
| Site override file `appsettings.site.json` + env `Agent__*` + command line | `SML-PDAOrder/apps/sql-sync-agent/Hosting/Cli.cs:51`, `Config/AgentOptions.cs:61` | none: only the shipped `appsettings.json` and the hub section (`module/settings.rs:32`) | thiếu | cao: a site whose `SiteTimeZone` (naive POS datetimes → UTC) or `PrintRelay` lived only in `appsettings.site.json` loses it at cutover; wrong zone shifts every captured time | cutover step: read the old `appsettings.site.json` once (installer or `pda-enroll --import-site-settings`) and write it into the hub's `modules.pda-sync`; or a checklist item. Needs owner OK (plan says never read the old folder). Size S |
| Cloud-pushed runtime config (`openCheckPollMs`, `masterScanSeconds`, `closedRescanDays`, `historyMonths`, `tableAllowlist`) persisted | `SML-PDAOrder/apps/sql-sync-agent/Config/RuntimeConfigStore.cs:25`, `Config/AgentOptions.cs:68` | `DevLibrary/crates/dl-agent-pda-sync/src/config/runtime.rs:197` | có | thấp | none |
| Health record `status.json` (healthy / degraded / reenroll_required + code + hint) | `SML-PDAOrder/apps/sql-sync-agent/Config/AgentHealth.cs:43`, `Config/AgentStatus.cs:6` | `DevLibrary/crates/dl-agent-pda-sync/src/config/health.rs:147`, mirrored to module state + one report per change `DevLibrary/crates/dl-agent-pda-sync/src/module/mod.rs:214` | có | thấp | none |
| Error codes (`AgentErrorCodes`) | `SML-PDAOrder/apps/sql-sync-agent/Config/AgentErrors.cs:4` | `DevLibrary/crates/dl-agent-pda-sync/src/config/errors.rs:8` | có | thấp | none |
| Secrets: DPAPI LocalMachine + entropy `MM.PdaOrder.SyncAgent.v1` (seed, agent token, SQL password, PIN key) | `SML-PDAOrder/apps/sql-sync-agent/Security/ISecretStore.cs:54` | `DevLibrary/crates/dl-agent-pda-sync/src/security/mod.rs:122` (same entropy, `dl-winsvc` DPAPI) | có | thấp (Windows run only on bench B6) | B6 bench |
| Ed25519 agent key | `SML-PDAOrder/apps/sql-sync-agent/Security/AgentKey.cs:7` | `DevLibrary/crates/dl-agent-pda-sync/src/transport/signer.rs:51` (`dl-ed25519`) | có | thấp | none |
| **Enrolment / SQL bootstrap** | | | | | |
| `enroll --code --url [--dry-run] [--sql-instance] [--database] [--sql-admin-user]` | `SML-PDAOrder/apps/sql-sync-agent/Hosting/EnrollCommand.cs:61`, `Bootstrap/EnrollWorkflow.cs:45` | `DevLibrary/crates/dl-agent-pda-sync/src/module/tool.rs:36` (`dl-agent pda-enroll`), `DevLibrary/crates/dl-agent-pda-sync/src/bootstrap/workflow.rs:122` | có | thấp | none |
| SQL admin password from env / hidden prompt only | `SML-PDAOrder/apps/sql-sync-agent/Hosting/SecretPrompt.cs:13` | `DevLibrary/crates/dl-agent-pda-sync/src/hosting/secret_prompt.rs` (`PDA_SQL_ADMIN_PASSWORD`) | có | thấp | none |
| Enrol redeem `POST /sync/v1/enroll/redeem` | `SML-PDAOrder/apps/sql-sync-agent/Transport/EnrollmentClient.cs:17` | `DevLibrary/crates/dl-agent-pda-sync/src/transport/enrollment.rs:14` | có | thấp | none |
| Cloud probe `GET /health/ready` + clock skew warn 2 min / block 5 min | `SML-PDAOrder/apps/sql-sync-agent/Bootstrap/CloudProbe.cs:29` | `DevLibrary/crates/dl-agent-pda-sync/src/bootstrap/cloud_probe.rs:18`, `:145` | có | thấp | none |
| Local SQL discovery (registry, SQL Browser UDP 1434, `.`) | `SML-PDAOrder/apps/sql-sync-agent/Bootstrap/LocalSqlDiscovery.cs:13` | `DevLibrary/crates/dl-agent-pda-sync/src/bootstrap/discovery.rs:72` | có | thấp (registry part Windows-only, bench B6) | B6 |
| Preflight: SQL ≥ 2012 (major 11), schema fingerprint ≥ 0.85, Windows-only auth finding | `SML-PDAOrder/apps/sql-sync-agent/Bootstrap/SqlPreflight.cs:62`, `Bootstrap/SchemaFingerprint.cs:12` | `DevLibrary/crates/dl-agent-pda-sync/src/bootstrap/preflight.rs:173`, `DevLibrary/crates/dl-agent-pda-sync/src/bootstrap/fingerprint.rs:25` | có | thấp | none (SQL 2008 refused on both sides) |
| Bootstrap scripts 010–090 (`pda_sync` login, user, `Pda_SyncCommandLog`, `Pda_SyncState`, grants, order grants) | `SML-PDAOrder/apps/sql-sync-agent/Bootstrap/SqlBootstrapper.cs:23`, `:72` | `DevLibrary/crates/dl-agent-pda-sync/assets/sql/010_login.sql`…`090_order_grants_ow1e.sql`, `DevLibrary/crates/dl-agent-pda-sync/src/bootstrap/bootstrapper.rs:239` | có | thấp (live 2017/2022) | none |
| SQL admin connection (Windows login or SQL admin, TrustServerCertificate) | `SML-PDAOrder/apps/sql-sync-agent/Bootstrap/SqlServerAdmin.cs:22`, `:136` | `DevLibrary/crates/dl-agent-pda-sync/src/bootstrap/admin.rs:94` | có | thấp (SSPI only on bench) | B6 |
| Mixed-mode switch with consent (installer) | `SML-PDAOrder/src/MM.PdaOrder.Api/wwwroot/downloads/install-sync-agent.ps1:123` | none: `pda-enroll` only explains error 18456 (`DevLibrary/crates/dl-agent-pda-sync/src/bootstrap/probes.rs:38`) | thiếu | thấp: no data risk; operator must switch mixed mode by hand | add to the dl-agent install runbook, or an installer step for `-Setup pda-enroll`. Size S, needs hub install script |
| `pda_sync` login probe (reads `Pda_SyncCommandLog`, `Pda_SyncState`) | `SML-PDAOrder/apps/sql-sync-agent/Bootstrap/AgentLoginProbe.cs:22` | `DevLibrary/crates/dl-agent-pda-sync/src/bootstrap/probes.rs:32` | có | thấp | none |
| Agent SQL connection (`pda_sync`, Encrypt mandatory, TrustServerCertificate, 10 s) | `SML-PDAOrder/apps/sql-sync-agent/Bootstrap/AgentSqlConnection.cs:21` | `DevLibrary/crates/dl-agent-pda-sync/src/config/sql.rs:64` (`Encrypt::Required`) | có | thấp; no TLS 1.0 rung for SQL 2012 RTM without TLS 1.2 patch (same as C#) | none |
| `grants [--dry-run]` verb | `SML-PDAOrder/apps/sql-sync-agent/Hosting/GrantsCommand.cs:71` | `DevLibrary/crates/dl-agent-pda-sync/src/module/tool.rs:45` (`dl-agent pda-grants`) | có | thấp | none |
| Order grants monitor + self-repair under service identity (GR1, AG1) | `SML-PDAOrder/apps/sql-sync-agent/Commands/OrderGrantsMonitor.cs:21`, `:109`, `Commands/OrderGrantsRepair.cs:19` | `DevLibrary/crates/dl-agent-pda-sync/src/commands/grants.rs:112`, `:270`, `:42` | có | thấp | none |
| Installer: download zip, SHA-256, `%ProgramFiles%\MM\SyncAgent`, Defender exclusion, upgrade in place re-applying grants | `SML-PDAOrder/src/MM.PdaOrder.Api/wwwroot/downloads/install-sync-agent.ps1:58`, `:177`, `:207` | `DevLibrary/services/agent-hub/src/install/script.ts:27` (`-Setup`, `-Replace`) | khác | trung: no `pda-sync` product gate; re-run does not re-apply grants (operator runs `dl-agent pda-grants`) | B3: hub product requirement + `-Replace PdaSyncAgent` documented, grants re-applied on upgrade. Size M, hub repo part of DevLibrary |
| Install proves the service is healthy (`status --wait`) | `SML-PDAOrder/src/MM.PdaOrder.Api/wwwroot/downloads/install-sync-agent.ps1:431` | `DevLibrary/services/agent-hub/src/install/script.ts` (waits for the core heartbeat file, not the PDA heartbeat) | khác | thấp | selftest row `cloud ready` covers it after install |
| **Capture (SMILE_POS → cloud)** | | | | | |
| Capture loop (snapshot pass, open checks 2 s, masters 120 s, ledger tail, closed rescan) | `SML-PDAOrder/apps/sql-sync-agent/Capture/SqlCaptureRunner.cs:97`, `:16` | `DevLibrary/crates/dl-agent-pda-sync/src/capture/runner.rs:326`, `:58`; wired `DevLibrary/crates/dl-agent-pda-sync/src/hosting/host.rs:291` | có | thấp (live 2017/2022) | none |
| Capture options from runtime config + site time zone | `SML-PDAOrder/apps/sql-sync-agent/Capture/SqlCaptureRunner.cs:417` | `DevLibrary/crates/dl-agent-pda-sync/src/capture/runner.rs:226`, `DevLibrary/crates/dl-agent-pda-sync/src/hosting/host.rs:278` | có | trung: zone comes only from hub config (see override row) | see `appsettings.site.json` row |
| Capture failure classification (SQL down, busy, backpressure) | `SML-PDAOrder/apps/sql-sync-agent/Capture/CaptureFailure.cs:27`, `:49` | `DevLibrary/crates/dl-agent-pda-sync/src/capture/failure.rs:41`, `:67` | có | thấp | none |
| Skipped-table report every 5 min, rule counts | `SML-PDAOrder/apps/sql-sync-agent/Capture/SqlCaptureRunner.cs:345`, `:412` | `DevLibrary/crates/dl-agent-pda-sync/src/capture/runner.rs:50`, `:187` | có | thấp | none |
| Capture status tracker (per-stream lag, SQL reachable) | `SML-PDAOrder/apps/sql-sync-agent/Capture/CaptureStatusTracker.cs:13` | `DevLibrary/crates/dl-agent-pda-sync/src/capture/status.rs:76` | có | thấp | none |
| Read-your-writes nudge / upload wake (ADR 0104) | `SML-PDAOrder/apps/sql-sync-agent/Capture/CaptureNudge.cs:16`, `SqlCaptureRunner.cs:277` | `DevLibrary/crates/dl-agent-pda-sync/src/capture/nudge.rs:20` | có | thấp | none |
| Capture state `capture.db` (shadow hashes, cursors) | `SML-PDAOrder/apps/sql-sync-agent/Capture/SqlCaptureRunner.cs:63` | `DevLibrary/crates/dl-agent-pda-sync/src/capture/sqlite_state.rs` | có | trung: fresh enrol has empty shadow (rows closed in the gap keep their open-check rows in the cloud) | owner decision (b-report "Known limits" 1) |
| TCVN3 text normalisation | (contracts, `Unicode2TCVN` mapping in shared catalog) | `DevLibrary/crates/dl-agent-pda-sync/src/contracts/mapping/tcvn3.rs` | có | thấp (golden tests) | none |
| **Outbox / upload** | | | | | |
| SQLite outbox with 2 GB cap (`OUTBOX_FULL`, capture pauses) | `SML-PDAOrder/apps/sql-sync-agent/Outbox/SqliteOutbox.cs:23`, `:99` | `DevLibrary/crates/dl-agent-pda-sync/src/outbox/batches.rs:81` | có | thấp | none |
| Batch uploader `POST /sync/v1/batches` (8 per cycle, 401 re-token, 409 gap re-align, 424 parent pending) | `SML-PDAOrder/apps/sql-sync-agent/Outbox/BatchUploader.cs:43`, `:54`, `:96` | `DevLibrary/crates/dl-agent-pda-sync/src/outbox/batch_uploader.rs:38`, `:222`, `:290` | có | thấp | none |
| Parent-pending escalation (15 min → resnapshot) | `SML-PDAOrder/apps/sql-sync-agent/Outbox/ParentPendingTracker.cs:30` | `DevLibrary/crates/dl-agent-pda-sync/src/outbox/parent_pending.rs:53` | có | thấp | none |
| Stream resnapshot requests (7-day history) | `SML-PDAOrder/apps/sql-sync-agent/Outbox/SqliteOutbox.Resnapshots.cs:46`, `:97` | `DevLibrary/crates/dl-agent-pda-sync/src/outbox/resnapshots.rs:117`, `:172` | có | thấp | none |
| Key repairs queue (ADR 0126/0128) | `SML-PDAOrder/apps/sql-sync-agent/Outbox/SqliteOutbox.KeyRepairs.cs:11` | `DevLibrary/crates/dl-agent-pda-sync/src/outbox/key_repairs.rs` | có | thấp | none |
| Blob queue (512 MB cap) + `PUT /sync/v1/blobs/{sha}` + `POST /sync/v1/blobs/refs` | `SML-PDAOrder/apps/sql-sync-agent/Outbox/SqliteBlobOutbox.cs:18`, `Outbox/BlobUploader.cs:17`, `:40` | `DevLibrary/crates/dl-agent-pda-sync/src/outbox/blobs.rs:64`, `DevLibrary/crates/dl-agent-pda-sync/src/outbox/blob_uploader.rs:76`, `DevLibrary/crates/dl-agent-pda-sync/src/contracts/wire/blobs.rs:112` | có | thấp | none |
| Heartbeat `POST /sync/v1/heartbeat` every 15 s, applies `config` | `SML-PDAOrder/apps/sql-sync-agent/Outbox/HeartbeatSender.cs:38`, `Hosting/AgentServices.cs:92` | `DevLibrary/crates/dl-agent-pda-sync/src/outbox/heartbeat.rs:24`, `:121` | có | thấp | none |
| Agent version sent to the cloud | `SML-PDAOrder/apps/sql-sync-agent/Outbox/HeartbeatSender.cs:20` | `DevLibrary/crates/dl-agent-pda-sync/src/hosting/mod.rs:32` (`dl-agent/<ver>`) | khác | thấp: cloud readiness rule `AgentBuild` reads it as unknown | PDA cloud change (plan §3.3), other repo |
| PDA cloud link for the watchdog | (none in C#) | `DevLibrary/crates/dl-agent-pda-sync/src/module/mod.rs:172`, `DevLibrary/crates/dl-agent-pda-sync/src/reporting/cloud_link.rs` | khác (better) | thấp | none |
| **Transport** | | | | | |
| Signed requests (fresh nonce/timestamp per attempt), retry with jitter | `SML-PDAOrder/apps/sql-sync-agent/Transport/RequestSigner.cs:36`, `Transport/RetryPolicy.cs:20` | `DevLibrary/crates/dl-agent-pda-sync/src/transport/signer.rs:51`, `DevLibrary/crates/dl-agent-pda-sync/src/transport/retry.rs:75` | có | thấp | none |
| Access token `POST /sync/v1/token` (refresh, refresh-failure signal) | `SML-PDAOrder/apps/sql-sync-agent/Transport/TokenProvider.cs:52`, `:22` | `DevLibrary/crates/dl-agent-pda-sync/src/transport/token.rs:19`, `:97` | có | thấp | none |
| HTTP client: system proxy + default credentials, connect 20 s, 2 min | `SML-PDAOrder/apps/sql-sync-agent/Transport/SyncHttpClient.cs:37` | `DevLibrary/crates/dl-agent-pda-sync/src/transport/http.rs:80`, `:86` (core proxy choice: env, WinHTTP, Internet Options) | khác (fixed in this stream; WPAD/PAC and NTLM proxy auth not followed) | trung: a site behind an auto-config or NTLM proxy cannot reach the cloud | fixed `d573278` for the common cases; PAC/NTLM proxy needs `dl-http` support (core, size M) |
| **Cloud commands (exactly once)** | | | | | |
| Command channel: WebSocket `/sync/v1/commands/ws`, fallback long-poll after 3 failures, result `POST /sync/v1/commands/result` | `SML-PDAOrder/apps/sql-sync-agent/Commands/CommandChannel.cs:45`, `:54`, `:63` | `DevLibrary/crates/dl-agent-pda-sync/src/commands/channel.rs:31`, `:166`, `:224`, `:281` | có | thấp | none |
| Exactly-once journal `dbo.Pda_SyncCommandLog` (UPDLOCK, HOLDLOCK, one tx with the write) | `SML-PDAOrder/apps/sql-sync-agent/Commands/SqlSiteCommandStore.cs:21`, `:26` | `DevLibrary/crates/dl-agent-pda-sync/src/commands/sql_text.rs:14`, `DevLibrary/crates/dl-agent-pda-sync/src/commands/sql_store.rs:150` | có | cao if wrong (double order); live redelivery tests pass on 2017/2022 | none |
| Guarded run (schema guard, POS WIN change refusal) | `SML-PDAOrder/apps/sql-sync-agent/Commands/SiteCommandExecutor.cs:97`, `Commands/TableSchemaGuard.cs:18` | `DevLibrary/crates/dl-agent-pda-sync/src/commands/executor.rs:312`, `DevLibrary/crates/dl-agent-pda-sync/src/commands/schema_guard.rs:88` | có | cao if wrong; tested | none |
| `kds.line_state` | `SML-PDAOrder/apps/sql-sync-agent/Commands/SiteCommandExecutor.cs:91` | `DevLibrary/crates/dl-agent-pda-sync/src/commands/executor.rs:346` | có | trung (live) | none |
| `reservation.save` / `reservation.cancel` | `SML-PDAOrder/apps/sql-sync-agent/Commands/SiteCommandExecutor.cs:112`, `:125` | `DevLibrary/crates/dl-agent-pda-sync/src/commands/executor.rs:385`, `:400` | có | trung (live) | none |
| `order.check_open` / `order.item_post` / `order.send` / `order.item_void` | `SML-PDAOrder/apps/sql-sync-agent/Commands/SiteCommandExecutor.Orders.cs:12`, `:25`, `:43`, `:58` | `DevLibrary/crates/dl-agent-pda-sync/src/commands/executor.rs:443`, `:458`, `:487`, `:510` | có | cao (money): live on fixture vendor routines only | B6: run on a real SMILE_POS (`PostITrn`, `UpdateTmpInvoice` bodies) |
| `order.covers` / `order.combo_post` | `SML-PDAOrder/apps/sql-sync-agent/Commands/SiteCommandExecutor.Orders.cs:84`, `Commands/SiteCommandExecutor.Combo.cs:12` | `DevLibrary/crates/dl-agent-pda-sync/src/commands/executor.rs:550`, `:583` | có | cao (money): fixture only | B6 |
| `table.change` / `table.transfer` / `check.merge` / `items.move` | `SML-PDAOrder/apps/sql-sync-agent/Commands/SiteCommandExecutor.Tables.cs:28`, `:43`, `:62`, `:84` | `DevLibrary/crates/dl-agent-pda-sync/src/commands/executor.rs:635`, `:656`, `:696`, `:742` | có | cao (money): fixture only | B6 |
| Order/table SQL batches (OW1c/d/e) | `SML-PDAOrder/apps/sql-sync-agent/Commands/OrderCommandSql.Batches.cs:7`, `Commands/SqlOrderCommandUnit.cs:46`, `Commands/SqlTableCommandUnit.cs:32` | `DevLibrary/crates/dl-agent-pda-sync/src/commands/batches.rs`, `DevLibrary/crates/dl-agent-pda-sync/src/commands/sql_order.rs:160`, `DevLibrary/crates/dl-agent-pda-sync/src/commands/sql_table.rs:176` | có | cao if wrong; byte-identical golden tests | none |
| `sync.resnapshot` | `SML-PDAOrder/apps/sql-sync-agent/Commands/ResnapshotCommandHandler.cs:26` | `DevLibrary/crates/dl-agent-pda-sync/src/commands/resnapshot.rs:66` | có | thấp | none |
| `sync.verify` (manifest `/sync/v1/manifest`, `/manifest/keys`, key repair) | `SML-PDAOrder/apps/sql-sync-agent/Commands/VerifyCommandHandler.cs:84`, `Commands/CloudManifestSource.cs:40` | `DevLibrary/crates/dl-agent-pda-sync/src/commands/verify.rs:154`, `DevLibrary/crates/dl-agent-pda-sync/src/commands/verify_manifest.rs:102` | có | trung: open-check tables and `CashierRVC` never healed (C# limit) | owner decision |
| Unknown command type → reject | `SML-PDAOrder/apps/sql-sync-agent/Commands/SiteCommandExecutor.cs:88` | `DevLibrary/crates/dl-agent-pda-sync/src/commands/executor.rs:261` | có | thấp | none |
| **Staff sync** | | | | | |
| Staff loop (first after 30 s, every 2 min) | `SML-PDAOrder/apps/sql-sync-agent/Staff/StaffSyncHosting.cs:16`, `:17` | `DevLibrary/crates/dl-agent-pda-sync/src/staff/hosting.rs:18`, `:20` | có | thấp | none |
| `dbo.CashierDef` read (SELECT only), PIN hash with tenant key, `POST /sync/v1/staff`, `/sync/v1/staff/pin-key` | `SML-PDAOrder/apps/sql-sync-agent/Staff/SqlStaffSource.cs:15`, `Staff/StaffPinHasher.cs:11`, `Staff/PinKeyProvider.cs:11`, `Staff/StaffCloudClient.cs:47` | `DevLibrary/crates/dl-agent-pda-sync/src/staff/source.rs:26`, `DevLibrary/crates/dl-agent-pda-sync/src/staff/pin_hasher.rs:14`, `DevLibrary/crates/dl-agent-pda-sync/src/staff/pin_key.rs:15`, `DevLibrary/crates/dl-agent-pda-sync/src/contracts/wire/staff_sync.rs:137` | có | trung (PIN data; live `staff_sql`) | none |
| **Print relay (ADR 0098/0105)** | | | | | |
| Order hub `/hubs/order`, `JoinPrintRelay`, reconnect | `SML-PDAOrder/apps/sql-sync-agent/PrintRelay/PrintRelayService.cs:100`, `:118` | `DevLibrary/crates/dl-agent-pda-sync/src/printrelay/service.rs:142`, `:364`, `:468` | có | trung: no real printer yet | B7 |
| Relay engine, crash-safe journal, acks (1 s→30 s) | `SML-PDAOrder/apps/sql-sync-agent/PrintRelay/PrintRelayEngine.cs:112`, `PrintRelay/PrintRelayEngine.Acks.cs:9`, `PrintRelay/PrintJournal.cs:41` | `DevLibrary/crates/dl-agent-pda-sync/src/printrelay/engine.rs:321`, `DevLibrary/crates/dl-agent-pda-sync/src/printrelay/journal.rs:30` | có | trung: a lost journal could reprint a ticket | B7 |
| Target policy (private CIDRs, ports 9100) | `SML-PDAOrder/apps/sql-sync-agent/PrintRelay/PrinterTargetPolicy.cs:16`, `PrintRelay/PrintRelayOptions.cs:18`, `:21` | `DevLibrary/crates/dl-agent-pda-sync/src/printrelay/policy.rs:130`, `DevLibrary/crates/dl-agent-pda-sync/src/config/options.rs:40` | có | thấp | none |
| Raw TCP 9100 printer | `SML-PDAOrder/apps/sql-sync-agent/PrintRelay/RawTcpPrinter.cs:29` | `DevLibrary/crates/dl-agent-pda-sync/src/printrelay/raw_tcp.rs` | có | thấp | B7 |
| Windows spooler RAW printing | `SML-PDAOrder/apps/sql-sync-agent/PrintRelay/WindowsSpoolerPrinter.cs:7` | `DevLibrary/crates/dl-agent-pda-sync/src/printrelay/spooler.rs:278` | có | thấp (Windows-only) | B7 |
| Print queue discovery + `POST /sync/v1/print/queues` (≥ 5 min) | `SML-PDAOrder/apps/sql-sync-agent/PrintRelay/PrintQueueDiscovery.cs:13`, `PrintRelay/PrintQueueReporter.cs:17`, `PrintRelay/PrintRelayService.cs:141` | `DevLibrary/crates/dl-agent-pda-sync/src/printrelay/discovery.rs:298`, `:381`, `DevLibrary/crates/dl-agent-pda-sync/src/printrelay/service.rs:452` | có | thấp | B6/B7 |
| Order hub proxy | `SML-PDAOrder/apps/sql-sync-agent/PrintRelay/PrintRelayService.cs:100` (SignalR client, system proxy) | `DevLibrary/crates/dl-agent-pda-sync/src/printrelay/service.rs:173` | khác (fixed `d573278`) | trung (same as transport row) | none left for env/WinHTTP/IE proxies |
| **Reporting / logs / diagnostics** | | | | | |
| Service log `logs/agent-*.log` JSON lines, secret scrubber | `SML-PDAOrder/apps/sql-sync-agent/Reporting/AgentLogging.cs:37`, `Reporting/SecretScrubber.cs:11` | `DevLibrary/crates/dl-agent-pda-sync/src/reporting/agent_logging.rs:88`, `DevLibrary/crates/dl-agent-pda-sync/src/reporting/scrubber.rs:197` | có | thấp | none |
| Site events: durable `events.db` (5000), log bridge, flusher `POST /sync/v1/events` every 15 s | `SML-PDAOrder/apps/sql-sync-agent/Reporting/SiteEventStore.cs:32`, `Reporting/SiteEventLogBridge.cs:17`, `Reporting/SiteEventFlusher.cs:17`, `Reporting/SiteEventService.cs:41` | `DevLibrary/crates/dl-agent-pda-sync/src/reporting/site_event_store.rs:172`, `DevLibrary/crates/dl-agent-pda-sync/src/reporting/site_event_bridge.rs:155`, `DevLibrary/crates/dl-agent-pda-sync/src/reporting/site_events.rs:462`, `:629`, `DevLibrary/crates/dl-agent-pda-sync/src/reporting/site_event_service.rs:29` | có | thấp | none |
| Site health monitor (UPLOAD_BACKLOG, TOKEN_REFRESH_FAILING, OUTBOX_FULL) | `SML-PDAOrder/apps/sql-sync-agent/Reporting/SiteHealthMonitor.cs:13` | `DevLibrary/crates/dl-agent-pda-sync/src/reporting/site_health.rs:25`, `:55` | có | thấp | none |
| Error events also as dl-agent problem reports | (none) | `DevLibrary/crates/dl-agent-pda-sync/src/reporting/module_reports.rs`, `DevLibrary/crates/dl-agent-pda-sync/src/module/mod.rs:131` | khác (better) | thấp | none |
| `diag [--out zip]`: status, identity, runtime config, newest 7 logs per kind, summary (queue depths, free disk) | `SML-PDAOrder/apps/sql-sync-agent/Hosting/Cli.cs:64`, `Diagnostics/DiagnosticBundle.cs:17` | core `collect_diagnostics` + `DevLibrary/crates/dl-agent-pda-sync/src/module/mod.rs:80` (status, runtime config, `logs/` folder) | khác (logs fixed `4d31827`; `agent.json` and the summary not in the bundle, queue depths are module metrics) | thấp | none |
| `status [--wait]` verb | `SML-PDAOrder/apps/sql-sync-agent/Hosting/StatusCommand.cs:20` | hub command `status` `DevLibrary/crates/dl-agent-pda-sync/src/module/commands.rs:33`; port verb `DevLibrary/crates/dl-agent-pda-sync/src/hosting/status.rs:26` not exposed as a dl-agent tool | khác | thấp | none (core `status` / selftest) |
| Hub commands `selftest`, `capture_dry_run` | (none) | `DevLibrary/crates/dl-agent-pda-sync/src/module/commands.rs:35`, `:37`, `DevLibrary/crates/dl-agent-pda-sync/src/module/selftest.rs:27` | khác (better) | thấp | H1: long `capture_dry_run` on big sites (core) |
| **Legacy verbs (ADR 0085/0089/0095)** | | | | | |
| `legacy-export --from-sql` (reads `dbo.Pda_*` in SMILE_POS) | `SML-PDAOrder/apps/sql-sync-agent/Hosting/Cli.cs:34`, `LegacyExport/LegacyExportCommand.cs` | `DevLibrary/crates/dl-agent-pda-sync/src/hosting/cli.rs:184` (port table entry, no dl-agent tool); contracts at `DevLibrary/crates/dl-agent-pda-sync/src/contracts/legacy/sql_store.rs` | thiếu | trung: a store still on the IIS PDA site cannot be exported from a dl-agent-only machine | B4 (size M); until then keep the old exe for the one-off export |
| `legacy-archive` (writes `PDA_LEGACY_ARCHIVE` DB) | `SML-PDAOrder/apps/sql-sync-agent/Hosting/Cli.cs:40`, `LegacyArchive/LegacyArchiver.cs` | none | thiếu (out of scope, plan) | thấp | none (owner: out) |
| `legacy-upload` (`PUT /sync/v1/legacy/import/{sha}`) | `SML-PDAOrder/apps/sql-sync-agent/Hosting/Cli.cs:46`, `LegacyUpload/LegacyUploadClient.cs:38` | wire contract only `DevLibrary/crates/dl-agent-pda-sync/src/contracts/wire/legacy_import.rs:11` | thiếu (out of scope, plan) | thấp | none (owner: out) |

### Lỗ hổng — PDAOrder SyncAgent

| # | gap | risk | work | size | depends on |
|---|---|---|---|---|---|
| 2.1 | Site overrides in `%ProgramData%\MM\SyncAgent\appsettings.site.json` (and `Agent__*` env) are not read; `SiteTimeZone`, `PrintRelay`, intervals set only there are lost at cutover | cao (wrong zone shifts captured times; relay could switch on/off) | cutover checklist + a one-shot import into the hub `modules.pda-sync` section (installer or `pda-enroll` flag) | S | owner OK to read the old folder once; hub install script |
| 2.2 | Order/covers/combo/table verbs verified only on fixture stand-ins of the vendor procs | cao (money) | B6 bench on a real SMILE_POS copy | M | Windows bench, PDA demo tenant |
| 2.3 | Open-check rows of checks closed while no agent ran stay open in the cloud; `CashierRVC` deletions never healed | trung | owner decision (C# limit); cutover with all checks closed | M | PDA cloud + owner |
| 2.4 | `pda-sync` product gate, grants re-applied on upgrade, old-agent removal (`-Replace PdaSyncAgent`, rename data folder) in the hub installer | trung | B3 | M | agent-hub install script |
| 2.5 | Mixed-mode switch with consent (old installer) not done by dl-agent | thấp | runbook step or installer step | S | hub install script |
| 2.6 | `legacy-export --from-sql` not ported | trung (only stores still on IIS PDA) | B4 | M | none |
| 2.7 | Proxy: WPAD/PAC auto-config and NTLM/Negotiate proxy auth (C# `GetSystemWebProxy` + `DefaultCredentials`) | trung | `dl-http` PAC/NTLM support (core) | M | core |
| 2.8 | PDA cloud: accept `dl-agent/<ver>` build, hide `irm …/agent` hints and old manifests for migrated tenants | thấp | PDA cloud change (plan §3) | S | SML-PDAOrder repo |
| 2.9 | Windows run (DPAPI, SChannel, SSPI admin, spooler, registry discovery) and real printer (B6/B7) | trung | bench | M | SR bench |
| 2.10 | Logs missing from the diagnostics bundle | thấp | **fixed** `4d31827` | — | — |
| 2.11 | Proxy from env only (WinHTTP / Internet Options proxy ignored) | trung | **fixed** `d573278` | — | — |

## 3. Speaker (`SML-PDAOrder/apps/speaker-agent`, Go) → `dl-agent-speaker`

| function | old agent (path:line) | dl-agent (path:line) | status | rủi ro tiền-dữ liệu | việc cần làm |
|---|---|---|---|---|---|
| Windows service `MMSpeakerAgent` (auto start) | `SML-PDAOrder/apps/speaker-agent/main.go:155`, `SML-PDAOrder/apps/speaker-agent/internal/service/service_windows.go:43`, `SML-PDAOrder/web/public/install-speaker.ps1:91` | core service + `DevLibrary/crates/dl-agent-speaker/src/module/mod.rs:44` (module `speaker`, opt-in) | khác | thấp | none |
| Service recovery | none in `install-speaker.ps1:91` (no `sc failure`) | core SCM recovery + supervisor | khác (better) | thấp | none |
| Console mode `-config <path>` | `SML-PDAOrder/apps/speaker-agent/main.go:147` | core `dl-agent run` | khác | thấp | none |
| Installer: download exe, SHA-256 from manifest, TLS 1.2, write config without BOM | `SML-PDAOrder/web/public/install-speaker.ps1:38`, `:54`, `:83` | `DevLibrary/services/agent-hub/src/install/script.ts` (dl-agent installer) | khác | thấp | none |
| Config `config.json` (`apiUrl`, `token`, `rvcList`) | `SML-PDAOrder/apps/speaker-agent/internal/config/config.go:15`, `:30` | hub `modules.speaker.rvcList`, `soundRefreshSeconds` `DevLibrary/crates/dl-agent-speaker/src/module/settings.rs:32`; cloud URL from the pda-sync enrolment `DevLibrary/crates/dl-agent-speaker/src/auth.rs:66` | khác | thấp: `rvcList` must be typed into the hub per site, the old `config.json` is not read | cutover checklist (or import `rvcList` once from `%ProgramData%\MM.SpeakerAgent\config.json`) |
| Credential: 365-day speaker-device JWT in plaintext `config.json` | `SML-PDAOrder/apps/speaker-agent/internal/config/config.go:17`, `SML-PDAOrder/web/public/install-speaker.ps1:70` | pda-sync identity → `POST /sync/v1/token` 15-min tokens `DevLibrary/crates/dl-agent-speaker/src/auth.rs:66`, `:99` | khác (by design) | cao (operations): the PDA cloud refuses a SyncAgent in `JoinRvc` and on `speaker/sound-mode` today, so nothing plays after the switch | PDA cloud change (crate README "Cloud change needed"), other repo |
| Speaker on a machine without SQL Server / without `pda-sync` enrolment | `SML-PDAOrder/apps/speaker-agent/main.go:38` (needs only `config.json`) | `DevLibrary/crates/dl-agent-speaker/src/module/mod.rs:82` (degraded until `pda-enroll` ran on the same machine) | thiếu | cao (operations): a kitchen/front-desk PC with only the speaker cannot run it without enrolling pda-sync (which needs SQL admin on SMILE_POS) | design decision: a speaker-only enrolment (device code on the PDA cloud, or pda-sync identity without SQL bootstrap). Size M, PDA cloud + DevLibrary |
| SignalR negotiate `/hubs/order/negotiate?negotiateVersion=1` + WebSocket | `SML-PDAOrder/apps/speaker-agent/internal/signalrclient/client.go:230`, `:210` | `DevLibrary/crates/dl-agent-speaker/src/hub.rs:200` (`dl-signalr`, negotiate on) | có | thấp | none |
| `JoinRvc(rvc)` per RVC | `SML-PDAOrder/apps/speaker-agent/internal/signalrclient/client.go:115` (fire and forget) | `DevLibrary/crates/dl-agent-speaker/src/hub.rs:240` (each awaited; refusal per RVC) | khác (better) | thấp | none |
| Reconnect schedule 0/2/5/10/30 s, reset after 30 s healthy | `SML-PDAOrder/apps/speaker-agent/internal/signalrclient/backoff.go:7`, `client.go:85` | `DevLibrary/crates/dl-agent-speaker/src/hub.rs:53`, `:26` | có | thấp | none |
| Keep-alive ping 15 s | `SML-PDAOrder/apps/speaker-agent/internal/signalrclient/invoke.go:35`, `:143` | `dl-signalr` default 15 s (`DevLibrary/crates/dl-signalr/src/options.rs:40`) | có | thấp | none |
| Hub/sound-mode HTTP proxy (Go: env only) | `SML-PDAOrder/apps/speaker-agent/internal/signalrclient/client.go:21` | `DevLibrary/crates/dl-agent-speaker/src/hub.rs:45` (core proxy choice, superset of env) | khác (aligned with pda-sync in `d573278`) | thấp | none |
| 401 handling | `SML-PDAOrder/apps/speaker-agent/internal/signalrclient/client.go:243` (backoff only; expired JWT = silent forever) | `DevLibrary/crates/dl-agent-speaker/src/hub.rs:284` (drops token, new exchange) | khác (better) | thấp | none |
| `GuestOrderRequested(rvc, table)` → `order` | `SML-PDAOrder/apps/speaker-agent/main.go:91`, `:122` | `DevLibrary/crates/dl-agent-speaker/src/decision.rs:18`, `:88` | có | thấp | none |
| `GuestServiceRequested(rvc, table, id, type)` → bill / charge / waiter | `SML-PDAOrder/apps/speaker-agent/main.go:95`, `:136` | `DevLibrary/crates/dl-agent-speaker/src/decision.rs:20`, `:55` | có | thấp | none |
| Garbled argument logged, alert still plays | `SML-PDAOrder/apps/speaker-agent/main.go:122` | `DevLibrary/crates/dl-agent-speaker/src/decision.rs:65`, `:76` | có | thấp | none |
| `ForceUninstall` → self-uninstall | `SML-PDAOrder/apps/speaker-agent/main.go:103`, `SML-PDAOrder/apps/speaker-agent/internal/service/uninstall_windows.go` | `DevLibrary/crates/dl-agent-speaker/src/hub.rs:215` (logged, ignored) | khác (by design) | thấp | none (disable `modules.speaker` from the hub) |
| Sound mode `GET /api/v1/speaker/sound-mode/{rvc}` at start + every 2 min, last good kept | `SML-PDAOrder/apps/speaker-agent/main.go:45`, `:60`, `SML-PDAOrder/apps/speaker-agent/internal/soundconfig/soundconfig.go:130` | `DevLibrary/crates/dl-agent-speaker/src/fetch.rs:15`, `:49`, `:74`, `DevLibrary/crates/dl-agent-speaker/src/module/mod.rs:143` (refresh configurable ≥ 30 s; first fetch runs beside the hub connect) | có | thấp | none |
| Mode rules (tone null/"" = off, voice, lang, default `guest`) | `SML-PDAOrder/apps/speaker-agent/internal/soundconfig/soundconfig.go:54`, `:64` | `DevLibrary/crates/dl-agent-speaker/src/mode.rs:66`, `:75`, `:131` | có | thấp | none |
| Per-RVC cache, default for unknown RVC | `SML-PDAOrder/apps/speaker-agent/internal/soundconfig/soundconfig.go:91` | `DevLibrary/crates/dl-agent-speaker/src/mode.rs:194` | có | thấp | none |
| Embedded WAV tones (11) | `SML-PDAOrder/apps/speaker-agent/internal/player/player.go:48` | `DevLibrary/crates/dl-agent-speaker/src/tones.rs:7` | có | thấp | none |
| Tone playback (winmm `PlaySoundA`, sync) | `SML-PDAOrder/apps/speaker-agent/internal/player/player_windows.go:26` | `DevLibrary/crates/dl-agent-speaker/src/sapi.rs:60` (SAPI `SpFileStream` from `<data>/modules/speaker/tones`) | khác | thấp: real audio only checked by hand on Windows | site run on a speaker PC |
| Phrase vi/en (`BuildPhrase`, `NormalizeLang`) | `SML-PDAOrder/apps/speaker-agent/internal/tts/tts.go:53`, `:25` | `DevLibrary/crates/dl-agent-speaker/src/phrase.rs:50`, `:12` | có | thấp | none |
| Speech: PowerShell `System.Speech`, first voice of the language culture | `SML-PDAOrder/apps/speaker-agent/internal/tts/tts.go:74` | `DevLibrary/crates/dl-agent-speaker/src/sapi.rs:41`, `:135`, LCIDs `DevLibrary/crates/dl-agent-speaker/src/phrase.rs:23` | khác (in-process SAPI; English now any culture, fixed `a199e7c`) | thấp | site run |
| Announcements serialised, tone then speech | `SML-PDAOrder/apps/speaker-agent/main.go:72` (in the event handler) | `DevLibrary/crates/dl-agent-speaker/src/output.rs:82`, `:157` (one worker, queue 32, overflow dropped and counted) | khác (better) | thấp | none |
| Print relay when `printerSubnets` set (TCP 9100, file queue, policy, acks) | `SML-PDAOrder/apps/speaker-agent/printrelay.go:17`, `SML-PDAOrder/apps/speaker-agent/internal/printrelay/relay.go:61`, `internal/printrelay/queue.go:57`, `internal/printrelay/policy.go:49` | not in speaker (`DevLibrary/crates/dl-agent-speaker/src/hub.rs:12`); the branch relay is `pda-sync` (`DevLibrary/crates/dl-agent-pda-sync/src/printrelay/service.rs:560`) | khác (by design, ADR 0026) | trung: a branch whose printers are reachable only from the speaker PC's LAN loses relayed printing unless pda-sync runs on that LAN | cutover check per branch: the pda-sync machine must reach the printers; else keep browser printing |
| Logs (Go `log.Printf` to stderr; lost in service mode) | `SML-PDAOrder/apps/speaker-agent/main.go:40` | `dl-log` via core | khác (better) | thấp | none |
| Hub commands `status`, `test` | (none) | `DevLibrary/crates/dl-agent-speaker/src/module/commands.rs:32`, `:42` | khác (better) | thấp | none |
| Join refusal report | (none) | `DevLibrary/crates/dl-agent-speaker/src/module/mod.rs:233` (`SPEAKER_JOIN_REFUSED`) | khác (better) | thấp | none |

### Lỗ hổng — Speaker

| # | gap | risk | work | size | depends on |
|---|---|---|---|---|---|
| 3.1 | PDA cloud refuses the SyncAgent principal in `OrderHub.JoinRvc` and on `GET /api/v1/speaker/sound-mode/{rvc}`: no alert plays after the switch | cao (operations; no money) | PDA cloud change described in the crate README | S–M | SML-PDAOrder repo |
| 3.2 | Speaker-only machines (no SQL, no pda-sync enrolment) cannot run the module | cao (operations) | speaker-only enrolment design (device code or identity without SQL bootstrap) | M | owner + PDA cloud |
| 3.3 | Print relay of branches that relied on the speaker PC (`printerSubnets`) | trung | per-branch check that the pda-sync host reaches the printers | S | site survey |
| 3.4 | `rvcList` not imported from the old `config.json` | thấp | cutover checklist or one-shot import | S | owner OK to read the old file |
| 3.5 | Real audio (SAPI voices, tone stream) unverified | thấp | site run | S | Windows speaker PC |
| 3.6 | English voice only for LCID 409/809 (Go took any English culture) | thấp | **fixed** `a199e7c` | — | — |
| 3.7 | Hub and sound-mode calls on env proxy while the token call used the core proxy | thấp | **fixed** `d573278` | — | — |

## 4. PMS site agent (`SmilePmsAgent`, SMILE-PMS `apps/site-agent`) → `dl-agent-pms` (`pms-site`)

Old side: the Go agent was removed from SMILE-PMS in `1297a55` (RT-0220); every old path below is read at
`1297a55^` (`git show 1297a55^:apps/site-agent/...`). The Rust port that lived beside it (`apps/site-agent/rust`)
is what `dl-agent-pms` was copied from; the Go agent is the behavioural reference. The SMILE-only contract
(SMILE-PMS `docs/decisions/2026-11-19-site-agent-smile-only-contract.md`) retired migrations, `MM_OpLog`,
retention and the schema gate on purpose; those rows are "khác (ADR)", not gaps.

| function | old agent (path:line) | dl-agent (path:line) | status | rủi ro tiền-dữ liệu | việc cần làm |
|---|---|---|---|---|---|
| CLI `run` (service or console) | SMILE-PMS/apps/site-agent/cmd/pms-agent/main.go:56 | DevLibrary/crates/dl-agent-pms/src/module/mod.rs:121 (module `run`), core host | khác: a dl-agent module, not its own process | thấp: none | none |
| CLI `install` (ACL state dir + config, LocalSystem, delayed auto start) | SMILE-PMS/apps/site-agent/cmd/pms-agent/main.go:88; internal/svc/svc_windows.go:29 | DevLibrary/crates/dl-agent/src/cli_install.rs:216 (core service); ACL by DevLibrary/crates/dl-agent-pms/src/agent.rs:143 at every start | khác: core installs `dl-agent`, module restricts state dir on start | thấp: none | none |
| SCM recovery 5 s / 30 s / 60 s, reset 24 h, failureflag | SMILE-PMS/apps/site-agent/internal/svc/svc_windows.go:18 | DevLibrary/crates/dl-winsvc/src/scm.rs:398 (core, re-applied each start) | có (core) | thấp | Windows bench check (INVENTORY §4) |
| CLI `uninstall` | SMILE-PMS/apps/site-agent/internal/svc/svc_windows.go:60 | core `dl-agent uninstall` (DevLibrary/crates/dl-agent/src/cli_uninstall.rs) | có (core) | thấp | switch-over step 2 in crate README removes `SmilePmsAgent` |
| CLI `version` | SMILE-PMS/apps/site-agent/cmd/pms-agent/main.go:53 | core `dl-agent --version`; `hello.agent_version` = DevLibrary/crates/dl-agent-pms/src/module/mod.rs:87 | khác: `dl-agent-<ver>` clipped to 32 chars | thấp | none |
| CLI `migrate -bundle` (signed bundle → MM_PMS) | SMILE-PMS/apps/site-agent/cmd/pms-agent/migrate.go (whole file) | DevLibrary/crates/dl-agent-pms/src/tools/migrate.rs:34 (prints "retired", exits config code) | khác (ADR: no DDL on site) | thấp: retired by owner | none |
| Stop policy: superseded / revoked / contract → exit 0, no restart | SMILE-PMS/apps/site-agent/internal/agent/agent.go:137,150 | DevLibrary/crates/dl-agent-pms/src/agent.rs:98,111; module parks degraded DevLibrary/crates/dl-agent-pms/src/module/mod.rs:233 | khác: module parks `degraded` + critical report, resumes on config/identity change or `pms.reconnect` | thấp | none |
| Config file + `PMS_AGENT_*` env, unknown keys refused | SMILE-PMS/apps/site-agent/internal/config/config.go:94 | DevLibrary/crates/dl-agent-pms/src/config.rs:214 | có | thấp | none |
| `cloud_url` must be https (http only loopback) | SMILE-PMS/apps/site-agent/internal/config/config.go:142 | DevLibrary/crates/dl-agent-pms/src/config.rs:318 | có | thấp | none |
| Default config path / state dir | SMILE-PMS/apps/site-agent/internal/config/config.go:71,82 | DevLibrary/crates/dl-agent-pms/src/config.rs:75,89; module key override DevLibrary/crates/dl-agent-pms/src/module/settings.rs:39 | có (+ `config_path`/`state_dir` module keys) | thấp | none |
| Scrub enrollment code after enrol | SMILE-PMS/apps/site-agent/internal/config/config.go:171 | DevLibrary/crates/dl-agent-pms/src/config.rs:339 | có | thấp | none |
| New site: write code into config (no tool in Go; edit by hand) | SMILE-PMS/apps/site-agent/README.md (Run section, notepad) | DevLibrary/crates/dl-agent-pms/src/tools/enroll.rs:106 (`pms-enroll`, code from arg or `DL_AGENT_PMS_CODE`) | khác: better (tool, atomic 0600 write) | thấp | none |
| Ed25519 key, DPAPI machine scope + entropy, 0600 off Windows | SMILE-PMS/apps/site-agent/internal/keystore/keystore.go:64; protect_windows.go:37 | DevLibrary/crates/dl-agent-pms/src/keystore.rs:29,75 (same entropy) | có | thấp: same key file, no re-enrol | none |
| `identity.json` save / load | SMILE-PMS/apps/site-agent/internal/keystore/keystore.go:111,127 | DevLibrary/crates/dl-agent-pms/src/keystore.rs (Store) | có | thấp | none |
| State dir + config protected DACL (SYSTEM + Admins), repair + `agent.state.acl_repaired` | SMILE-PMS/apps/site-agent/internal/acl/acl.go:52; acl_windows.go:24 | DevLibrary/crates/dl-agent-pms/src/acl.rs:69,113; DevLibrary/crates/dl-agent-pms/src/agent.rs:143 | có | thấp | Windows bench check |
| Enrol `POST /agent/v1/enroll` + hostInfo; 401 stops; 429/net retried | SMILE-PMS/apps/site-agent/internal/enroll/client.go:102; internal/agent/agent.go:187 | DevLibrary/crates/dl-agent-pms/src/enroll.rs:266; DevLibrary/crates/dl-agent-pms/src/agent.rs:284 | có | thấp | none |
| Signed `POST /agent/v1/token` (ts, nonce, Ed25519 over canonical request) | SMILE-PMS/apps/site-agent/internal/enroll/client.go:122,147,165 | DevLibrary/crates/dl-agent-pms/src/enroll.rs:144,153,297 | có | thấp | none |
| JWT cache, refresh 2 min before expiry, invalidate on 401 | SMILE-PMS/apps/site-agent/internal/enroll/tokens.go:23,38 | DevLibrary/crates/dl-agent-pms/src/enroll.rs:37,343,358 | có | thấp | none |
| Proxy `HTTPS_PROXY`/`NO_PROXY`, CONNECT tunnel, TLS ≥ 1.2, HTTP/1.1 | SMILE-PMS/apps/site-agent/internal/conn/client.go:134 | DevLibrary/crates/dl-agent-pms/src/enroll.rs:116 (dl-http, `Proxy::FromEnv`) | có | thấp | optional: dl-agent proxy choice (WinHTTP) like pms-device fix |
| WSS stream `/agent/v1/stream`, `hello` first | SMILE-PMS/apps/site-agent/internal/conn/client.go:205,301 | DevLibrary/crates/dl-agent-pms/src/conn.rs:299,341 | khác: `schema_version: 0` + `ops` caps (ADR §3) | thấp | none |
| Ping → pong same `ts`; 35 s silence → reconnect | SMILE-PMS/apps/site-agent/internal/conn/client.go:314,343 | DevLibrary/crates/dl-agent-pms/src/conn.rs:123,242 (10 s × 3 + 5 s) | có | thấp | none |
| `cfg` frame logged | SMILE-PMS/apps/site-agent/internal/conn/client.go:363 | DevLibrary/crates/dl-agent-pms/src/conn.rs:456 | có | thấp | none |
| `update` frame ignored | SMILE-PMS/apps/site-agent/internal/conn/client.go:369 | DevLibrary/crates/dl-agent-pms/src/conn.rs (logged, ignored) | có | thấp | none |
| `migrate` frame → migrator (bundle run, progress, result) | SMILE-PMS/apps/site-agent/internal/conn/client.go:377; internal/remotemigrate/handler.go | DevLibrary/crates/dl-agent-pms/src/wire/mod.rs:283 (`RETIRED_FRAMES`, logged `agent.frame.retired`) | khác (ADR) | thấp | none |
| ≤ 8 ops in flight, `AGENT_BUSY` when full or closing | SMILE-PMS/apps/site-agent/internal/conn/client.go:403 | DevLibrary/crates/dl-agent-pms/src/conn.rs:473 | có | thấp | none |
| Op under `deadline_ms` → `OP_TIMEOUT` | SMILE-PMS/apps/site-agent/internal/conn/client.go:420 | DevLibrary/crates/dl-agent-pms/src/conn.rs:566 | có | thấp | none |
| Shutdown: no new ops, 5 s grace, close 1000 | SMILE-PMS/apps/site-agent/internal/conn/client.go:453 | DevLibrary/crates/dl-agent-pms/src/conn.rs:518 | có | thấp | none |
| Back-off 1→30 s jitter; close-code rules 4409/4426/4401/4400… | SMILE-PMS/apps/site-agent/internal/conn/client.go:153,251 | DevLibrary/crates/dl-agent-pms/src/conn.rs:133,328 | có | thấp | none |
| Strict frames, 8 MiB cap before/after gunzip, bad frame → 4400 | SMILE-PMS/apps/site-agent/internal/wire/codec.go:194 | DevLibrary/crates/dl-agent-pms/src/wire/decode.rs:337 | có | thấp | none |
| Op `target_db` routing (old: FO→SMILE_FO, POS→SMILE_POS, PMS/BO→MM_PMS) | SMILE-PMS/apps/site-agent/internal/exec/executor.go:45 | DevLibrary/crates/dl-agent-pms/src/exec/mod.rs:368 (frame `target_db` must equal manifest; FO/POS/BO → SMILE_FO/POS/BO) | khác (ADR §1): SMILE_BO direct, MM_PMS ignored | thấp | none |
| Databases: `databases.json` pools (≤ 9 conns), MM_PMS required | SMILE-PMS/apps/site-agent/internal/exec/schema.go:31 | DevLibrary/crates/dl-agent-pms/src/db.rs:41,457 (`sql.json` first, `databases.json` fallback, MM_PMS ignored) | khác: better (sealed sql.json, TLS rung) | thấp | none |
| SQL 2008 TLS 1.0 (`tlsmin=1.0` DSN) | SMILE-PMS/apps/site-agent/rust (DevLibrary PR #4) / go-mssqldb DSN | dl-tds ladder via core `sql.json` (DevLibrary/crates/dl-agent-pms/src/db.rs:374 `rung`) | có (core ladder) | trung: untested at a real SQL 2008 site | Mũi Né SQL 2008 bench (RT-0252) |
| Manifest allow-list, excluded patterns, agent floor (`xp_*`, `sp_executesql`, …) | SMILE-PMS/apps/site-agent/internal/ops/manifest.go:100,143,155 | DevLibrary/crates/dl-agent-pms/src/ops.rs:45,254,334 | có | thấp | none |
| Unknown op / in-process-only / contract checks | SMILE-PMS/apps/site-agent/internal/exec/executor.go:81 | DevLibrary/crates/dl-agent-pms/src/exec/mod.rs:342 | có | thấp | none |
| Bind types (bit…varbinary, ranges, nullable) | SMILE-PMS/apps/site-agent/internal/exec/binds.go:44,97 | DevLibrary/crates/dl-agent-pms/src/exec/bind.rs:79,157 | có | thấp: only op today is a read | re-check `varchar` code page when the first write op lands |
| One tx + `SET LOCK_TIMEOUT 10000`, read all sets/outputs/return | SMILE-PMS/apps/site-agent/internal/exec/executor.go:137,193 | DevLibrary/crates/dl-agent-pms/src/exec/mod.rs:48,261,307 | có | thấp | none |
| Deadlock retry 3 tries, 50 ms ×2 ±50 %, only reads/idempotent/keyed | SMILE-PMS/apps/site-agent/internal/exec/executor.go:276 | DevLibrary/crates/dl-agent-pms/src/exec/mod.rs:250,393 | có | thấp | none |
| Result encoding (decimal as string, bigint string, dates UTC, TCVN3 passthrough) | SMILE-PMS/apps/site-agent/internal/exec/encode.go:95 | DevLibrary/crates/dl-agent-pms/src/exec/encode.rs:133 | có | thấp | none |
| Caps 10 000 rows / 7 MiB → `RESULT_TOO_LARGE`, never truncated | SMILE-PMS/apps/site-agent/internal/exec/executor.go:240 | DevLibrary/crates/dl-agent-pms/src/exec/mod.rs:42,44 | có | thấp | none |
| Error classification (1205, 1222, sev ≥ 20, commit unknown, driver text hidden) | SMILE-PMS/apps/site-agent/internal/exec/errors.go:22 | DevLibrary/crates/dl-agent-pms/src/exec/mod.rs:172 | có | thấp | none |
| Exactly-once: `MM_Op_GetLog`/`MM_Op_CreateLog` in the op tx | SMILE-PMS/apps/site-agent/internal/exec/oplog.go:47,64 | DevLibrary/crates/dl-agent-pms/src/journal/exec.rs:104; DevLibrary/crates/dl-agent-pms/src/journal/mod.rs:345 (file journal after commit, 8 days, caps → `JOURNAL_FULL`) | khác (ADR §2): cloud ledger authoritative, replay-only frames | trung: crash between commit and journal write answers `REQUEST_KEY_UNKNOWN` (cloud treats as unknown outcome, never re-runs) | none (accepted by ADR) |
| `MM_OpLog` retention purge (30 d, 6 h) | SMILE-PMS/apps/site-agent/internal/retention/retention.go:81; internal/agent/agent.go:229 | journal prune DevLibrary/crates/dl-agent-pms/src/journal/mod.rs:40,387 | khác (ADR) | thấp | none |
| Schema version from `MM_SchemaVersion` | SMILE-PMS/apps/site-agent/internal/conn/client.go:114 | `SCHEMA_VERSION_UNKNOWN` DevLibrary/crates/dl-agent-pms/src/wire/mod.rs:75 | khác (ADR §3) | thấp | none |
| Self-update skeleton (signed manifest, ring, stage, rollback) | SMILE-PMS/apps/site-agent/internal/update/manifest.go:62; stage.go:80,111 | DevLibrary/crates/dl-agent/src/update.rs:120; DevLibrary/crates/dl-agent/src/agent_update.rs:126 | khác: core (real, not skeleton) | thấp | none |
| JSON log, secret-named keys redacted, rotated 10 MB × 5 | SMILE-PMS/apps/site-agent/internal/log/log.go:22; rotate.go:35 | DevLibrary/crates/dl-agent-pms/src/log.rs:19,43 (target `pms-site`); rotation DevLibrary/crates/dl-agent/src/cli.rs:25 | có | thấp | none |
| Error reporting to cloud | none (logs only) | DevLibrary/crates/dl-agent-pms/src/telemetry.rs:20-30 (6 codes, 10 min limit) | khác: better | thấp | none |
| Hub commands status / reconnect / selftest / op dry-run | none | DevLibrary/crates/dl-agent-pms/src/module/commands.rs:47,106,148; DevLibrary/crates/dl-agent-pms/src/module/selftest.rs:42 | khác: new | thấp | none |
| Stub executor when no `databases.json` (`not_implemented`) | SMILE-PMS/apps/site-agent/internal/exec/exec.go:45 | DevLibrary/crates/dl-agent-pms/src/exec/mod.rs:63; missing db → `DB_UNAVAILABLE` DevLibrary/crates/dl-agent-pms/src/exec/mod.rs:384 | khác: module starts without databases, per-op `DB_UNAVAILABLE` | thấp | none |

### Lỗ hổng (pms-site)

1. **Live bench end to end** (trung): fake cloud + SQL 2017/2022 only; never against the real SMILE-PMS cloud nor
   Windows SCM/DPAPI/ACL. Work: SR bench run with a demo tenant (switch steps in crate README). ~0.5 day; needs
   SMILE-PMS cloud tenant + Windows VM.
2. **SQL Server 2008 (TLS 1.0) site** (trung): core ladder covers it on paper; Mũi Né bench pending (RT-0252).
   ~2 h on site; needs the site.
3. **Write ops** (thấp today): the only op is the read `fo.hotelDate.get@1`; `varchar` code-page binding and the
   journal-after-commit window get their first real use with the first write op. Work: add a write-op fixture +
   live SQL test when SMILE-PMS registers one. Depends on SMILE-PMS (other repo).
4. No small gap found to fix in DevLibrary.

## 5. PMS device agent (`SmilePmsDeviceAgent`, SMILE-PMS `apps/device-agent/src`) → `dl-agent-pms-device` (`pms-device`)

Old side: the .NET 10 service (behavioural reference) and its Rust port in `apps/device-agent/rust` (the module's
source). Paths below are the C# files.

| function | old agent (path:line) | dl-agent (path:line) | status | rủi ro tiền-dữ liệu | việc cần làm |
|---|---|---|---|---|---|
| Windows service host `SmilePmsDeviceAgent` | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Program.cs:6 | DevLibrary/crates/dl-agent-pms-device/src/module/mod.rs (module `pms-device`), core service | khác: module of dl-agent | thấp | none |
| Install / token provisioning (runbook, token file with ACL) | SMILE-PMS/docs/runbooks/device-agent-install.md | none (module reads the existing token file); core `install.ps1` | thiếu: no enrolment, token still issued and placed by hand | trung: a wrong/shared token claims another workstation's queue | large gap 1 |
| Settings `appsettings.json` + `.{env}.json` + `Agent__*` env | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Program.cs:8 | DevLibrary/crates/dl-agent-pms-device/src/module/prepare.rs:43; DevLibrary/crates/dl-agent-pms-device/src/module/settings.rs (+ `overrides`) | khác: env is dl-agent's, not the old service's | thấp | README switch step 3 (`overrides`) |
| Option defaults (`ApiBaseUrl`, `PollInterval` 1 s, `CommandTimeout` 30 s, `Adapter` none, `DataDirectory`) | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Agent/AgentOptions.cs:7-27 | DevLibrary/crates/dl-agent-pms-device/src/config/options.rs:54,60 | có | thấp | none |
| Validator (https or loopback http, AgentId pattern, timeout ≤ 45 s, poll 250 ms–60 s, absolute DataDirectory, known adapter) | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Agent/AgentOptionsValidator.cs:16 | DevLibrary/crates/dl-agent-pms-device/src/config/validator.rs:34 | có | thấp | none |
| `RunOnce` smoke mode | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Agent/DeviceAgentWorker.cs:26 | DevLibrary/crates/dl-agent-pms-device/src/agent/worker.rs:110 (kept in port; module forces it off, DevLibrary/crates/dl-agent-pms-device/src/module/prepare.rs:90) | khác: ignored in module | thấp | none |
| Token file read once, trimmed, ≥ 32 chars, errors name the path only | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Agent/TokenFile.cs:16 | DevLibrary/crates/dl-agent-pms-device/src/agent/token.rs:79,95 (+ BOM handling) | có | thấp | none |
| Poll loop: work while queued, else `PollInterval`; failures back off 2ⁿ s ≤ 30 s | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Agent/DeviceAgentWorker.cs:15 | DevLibrary/crates/dl-agent-pms-device/src/agent/worker.rs:91 | có | thấp | none |
| Claim `POST device-agent/commands/claim` (204 none, 200 command, lease from send time) | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Queue/ApiCommandQueue.cs:27 | DevLibrary/crates/dl-agent-pms-device/src/queue/api.rs:150 | có | thấp | none |
| Headers `Authorization: DeviceAgent`, `X-Device-Agent-Id`, `X-Device-Adapter` | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Queue/ApiCommandQueue.cs:17-25,56 | DevLibrary/crates/dl-agent-pms-device/src/queue/api.rs:16-22,71 | có | thấp | none |
| Result `POST …/{id}/result`, 404 Missing, 409 NotHeld / OutcomeConflict | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Queue/ApiCommandQueue.cs:41 | DevLibrary/crates/dl-agent-pms-device/src/queue/api.rs:176 | có | thấp | none |
| Problem detail text ≤ 300 chars | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Queue/ApiCommandQueue.cs:78 | DevLibrary/crates/dl-agent-pms-device/src/queue/api.rs:27,226 | có | thấp | none |
| HTTP client 15 s, pooled connections refreshed every 5 min | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Program.cs:28 | DevLibrary/crates/dl-agent-pms-device/src/queue/api.rs:24,54 | có (timeout); connection lifetime per dl-http pool | thấp | none |
| Proxy: env + Windows system proxy | .NET `SocketsHttpHandler` default (SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Program.cs:29) | DevLibrary/crates/dl-agent-pms-device/src/queue/api.rs:61 (`with_proxy`), DevLibrary/crates/dl-agent-pms-device/src/module/mod.rs:160 (`dl_agent::proxy::current()`) | có (fixed in this stream; was env only) | thấp | done: commit `cedabd1` |
| Flush owed results before each claim; owed results block claiming | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Agent/CommandProcessor.cs:20; Agent/ResultReporter.cs:53 | DevLibrary/crates/dl-agent-pms-device/src/agent/processor.rs:85; DevLibrary/crates/dl-agent-pms-device/src/agent/reporter.rs:99 | có | thấp | none |
| Outbox: save before send, atomic tmp+rename, corrupt → `*.corrupt`, oldest first | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Agent/ResultOutbox.cs:10,22 | DevLibrary/crates/dl-agent-pms-device/src/agent/outbox.rs:43,62 | có | thấp | none |
| Report: send with 20 s timeout, retry 1→8 s while lease lasts, else keep in outbox | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Agent/ResultReporter.cs:12,18 | DevLibrary/crates/dl-agent-pms-device/src/agent/reporter.rs:20,37,58 | có | thấp | none |
| 400/422 → drop as invalid; stale claim → drop with log | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Agent/ResultReporter.cs:91-100 | DevLibrary/crates/dl-agent-pms-device/src/agent/reporter.rs:136,150,182 | có | trung: device acted but result dropped (same as .NET; logged with card data for hand recovery) | none |
| Command timeout → `TIMEOUT`, bad payload → `BAD_PAYLOAD`, adapter exception → `ADAPTER_FAILED`, unknown kind | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Agent/CommandProcessor.cs:36,70 | DevLibrary/crates/dl-agent-pms-device/src/agent/processor.rs:120,196 | có | thấp | none |
| Lock verbs issue/duplicate/extend/read/erase/cancelAll | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Agent/CommandProcessor.cs:72-77 | DevLibrary/crates/dl-agent-pms-device/src/agent/processor.rs:200-205 | có | trung: key cards (room access) | hardware bench |
| Adapter `none` → DEVICE_NOT_CONNECTED | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Adapters/NotConnectedAdapter.cs:7 | DevLibrary/crates/dl-agent-pms-device/src/adapters/not_connected.rs:13 | có | thấp | none |
| Adapter `simulated-lock` | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Adapters/SimulatedLockAdapter.cs:11 | DevLibrary/crates/dl-agent-pms-device/src/adapters/simulated.rs:22 | có | thấp | none |
| `smile-lock` router by key type / MULTI → RoomLockDef.LockType, pending vendors → SDK_NOT_INSTALLED | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Adapters/Lock/SmileLockRouter.cs:19,35 | DevLibrary/crates/dl-agent-pms-device/src/adapters/lock/router.rs:46,86,109 | có | thấp | none |
| MFCARD via LockSDK.dll (x86, TP_* stdcall) | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Adapters/Lock/MfLockAdapter.cs:12; Lock/TpNative.cs:17-66 | DevLibrary/crates/dl-agent-pms-device/src/adapters/lock/mf.rs:29; DevLibrary/crates/dl-agent-pms-device/src/native/locksdk.rs | có (32-bit build only) | trung: real encoder never driven by the Rust code | hardware bench (lock encoder) |
| Hune HSmart V8, Orbita CLock (no DLL in hand → refuse) | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Adapters/Lock/HuneHSmartV8Adapter.cs:28; Lock/OrbitaCLockAdapter.cs:30 | DevLibrary/crates/dl-agent-pms-device/src/adapters/lock/hune.rs:19; DevLibrary/crates/dl-agent-pms-device/src/adapters/lock/orbita.rs:20 | có | thấp | none |
| TTLock (cloud API) | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Adapters/Lock/TtLockAdapter.cs:12 | DevLibrary/crates/dl-agent-pms-device/src/adapters/lock/ttlock.rs:23 | có | thấp | none |
| POS router by `target.transport` | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Adapters/Pos/PosDeviceRouter.cs:51-144 | DevLibrary/crates/dl-agent-pms-device/src/adapters/pos/router.rs:134,208-278 | có | thấp | none |
| ESC/POS TCP 9100 | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Adapters/Pos/EscPosTcpAdapter.cs:10 | DevLibrary/crates/dl-agent-pms-device/src/adapters/pos/tcp.rs:15 | có | thấp | printer bench |
| ESC/POS serial, pole display | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Adapters/Pos/EscPosSerialAdapter.cs:23; Pos/SerialPoleAdapter.cs:7 | DevLibrary/crates/dl-agent-pms-device/src/adapters/pos/serial.rs:29 | có | thấp | bench |
| Windows spooler RAW | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Adapters/Pos/WindowsSpoolerAdapter.cs:19 | DevLibrary/crates/dl-agent-pms-device/src/adapters/pos/spooler.rs:12 | có | thấp | bench |
| ESC/POS encoding (Vietnamese → printer code page) | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Adapters/Pos/EscPos.cs:14 | DevLibrary/crates/dl-agent-pms-device/src/adapters/pos/escpos.rs:19; unicode.rs | có | thấp | none |
| Cash drawer kick (printer pulse) | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Adapters/Pos/PrinterKickDrawerAdapter.cs:7 | DevLibrary/crates/dl-agent-pms-device/src/adapters/pos/router.rs:224 | có | trung: opens the cash drawer | bench |
| Card read serial / PC/SC NFC (ACR122) | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Adapters/Pos/SerialCardReaderAdapter.cs:10; Pos/AcrNfcAdapter.cs:19 | DevLibrary/crates/dl-agent-pms-device/src/adapters/pos/card.rs:12; DevLibrary/crates/dl-agent-pms-device/src/adapters/pos/nfc.rs:26 | có | thấp | bench |
| SAPI speech | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Adapters/Pos/SpeakerAdapter.cs:18 | DevLibrary/crates/dl-agent-pms-device/src/adapters/pos/speaker.rs:27 | có | thấp | bench |
| OPOS: refuses every verb | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Adapters/Pos/OposAdapter.cs:18-25 | DevLibrary/crates/dl-agent-pms-device/src/adapters/pos/opos.rs:78 (drives CashDrawer + LineDisplay controls when registered) | khác: new behaviour from the Rust port | trung: a registered OPOS drawer now opens on `pos.kick` | owner to confirm; bench |
| Bluetooth refusal | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/Adapters/Pos/OposAdapter.cs:40 | DevLibrary/crates/dl-agent-pms-device/src/adapters/pos/router.rs:143 | có | thấp | none |
| Logs: console + Event Log (Warning+) | SMILE-PMS/apps/device-agent/src/SmilePms.DeviceAgent/appsettings.json:2; Program.cs:6 | DevLibrary/crates/dl-agent-pms-device/src/module/logger.rs:11,36 (dl-agent log, target `pms-device`) | khác: no Event Log | thấp | none |
| Error reporting to cloud | none | DevLibrary/crates/dl-agent-pms-device/src/module/telemetry.rs:20-26 (4 codes, 10 min) | khác: new | thấp | none |
| Hub commands status / flush_outbox, metrics | none | DevLibrary/crates/dl-agent-pms-device/src/module/commands.rs:18,20,106,136 | khác: new | thấp | none |
| Settings / token change → restart worker | process restart by hand | DevLibrary/crates/dl-agent-pms-device/src/module/mod.rs (30 s check) | khác: better | thấp | none |
| Stop: worker 5 s, blocked device call left, result to outbox | .NET host shutdown (Program.cs:46) | DevLibrary/crates/dl-agent-pms-device/src/module/mod.rs (stop path), DevLibrary/crates/dl-agent-pms-device/src/agent/stop.rs:20 | có | thấp | none |

### Lỗ hổng (pms-device)

1. **Enrolment instead of a hand-placed token** (trung, large): the module still needs a per-workstation token file
   issued in the PMS console and copied by hand; no `pms-device-enroll` and no hub-side issuance. Work: PMS API route
   to issue a device-agent token from an enrolment code + a dl-agent tool that writes `TokenFile` with the state ACL.
   ~1–2 days; depends on SMILE-PMS (other repo).
2. **Hardware bench** (trung): locks (LockSDK x86 on the 32-bit build, `i686-win7` target not built locally),
   printers (TCP/serial/spooler), drawer, NFC, SAPI only covered by fakes. ~1 day on the SR bench; needs devices.
3. **OPOS behaviour change** (trung): the port drives registered OPOS CashDrawer/LineDisplay where .NET refused.
   Needs an owner decision (keep, or refuse like .NET); a one-line switch in `adapters/pos/router.rs` either way.
4. **Proxy** (thấp, small): fixed here, see Fixes.

## 6. Check-In (SmileCheckin `src/MM.Arrival.Agent` → `dl-agent-arrival`)

Old agent read: `SmileCheckin/src/MM.Arrival.Agent` (.NET 10) + `MM.Arrival.Sync.Contracts`. New: `DevLibrary/crates/dl-agent-arrival` + core. Customer: Victory and the Check-In hotels.

| function | old agent (path:line) | dl-agent (path:line) | status | rủi ro tiền-dữ liệu | việc cần làm |
|---|---|---|---|---|---|
| Windows service host `MM.Arrival.Agent` | SmileCheckin/src/MM.Arrival.Agent/Program.cs:13 | DevLibrary/crates/dl-agent/src/cli.rs:420; DevLibrary/crates/dl-agent-arrival/src/module/mod.rs:168 | có | thấp | none |
| Settings `Agent` section (appsettings + Production + env) | SmileCheckin/src/MM.Arrival.Agent/AgentOptions.cs:4 | DevLibrary/crates/dl-agent-arrival/src/agent/settings.rs:217 | có | thấp | none |
| Install dir + relative paths (System32 working dir) | SmileCheckin/src/MM.Arrival.Agent/AgentOptions.cs:15, :72 | DevLibrary/crates/dl-agent-arrival/src/agent/layout.rs:39, :85 | có | thấp | none |
| Identity load (DPAPI LocalMachine + entropy), wait for enroll 1 min | SmileCheckin/src/MM.Arrival.Agent/AgentIdentity.cs:52; CloudLinkWorker.cs:331 | DevLibrary/crates/dl-agent-arrival/src/agent/secrets.rs:180; module/mod.rs:206 (60 s recheck) | có | thấp | none |
| `enroll <token>` | SmileCheckin/src/MM.Arrival.Agent/Program.cs:62; AgentIdentity.cs:20, :37 | DevLibrary/crates/dl-agent-arrival/src/tools/enroll.rs:167 (`arrival-enroll`, token from env/stdin) | khác | thấp: also writes settings, probes PMS | none |
| `test-pms` (`MM_TEST_PMS_CS`, exit 0/1/3, TLS step-down) | SmileCheckin/src/MM.Arrival.Agent/PmsConnectionProbe.cs:16, :44 | DevLibrary/crates/dl-agent-arrival/src/tools/enroll.rs:475; module/selftest.rs | khác | thấp: inside enroll / selftest, no stand-alone verb | none |
| `export-key` (print tenant master key) | SmileCheckin/src/MM.Arrival.Agent/Program.cs:81 | — (only fingerprint in `status`; `crypto.export-key` op for the cloud escrow: DevLibrary/crates/dl-agent-arrival/src/contracts/key_vault.rs:75) | thiếu | trung: the hotel's offline copy of the legal-vault key cannot be printed after the switch | owner decision: keep the cloud escrow only, or add a local `arrival-export-key` tool |
| `install-procedures` (force) | SmileCheckin/src/MM.Arrival.Agent/Program.cs:90 | DevLibrary/crates/dl-agent-arrival/src/module/commands.rs:45 (`install_procedures`) | khác | thấp: hub command, not CLI | none |
| Auto-install proc bundle at start (marker, picture phase) | SmileCheckin/src/MM.Arrival.Agent/Program.cs:110; PmsProcedureInstaller.cs:94 | DevLibrary/crates/dl-agent-arrival/src/agent/runtime.rs:469; agent/installer.rs:155 | có | thấp: same markers, byte-identical bundle | none |
| Tenant key load/create (DPAPI + entropy, or `TenantKey`) | SmileCheckin/src/MM.Arrival.Agent/TenantKeyStore.cs:21 | DevLibrary/crates/dl-agent-arrival/src/agent/secrets.rs:213, :245; legacy/tenant_key.rs | có | thấp: legacy import refuses ambiguous keys | none |
| HTTP client: HTTPS only, 60 s timeout, connect 15 s | SmileCheckin/src/MM.Arrival.Agent/Program.cs:33 | DevLibrary/crates/dl-agent-arrival/src/agent/cloud.rs:42, :113 | có | thấp | none |
| HTTP/2 + keep-alive PING 20 s (dead NAT seen in ~30 s) | SmileCheckin/src/MM.Arrival.Agent/Program.cs:40, :49 | DevLibrary/crates/dl-agent-arrival/src/agent/cloud.rs:27 (HTTP/1.1, 60 s poll timeout) | khác | thấp: a silently dropped link is seen after 60 s instead of ~30 s | none (confirm HTTP/1.1 on the canary) |
| Proxy (HttpClient default = machine settings) | SmileCheckin/src/MM.Arrival.Agent/Program.cs:42 | DevLibrary/crates/dl-agent-arrival/src/agent/cloud.rs:108 | có | thấp | fixed in this stream (was env only) |
| Poll loop, hello, backoff 0.5–60 s jitter | SmileCheckin/src/MM.Arrival.Agent/CloudLinkWorker.cs:86, :351 | DevLibrary/crates/dl-agent-arrival/src/agent/runtime.rs:366; agent/clock.rs:46 | có | thấp | none |
| Hello: version, catalog, machine, bundle.schema fingerprint, procedures report, problems, inventory | SmileCheckin/src/MM.Arrival.Agent/CloudLinkWorker.cs:103 | DevLibrary/crates/dl-agent-arrival/src/agent/runtime.rs:435 | có | thấp | none |
| Problem resend in hello at most every 5 min | SmileCheckin/src/MM.Arrival.Agent/CloudLinkWorker.cs:100 | DevLibrary/crates/dl-agent-arrival/src/agent/runtime.rs:460 | có | thấp | none |
| Update check / hint / manifest in poll | SmileCheckin/src/MM.Arrival.Agent/CloudLinkWorker.cs:114, :126; Update/UpdateCoordinator.cs:136 | DevLibrary/crates/dl-agent/src/agent_update.rs:50 | khác | thấp: dl-agent updates itself, cloud offers ignored | none |
| DlSwitch state + directive in poll | SmileCheckin/src/MM.Arrival.Agent/CloudLinkWorker.cs:114, :128; DlSwitch/DlSwitchRunner.cs | — | khác | thấp: only meaningful on the .NET side | none |
| 409 catalog rejected (sealed manifest) | SmileCheckin/src/MM.Arrival.Agent/CloudLinkWorker.cs:136 | DevLibrary/crates/dl-agent-arrival/src/agent/runtime.rs:392 | khác | thấp: backs off + degraded, offered build ignored | none |
| 401 refused (clock vs key) | SmileCheckin/src/MM.Arrival.Agent/CloudLinkWorker.cs:277 | DevLibrary/crates/dl-agent-arrival/src/agent/cloud.rs:157; runtime.rs:425 | có | thấp | none |
| Envelope seal/open AES-GCM + HKDF, ±120 s replay guard | SmileCheckin/src/MM.Arrival.Agent/CloudLinkWorker.cs:257, :303 | DevLibrary/crates/dl-agent-arrival/src/agent/cloud.rs:167; contracts/envelope.rs:182, :261, :302 | có | thấp: golden vectors | none |
| Results uplink (50/post, 50 ms window) | SmileCheckin/src/MM.Arrival.Agent/CloudLinkWorker.cs:80, :168 | DevLibrary/crates/dl-agent-arrival/src/agent/runtime.rs:561; agent/outbox.rs:120 | có | thấp | none |
| Notices uplink (200/post, 250 ms, bounded) | SmileCheckin/src/MM.Arrival.Agent/CloudLinkWorker.cs:82; AgentOutbox.cs:12 | DevLibrary/crates/dl-agent-arrival/src/agent/runtime.rs:320; agent/outbox.rs:59 | có | thấp | none |
| Upload retry 5×, 15 s, then drop | SmileCheckin/src/MM.Arrival.Agent/CloudLinkWorker.cs:194 | DevLibrary/crates/dl-agent-arrival/src/agent/runtime.rs:585 | có | thấp: writes replay from journal | none |
| Dispatch: every request answered, "Agent error" on crash | SmileCheckin/src/MM.Arrival.Agent/CloudLinkWorker.cs:221, :235 | DevLibrary/crates/dl-agent-arrival/src/agent/runtime.rs:520, :536 | có | thấp | none |
| PMS concurrency (`MaxConcurrency` 4) | SmileCheckin/src/MM.Arrival.Agent/RequestProcessor.cs:22 | DevLibrary/crates/dl-agent-arrival/src/agent/runtime.rs:155 (PMS lane threads) + fast lane | có | thấp | none |
| Request validation against catalog | SmileCheckin/src/MM.Arrival.Agent/RequestProcessor.cs:29 | DevLibrary/crates/dl-agent-arrival/src/agent/processor.rs:106; contracts/validator.rs:38 | có | thấp | none |
| Write idempotency replay (48 h journal) | SmileCheckin/src/MM.Arrival.Agent/RequestProcessor.cs:35; IdempotencyLog.cs:44 | DevLibrary/crates/dl-agent-arrival/src/agent/processor.rs:92; agent/journal.rs:137 | có | thấp | none |
| Journal append after write | SmileCheckin/src/MM.Arrival.Agent/IdempotencyLog.cs:55 | DevLibrary/crates/dl-agent-arrival/src/agent/journal.rs:160 | khác | thấp: .NET answered "Agent error" when the append threw (cloud could retry the write); Rust answers Ok and logs (safer) | none |
| Journal load/compact (torn last line, 48 h cutoff) | SmileCheckin/src/MM.Arrival.Agent/IdempotencyLog.cs:22, :67 | DevLibrary/crates/dl-agent-arrival/src/agent/journal.rs:87 | có | thấp: same file kept at switch | none |
| Snapshots LRU 2000, NotModified / Delta | SmileCheckin/src/MM.Arrival.Agent/RequestProcessor.cs:53; SnapshotStore.cs:18, :29 | DevLibrary/crates/dl-agent-arrival/src/agent/snapshots.rs:43, :56; contracts/delta.rs:30 | có | thấp | none |
| PMS exec: SP + typed params, `SET LOCK_TIMEOUT`, 15 s command | SmileCheckin/src/MM.Arrival.Agent/PmsExecutor.cs:38, :64 | DevLibrary/crates/dl-agent-arrival/src/agent/tds.rs:360; agent/executor.rs:58 | khác | thấp: `DateTime` params bound as datetime2(7) (SqlClient: datetime); SQL rounds the same way | none |
| Result sets: skip column-less, drop uncatalogued, `MaxRows` truncation | SmileCheckin/src/MM.Arrival.Agent/PmsExecutor.cs:72–91 | DevLibrary/crates/dl-agent-arrival/src/agent/tds.rs:288 | khác | thấp: Rust buffers the whole set before truncating (memory only) | none |
| Projection of columns | SmileCheckin/src/MM.Arrival.Agent/PmsExecutor.cs:159 | DevLibrary/crates/dl-agent-arrival/src/agent/executor.rs:78, :97 | có | thấp | none |
| Lock timeout 1222 → lock holders log | SmileCheckin/src/MM.Arrival.Agent/RequestProcessor.cs:67; PmsExecutor.cs:118 | DevLibrary/crates/dl-agent-arrival/src/agent/processor.rs:217; agent/executor.rs:71 | có | thấp | none |
| Only SQL error number on the wire | SmileCheckin/src/MM.Arrival.Agent/RequestProcessor.cs:77 | DevLibrary/crates/dl-agent-arrival/src/agent/processor.rs:209 | có | thấp | none |
| Schema fingerprint (sys.parameters SHA-256/16) | SmileCheckin/src/MM.Arrival.Agent/PmsExecutor.cs:95 | DevLibrary/crates/dl-agent-arrival/src/agent/executor.rs:67, :205 | có | thấp | none |
| Key ops wrap/unwrap/export | SmileCheckin/src/MM.Arrival.Agent/RequestProcessor.cs:90 | DevLibrary/crates/dl-agent-arrival/src/agent/processor.rs:249 | có | thấp | none |
| Vault ops put/get/delete (path normalize, size cap, sha) | SmileCheckin/src/MM.Arrival.Agent/RequestProcessor.cs:114; LegalVault.cs:22, :35, :41 | DevLibrary/crates/dl-agent-arrival/src/agent/processor.rs:286; agent/vault.rs:70, :95, :109 | có | thấp | none |
| Change watcher (`{today}`, ≥5 s, new day resets ETag) | SmileCheckin/src/MM.Arrival.Agent/ChangeWatcher.cs:29, :41 | DevLibrary/crates/dl-agent-arrival/src/agent/watcher.rs:146, :157 | khác | thấp: `{today}` from `utc_offset_minutes` (420) instead of the server's time zone | none (set the option for a non-UTC+7 site) |
| PMS connection string defaults (`Encrypt=true`) | SmileCheckin/src/MM.Arrival.Agent/PmsExecutor.cs:61 | DevLibrary/crates/dl-agent-arrival/src/agent/tds.rs:127; module/mod.rs:277 (core `sql.json` wins) | khác | thấp | none |
| Link status file + heartbeat file | SmileCheckin/src/MM.Arrival.Agent/LinkStatus.cs:42, :57; CloudLinkWorker.cs:286, :296 | DevLibrary/crates/dl-agent-arrival/src/agent/link_status.rs:98, :105; runtime.rs:486, :508 | có | thấp | none |
| Problem journal (record, unclean stop, recent) | SmileCheckin/src/MM.Arrival.Agent/Diagnostics/ProblemJournal.cs:72, :86, :104, :135 | DevLibrary/crates/dl-agent-arrival/src/agent/problems.rs:116, :127, :160, :198 | có | thấp | none |
| Every logged warning copied into the journal | SmileCheckin/src/MM.Arrival.Agent/Diagnostics/ProblemLoggerProvider.cs | DevLibrary/crates/dl-agent-arrival/src/module/reporter.rs:89 (reports only) | khác | thấp: fewer entries (no per-request warnings) | none |
| Event log history import (14 days) | SmileCheckin/src/MM.Arrival.Agent/Diagnostics/EventLogHistory.cs:36 | DevLibrary/crates/dl-agent-arrival/src/agent/eventlog.rs:79 | có | thấp | none |
| Install inventory in hello | SmileCheckin/src/MM.Arrival.Agent/AgentInstallInventory.cs:16 | DevLibrary/crates/dl-agent-arrival/src/agent/inventory.rs:80 | có | thấp | none |
| Self-update (stage, verify SHA, swap, health 300 s, rollback) | SmileCheckin/src/MM.Arrival.Agent/Update/UpdateApplier.cs:51; Update/PackageVerifier.cs:37 | DevLibrary/crates/dl-agent/src/update.rs:412, :438, :480 | có | thấp | none |
| Drain link before update (120 s) | SmileCheckin/src/MM.Arrival.Agent/Update/LinkGate.cs:47 | DevLibrary/crates/dl-agent/src/agent_update.rs:126; DevLibrary/crates/dl-agent-arrival/src/agent/runtime.rs:42 (`GRACE` 5 s) | khác | thấp: running PMS calls get 5 s; journal replays a write the cloud asks again | none |
| `AutoUpdate=false` opt-out | SmileCheckin/src/MM.Arrival.Agent/AgentOptions.cs:60 | DevLibrary/crates/dl-agent/src/optout.rs:41 | khác | thấp: setting not carried | none |
| Service recovery (`sc failure`, `failureflag`) | SmileCheckin/src/MM.Arrival.Agent/SelfCare/SelfCarePlan.cs; SelfCare/SelfCareService.cs:79 | DevLibrary/crates/dl-agent/src/service_watch.rs:131 | có | thấp | none |
| Watchdog task (5 min; restart on stale cloud link) | SmileCheckin/src/MM.Arrival.Agent/SelfCare/watchdog.ps1:36 | DevLibrary/crates/dl-agent/src/watchdog.rs:209 | khác | thấp: watches the dl-agent hub heartbeat, not the Check-In link; a hung arrival link shows as `degraded`, not restarted | none |
| Old watchdog task removed at switch | SmileCheckin/src/MM.Arrival.Agent/SelfCare/SelfCarePlan.cs (`MM-Arrival Agent Watchdog`) | DevLibrary/services/agent-hub/src/install/benchLegacy.ts:103, :223 | có | trung: a left task would restart the .NET service (two agents, one identity) | keep in the switch checklist |
| Hub commands status / vault_check / op_sweep / picture_probe (new) | — | DevLibrary/crates/dl-agent-arrival/src/module/commands.rs:79, :135 | khác | thấp | none |

### Lỗ hổng (Check-In)

1. **No local `export-key`** — trung. After the switch the hotel cannot print its tenant master key (legal vault + cloud data keys) for an offline copy; only the cloud escrow (`crypto.export-key`) and the fingerprint remain. Work: owner decision; if yes, an `arrival-export-key` tool that prints the key once to the console (≈ 1 h, DevLibrary only). Not fixed: it prints a secret, so it needs the owner's yes.
2. **Link liveness** — thấp. HTTP/1.1 without PING: a silently dropped NAT path is noticed after the 60 s poll timeout instead of ~30 s, and the watchdog no longer restarts on a stale Check-In link. Work: none unless the canary shows long gaps; option: shorter poll timeout when the cloud holds < 25 s (needs the cloud hold value).
3. **`{today}` from a fixed offset** — thấp. Work: none for VN sites; document `utc_offset_minutes`.
4. Verification: real DPAPI identity / tenant key of a production install, HTTP/1.1 against the real cloud (INVENTORY §4b, task 646/727).

## 7. Membership (MM-MEMBERSHIP `src/MM.Membership.Agent` → `dl-agent-membership`)

Old agent read: `MM-MEMBERSHIP/src/MM.Membership.Agent` (.NET 10, 19 k lines, HEAD with `58a4524`). New: `DevLibrary/crates/dl-agent-membership` + core. Customers: Victory (VIC-SGN, VICVT), Mũi Né Bay.

| function | old agent (path:line) | dl-agent (path:line) | status | rủi ro tiền-dữ liệu | việc cần làm |
|---|---|---|---|---|---|
| Service host + content root at exe dir | MM-MEMBERSHIP/src/MM.Membership.Agent/Program.cs:16 | DevLibrary/crates/dl-agent/src/cli.rs:420; DevLibrary/crates/dl-agent-membership/src/module/mod.rs:92 | có | thấp | none |
| `--version` | MM-MEMBERSHIP/src/MM.Membership.Agent/Program.cs:50; Cli/VersionCommand.cs:16 | `dl-agent version` (core) | có | thấp | none |
| `enroll --stdin` (code, hub URL same-origin policy) | MM-MEMBERSHIP/src/MM.Membership.Agent/Program.cs:56; Cli/EnrollCommand.cs:24; Cli/EnrollHubUrlPolicy.cs:17 | DevLibrary/crates/dl-agent-membership/src/enroll/mod.rs:80, :184 | có | thấp | none |
| `config.dpapi` seal/unseal (DPAPI LocalMachine) | MM-MEMBERSHIP/src/MM.Membership.Agent/Configuration/AgentSecureConfig.cs:34, :60 | DevLibrary/crates/dl-agent-membership/src/config.rs:197, :210 | có | thấp | none |
| Seed from appsettings / `Agent__*` / portable store | MM-MEMBERSHIP/src/MM.Membership.Agent/Configuration/AgentSecretStore.cs:74; PortableAgentConfigStore.cs:30 | DevLibrary/crates/dl-agent-membership/src/enroll/legacy.rs:161, :193 | có | thấp | none |
| Switch from .NET + rollback (new) | — | DevLibrary/crates/dl-agent-membership/src/enroll/switch.rs:176, :422 | khác | thấp | none |
| `check-smile [--diagnose]` | MM-MEMBERSHIP/src/MM.Membership.Agent/Program.cs:70; Cli/CheckSmileCommand.cs:37, :98 | DevLibrary/crates/dl-agent-membership/src/module/selftest.rs:95 | khác | thấp: connection check only, `--diagnose` missing | see SmileDiagnostics gap |
| `update-apply`, `diag-upload` | MM-MEMBERSHIP/src/MM.Membership.Agent/Program.cs:93, :98 | DevLibrary/crates/dl-agent/src/update.rs:412; core `collect_diagnostics` | khác | thấp | none |
| `--backfill-from/--to/--page-size/--state` | MM-MEMBERSHIP/src/MM.Membership.Agent/Program.cs:104; Worker/WalkInBackfillRunner.cs:85 | DevLibrary/crates/dl-agent-membership/src/checkout/tool.rs:40; checkout/backfill.rs:252 | có | thấp: hub dedups `CONF:` | none |
| Hub auth: Bearer + `X-Hotel-Id` | MM-MEMBERSHIP/src/MM.Membership.Agent/Hub/AgentAuthHandler.cs:25 | DevLibrary/crates/dl-agent-membership/src/hub/mod.rs:243 | có | thấp | none |
| `X-Correlation-Id` per request | MM-MEMBERSHIP/src/MM.Membership.Agent/Observability/AgentCorrelationHandler.cs:15 | DevLibrary/crates/dl-agent-membership/src/hub/mod.rs:46 | có | thấp | fixed in this stream |
| Proxy (HttpClient default = machine settings) | MM-MEMBERSHIP/src/MM.Membership.Agent/Program.cs:216 | DevLibrary/crates/dl-agent-membership/src/hub/mod.rs:181 | có | thấp | fixed in this stream |
| Token rejection gate (15 min, one probe) | MM-MEMBERSHIP/src/MM.Membership.Agent/Hub/HubTokenRejectionGate.cs:50, :62 | DevLibrary/crates/dl-agent-membership/src/hub/gate.rs:64, :78 | có | thấp | none |
| `POST jobs/pull` → `GET jobs` fallback | MM-MEMBERSHIP/src/MM.Membership.Agent/Hub/HubClient.cs:52 | DevLibrary/crates/dl-agent-membership/src/hub/mod.rs:344 | có | thấp | none |
| `POST jobs/ack` | MM-MEMBERSHIP/src/MM.Membership.Agent/Hub/HubClient.cs:69 | DevLibrary/crates/dl-agent-membership/src/hub/mod.rs:313 | có | thấp | none |
| `POST heartbeat` (+ run-diagnostics, update-available headers) | MM-MEMBERSHIP/src/MM.Membership.Agent/Hub/HubClient.cs:80 | DevLibrary/crates/dl-agent-membership/src/hub/mod.rs:317; sync.rs:275 | khác | thấp: run-diagnostics only logged, update header ignored (core updates) | see SmileDiagnostics gap |
| Heartbeat body (version, SmileOk, WalkInEarnOk, scan watermark/lag, DbIdentity, ForeignActivity, PmsNow, capabilities) | MM-MEMBERSHIP/src/MM.Membership.Agent/Worker/SyncWorker.cs:355 | DevLibrary/crates/dl-agent-membership/src/sync.rs:367 | khác | thấp: `Diagnostics` field always null | see SmileDiagnostics gap |
| Independent heartbeat loop (never waits on the job pump) | MM-MEMBERSHIP/src/MM.Membership.Agent/Worker/SyncWorker.cs:133 | DevLibrary/crates/dl-agent-membership/src/sync.rs:192 (heartbeat after the pump in one step) | khác | trung: a slow batch (20 jobs × up to 30 s) or a failed pull delays/skips the heartbeat → hub watchdog may show the agent offline | split the heartbeat onto its own thread |
| `GET sync-config` (null = both on, special code) | MM-MEMBERSHIP/src/MM.Membership.Agent/Hub/HubClient.cs:258 | DevLibrary/crates/dl-agent-membership/src/hub/mod.rs:330; module/state.rs:151 | có | thấp | none |
| Main loop: pump, backoff 5 s→5 min | MM-MEMBERSHIP/src/MM.Membership.Agent/Worker/SyncWorker.cs:52, :153, :243 | DevLibrary/crates/dl-agent-membership/src/sync.rs:30, :130, :241 | có | thấp | none |
| SMILE probe per heartbeat | MM-MEMBERSHIP/src/MM.Membership.Agent/Worker/SyncWorker.cs:402 | DevLibrary/crates/dl-agent-membership/src/sync.rs:307 | có | thấp | none |
| DB identity fingerprint + pin, scans pause on mismatch | MM-MEMBERSHIP/src/MM.Membership.Agent/Smile/SmileDbIdentityProbe.cs:99; Worker/PmsIdentityTracker.cs:51 | DevLibrary/crates/dl-agent-membership/src/sync.rs:333; smile/tds.rs:356 | có | thấp | none |
| PMS write gate (identity mismatch → refuse writes) | MM-MEMBERSHIP/src/MM.Membership.Agent/Jobs/PmsWriteGate.cs:69, :91 | DevLibrary/crates/dl-agent-membership/src/jobs/gate.rs:78 | có | thấp | none |
| SMILE connection defaults (15 s connect, 30 s command, Encrypt + TrustServerCertificate) | MM-MEMBERSHIP/src/MM.Membership.Agent/Smile/SmileConnectionFactory.cs:47 | DevLibrary/crates/dl-agent-membership/src/smile/tds.rs:106 | có | thấp | none |
| SQL error classification → ack code/class | MM-MEMBERSHIP/src/MM.Membership.Agent/Observability/SmileErrorClassifier.cs:30 | DevLibrary/crates/dl-agent-membership/src/jobs/result.rs:59, :71 | có | thấp | none |
| Job dispatch by action | MM-MEMBERSHIP/src/MM.Membership.Agent/Jobs/JobDispatcher.cs:94 | DevLibrary/crates/dl-agent-membership/src/jobs/dispatch.rs:128 | có | thấp | none |
| Create booking (applock, numbers, duplicate guard, rates, AdditionalName, note) | MM-MEMBERSHIP/src/MM.Membership.Agent/Jobs/JobDispatcher.cs:135; Smile/FolioWriter.Atomic.cs:93; Smile/FolioWriter.Reservation.cs:53 | DevLibrary/crates/dl-agent-membership/src/jobs/dispatch.rs:218; writer/folio.rs:129, :193 | có | thấp: golden SQL | none |
| Modify booking (room diff, locked folio, extra bed) | MM-MEMBERSHIP/src/MM.Membership.Agent/Jobs/JobDispatcher.cs:161; Jobs/RoomDiffCalculator.cs | DevLibrary/crates/dl-agent-membership/src/jobs/dispatch.rs:260; jobs/room_diff.rs:143 | có | thấp | none |
| Cancel | MM-MEMBERSHIP/src/MM.Membership.Agent/Jobs/JobDispatcher.cs:232; Smile/FolioWriter.Cancel.cs:32 | DevLibrary/crates/dl-agent-membership/src/jobs/dispatch.rs:373; writer/folio.rs:519 | có | thấp | none |
| Redeem, single `voucher` (AmountOff, PercentOff with cap, idempotent by RefNumber) | MM-MEMBERSHIP/src/MM.Membership.Agent/Jobs/JobDispatcher.cs:293 | DevLibrary/crates/dl-agent-membership/src/jobs/dispatch.rs:462 | có | thấp | none |
| Redeem, stacked `vouchers` array (hub `AppliedAmount`, one line per voucher, all-or-manual) | MM-MEMBERSHIP/src/MM.Membership.Agent/Jobs/JobDispatcher.cs:299, :380; Jobs/JobPayload.cs:198 | — (DevLibrary/crates/dl-agent-membership/src/jobs/payload.rs:229 reads `voucher` only) | thiếu | cao: one PercentOff voucher is posted from the folio's first-night rate instead of the hub's whole-stay `AppliedAmount` (guest pays the folio); several vouchers go to manual | port `ParseVouchers` + `HandleStackedRedeemAsync` with golden SQL |
| GrantReward (Spa/Fnb credit, Late/Early no-op, Upgrade/FreeNight manual) | MM-MEMBERSHIP/src/MM.Membership.Agent/Jobs/JobDispatcher.cs:435 | DevLibrary/crates/dl-agent-membership/src/jobs/dispatch.rs:525 | có | thấp | none |
| EarnPoints job | MM-MEMBERSHIP/src/MM.Membership.Agent/Jobs/JobDispatcher.cs:522 | DevLibrary/crates/dl-agent-membership/src/jobs/dispatch.rs:574; writer/smile_writer.rs:157 | có | thấp: golden bodies | none |
| LinkFolio | MM-MEMBERSHIP/src/MM.Membership.Agent/Jobs/JobDispatcher.cs:117; Smile/FolioWriter.Link.cs:24 | DevLibrary/crates/dl-agent-membership/src/jobs/dispatch.rs:580; writer/link.rs:118 | có | thấp | none |
| Availability job (ack only) | MM-MEMBERSHIP/src/MM.Membership.Agent/Jobs/JobDispatcher.cs:104 | DevLibrary/crates/dl-agent-membership/src/jobs/dispatch.rs:158 | có | thấp | none |
| Member profile (GetProfileNumber, legacy adopt, fill blank) | MM-MEMBERSHIP/src/MM.Membership.Agent/Smile/ProfileWriter.cs:47, :150, :177 | DevLibrary/crates/dl-agent-membership/src/writer/profile.rs:158, :219, :255 | có | thấp | none |
| Travel agent read + sanitize | MM-MEMBERSHIP/src/MM.Membership.Agent/Smile/TravelAgentReader.cs:87, :138 | DevLibrary/crates/dl-agent-membership/src/writer/travel_agent.rs:27; jobs/travel_agent.rs:67 | có | thấp | none |
| Guest data resolve (split name, gender) | MM-MEMBERSHIP/src/MM.Membership.Agent/Jobs/GuestDataResolver.cs:29 | DevLibrary/crates/dl-agent-membership/src/jobs/guest.rs:56 | có | thấp | none |
| Applock + lock timeout | MM-MEMBERSHIP/src/MM.Membership.Agent/Smile/AppLock.cs:53, :96 | DevLibrary/crates/dl-agent-membership/src/writer/applock.rs:27, :79 | có | thấp | none |
| Post discount / has posted ref | MM-MEMBERSHIP/src/MM.Membership.Agent/Smile/FolioWriter.cs:302, :346 | DevLibrary/crates/dl-agent-membership/src/writer/post.rs:31, :73 | có | thấp | none |
| Hand-entered duplicate guard (TCVN3 decode of LastName) | MM-MEMBERSHIP/src/MM.Membership.Agent/Smile/FolioWriter.DuplicateGuard.cs:23, :47 | DevLibrary/crates/dl-agent-membership/src/writer/lookup.rs:75, :92 (dl-vn decode) | khác | thấp: dl-vn table instead of the agent's `Tcvn3.Decode` (name compare only) | none |
| TCVN3 encode of names / notes on write | MM-MEMBERSHIP/src/MM.Membership.Agent/Smile/Tcvn3.cs:55; FolioWriter.Reservation.cs:156 | DevLibrary/crates/dl-agent-membership/src/writer/tcvn3.rs; writer/additional_name.rs:80 | có | thấp | none |
| SMILE optional-column whitelist (schema probe) | MM-MEMBERSHIP/src/MM.Membership.Agent/Smile/SmileSchemaProbe.cs:57, :118 | DevLibrary/crates/dl-agent-membership/src/writer/schema.rs:107, :153 | có | thấp | none |
| Extra-bed SMILE_SPECIAL preflight + Folio_Special rows | MM-MEMBERSHIP/src/MM.Membership.Agent/Smile/ExtraBedPreflight.cs; Smile/FolioWriter.ExtraBedSpecial.cs:73 | DevLibrary/crates/dl-agent-membership/src/jobs/preflight.rs:73; writer/special_rows.rs:91 | có | thấp: golden SQL | none |
| Extra-bed terms + SpecialService catalogue reports | MM-MEMBERSHIP/src/MM.Membership.Agent/Sync/ExtraBedTermsSync.cs:45; Worker/CheckoutScanner.cs:266 | DevLibrary/crates/dl-agent-membership/src/terms/mod.rs:258; sync.rs:109 | khác | thấp: only when writes are on (.NET always) | none |
| Earn pair resolve (override, ambiguous) | MM-MEMBERSHIP/src/MM.Membership.Agent/Sync/EarnPairSource.cs:58 | DevLibrary/crates/dl-agent-membership/src/earn/pairs.rs:52 | có | thấp | none |
| Earn pair 24 h drift check (log only) | MM-MEMBERSHIP/src/MM.Membership.Agent/Sync/EarnPairSource.cs:116 | — | thiếu | thấp: log only | port with the diagnostics work |
| Checkout scanner (pending bookings, earn, revenue audit, summary) | MM-MEMBERSHIP/src/MM.Membership.Agent/Worker/CheckoutScanner.cs:239, :343, :325 | DevLibrary/crates/dl-agent-membership/src/checkout/scanner.rs:169, :215 | khác | thấp: default off while `read_only`; pauses with the other scans on identity mismatch (hub keeps pending) | none |
| PMS-cancelled report (older hub → off) | MM-MEMBERSHIP/src/MM.Membership.Agent/Worker/CheckoutScanner.cs:451 | DevLibrary/crates/dl-agent-membership/src/checkout/scanner.rs:356 | có | thấp | none |
| Walk-in earn by phone | MM-MEMBERSHIP/src/MM.Membership.Agent/Worker/CheckoutScanner.cs:506 | DevLibrary/crates/dl-agent-membership/src/checkout/scanner.rs:414, :498 | có | thấp | none |
| Availability scanner (probe, full push every 24, horizon 365) | MM-MEMBERSHIP/src/MM.Membership.Agent/Worker/AvailabilityScanner.cs:208, :230 | DevLibrary/crates/dl-agent-membership/src/availability/mod.rs:120, :146 | có | thấp | none |
| Group blocks / physical rooms / extra-bed usage | MM-MEMBERSHIP/src/MM.Membership.Agent/Worker/AvailabilityScanner.GroupBlocks.cs; AvailabilityScanner.PhysicalRooms.cs; AvailabilityScanner.ExtraBedUsage.cs:21 | DevLibrary/crates/dl-agent-membership/src/availability/count.rs:117, :173; availability/mod.rs:271 | có | thấp | none |
| Room inventory + room types (housekeeping derivation) | MM-MEMBERSHIP/src/MM.Membership.Agent/Worker/RoomInventoryScanner.cs:68; Worker/HousekeepingDerivation.cs:21; Smile/RoomTypeCatalogReader.cs:26 | DevLibrary/crates/dl-agent-membership/src/rooms.rs:22, :131 | có | thấp | none |
| Folio observation scanner (cursor moves after 2xx, 202 held) | MM-MEMBERSHIP/src/MM.Membership.Agent/Worker/FolioObservationScanner.cs:155 | DevLibrary/crates/dl-agent-membership/src/folio/scanner.rs:105 | có | thấp | none |
| Folio cursor store (v1 adopt, identity key) | MM-MEMBERSHIP/src/MM.Membership.Agent/Worker/FolioObservationCursorStore.cs:90, :128 | DevLibrary/crates/dl-agent-membership/src/folio/cursor.rs:148, :184 | có | thấp | none |
| Folio poster (halve on timeout, hash dedup) | MM-MEMBERSHIP/src/MM.Membership.Agent/Worker/FolioObservationPoster.cs:63 | DevLibrary/crates/dl-agent-membership/src/folio/poster.rs:71 | có | thấp | none |
| Hub endpoints availability / room-inventory / room-types / folio-observations | MM-MEMBERSHIP/src/MM.Membership.Agent/Hub/HubClient.cs:194, :201, :208, :172 | DevLibrary/crates/dl-agent-membership/src/hub/mod.rs:384, :392, :400, :404 | có | thấp | none |
| Hub endpoints earn, walk-in, pending-checkout, pms-cancelled, folio-revenue, summary | MM-MEMBERSHIP/src/MM.Membership.Agent/Hub/HubClient.cs:127, :142, :96, :109, :230, :237 | DevLibrary/crates/dl-agent-membership/src/hub/earn.rs:61, :70, :87, :97, :114, :123 | có | thấp: golden bodies | none |
| Hub endpoints extra-bed-terms / special-services (404/405 → false) | MM-MEMBERSHIP/src/MM.Membership.Agent/Hub/HubClient.cs:218, :244 | DevLibrary/crates/dl-agent-membership/src/hub/mod.rs:368, :376 | có | thấp | none |
| Update check/status/logs (`update/*`), maintenance window, PILOT/GENERAL | MM-MEMBERSHIP/src/MM.Membership.Agent/Hub/HubClient.cs:268, :279, :300; Update/AgentUpdateWorker.cs:57 | DevLibrary/crates/dl-agent/src/agent_update.rs:50; channel.rs | khác | thấp: dl-agent's own channel | none |
| SMILE diagnostics (periodic, after failed job, on hub request) | MM-MEMBERSHIP/src/MM.Membership.Agent/Worker/SmileDiagnosticsCoordinator.cs:51, :65, :80; Smile/SmileDiagnosticsProbe.cs:62 | — (DevLibrary/crates/dl-agent-membership/src/sync.rs:290 logs the request) | thiếu | trung: no remote view of SQL rights, orphaned users, broken references, triggers (the VIC-SGN 2026-10-07 class of incident) | port the read-only probe + coordinator, carry it in the heartbeat |
| File logs (Serilog, rolling) | MM-MEMBERSHIP/src/MM.Membership.Agent/Observability/AgentSerilogSetup.cs:63 | DevLibrary/crates/dl-agent/src/cli.rs:379 | có | thấp | none |
| Liveness file (container healthcheck) | MM-MEMBERSHIP/src/MM.Membership.Agent/Worker/LivenessFile.cs:36 | DevLibrary/crates/dl-agent/src/watchdog.rs:130 (heartbeat file) | khác | thấp: container-only feature | none |
| Commands (new) `membership.status` | — | DevLibrary/crates/dl-agent-membership/src/module/mod.rs:478 | khác | thấp | none |

### Lỗ hổng (Membership)

1. **Stacked `vouchers` in REDEEM jobs** — cao (money at the counter). .NET `58a4524` (2026-11-19) posts each voucher at the hub's `AppliedAmount` (whole stay), one line per voucher, all-or-manual when an amount is missing. The Rust module reads only the legacy `voucher`: for one PercentOff voucher it posts `percent × live folios' first-night RateAmount` (the old .NET bug the change fixed); with several vouchers the hub sends `voucher = null` and the job goes manual (safe). Work: port `ParseVouchers` (sequence order, non-object entry = format error) and `HandleStackedRedeemAsync` (per-voucher `HasPostedRef` skip, `MemberDiscountCode` checks) with golden SQL and a fake-hub test; ≈ 3 h; DevLibrary only, but it changes posted amounts → not fixed in this stream (brief). Must land before Victory/Mũi Né Bay run with `read_only: false`.
2. **SmileDiagnostics** — trung (ops, no money). Port `SmileDiagnosticsProbe` (read-only catalog queries, 10 s per query, 30 s per run) + the coordinator (startup, periodic, after a failed job, hub request) and send the result in the heartbeat `Diagnostics` field; also the earn-pair drift check and `check-smile --diagnose` as a command. ≈ 4–6 h; heartbeat body shape must match the hub (golden from .NET).
3. **Heartbeat tied to the pump** — trung. A failed pull returns before the heartbeat, a slow batch delays it; .NET runs an independent heartbeat loop. Work: heartbeat thread with its own SMILE probe connection; ≈ 2 h; DevLibrary only.
4. Verification: real DPAPI `config.dpapi`, `--switch` SCM calls, Victory SMILE shapes (INVENTORY §4b).

## 8. HotelLink (`SMILE-HLAPI/go/cmd/agent` + `go/internal/agent|pms`, Go) → `dl-agent-hotellink`

| function | old agent (path:line) | dl-agent (path:line) | status | rủi ro tiền-dữ liệu | việc cần làm |
|---|---|---|---|---|---|
| Windows service `SmileHLAgent` + console mode | `SMILE-HLAPI/go/cmd/agent/main.go:52`, `SMILE-HLAPI/go/internal/agent/service_windows.go:16` | core service + `DevLibrary/crates/dl-agent-hotellink/src/module/mod.rs:74` (opt-in) | khác | thấp | none |
| SCM recovery (5 s/30 s/60 s) | `SMILE-HLAPI/go/deploy/agent-install.ps1:53` | core | khác | thấp | none |
| Installer (copy exe, sample config, service) / hub `/install` with dl option | `SMILE-HLAPI/go/deploy/agent-install.ps1:1`, `SMILE-HLAPI/go/internal/hub/dlinstall.go:3` | `DevLibrary/services/agent-hub/src/install/script.ts` (`-Setup hl-enroll -Replace SmileHLAgent`) | có | thấp | none |
| `-version`, `-config` flags | `SMILE-HLAPI/go/cmd/agent/main.go:32`, `:33` | core `dl-agent --version`; config path `modules.hotellink.config_path` / `SMILEHL_AGENT_CONFIG` `DevLibrary/crates/dl-agent-hotellink/src/config.rs:161` | khác | thấp | none |
| Config file `%ProgramData%\SMILE-HL-Agent\config.json` (hubUrl, hubUrlFallbacks, hotelId, token, instanceId, sql.*) | `SMILE-HLAPI/go/internal/agent/config.go:21`, `:84` | `DevLibrary/crates/dl-agent-hotellink/src/config.rs:128`, `:198` (reads the Go file in place) | có | thấp | none |
| DPAPI `enc:` secrets; plaintext encrypted on first load | `SMILE-HLAPI/go/internal/agent/config.go:221`, `:266`, `SMILE-HLAPI/go/internal/agent/dpapi_windows.go:46` | `DevLibrary/crates/dl-agent-hotellink/src/config.rs:251`, `:315`, `:320` | có | thấp (Windows DPAPI on bench only) | Windows check |
| Sample config written when absent | `SMILE-HLAPI/go/internal/agent/config.go:138` | `DevLibrary/crates/dl-agent-hotellink/src/enroll.rs:24` (`dl-agent hl-enroll`), `DevLibrary/crates/dl-agent-hotellink/src/config.rs:284` | khác (better: enrol tool, secrets from env/stdin) | thấp | none |
| Config validation | `SMILE-HLAPI/go/internal/agent/config.go:147` | `DevLibrary/crates/dl-agent-hotellink/src/config.rs:109`; degraded and rechecked every 30 s `DevLibrary/crates/dl-agent-hotellink/src/module/mod.rs:117` | có | thấp | none |
| SQL connection string (trusted / SQL login / override) | `SMILE-HLAPI/go/internal/agent/config.go:200` | `DevLibrary/crates/dl-agent-hotellink/src/config.rs:57`, SqlClient-style override `DevLibrary/crates/dl-agent-hotellink/src/config/sqlclient.rs:22`, dl-tds ladder `DevLibrary/crates/dl-agent-hotellink/src/module/mod.rs:110` | có | thấp (SSPI trusted connection on bench only) | Windows check |
| `ui.*` (port 8088), `configToken` (CSRF) | `SMILE-HLAPI/go/internal/agent/config.go:28`, `:164`, `:235` | ignored | thiếu (see config UI row) | — | — |
| `update.*` (enabled, checkIntervalMin, window) | `SMILE-HLAPI/go/internal/agent/config.go:44`, `:191` | core `update.window` / `update.auto` (site config), `no-update` file | khác | thấp: a hotel's window in `config.json` is not carried | cutover: copy `update.window` into the hub site config |
| Rolling log files, 31 days | `SMILE-HLAPI/go/cmd/agent/main.go:40`, `SMILE-HLAPI/go/internal/agent/logfile.go:22` | `dl-log` via core (10 MB × 5), `DevLibrary/crates/dl-agent-hotellink/src/log.rs:7` | khác | thấp: shorter history | none |
| Wait for SQL at boot (1 s → 60 s) | `SMILE-HLAPI/go/cmd/agent/main.go:123` | `DevLibrary/crates/dl-agent-hotellink/src/run/mod.rs:356` | có | thấp | none |
| Seed `GlobalParameter.HLInventorySync=1` when absent | `SMILE-HLAPI/go/cmd/agent/main.go:141`, `SMILE-HLAPI/go/internal/pms/repo.go:358` | `DevLibrary/crates/dl-agent-hotellink/src/run/mod.rs:385`, `DevLibrary/crates/dl-agent-hotellink/src/pms/repo.rs:437` | có | trung (turns availability push on) | none |
| Create/seed `RoomTypeMapped` from `RoomType` | `SMILE-HLAPI/go/cmd/agent/main.go:149`, `SMILE-HLAPI/go/internal/pms/repo.go:376` | `DevLibrary/crates/dl-agent-hotellink/src/pms/repo.rs:536` | có | trung | none |
| Runner: policy cache seed, initial mapping push, 3 supervised loops | `SMILE-HLAPI/go/internal/agent/run.go:108` | `DevLibrary/crates/dl-agent-hotellink/src/run/mod.rs:311` | có | thấp | none |
| Loop supervision + panic recovery | `SMILE-HLAPI/go/internal/agent/supervise.go:22`, `SMILE-HLAPI/go/internal/agent/gosafe.go:12` | `DevLibrary/crates/dl-agent-hotellink/src/run/mod.rs:460` | có | thấp | none |
| Loop liveness (stalled loops in heartbeat) | `SMILE-HLAPI/go/internal/agent/liveness.go:24`, `:60` | `DevLibrary/crates/dl-agent-hotellink/src/liveness.rs:11`, `:40` | có | thấp | none |
| Booking loop: long poll `GET /api/agent/jobs?wait=25`, backoff 1 s → 60 s | `SMILE-HLAPI/go/internal/agent/run.go:159`, `SMILE-HLAPI/go/internal/agent/hubclient.go:198` | `DevLibrary/crates/dl-agent-hotellink/src/run/booking.rs:26`, `DevLibrary/crates/dl-agent-hotellink/src/hubclient.rs:276` | có | thấp | none |
| Durable local queue `queue.jsonl` (enqueue, applied, acked, compact) | `SMILE-HLAPI/go/internal/agent/jobqueue/file.go:28`, `:73`, `:85`, `:101`, `:140` | `DevLibrary/crates/dl-agent-hotellink/src/jobqueue.rs:94`, `:162`, `:179`, `:205`, `:239` | có | cao if wrong (lost booking); RT-0259 crash e2e | none |
| Import the Go agent's unfinished queue once | (n/a) | `DevLibrary/crates/dl-agent-hotellink/src/jobqueue.rs:257`, `DevLibrary/crates/dl-agent-hotellink/src/module/mod.rs:166` | khác (better) | thấp | none |
| Process job (hotel date check, apply, ack) | `SMILE-HLAPI/go/internal/agent/run.go:207` | `DevLibrary/crates/dl-agent-hotellink/src/run/booking.rs:122` | có | cao (bookings); e2e | none |
| Ack `POST /api/agent/jobs/{id}/ack` with last 20 errors + PMS result | `SMILE-HLAPI/go/internal/agent/run.go:261`, `SMILE-HLAPI/go/internal/agent/hubclient.go:209` | `DevLibrary/crates/dl-agent-hotellink/src/run/booking.rs:228`, `DevLibrary/crates/dl-agent-hotellink/src/hubclient.rs:293`, errors ring `DevLibrary/crates/dl-agent-hotellink/src/run/mod.rs:433` | có | thấp | none |
| Re-ack lost acks / drain held jobs after PMS down | `SMILE-HLAPI/go/internal/agent/run.go:269`, `:283` | `DevLibrary/crates/dl-agent-hotellink/src/run/booking.rs:253`, `:286` | có | cao if wrong; e2e ack replay | none |
| PMS breaker (5 failures, 30 s → 5 min) | `SMILE-HLAPI/go/internal/agent/run.go:89`, `SMILE-HLAPI/go/internal/agent/breaker/breaker.go:26` | `DevLibrary/crates/dl-agent-hotellink/src/breaker.rs:20`, `:65`, `:87` | có | thấp (e2e with SQL down) | none |
| PMS-down classification | `SMILE-HLAPI/go/internal/agent/pmserr.go:15` | `DevLibrary/crates/dl-agent-hotellink/src/error.rs:76` | có | thấp | none |
| Single-active: 403 superseded stops every loop | `SMILE-HLAPI/go/internal/agent/run.go:146`, `SMILE-HLAPI/go/internal/agent/hubclient.go:171` | `DevLibrary/crates/dl-agent-hotellink/src/run/mod.rs:405`, `DevLibrary/crates/dl-agent-hotellink/src/hubclient.rs:247` | khác | thấp: Go process exits; module stays degraded (no restart churn) | none |
| Hub URL fallbacks + `smile-hl-hub.fly.dev` | `SMILE-HLAPI/go/internal/agent/hubclient.go:51`, `:55` | `DevLibrary/crates/dl-agent-hotellink/src/hubclient.rs:18`, `:137`, `DevLibrary/crates/dl-agent-hotellink/src/module/mod.rs:161` | có | thấp | none |
| Reconcile: apply new/modify/cancel in one transaction | `SMILE-HLAPI/go/internal/agent/reconcile.go:167`, `:194`, `:235` | `DevLibrary/crates/dl-agent-hotellink/src/reconcile/mod.rs:160`, `:189`, `:241` | có | cao (Folio/FolioRate money); SQL e2e | none |
| Find existing folios (ext confirm no., booker) | `SMILE-HLAPI/go/internal/agent/reconcile.go:118`, `SMILE-HLAPI/go/internal/pms/repo.go:50`, `:57` | `DevLibrary/crates/dl-agent-hotellink/src/reconcile/mod.rs:139`, `DevLibrary/crates/dl-agent-hotellink/src/pms/repo.rs:216`, `:220` | có | cao | none |
| Create rooms / diff / update folio / diff rates | `SMILE-HLAPI/go/internal/agent/reconcile.go:363`, `:433`, `:738`, `:827` | `DevLibrary/crates/dl-agent-hotellink/src/reconcile/diff.rs:38`, `:107`, `:268`, `:373` | có | cao | none |
| Split items / merge rooms by item | `SMILE-HLAPI/go/internal/agent/reconcile.go:634`, `:652` | `DevLibrary/crates/dl-agent-hotellink/src/reconcile/build.rs:39`, `:55` | có | cao | none |
| Manual-change and user-cancel rules, protected folios, guards (room lock, checked-in) | `SMILE-HLAPI/go/internal/agent/reconcile.go:414`, `:869`, `:887`, `:904`, `:913`, `SMILE-HLAPI/go/internal/pms/repo.go:109` | `DevLibrary/crates/dl-agent-hotellink/src/reconcile/mod.rs:369`, `:382`, `:392`, `:404`, `:411`, `DevLibrary/crates/dl-agent-hotellink/src/pms/repo.rs:227` | có | cao | none |
| Cancel policy from hub + `policy-cache.json` | `SMILE-HLAPI/go/internal/agent/reconcile.go:85`, `SMILE-HLAPI/go/internal/agent/policycache.go:18`, `:29` | `DevLibrary/crates/dl-agent-hotellink/src/policycache.rs:14`, `:20`, `DevLibrary/crates/dl-agent-hotellink/src/run/side.rs:101` | có | trung | none |
| Travel-agent lookup (`CLIENT`, `ClientContacts`, `ClientSts`) | `SMILE-HLAPI/go/internal/agent/reconcile.go:929`, `SMILE-HLAPI/go/internal/pms/repo.go:539`, `:561`, `:575` | `DevLibrary/crates/dl-agent-hotellink/src/reconcile/mod.rs:421`, `DevLibrary/crates/dl-agent-hotellink/src/pms/repo.rs:455`, `:469`, `:487` | có | trung | none |
| Build Folio defaults | `SMILE-HLAPI/go/internal/agent/reconcile.go:952`, `SMILE-HLAPI/go/internal/pms/entities.go:124` | `DevLibrary/crates/dl-agent-hotellink/src/reconcile/build.rs:144`, `DevLibrary/crates/dl-agent-hotellink/src/pms/entities.rs:307` | có | cao | none |
| Folio numbers `EXEC GetFolioNum` / new confirm no. | `SMILE-HLAPI/go/internal/pms/repo.go:236`, `:241` | `DevLibrary/crates/dl-agent-hotellink/src/pms/repo.rs:261`, `:265` | có | cao | none |
| Insert Folio + FolioRate + AdditionalName (reflection insert) | `SMILE-HLAPI/go/internal/pms/repo.go:291`, `SMILE-HLAPI/go/internal/pms/insert.go:12` | `DevLibrary/crates/dl-agent-hotellink/src/pms/repo.rs:280`, `DevLibrary/crates/dl-agent-hotellink/src/pms/insert.rs:10` | có | cao | none |
| Update folio fields / replace rates / update AdditionalName / cancel folios | `SMILE-HLAPI/go/internal/pms/repo.go:159`, `:208`, `:226`, `:314` | `DevLibrary/crates/dl-agent-hotellink/src/pms/repo.rs:339`, `:390`, `:407`, `:314` | có | cao | none |
| Truncation diagnosis (column lengths) | `SMILE-HLAPI/go/internal/pms/truncation.go:20`, `:82` | `DevLibrary/crates/dl-agent-hotellink/src/pms/insert.rs:29`, `:36`, `DevLibrary/crates/dl-agent-hotellink/src/pms/repo.rs:137` | có | thấp | none |
| Hotel date (`HotelParameter`) | `SMILE-HLAPI/go/internal/pms/repo.go:416` | `DevLibrary/crates/dl-agent-hotellink/src/pms/repo.rs:513` | có | trung | none |
| Mapping push `POST /api/agent/mapping` with hash | `SMILE-HLAPI/go/internal/agent/run.go:305`, `:396`, `SMILE-HLAPI/go/internal/agent/hubclient.go:219` | `DevLibrary/crates/dl-agent-hotellink/src/run/booking.rs:304`, `DevLibrary/crates/dl-agent-hotellink/src/jobmodel/mod.rs:357`, `DevLibrary/crates/dl-agent-hotellink/src/hubclient.rs:317` | có | thấp | none |
| Mapping commands from the hub + ack `/api/agent/mapping-cmds/{id}/ack` | `SMILE-HLAPI/go/internal/agent/run.go:335`, `:374`, `SMILE-HLAPI/go/internal/agent/hubclient.go:204` | `DevLibrary/crates/dl-agent-hotellink/src/run/booking.rs:336`, `:408`, `DevLibrary/crates/dl-agent-hotellink/src/hubclient.rs:282` | có | trung | none |
| `RoomTypeMapped` insert/update/delete + validation | `SMILE-HLAPI/go/internal/pms/mapping.go:18`, `:70`, `:121`, `:219` | `DevLibrary/crates/dl-agent-hotellink/src/pms/mapping.rs:29`, `DevLibrary/crates/dl-agent-hotellink/src/pms/repo.rs:667`, `:671`, `:675` | có | trung | none |
| Availability loop: `GET /api/agent/config`, gate `HLInventorySync`, `EXEC AvailabilityChart`, drop past rows, `POST /api/agent/availability` (or disabled) | `SMILE-HLAPI/go/internal/agent/run.go:412`, `:547`, `:593`, `:644`, `SMILE-HLAPI/go/internal/pms/repo.go:468`, `SMILE-HLAPI/go/internal/agent/hubclient.go:225`, `:231`, `:246` | `DevLibrary/crates/dl-agent-hotellink/src/run/side.rs:32`, `:70`, `:131`, `:23`, `DevLibrary/crates/dl-agent-hotellink/src/pms/repo.rs:600`, `DevLibrary/crates/dl-agent-hotellink/src/hubclient.rs:328`, `:338`, `:349` | có | trung (overbooking if wrong) | none |
| Available-room formula | `SMILE-HLAPI/go/internal/pms/repo.go:462` | `DevLibrary/crates/dl-agent-hotellink/src/pms/mod.rs:114` | có | trung | none |
| Heartbeat `POST /api/agent/heartbeat` every 30 s (version, pmsOk, breaker, queue, errors, capabilities, loopsStalled) | `SMILE-HLAPI/go/internal/agent/run.go:438`, `:520`, `SMILE-HLAPI/go/internal/agent/hubclient.go:253` | `DevLibrary/crates/dl-agent-hotellink/src/run/side.rs:191`, `DevLibrary/crates/dl-agent-hotellink/src/hubclient.rs:364` | khác | thấp: `pendingVer` never sent; version is `dl-agent-<ver>` | none |
| Self-update via HL hub (`/api/agent/update`, blob, `update-ack`, AckOnBoot, window, staged swap) | `SMILE-HLAPI/go/internal/agent/run.go:457`, `:489`, `SMILE-HLAPI/go/internal/agent/hubclient.go:284`, `:297`, `:330`, `SMILE-HLAPI/go/cmd/agent/main.go:97`, `SMILE-HLAPI/go/internal/agent/update/apply.go` | core `DevLibrary/crates/dl-agent/src/update.rs` (never calls the HL update routes) | khác | thấp | HL hub: stop offering Go builds to hotels on dl-agent (SMILE-HLAPI repo) |
| Local config UI on 127.0.0.1:8088 (config get/put, test SQL, test hub, status, mapping CRUD, resync) | `SMILE-HLAPI/go/cmd/agent/main.go:101`, `SMILE-HLAPI/go/internal/agent/configui/server.go:75`, `:103` | none (README: not ported) | thiếu | trung: on-site staff lose the local screen to fix the hub token/SQL login and edit `RoomTypeMapped` (mapping drives which rooms get bookings) | port as a hub-side page + `hl.*` commands (mapping CRUD already via hub mapping-cmds), or a small local UI. Size M |
| Module health, metrics, rate-limited reports (`hl_*`) | (Go: logs + heartbeat only) | `DevLibrary/crates/dl-agent-hotellink/src/module/health.rs:67`, `:104`, `:143` | khác (better) | thấp | none |
| Hub commands `hl.status`, `hl.sync-mapping` | `SMILE-HLAPI/go/internal/agent/configui/handler_mapping.go:21` (UI resync) | `DevLibrary/crates/dl-agent-hotellink/src/module/mod.rs:317` | khác | thấp | none |
| Selftest rows | (none) | `DevLibrary/crates/dl-agent-hotellink/src/module/selftest.rs:99` | khác (better) | thấp | none |
| Stop: in-flight hub call cut, SQL call finishes (5 s) | `SMILE-HLAPI/go/internal/agent/service_windows.go:28` (ctx cancel) | `DevLibrary/crates/dl-agent-hotellink/src/hubclient.rs:167`, `DevLibrary/crates/dl-agent-hotellink/src/module/mod.rs:74` | có | thấp | none |

### Lỗ hổng — HotelLink

| # | gap | risk | work | size | depends on |
|---|---|---|---|---|---|
| 8.1 | Local config UI (127.0.0.1:8088) not ported: no on-site screen for token/SQL fixes and `RoomTypeMapped` edits | trung | hub-side screen over `hl.*` commands, or a local UI in the module | M | owner choice; SMILE-HLAPI hub if hub-side |
| 8.2 | `update.window` of `config.json` not carried to the core site config | thấp | cutover checklist (or read it once in `hl-enroll`) | S | none |
| 8.3 | HL hub still offers Go agent builds; heartbeat lacks `pendingVer` | thấp | HL hub: skip update offers for `dl-agent-*` versions | S | SMILE-HLAPI repo |
| 8.4 | Windows checks: DPAPI `enc:` fields, trusted SQL connection (SSPI), real kill -9 of the service | trung | bench | S | SR bench |

## 9. SRBackup agent (`SRBackupAgent`, SRBackup `agent/`) → `dl-agent-backup` (`backup`)

| function | old agent (path:line) | dl-agent (path:line) | status | rủi ro tiền-dữ liệu | việc cần làm |
|---|---|---|---|---|---|
| CLI flags `--server --tenant-id --api-key --agent-id --enroll-key --config` | SRBackup/agent/cmd/agent/main.go:32-37 | DevLibrary/crates/dl-agent-backup/src/enroll/tool.rs:28 (`backup-enroll --hub-url [--force]`, key env/stdin) | khác: no key on argv | thấp | none |
| `--version` | SRBackup/agent/cmd/agent/main.go:50 | core `dl-agent --version` | có (core) | thấp | none |
| `--install-service` / `--uninstall-service` / `--service-status` | SRBackup/agent/cmd/agent/service_windows.go:228,330,360 | core DevLibrary/crates/dl-agent/src/cli_install.rs:216, cli_uninstall.rs, `dl-agent status` | có (core) | thấp | none |
| SCM recovery actions | SRBackup/agent/cmd/agent/service_windows.go:213 | DevLibrary/crates/dl-winsvc/src/scm.rs:398 | có (core) | thấp | none |
| Service mode detection / Execute | SRBackup/agent/cmd/agent/service_windows.go:41,53 | core host | có (core) | thấp | none |
| `--provision-only` (install.ps1 registers then exits) | SRBackup/agent/cmd/agent/main.go:143; scripts/install.ps1:190-206 | DevLibrary/crates/dl-agent-backup/src/enroll/tool.rs:71; core `install.ps1 -Setup backup-enroll -Replace SRBackupAgent` | có | thấp | none |
| Installer `irm …/api/agent/install/<key> \| iex`, update in place | SRBackup/agent/scripts/install.ps1:16 | core install.ps1 (agent hub) | khác: one dl-agent installer | thấp | none |
| Bootstrap register: `POST /agent/register` (tenantId, unauthenticated) | SRBackup/agent/internal/syncloop/syncloop.go:27; internal/hubclient/client.go:97 | none (self-register only) | khác: dropped (unauthenticated route) | thấp | SRBackup hub: consider removing `/register` (other repo) |
| Self-register `POST /agent/self-register` (tenant key) | SRBackup/agent/internal/hubclient/client.go:111 | DevLibrary/crates/dl-agent-backup/src/hub/mod.rs:122; DevLibrary/crates/dl-agent-backup/src/enroll/mod.rs:129 | có | thấp | none |
| Adopt Go identity (agent.json `api_key` `enc:v1:` HKDF MachineGuid) | SRBackup/agent/internal/config/config.go:70,144; internal/secret/secret.go:81,150 | DevLibrary/crates/dl-agent-backup/src/enroll/goagent.rs:97,171,198; DevLibrary/crates/dl-agent-backup/src/enroll/mod.rs:208 | khác: exe-dir candidate not searched | thấp: falls back to self-register = new agent id (orphans hub history link) | gap 5 |
| Config/env `SRBACKUP_SERVER/API_KEY/TENANT_ID/AGENT_ID` | SRBackup/agent/internal/config/config.go:202-215 | none (enrollment file only) | khác: env overrides dropped | thấp | none |
| Config keys `log_level`, `health_port`, `log_ship_url`, `max_concurrent_jobs` | SRBackup/agent/internal/config/config.go:34-45 | DevLibrary/crates/dl-agent-backup/src/module/settings.rs:29 (`concurrency`, `backup_dir`, `stop_grace_seconds`, `poll_interval_seconds`) | khác | thấp | none |
| Identity storage: agent.json with sealed `master_key` | SRBackup/agent/internal/config/config.go:291; internal/secret/sealed.go | DevLibrary/crates/dl-agent-backup/src/enroll/mod.rs:32,83 (`enrollment.enc`, DPAPI machine); master key in memory only DevLibrary/crates/dl-agent-backup/src/core.rs:266 | khác: better | thấp | none |
| hwid (MachineGuid) | SRBackup/agent/internal/hwid/hwid.go:20 | random UUID kept across re-enrol (DevLibrary/crates/dl-agent-backup/src/enroll/mod.rs) | khác | thấp | none |
| Master key `GET /agent/master-key`, refetch once on failed decrypt | SRBackup/agent/internal/syncloop/masterkey.go:36,62 | DevLibrary/crates/dl-agent-backup/src/core.rs:238,266; DevLibrary/crates/dl-agent-backup/src/hub/mod.rs:162 | có | thấp | none |
| Credential decrypt AES-256-GCM / HKDF (hub `encryptCredential`) | SRBackup/agent/internal/hubclient/decrypt.go:17,57 | DevLibrary/crates/dl-agent-backup/src/crypto/ (fixtures), core.rs:238 | có | thấp | none |
| `credentialError` from hub | logged only (SRBackup/agent/internal/syncloop/loop.go:247) | DevLibrary/crates/dl-agent-backup/src/sync/mod.rs:273 (reported Failed) | khác: better | thấp | none |
| Config poll `GET /agent/config`, 60 s, back-off ×2 to 5 min | SRBackup/agent/internal/syncloop/loop.go:136,185,200 | DevLibrary/crates/dl-agent-backup/src/sync/mod.rs:43,71,99,331 | có | thấp | none |
| 401 handling: stop ticking for good | SRBackup/agent/internal/syncloop/loop.go:147 | DevLibrary/crates/dl-agent-backup/src/sync/mod.rs:71 (keeps polling, degraded, `backup_auth_failed`) | khác: re-enrol heals without restart | thấp | none |
| Schedule due check: cron + IANA zone, `lastRun` in memory | SRBackup/agent/internal/scheduler/scheduler.go:18,34; internal/syncloop/loop.go:593 | DevLibrary/crates/dl-agent-backup/src/schedule/mod.rs:11,20; tz table DevLibrary/crates/dl-agent-backup/src/schedule/tz.rs:8,36; runs persisted DevLibrary/crates/dl-agent-backup/src/state/runs.rs:40 | khác: built-in zone table (DST zones refused); last run survives a restart (Go re-ran every schedule after a restart) | trung: a site with a DST zone gets no backups (reported `backup_schedule_invalid`) | gap 4 |
| Enqueue due schedule (server for database, params, dest creds) | SRBackup/agent/internal/syncloop/loop.go:247,297,353,382 | DevLibrary/crates/dl-agent-backup/src/sync/mod.rs:161,213,259; DevLibrary/crates/dl-agent-backup/src/hub/types.rs:239 | có | thấp | none |
| Backup type map Full/Differential/TransactionLog | SRBackup/agent/internal/syncloop/loop.go:372 | DevLibrary/crates/dl-agent-backup/src/backup/mod.rs:32 | có | thấp | none |
| Durable queue (bbolt), idempotent enqueue, 5 min lease, 3 attempts, dead letter | SRBackup/agent/internal/queue/queue.go:71,103,151,228,261,304 | DevLibrary/crates/dl-agent-backup/src/state/queue.rs:17,19,142,152,180,207,233 (sealed file) | có | thấp | none |
| Go queue/history on disk imported at switch | SRBackup/agent/cmd/agent/main.go:202,218 (`jobs.db`, `backups.db`) | none | thiếu (by design) | trung: retention on the new agent does not know backups the Go agent made, so those cloud copies are never expired by the agent | gap 3 |
| Executor N workers, one backup per instance, lease renewal every lease/3 | SRBackup/agent/internal/executor/executor.go:157,185,345; concurrency.go:45 | DevLibrary/crates/dl-agent-backup/src/executor.rs:39,53,157 | có | thấp | none |
| Dead letter reported once | SRBackup/agent/internal/executor/executor.go:291,329 | DevLibrary/crates/dl-agent-backup/src/executor.rs:283 | có | thấp | none |
| Permanent failures retried 3× | SRBackup/agent/internal/executor/executor.go:378 | DevLibrary/crates/dl-agent-backup/src/executor.rs:45 (`is_permanent` → dropped at once) | khác: better | thấp | none |
| Panic recovery per worker / loop (`safe.Go`) | SRBackup/agent/internal/safe/safe.go:35 | DevLibrary/crates/dl-agent/src/supervisor.rs:486 (catch_unwind per module, restart with back-off) | khác: module-level | thấp | none |
| Connect: `encrypt=true;trustservercertificate=true`, 10 s, Windows trusted, named instance | SRBackup/agent/internal/backup/pipeline.go:308,346 | DevLibrary/crates/dl-agent-backup/src/sql/mod.rs:73,148 (dl-tds ladder, SSPI, SQL Browser) | khác: better (TLS ladder reaches SQL 2008 without TLS 1.2) | thấp | SQL 2008 bench (RT-0252) |
| Backup dir = `InstanceDefaultBackupPath`, fallback | SRBackup/agent/internal/backup/pipeline.go:124,275 | DevLibrary/crates/dl-agent-backup/src/backup/pipeline.rs:31,249; module key `backup_dir` | có | thấp | none |
| `BACKUP … WITH COMPRESSION, STATS` spliced text | SRBackup/agent/internal/backup/pipeline.go:138-152 | DevLibrary/crates/dl-agent-backup/src/sql/options.rs:42,77,92 (COMPRESSION only where supported, CHECKSUM, parameters) | khác: better (Go failed on Express/Web) | thấp | none |
| Log backup on SIMPLE recovery | SQL Server error passed through | DevLibrary/crates/dl-agent-backup/src/backup/pipeline.rs:249 (`backup_recovery_model` before SQL) | khác: better | thấp | none |
| BACKUP timeout 4 h | SRBackup/agent/internal/backup/pipeline.go:159 | DevLibrary/crates/dl-agent-backup/src/sql/mod.rs:20 | có | thấp | none |
| SHA-256 checksum + size, halts on error | SRBackup/agent/internal/backup/pipeline.go:166-190,365 | DevLibrary/crates/dl-agent-backup/src/backup/pipeline.rs:299,315 | có | thấp | none |
| Encrypt upload under per-tenant key (`GET /api/agent/backup-key`, `DLBKENC1`) | none (plaintext upload) | DevLibrary/crates/dl-agent-backup/src/backup/pipeline.rs:167; DevLibrary/crates/dl-agent-backup/src/core.rs:129,153; DevLibrary/crates/dl-agent-backup/src/hub/mod.rs:172,184 | khác: new (404 → plaintext as before) | cao: once the hub serves keys, a `.bak.dlbk` copy can only be restored through dl-agent with the tenant key (no manual / portal restore path) | gap 2 |
| Upload GoogleDrive / S3 / OneDrive / Azure | SRBackup/agent/internal/uploader/uploader.go:91; up_gdrive.go:97; up_s3.go:74; up_onedrive.go:133; up_azure.go:173 | DevLibrary/crates/dl-agent-backup/src/upload/gdrive.rs:381; s3.rs:321; onedrive.rs:587; azure.rs:383 | có | thấp | none |
| `LocalFolder`, `CloudflareR2`/`R2` destinations | Go: unknown provider → upload "skipped", backup Succeeded (SRBackup/agent/internal/uploader/uploader.go:111) | DevLibrary/crates/dl-agent-backup/src/upload/local.rs:93; s3.rs (R2 endpoint) | khác: better (Go reported success with no off-site copy) | thấp | none |
| Upload timeout 2 h per upload | SRBackup/agent/internal/backup/pipeline.go:215 | DevLibrary/crates/dl-agent-backup/src/upload/mod.rs:190 (15 min per HTTP request, chunked) | khác | thấp | none |
| Azure SAS token stored in cloud path | SRBackup/agent/internal/uploader/up_azure.go:173 | DevLibrary/crates/dl-agent-backup/src/upload/azure.rs:383 (path without SAS) | khác: better | thấp | none |
| Cleanup: delete local `.bak` after a cloud copy | SRBackup/agent/internal/backup/pipeline.go:240-251 | DevLibrary/crates/dl-agent-backup/src/backup/pipeline.rs (cleanup step) | có | thấp | none |
| Step/progress reporting | SRBackup/agent/internal/hubclient/reporter.go:170,180 (local only) | DevLibrary/crates/dl-agent-backup/src/backup/mod.rs:72-110 (`Steps` in report) | có | thấp | none |
| Report `POST /agent/report` backup / restore / command | SRBackup/agent/internal/hubclient/client.go:146,160,172 | DevLibrary/crates/dl-agent-backup/src/hub/mod.rs:205,258,289,299 | có (+ `encryptionKeyId`) | thấp | none |
| Report outbox (bbolt), drain every 2 min + id store | SRBackup/agent/internal/outbox/outbox.go:75; internal/syncloop/outbox.go:73; internal/hubclient/idstore.go:51 | DevLibrary/crates/dl-agent-backup/src/state/outbox.rs:13,76,91; DevLibrary/crates/dl-agent-backup/src/core.rs:211,219 (drained after each poll, cap 1000) | có | thấp | none |
| Restore requests (local file or cloud download, `WITH REPLACE`) | SRBackup/agent/internal/syncloop/loop.go:414,430; internal/backup/restore.go:28 | DevLibrary/crates/dl-agent-backup/src/sync/work.rs:54; DevLibrary/crates/dl-agent-backup/src/backup/restore.rs:55,98,271 (+ `MOVE` to new names, `.part` then rename) | khác: better; once per request id (Go re-ran each poll) | trung: overwrites a database (REPLACE) — same as Go | none |
| Commands TestConnection / DiscoverDatabases | SRBackup/agent/internal/syncloop/loop.go:527,545 | DevLibrary/crates/dl-agent-backup/src/sync/work.rs:144 | có | thấp | none |
| Discovery `POST /agent/discover` first poll + every 10th | SRBackup/agent/internal/syncloop/loop.go:607; internal/discovery/discovery.go:65,100 | DevLibrary/crates/dl-agent-backup/src/discovery.rs:7,18; DevLibrary/crates/dl-agent-backup/src/sync/mod.rs:46 | có | thấp | none |
| Retention by age / count: cloud copy, local file, history | SRBackup/agent/internal/retention/retention.go:24,50; internal/syncloop/loop.go:648,720 | DevLibrary/crates/dl-agent-backup/src/retention.rs:10; history DevLibrary/crates/dl-agent-backup/src/state/history.rs:131,151 | có | trung: deletes backups (same rules as Go) | none |
| Backup history store (bbolt per schedule) | SRBackup/agent/internal/jobhistory/store.go:79,106,149,178 | DevLibrary/crates/dl-agent-backup/src/state/history.rs:120,131,143,151 | có | thấp | none |
| Log shipping to hub (`/api/v1/agents/{id}/logs`, 2 s batches; `POST /agent/logs`) | SRBackup/agent/internal/logger/logger.go:101-106,225; internal/hubclient/client.go:210 | DevLibrary/crates/dl-agent-backup/src/hub/mod.rs:222 (defined, unused); core reports + `collect_diagnostics` | khác: replaced by dl-agent reports | thấp | none |
| Health server `:9090` `/healthz` (liveness per loop) + `/metrics` | SRBackup/agent/internal/health/health.go:61,97,124; liveness.go:26 | core heartbeat + module metrics DevLibrary/crates/dl-agent-backup/src/report.rs:166,183 | khác: no local HTTP endpoint | thấp | none |
| Windows registry perf counters sink | SRBackup/agent/internal/metrics/sink_windows.go:29,49 | none | thiếu | thấp: monitoring only | none (hub metrics replace) |
| Self-update `GET /agent/update-check`, download, signature, re-exec, rollback | SRBackup/agent/internal/updater/updater.go:97,166,197,389; verify.go:36 | DevLibrary/crates/dl-agent/src/update.rs:120; DevLibrary/crates/dl-agent/src/agent_update.rs:126 | khác: core (never downgrade, probation) | thấp | none |
| Update waits until no backup runs (idle checker) | SRBackup/agent/cmd/agent/main.go:280-283; internal/updater/updater.go:89 | DevLibrary/crates/dl-agent/src/agent_update.rs:126 (`stop_all(drain_wait 30 s)`); backup stop grace DevLibrary/crates/dl-agent-backup/src/module/mod.rs:265 | khác: an update cancels a running BACKUP after the grace; the job is re-queued without spending an attempt | trung: a long backup restarts from zero; with a maintenance window it does not happen in business hours | gap 1 |
| Stop: cancel running BACKUP (TDS attention), re-queue | context cancel (SRBackup/agent/cmd/agent/main.go:289) | DevLibrary/crates/dl-agent-backup/src/module/mod.rs:48,265 | khác: better (job released, no attempt spent) | thấp | none |
| Problem reports to hub | none | DevLibrary/crates/dl-agent-backup/src/report.rs:16,78,120 (9 codes, 15 min, scrubbed) | khác: new | thấp | none |
| Hub commands `backup.status`, `backup.run-now` | none | DevLibrary/crates/dl-agent-backup/src/module/mod.rs:303,320 | khác: new | thấp | none |
| Self-test (hub, servers, BACKUP permission, destinations round trip) | none | DevLibrary/crates/dl-agent-backup/src/module/selftest.rs:31,65 | khác: new | thấp | none |
| Proxy (env only, Go default transport) | Go `http.DefaultTransport` | dl-http `Proxy::FromEnv` (DevLibrary/crates/dl-agent-backup/src/upload/mod.rs:190; hub/mod.rs:68) | có | thấp | optional: dl-agent proxy choice |

### Lỗ hổng (backup)

1. **Update during a running backup** (trung, core): Go applied an update only while idle; dl-agent drains every
   module before the swap (30 s) and the backup module cancels a running BACKUP after its grace, so the job starts
   again from zero after the restart. Work: a module "busy" hint the update path honours (defer up to N hours,
   `urgent` overrides) in `dl-agent` core + the backup module reporting busy while a job runs. ~0.5 day; core API
   change (PROTOCOL §6), not a small fix. Mitigation today: site `update.window`.
2. **Restore path for encrypted copies** (cao, owner decision): when the SRBackup hub starts serving `backup-key`,
   cloud copies become `.bak.dlbk` that only dl-agent can open. Needs an operator restore tool (decrypt with the
   tenant key) or a hub/portal path, plus key escrow rules, before the hub route is turned on. Depends on SRBackup
   (other repo); hub route itself pending (`docs/plans/2026-11-19-rt0259-backup-tenant-keys.md`).
3. **Go history not imported** (trung): retention on dl-agent knows only backups it made; cloud copies made by the
   Go agent are never expired by the agent (storage cost, no data loss). Work: import `backups.db` (bbolt) at
   `backup-enroll` adoption, or a hub-side retention sweep. ~1 day (bbolt reader). Brief said production has no Go
   agents, so low priority unless one exists.
4. **Time zones with DST** (trung, small-ish): schedules in a DST zone are refused (`backup_schedule_invalid`). VN
   sites use `Asia/Ho_Chi_Minh`; a tz database would add a dependency. Owner decision if a non-VN site appears.
5. **Adoption misses `agent.json` next to the Go exe** (thấp): candidates are PROGRAMDATA, APPDATA, LOCALAPPDATA;
   Go also looked beside its exe. On a miss the tool self-registers a new agent id (history link lost on the hub).
   Work: read the old service's `ImagePath` from the registry to add that folder. ~1 h but Windows-only (registry),
   not testable on Linux, so not fixed here.

## Fixes made in this review (PR #68)

Branch `cloud/rt-0260-pa` (worktree `/home/user/wt/pa`), not pushed.

| Commit | What |
|---|---|
| `a0b6912` | feat(dl-agent-membership): every hub call carries a fresh `X-Correlation-Id` (32 hex, like `AgentCorrelationHandler`). Tests: `hub::tests::correlation_ids_are_fresh_32_hex_digits`, `fake_hub::heartbeat_and_sync_config_carry_the_token_and_the_hotel` (header present, one id per request). |
| `a98d679` | fix(dl-agent-arrival, dl-agent-membership): the Check-In cloud link, its self-test probe, the Membership hub client and `membership-enroll` use `dl_agent::proxy::current()` (environment → WinHTTP → Internet Options) instead of the environment only, as .NET HttpClient did. Tests: `agent::cloud::tests::the_link_uses_the_core_proxy_choice`, `hub::tests::the_hub_client_uses_the_core_proxy_choice`. |
| `11ee78c` | docs: arrival README (commands, problem journal, proxy), membership README "Not ported yet" (stacked vouchers, SmileDiagnostics; TCVN3 is ported), CHANGELOG Fixed entry. |

Gates (`CARGO_TARGET_DIR=/home/user/wt/target-pa`):

- `cargo fmt --all --check`: clean.
- `cargo clippy -p dl-agent-arrival -p dl-agent-membership --all-targets -- -D warnings`: clean; same with `--target x86_64-pc-windows-gnu`: clean.
- `cargo test -p dl-agent-arrival -p dl-agent-membership`: all passed (membership lib 209, arrival lib 84, every integration test binary green; live SQL tests skip without their env variables).

Not run: live SQL Server tests, Windows (DPAPI/SCM/Schannel), real clouds.

Branch `cloud/rt-0260-pb` (worktree `/home/user/wt/pb`), not pushed:

| commit | what | tests |
|---|---|---|
| `4d31827` fix(pda-sync): put the module's logs in the diagnostics bundle | `module/mod.rs` `name_diagnostics` adds `logs/` with `add_diagnostic_folder`; plan row 4 ✅ | new `module::tests::the_diagnostics_bundle_gets_status_runtime_config_and_the_log_folder` |
| `d573278` fix(pda-sync): reach the PDA cloud through dl-agent's proxy choice | `transport/http.rs` `proxy()` = `dl_agent::proxy::current()` for HTTP + command WebSocket; print relay hub client (`printrelay/service.rs` `hub_client_options`); speaker hub + sound-mode client (`hub.rs` `http_options`) | new `transport::http::tests`, `printrelay::service::tests`, speaker `hub::tests::hub_and_sound_modes_use_the_pda_sync_proxy` |
| `a199e7c` fix(speaker): pick a voice of any English culture | `phrase.rs` `voice_lcids` (409, 809, then every English LCID), used by `sapi.rs`; README | new `phrase::tests::voices_of_every_english_culture_are_tried_us_and_uk_first` |

Gates (`CARGO_TARGET_DIR=/home/user/wt/target-pb`): `cargo fmt --all --check` ok;
`cargo clippy -p dl-agent-pda-sync -p dl-agent-speaker --all-targets -- -D warnings` ok, same with
`--target x86_64-pc-windows-gnu` ok; `cargo test -p dl-agent-pda-sync` all suites ok (lib 407, no failure;
live SQL tests skipped without `PDA_SYNC_TEST_SQL`); `cargo test -p dl-agent-speaker` ok (lib 26, hub_fake 3).
`dl-agent-hotellink` not touched.

- `cedabd1` fix(dl-agent-pms-device): use dl-agent's proxy choice for the device API. `ApiCommandQueue::with_proxy`
  (new; `new` keeps `Proxy::FromEnv`), the module passes `dl_agent::proxy::current().proxy` (env, then WinHTTP,
  then Internet Options), matching the .NET agent's system proxy. Test
  `tests/api_command_queue.rs::with_proxy_sends_the_claim_through_the_proxy` (the fake API plays the proxy and
  receives the absolute-form claim). README differences row and CHANGELOG (Fixed) updated.
- Gates (`CARGO_TARGET_DIR=/home/user/wt/target-pc`): `cargo fmt --all --check` clean;
  `cargo clippy -p dl-agent-pms-device --all-targets -- -D warnings` clean, also with
  `--target x86_64-pc-windows-gnu`; `cargo test -p dl-agent-pms-device` 253 passed, 0 failed.
- Not run: SQL-gated tests (no SQL Server here), Windows run, hardware. pms-site and backup crates untouched.
