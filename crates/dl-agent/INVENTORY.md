# On-prem agent inventory and requirements (2026-11-18)

Source survey for `dl-agent`: every on-prem agent in the current projects, what it does, and what the
shared agent must cover before that project can switch. One site runs **one** `dl-agent`; each product
is a **module**. Business data still goes from the module to its own product cloud; lifecycle
(enroll, heartbeat, commands, reports, releases, canary) goes to the shared agent hub (PROTOCOL.md).

## 1. Agents to replace

| # | Project | Agent today | Runtime | Data touched | Module name |
|---|---------|-------------|---------|--------------|-------------|
| 1 | MM-EINVOICE | `cmd/agent` (Go, live) + `rust/agent` (Rust on DevLibrary, not shipped) | Go / Rust | SMILE_FO, SMILE_POS (read via procs, write-back `MMEI_Collected`) | `einvoice` |
| 2 | SML-PDAOrder | `apps/sql-sync-agent` (.NET 10, live) | C# | SMILE_POS (capture, exactly-once commands), legacy `pda` schema | `pda-sync` |
| 3 | SML-PDAOrder | `apps/speaker-agent` (Go, live) | Go | none (SignalR, WAV/SAPI, print relay) | `speaker` (fold into `pda-sync` print relay) |
| 4 | SMILE-PMS | `apps/site-agent` (Go + Rust port on DevLibrary) | Go / Rust | SMILE_FO, SMILE_BO, SMILE_POS (allow-listed ops; MM_PMS is cloud-only) | `pms-site` |
| 5 | SMILE-PMS | `apps/device-agent` (.NET 10 + Rust port) | C# / Rust | door locks (LockSDK x86), ESC/POS, serial, PC/SC, OPOS, SAPI | `pms-device` |
| 6 | SmileCheckin | `src/MM.Arrival.Agent` (.NET 10, live) | C# | SMILE_FO (`srCI_*` procs), SMILE_PICTURE, LegalVault | `arrival` |
| 7 | MM-MEMBERSHIP | `src/MM.Membership.Agent` (.NET 10, live at VIC-SGN, VICVT) | C# | SMILE_FO (jobs, checkout/availability/folio scanners) | `membership` |
| 8 | SMILE-HL (HLAPI) | `go/cmd/agent` (Go, live) | Go | SMILE_FO (`Folio`, `RoomTypeMapped`, availability proc) | `hotellink` |
| 9 | SRBackup | `agent` (Go) | Go | any SQL Server: BACKUP/RESTORE, uploads to Drive/OneDrive/S3/Azure | `backup` |

Not replaceable / out of scope: wifi-hub (RouterOS scripts – protocol ideas only), MM.Licensing.Client
(.NET in-process SDK for desktop/IIS apps), BFCheck/Inventory/SRPos (apps, no service agent),
adms (on-demand CLI), SPA-MEMBERSHIP (agent removed; ADRs 0037/0040/0044/0046 are design input).

**PMS note (owner 2026-11-18):** the shared agent also replaces the SMILE-PMS agent and must cover the
SMILE_FO, SMILE_BO and SMILE_POS databases (plus MM_PMS) — one `sql` connection pool per database,
shared by every module on the site.

## 2. Platform features (in `dl-agent` core) – merged best of all agents

