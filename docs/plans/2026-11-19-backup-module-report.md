# Report B2a: `backup` module in dl-agent

Brief: `docs/plans/2026-11-19-backup-module-brief.md`. Branch `feat/dl-agent-backup`, worktree
`~/Projects/DevLibrary-bk`. Not pushed, merged or deployed.

## Status

**DONE.** Every item of the brief is built and tested on macOS (host) with the
env-gated SQL Server 2022 run green; the Windows build is cross-compiled and linted, not run.
Drive / OneDrive are slice B2b as the brief says. Not pushed, merged or deployed.

## What was built

New crate `crates/dl-agent-backup` (about 12k lines, no third-party crates; largest file
547 lines). It ports the SRBackup Go agent as dl-agent module `backup`, plus a dl-tds
addition.

| Go package | Rust | Notes |
|---|---|---|
| `hubclient` (+ `decrypt.go`) | `hub/`, `crypto/` | self-register, config, master-key, report, discover, logs; AES-256-GCM + HKDF-SHA256 credentials, byte-compatible with the hub's `encryptCredential` (fixtures in `tests/fixtures/credentials.json`, made with `make-credentials.ts`). |
| `syncloop` (+ `masterkey.go`) | `sync/` | config poll with back-off to 5 min, master key in memory with one re-fetch on a failed decrypt, restore requests, commands (TestConnection, DiscoverDatabases), discovery on the first poll and every tenth, retention, report outbox. |
| `scheduler` | `schedule/` | 5-field cron in the schedule's time zone; a schedule that never ran is due at once and missed runs collapse into one, as in Go. |
| `executor` | `executor.rs` | N workers, one backup per SQL Server instance (gate), lease renewal, three attempts, dead letter reported once. |
| `queue` | `state/queue.rs` | durable, idempotent enqueue, leases; sealed (DPAPI machine scope on Windows). |
| `jobhistory` | `state/history.rs`, `state/runs.rs` | history for retention and `backup.status`; last run per schedule. |
| `backup` | `backup/pipeline.rs`, `backup/restore.rs` | connect → backup → checksum → upload → cleanup; restore from the local file or a downloaded cloud copy, `MOVE` to new names. |
| `uploader` | `upload/` | local folder, S3 / R2 / MinIO (SigV4, multipart > 16 MiB), Azure Blob (Shared Key or SAS, block lists > 32 MiB). Drive / OneDrive → `backup_destination_unsupported`. |
| `retention` | `retention.rs` | by age and count; destination copy, local file, history entry. |
| `discovery` | `discovery.rs` | user databases per server → `POST /discover`. |
| — | `enroll/` | `dl-agent backup-enroll --hub-url <origin> [--force]`, key from `DL_AGENT_BACKUP_KEY` or stdin. |
| — | `module/` | the dl-agent module: lifecycle, metrics, commands, stop. |
| — | `report.rs`, `seal.rs` | rate-limited, scrubbed problem reports; sealed state files. |

Dropped, as the brief says (dl-agent owns them): service host, `updater`, `logger`, `health`,
`metrics`, `hwid`, `secret`, `cmd/agent`. The hwid is a random UUID made at the first
enrollment and kept on re-enrollment.

`dl-tds`: `CancelToken` + `Client::set_cancel`, so module stop can cancel a running `BACKUP`
with a TDS attention (separate commit). `dl-agent-bin` registers the module and the tool
(one `use`, one line). `.cargo/config.toml`: the crate is left out of `cargo wasm` (it needs
sockets, files and threads, like the other agent modules). README for the crate, CHANGELOG
entries for both crates, dl-tds README.

## Module behaviour

- Opt-in: `Disabled` unless `modules.backup.enabled`.
- Not enrolled → `degraded("not enrolled")`, waits for stop or an enrollment (re-checked every
  30 s, so `backup-enroll` takes effect without a restart).
- `running` after the first successful config poll; `degraded` with the reason when the hub is
  still unreachable after one back-off or answers 401/403 (hint: `backup-enroll --force`).
