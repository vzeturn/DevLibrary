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
| 4 | SMILE-PMS | `apps/site-agent` (Go + Rust port on DevLibrary) | Go / Rust | **MM_PMS, SMILE_FO, SMILE_BO, SMILE_POS** (allow-listed procs, signed migrations) | `pms-site` |
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
| External watchdog (hung service, stale heartbeat) | Arrival scheduled task | built-in `watchdog` sub-command + scheduled task |
| Enrollment: one-time code, Ed25519 key, proof of possession | PDA, site-agent, Membership (ADR-0037) | PROTOCOL §1–2 |
| Secrets: DPAPI LocalMachine + entropy, ACL-locked data dir, secrets via stdin only | site-agent, Arrival, Membership | `identity.rs` SecretStore |
| Single-active / superseded agent stops cleanly | EINVOICE `device_replaced`, HL 403, site-agent 4409 | `revoke` command + 401 `superseded` |
| Signed manifest, key rotation, anti-replay, never downgrade, `min_version` | Membership ADR-0044, HL keys | `manifest.rs`, `update.rs` |
| Staged swap, previous kept, probation, rollback after 3 boots, failed-version blacklist | EINVOICE ADR-0017, Arrival ADR-001 | `update.rs` |
| Drain in-flight work before swap; maintenance window; urgent flag | Arrival LinkGate, Membership window | module `drain()` + manifest `urgent` (todo) |
| Canary channel for demo tenants, gate, staged rollout % | Membership PILOT/GENERAL, HL rollout % | `channel.rs` + hub gate |
| Kill switch / pause / hold tenant | Membership ADR-0046, Arrival HoldTenants | hub release `paused`, agent override |
| Local opt-out | PDA `AutoUpdate=false`, EINVOICE `no-update` | `no-update` file |
| Structured problem reports, fingerprint dedup, durable spool | PDA site events, Arrival problems.json | `report.rs` (PROTOCOL §7) |
| Heartbeat with module health + metrics | PDA, Membership | PROTOCOL §3 |
| Remote config + last-known-good | HL policy cache, PDA runtime-config | `config.rs` (PROTOCOL §8) |
| Remote commands (restart, diagnostics, log level) | PDA `diag`, Membership `X-MM-Run-Diagnostics` | PROTOCOL §4 |
| Diagnostic bundle upload | PDA `diag`, Membership `diag-upload` | `collect_diagnostics` |
| JSON logs, rotation, secret scrubbing | PDA Serilog, site-agent | `log` (10 MB × 5) |
| Proxy (env + Windows Internet Options), TLS 1.2+, TLS 1.0 fallback for SQL 2008 | EINVOICE ladder | `dl-http`, `dl-tds` |
| HTTP hub fallback host (DNS/TLS outage) | HL `smile-hl-hub.fly.dev` | hub URL list |
| Installer: `irm … | iex`, SHA-256, idempotent re-run = upgrade/repair, uninstall/purge, Defender exclusion guidance | Membership install.ps1, Arrival Install.ps1 | one `install.ps1` served by the hub |

## 3. Module requirements (port list)

- **einvoice**: collect FO/POS via procs, TCVN3, VAT apportion, push, poll write-back + ack, trn-types,
  provider egress relay (allow-listed VNPT hosts), proc install with SQL 2008 rewrite. Rust port exists → move.
- **pda-sync**: TDS capture (open checks 2 s, closed ledger keyset, masters 120 s, shadow hashes), outbox
  2 GB with 424 parking, blobs, staff sync, exactly-once commands (`order.*`, `kds.*`, `reservation.*`,
  `sync.*`) in one SQL tx with log row, WebSocket + long-poll fallback, SignalR print relay (TCP 9100 +
  spooler RAW, crash-safe journal), SQL bootstrap/grants, legacy-export/archive/upload verbs.
- **speaker**: SignalR `JoinRvc`, embedded WAV tones, SAPI vi/en, sound mode per RVC. Replace 365-day
  plaintext JWT with enrolled identity.
- **pms-site**: WSS stream with JWT, allow-listed procs on MM_PMS/SMILE_FO/SMILE_BO/SMILE_POS, ≤ 8 ops in
  flight, deadlock retry, result caps, signed migration bundles (backup gate, drift check, dry run),
  OpLog purge. `databases.json` must move to DPAPI. FO/BO/POS request keys still missing.
- **pms-device**: claim/result polling 1 s with durable outbox, locks (LockSDK x86 → 32-bit helper process),
  ESC/POS TCP/serial/spooler, PC/SC, OPOS IDispatch, SAPI. Needs enrollment (today: manual token).
- **arrival**: HTTP/2 long poll with AES-GCM envelopes (HKDF per direction, ±120 s), snapshots/ETag,
  change watcher, LegalVault, proc bundle install, tenant master key wrap/unwrap, idempotency journal 48 h.
- **membership**: job pull/ack (Create/Modify/Cancel/Redeem/Earn/Availability/GrantReward/LinkFolio),
  checkout/availability/room-inventory/folio scanners with hub-confirmed cursors, backfill mode,
  DB identity fingerprint, 401 gate, older-hub fallbacks.
- **hotellink**: job long-poll + local queue + re-ack, availability push, mapping commands, PMS circuit
  breaker, local config UI on 127.0.0.1:8088.
- **backup**: cron schedules with tz, BACKUP/RESTORE, uploads (Drive/OneDrive/S3/Azure), retention,
  per-instance serialisation, bbolt-style job queue with leases + dead letter. **Fix: per-tenant keys
  instead of the shared global master key.**

## 4. Known risks to carry into the migration

- No agent's update/service path is verified on real Windows by automated tests; Membership pilots are the
  only field evidence. Every module switch needs a Windows smoke run on the demo tenant first.
- SRBackup hands every agent the same master key (cross-tenant exposure) — fix in the hub before porting.
- SMILE-HL and SRBackup allow downgrades; EINVOICE has no hash check at install; speaker token in plaintext.
- No code signing anywhere except Membership's self-signed Authenticode; antivirus (Kaspersky/Defender)
  untested on most.

## 5. Migration order (all inside RT-0164 as tasks, one demo/canary tenant first each)

1. einvoice (Rust port already on DevLibrary) → 2. pms-site + pms-device (Rust ports exist; SMILE_FO/BO/POS)
→ 3. hotellink → 4. backup → 5. arrival → 6. membership → 7. pda-sync + speaker (largest).
Each step: module in `dl-agent`, demo tenant on canary, gate passes, then stable rollout, then the old
agent's installer upgrades in place to `dl-agent` and removes the old service.