| Feature | Best existing design | dl-agent |
|---|---|---|
| Service host + SCM recovery + failureflag + delayed start | site-agent, SRBackup | `dl-winsvc` scm; re-applied each start |
| In-process watchdog per job loop, panic recovery, backoff | HL `supervise.go`, PDA `ResilientService` | `supervisor.rs` (thread per module, catch_unwind, 1 s → 5 min) |
| External watchdog (hung service, stale heartbeat) | Arrival scheduled task | `watchdog.rs` + `service_watch.rs`: heartbeat file, `watchdog` sub-command every 5 min (schtasks / systemd timer), backoff 1 → 12 h |
| Enrollment: one-time code, Ed25519 key, proof of possession | PDA, site-agent, Membership (ADR-0037) | PROTOCOL §1–2 |
| Secrets: DPAPI LocalMachine + entropy, ACL-locked data dir, secrets via stdin only | site-agent, Arrival, Membership | `identity.rs` SecretStore |
| Single-active / superseded agent stops cleanly | EINVOICE `device_replaced`, HL 403, site-agent 4409 | `revoke` command + 401 `superseded` |
| Signed manifest, key rotation, anti-replay, never downgrade, `min_version` | Membership ADR-0044, HL keys | `manifest.rs`, `update.rs` |
| Staged swap, previous kept, probation, rollback after 3 boots, failed-version blacklist | EINVOICE ADR-0017, Arrival ADR-001 | `update.rs`; failure cap 3 per 24 h, 9 for good, `retry_update` (PROTOCOL §6.2) |
| Drain in-flight work before swap; maintenance window; urgent flag | Arrival LinkGate, Membership window | module `drain()`; site `update.window` / `update.auto`; signed manifest `urgent` / `required` (PROTOCOL §6.1) |
| Canary channel for demo tenants, gate, staged rollout % | Membership PILOT/GENERAL, HL rollout % | `channel.rs` + hub gate |
| Kill switch / pause / hold tenant | Membership ADR-0046, Arrival HoldTenants | hub `POST /admin/releases/:product/:version/pause` + `/resume` (never offered, canary or stable) and `POST/DELETE /admin/tenants/:tenant/hold` (tenant or site, one product or all); the operator's `update_now` still works (PROTOCOL §5.3) |
| Local opt-out | PDA `AutoUpdate=false`, EINVOICE `no-update` | `<data dir>/no-update` file: no self-update on `update_check`, `update_blocked: "local"` in the heartbeat, an info report `update_blocked_local` per start, a selftest warn row; `update_now` still works (PROTOCOL §6.5) |
| Structured problem reports, fingerprint dedup, durable spool | PDA site events, Arrival problems.json | `report.rs` (PROTOCOL §7) |
| Heartbeat with module health + metrics | PDA, Membership | PROTOCOL §3 |
| Remote config + last-known-good | HL policy cache, PDA runtime-config | `config.rs` (PROTOCOL §8) |
| Remote commands (restart, diagnostics, log level) | PDA `diag`, Membership `X-MM-Run-Diagnostics` | PROTOCOL §4 |
| Diagnostic bundle upload | PDA `diag`, Membership `diag-upload` | `collect_diagnostics` → scrubbed zip in R2 `diag/` (30 days), admin list/download |
| JSON logs, rotation, secret scrubbing | PDA Serilog, site-agent | `log` (10 MB × 5), `dl-log::scrub` |
| Proxy (env + Windows Internet Options), TLS 1.2+, TLS 1.0 fallback for SQL 2008 | EINVOICE ladder | `dl-http` `ProxyRules` (env, WinHTTP, Internet Options), `dl-tds` ladder with the rung saved by `sql-setup` |
| HTTP hub fallback host (DNS/TLS outage) | HL `smile-hl-hub.fly.dev` | `hub_urls` (`HUB_URLS`, allowlisted hosts, failover) |
| Installer: `irm … | iex`, SHA-256, idempotent re-run = upgrade/repair, uninstall/purge, Defender exclusion guidance | Membership install.ps1, Arrival Install.ps1 | one `install.ps1` served by the hub |

## 3. Module requirements (port list)

- **einvoice**: collect FO/POS via procs, TCVN3, VAT apportion, push, poll write-back + ack, trn-types,
  provider egress relay (allow-listed VNPT hosts), proc install with SQL 2008 rewrite. Built as `dl-agent-einvoice`: FO/POS collect, outbox, relay, write-back,
  switch from Go (`einvoice-enroll --switch`), hub checks (`status`, `selftest`, `collect_dry_run`, `writeback_probe`);
  Windows checks pending on the SR bench.