- Stop: workers take no new job; a running `BACKUP` gets `stop_grace_seconds` (default 10 s),
  then `CancelToken` → ATTENTION; the job is released to the queue without spending an attempt.
- Metrics: `queue_depth`, `outbox_depth`, `jobs_ok`, `jobs_failed`, `dead_lettered`,
  `bytes_uploaded`, `last_success` (per database).
- Reports: `backup_hub_failed`, `backup_auth_failed`, `backup_sql_failed`,
  `backup_upload_failed`, `backup_credential_error`, `backup_destination_unsupported`,
  `backup_recovery_model` (+ `backup_schedule_invalid`, `backup_io_failed`), one per code and
  subject per 15 min, scrubbed of passwords, keys, signatures and tokens.
- Commands: `backup.status`, `backup.run-now {"scheduleId": …}` (also a bare string).

## Differences from Go, and why

1. **Compression fix**: Go always sent `WITH COMPRESSION`, which fails on Express, Web,
   Workgroup, 2005 and 2008 non-Enterprise. The connection probes `EngineEdition`, `Edition` and
   the version once and adds `COMPRESSION` only where supported, `CHECKSUM` from 2005 on; the
   choice is written into the `backup` step of the report.
2. **Log backup on SIMPLE recovery** stops before SQL Server is asked, with
   `backup_recovery_model` and a clear message (Go passed SQL Server's raw error).
3. **No on-disk import**: production has no SRBackup agents, so there is no `agent.json` /
   bbolt queue to migrate; the module starts clean (brief).
4. **Statement parameters** for the database name and paths instead of spliced text;
   identifiers that must be spliced are validated and bracket-quoted as in Go.
5. **Permanent failures** (SIMPLE recovery, unsupported destination, credential error, bad
   schedule) are reported and dropped at once; Go retried them three times.
6. **Credential errors are reported** to the hub as a Failed backup / restore / command with the
   reason; Go only logged them.
7. **Restores and commands run once** per request id; Go ran them on every poll until the hub
   saw the report.
8. **401/403 keeps polling** (degraded) instead of exiting, so a re-enrollment heals it.
9. **Azure cloud path without the SAS token** (Go stored the token in backup history).
10. **Outbox ack is body-checked**: a report is removed only if the stored body is still the
    one that was sent, so a newer report under the same key (dead letter after a failure) is
    never lost.
11. A remote server whose `.bak` the agent cannot read gets "written by SQL Server on … but is
    not reachable from the agent" (Go's semantics, clearer text).
12. `POST /api/agent/logs` is implemented in the client but, as in Go (`ShipLog` has no
    callers), nothing sends logs; dl-agent's own logging and problem reports cover it.

## Gaps

- Google Drive / OneDrive (OAuth refresh): slice B2b. The `Uploaders` trait takes a
  destination with decrypted tokens and returns a cloud path, which fits them.
- **Windows not run**: DPAPI sealing, SSPI and the service stop path were only cross-compiled
  and linted for `x86_64-pc-windows-gnu`, not executed.
- **Time zones**: no tz database (no third-party crates). The module knows the zones without
  daylight saving that SRBackup's sites use, fixed offsets (`+07:00`, `UTC+7`, `Etc/GMT-7`) and
  UTC; any other zone fails that schedule with `backup_schedule_invalid` rather than running at
  a shifted time. Go used the full IANA database.
- The 30 s enrollment re-check of a running module is not covered by a test (the interval is a
  constant).
- `crates/dl-agent-bin/src/main.rs`: the hotellink branch edits the same chain; expect a
  trivial merge conflict on the last `.module(...)` line.
- `crates/dl-tds/tests/integration.rs` was already 1180 lines (over 800) at HEAD; the new
  cancel test went into its own file, `tests/cancel.rs`, instead of growing it.

## Tests

All run on the iMac (x86_64 macOS) through `~/.claude/scripts/heavy.sh`, worktree
`~/Projects/DevLibrary-bk`.

| Command | Result |
|---|---|
| `cargo fmt --all` | clean |
| `cargo test -p dl-agent-backup -p dl-agent-bin` | 191 passed, 0 failed (dl-agent-bin has no tests of its own; it builds with the module) |
| `BACKUP_AGENT_TEST_SQL="server=127.0.0.1,14334;user id=sa;password=…;encrypt=disable" BACKUP_AGENT_TEST_DIR=<bind mount> cargo test -p dl-agent-backup --lib sql_tests` | 3 passed against `mcr.microsoft.com/mssql/server:2022-latest` (not skipped) |
| `DL_TDS_TEST_URL="sqlserver://sa:…@127.0.0.1:14334?encrypt=disable" cargo test -p dl-tds` | 67 unit + 18 integration + 1 cancel + the other suites, all passed (not skipped) |
| `cargo clippy -p dl-agent-backup -p dl-agent-bin -p dl-tds --all-targets -- -D warnings` | clean |
| same with `--target x86_64-pc-windows-gnu` | clean |
| `cargo build -p dl-tds --target wasm32-unknown-unknown --profile min-size` | builds (dl-tds stays in `cargo wasm`; dl-agent-backup is excluded) |

The 191 `dl-agent-backup` tests by area: backup pipeline / restore / step reports 36 (3 of them
the env-gated SQL Server tests), uploaders 33 (SigV4 against the worked examples in the AWS S3 SigV4 documentation, Azure
Shared Key / SAS against fixed vectors, multipart / block-list and abort paths), state (queue,
history, runs, outbox) 24, crypto (HKDF, AES-GCM, hub credential fixtures) 16, sync loop against
the fake hub 15 (enroll → config → job → report, master-key rotation with one re-fetch,
`credentialError`, hub down → outbox → replay, 401 → degraded, restores, commands, retention),
schedule (cron, time zones, missed runs) 13, SQL options / quoting 10, executor 10 (gate, lease,
retries, dead letter once, stop releases the job), module lifecycle 9 (disabled, degraded until
enrolled, running after the first poll, stop grace then cancel, metrics, `backup.status`,
`backup.run-now`), enroll 8 (env or stdin, never echoed, `--force`, other hub refused, bad
origins, unreachable hub stores nothing), retention 4, hub client 4, reports 3, seal 2, error 2,
discovery 2. The Go unit tests of `scheduler`, `queue`, `retention`, `backup`, `uploader`,
`hubclient` and `syncloop` are ported into these.

SQL Server run (2022, Developer edition in the container):
- full backup with `COMPRESSION, CHECKSUM`, checked in `msdb.dbo.backupset`
  (`has_backup_checksums = 1`, compressed size below raw size);
- the same database through a connection that reports Express: `no COMPRESSION`, checksums
  only, compressed size equal to raw size;
- a log backup on FULL recovery succeeds; one on SIMPLE fails with `backup_recovery_model`
  before SQL Server is asked;
- `RESTORE … WITH REPLACE` to a new name next to its still-online source (files moved), twice,
  the row checked in both databases.
- dl-tds: `CancelToken` during `WAITFOR DELAY '00:00:10'` returns `Error::Cancelled` in well
  under 5 s, the connection is not broken and works again with a fresh token.

Fixed while testing: two test assertions (a test that held the fake SQL mutex twice and hung,
a dead-lettered job looked up only among pending ones), four clippy findings, and the SQL test
helpers (dynamic SQL through variables; `encrypt=disable` because dl-tds has no TLS backend on
macOS). The test container and its files were removed afterwards.

Not run:
- anything on Windows (DPAPI, SSPI, service stop path): no Windows machine in this slice;
- the full workspace test suite and `cargo check-all`: the main session runs the gates for the
  whole PR;
- a real S3 / R2 / Azure upload (vectors and a local fake server only).

## Commits

- `97f5283` feat(dl-tds): cancel a running request with a CancelToken
- `34419a4` feat(dl-agent-backup): port the SRBackup agent as dl-agent module backup (with the
  brief)
- this report (docs commit on top)
