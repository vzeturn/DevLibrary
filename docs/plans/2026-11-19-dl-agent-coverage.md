# dl-agent coverage wave (RT-0164 task 599) — plan

Owner 2026-11-19: "cover all projects first, especially the recent problems" before moving any
project (task 557). Source: survey of recent field fixes in every agent repo (EINVOICE, PDAOrder,
PMS, Arrival, Membership, HL, SRBackup) and INVENTORY.md §2–4. Each slice = one PR on main,
`cargo test -p dl-agent` + clippy (host and `--target x86_64-pc-windows-gnu`) green.

| # | Slice | Covers (recent problem → source) | State |
|---|-------|----------------------------------|-------|
| C1 | Windows builds: x64, x86, Win7/2008R2 x64+x86 (docker mingw, static), `current_target` = `windows7-*` on Win7 builds, wine smoke | old Windows + x86 lock SDK (PMS a36bc9ffd, EINVOICE 544e419, Arrival 34a987d0) | done |
| C2 | SQL transport ladder TLS1.2 → TLS1.0 → off (also on reset after handshake), rung remembered + reported; `SET LOCK_TIMEOUT` + command timeout defaults | SQL 2008 R2 RTM reset (EINVOICE e374901), `tlsmin` (PMS ac0f22eac), locked rows hang (Arrival 4f226dc8) | done |
| C3 | Site survey: OS build/arch/Win7, per DB engine version/edition/compat level/collations/rung/grants; `site.survey` command + report on start/daily | legacy site variety (PDAOrder ADR 0093/0094), missing grants (Arrival d46a8bbe, PDA ADR 0108) | todo |
| C4 | Schema probe: requirement JSON (tables, columns, procs, functions, config rows) → missing list; `sql.probe`; SQL 207/208/2812 classified as `MissingObject` | missing columns/procs (Arrival 3cd56f27, PDA 081aad2e, EINVOICE 7025be5, PMS 05a8a16e3) | todo |
| C5 | Signed SQL bundle installer: Ed25519 signature, T-SQL 2008 guard (reject 2012+ syntax when engine < 11 / compat < 110, rewrite CREATE OR ALTER), backup OBJECT_DEFINITION, dry run (tx rollback), apply in tx, local ledger, grants, report; install-and-retry-once on 2812 | proc install per site (EINVOICE b7ee7ea, Arrival c9736703, PMS signed migrations) | todo |
| C6 | `dl-vn` TCVN3 ⇄ Unicode, CHAR padding trim helper | SMILE text (EINVOICE tcvn3.rs), padding (PDA ADR 0123) | todo |
| C7 | Update hardening: failure cap per 24 h + lifetime 9, `urgent` bypasses window, hub-required build, heartbeat file + `watchdog` sub-command, no-cache on artifacts | Membership 309e72114, Arrival 364aad38/7882b43e, PDA d2d2656f | todo |
| C8 | `install.ps1` from the hub: `irm | iex`, SHA-256, re-run keeps identity, removes old copies, clock check, waits for the link, Defender exclusion + Kaspersky note, OS gate → win7 artifact, remote SQL prompt | PDA da7402e9/d2d2656f, Arrival 02aed898/587d0ee7, EINVOICE 544e419 | todo |
| C9 | Hub: store survey, `site_eligible` gate check per product requirements, admin view | gate blocks sites not ready | todo |

Not testable here (say so in deliver): real Windows 7 / Server 2008 R2 hosts and SQL Server
2008/2012 engines (docker has 2017+ only); wine smoke and TDS unit tests stand in.