- **pda-sync**: TDS capture (open checks 2 s, closed ledger keyset, masters 120 s, shadow hashes), outbox
  2 GB with 424 parking, blobs, staff sync, exactly-once commands (`order.*`, `kds.*`, `reservation.*`,
  `sync.*`) in one SQL tx with log row, WebSocket + long-poll fallback, SignalR print relay (TCP 9100 +
  spooler RAW, crash-safe journal), SQL bootstrap/grants, legacy-export/archive/upload verbs. Phase A built as
  `dl-agent-pda-sync` (opt-in `modules.pda-sync`; tools `pda-enroll`, `pda-grants`; commands `status`, `selftest`,
  `capture_dry_run`); phase B (RT-0257, PR #64): `sync.verify` + repair by key, order/combo/table verbs exactly-once live on SQL 2017/2022,
  watchdog cloud link, site events as reports. Open: closed open-checks are not healed by verify (C# parity, owner
  decision), capture.db carry-over (B8, owner decision), Windows bench (B6) and printer (B7). Report:
  docs/plans/2026-11-19-pda-sync-b-report.md.
- **speaker**: SignalR `JoinRvc`, embedded WAV tones, SAPI vi/en, sound mode per RVC. Replace 365-day
  plaintext JWT with enrolled identity. Built as `dl-agent-speaker` (RT-0257): uses the pda-sync enrolment, no
  print relay. Live use needs the PDA cloud to accept a SyncAgent in `OrderHub.JoinRvc` and on
  `speaker/sound-mode`.
- **pms-site**: WSS stream with JWT, allow-listed ops on SMILE_FO/SMILE_BO/SMILE_POS only (`target_db` per op,
  SMILE-only contract of SMILE-PMS ADR 2026-11-19), ≤ 8 ops in flight, deadlock retry, result caps, local op
  journal for request-key replay; settings from core `sql.json` (`databases.json` only as fallback); commands
  `pms.status`, `pms.selftest`, `pms.op_dry_run`; ops `fo.hotelDate.get@1`, the clerk login `fo.clerk.login@1` + `fo.clerk.rights.get@1` and five FO reads (RT-0528). MM_PMS on site removed.
  Bench end-to-end pending.
- **pms-device**: claim/result polling 1 s with durable outbox, locks (LockSDK x86 → 32-bit helper process),
  ESC/POS TCP/serial/spooler, PC/SC, OPOS IDispatch, SAPI. Needs enrollment (today: manual token).
- **arrival**: HTTP/2 long poll with AES-GCM envelopes (HKDF per direction, ±120 s), snapshots/ETag,
  change watcher, LegalVault, proc bundle install, tenant master key wrap/unwrap, idempotency journal 48 h.
- **membership**: job pull/ack (Create/Modify/Cancel/Redeem/Earn/Availability/GrantReward/LinkFolio),
  checkout/availability/room-inventory/folio scanners with hub-confirmed cursors, backfill mode,
  DB identity fingerprint, 401 gate, older-hub fallbacks. Built as `dl-agent-membership`; RT-0256 (PR #63) added
  EarnPoints, the checkout / walk-in scanner + backfill tool, SMILE_SPECIAL extra beds and terms sync, and
  `membership-enroll --switch/--rollback` that keeps the .NET `config.dpapi`. Open: Windows DPAPI/SCM run and real
  SMILE_FO data at Victory.
- **hotellink**: job long-poll + local queue + re-ack, availability push, mapping commands, PMS circuit
  breaker, local config UI on 127.0.0.1:8088. Built as `dl-agent-hotellink`; RT-0259 (PR #66) added the SQL
  end-to-end tests (booking, ack replay after a crash, breaker), site `Encrypt=false;TrustServerCertificate=true`
  strings and a prompt stop.
- **backup**: cron schedules with tz, BACKUP/RESTORE, uploads (Drive/OneDrive/S3/Azure), retention,
  per-instance serialisation, bbolt-style job queue with leases + dead letter. **Fix: per-tenant keys
  instead of the shared global master key.** Built as `dl-agent-backup`; RT-0259 (PR #66) added the
  RESTORE WITH REPLACE end-to-end test and per-tenant sealing of uploads (agent side; SRBackup hub route pending).

## 4. Known risks to carry into the migration

- No agent's update/service path is verified on real Windows by automated tests; the fault build
  (`--fault probation`, PROTOCOL §6.6) is run on the SR bench instead. Every module switch needs a Windows
  smoke run on the demo tenant first.
- SRBackup hands every agent the same master key (cross-tenant exposure) — fix in the hub before porting.
  Status (RT-0259): credentials are already sealed under a per-agent key derived from the hub master key
  (SRBackup `f9ffb01`, `hub/src/lib/agentKey.ts`); backup files were never encrypted. dl-agent-backup now
  seals uploaded backups under a per-tenant data key; the hub endpoint is specified in
  `docs/plans/2026-11-19-rt0259-backup-tenant-keys.md` (SRBackup repo, not yet built).
- SMILE-HL and SRBackup allow downgrades; EINVOICE has no hash check at install; speaker token in plaintext.
- No code signing anywhere except Membership's self-signed Authenticode; antivirus (Kaspersky/Defender)
  untested on most.

## 4b. Coverage status (2026-11-19, task 644)

| Module | Crate | Ported | Verified on Linux (unit + live SQL) | Still open |
|---|---|---|---|---|
| core | `dl-agent`, `dl-tds`, `dl-http` | service, enrol, signed update + rollback, reports, remote config, canary gate, SQL ladder, survey, diagnostics, NTLMv2, named instance, TDS 7.3 (RT-0258) | yes; Windows build by cross clippy | Windows SSPI/SCM bench, SQL 2008 at Muine Bay Resort (RT-0252) |
| einvoice | `dl-agent-einvoice` | full | yes (SQL 2017/2022) | SR bench Windows checks |
| pms-site / pms-device | `dl-agent-pms`, `dl-agent-pms-device` | full | yes | device hardware (locks, printers) on bench |
| hotellink | `dl-agent-hotellink` | full | yes (RT-0259 e2e) | real kill -9, site UI |
| backup | `dl-agent-backup` | full + per-tenant sealing | yes (RT-0259 restore REPLACE) | SRBackup hub `backup-key` route |
| arrival | `dl-agent-arrival` | full | yes | Check-In pilot RT-0252 / task 646, 727 |
| membership | `dl-agent-membership` | full (RT-0256) | yes (SQL 2022) | Victory cutover on Windows |
| pda-sync | `dl-agent-pda-sync` | phase A + B (RT-0257) | yes (SQL 2017/2022) | B6/B7/B8, open-check healing decision |
| speaker | `dl-agent-speaker` | full, no print relay | fake hub only | PDA cloud change, real audio |

## 5. Migration order (all inside RT-0164 as tasks, one demo/canary tenant first each)

1. einvoice (Rust port already on DevLibrary) → 2. pms-site + pms-device (Rust ports exist; SMILE_FO/BO/POS)
→ 3. hotellink → 4. backup → 5. arrival → 6. membership → 7. pda-sync + speaker (largest).
Each step: module in `dl-agent`, demo tenant on canary, gate passes, then stable rollout, then the old
agent's installer upgrades in place to `dl-agent` and removes the old service.
